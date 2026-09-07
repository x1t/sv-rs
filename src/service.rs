//! Linux 系统服务管理(镜像 Go `pkg/supervisor/service_manager.go`,去掉跨平台层)。
//!
//! 双后端:优先 systemd(unit 文件 + systemctl),否则回退 SysV(/etc/init.d 脚本)。
//! 目录与后端均可在测试中注入,避免单元测试触碰真实系统路径。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::service_files::{
    SERVICE_NAME, ServiceBackend, ServiceStatus, detect_backend, make_symlink,
    resolve_own_executable, systemd_unit_text, sysv_init_text, wait_for_signal,
};
use crate::util::{io_context, run_command};

const SYMLINK_DEFAULT: &str = "/usr/local/bin/sv";
const SYSTEMD_DIR_DEFAULT: &str = "/etc/systemd/system";
const INIT_DIR_DEFAULT: &str = "/etc/init.d";

/// 服务管理器:持有可执行文件、软链接与后端的落地路径。
pub struct ServiceManager {
    executable: String,
    symlink_path: PathBuf,
    unit_path: PathBuf,
    init_script_path: PathBuf,
    backend: ServiceBackend,
}

impl ServiceManager {
    /// 使用真实环境构造:解析当前可执行文件并探测后端。
    pub fn new() -> Self {
        let executable = resolve_own_executable();
        ServiceManager {
            executable,
            symlink_path: PathBuf::from(SYMLINK_DEFAULT),
            unit_path: PathBuf::from(SYSTEMD_DIR_DEFAULT).join(format!("{SERVICE_NAME}.service")),
            init_script_path: PathBuf::from(INIT_DIR_DEFAULT).join(SERVICE_NAME),
            backend: detect_backend(),
        }
    }

    /// 为测试注入全部落地路径与后端。
    #[cfg(test)]
    pub fn for_testing(
        executable: String,
        symlink_path: PathBuf,
        unit_path: PathBuf,
        init_script_path: PathBuf,
        backend: ServiceBackend,
    ) -> Self {
        ServiceManager {
            executable,
            symlink_path,
            unit_path,
            init_script_path,
            backend,
        }
    }

    /// 处理 `sv service <action>` 子命令。
    pub fn handle_command(&self, args: &[String], out: &mut dyn Write) -> Result<(), String> {
        if args.is_empty() {
            print_service_usage(out)?;
            return Err("缺少服务操作".to_string());
        }
        if args.len() > 1 {
            return Err(format!("服务操作不接受额外参数: {}", args[1..].join(" ")));
        }
        match args[0].as_str() {
            "install" => self.install(out),
            "uninstall" => self.uninstall(out),
            "start" => self.start(out),
            "stop" => self.stop(out),
            "restart" => self.restart(out),
            "status" => self.status(out),
            other => {
                print_service_usage(out)?;
                Err(format!("未知服务操作: {other}"))
            }
        }
    }

    /// 安装系统服务并创建命令软链接。
    fn install(&self, out: &mut dyn Write) -> Result<(), String> {
        write_line(out, "🔧 正在安装 SV 系统服务...")?;
        match self.backend {
            ServiceBackend::Systemd => {
                self.write_unit_file()
                    .map_err(|e| format!("安装服务失败: {e}"))?;
                let _ = self.run_systemctl(&["daemon-reload"]);
                self.run_systemctl(&["enable", &self.unit_file_name()])
                    .map_err(|e| format!("安装服务失败: {e}"))?;
            }
            ServiceBackend::SysV => {
                self.write_init_script()
                    .map_err(|e| format!("安装服务失败: {e}"))?;
            }
        }
        self.create_symlink(out)
            .map_err(|e| format!("服务已安装，但创建命令软链接失败: {e}"))?;
        write_line(out, "✅ SV 系统服务安装成功")
    }

    /// 卸载系统服务并移除本程序创建的软链接。
    fn uninstall(&self, out: &mut dyn Write) -> Result<(), String> {
        match self.backend {
            ServiceBackend::Systemd => {
                let _ = self.run_systemctl(&["disable", &self.unit_file_name()]);
                if self.unit_path.exists() {
                    fs::remove_file(&self.unit_path).map_err(|e| io_context("卸载服务失败", e))?;
                }
                let _ = self.run_systemctl(&["daemon-reload"]);
            }
            ServiceBackend::SysV => {
                if self.init_script_path.exists() {
                    fs::remove_file(&self.init_script_path)
                        .map_err(|e| io_context("卸载服务失败", e))?;
                }
            }
        }
        self.remove_symlink(out)
            .map_err(|e| format!("服务已卸载，但移除命令软链接失败: {e}"))?;
        write_line(out, "✅ SV 系统服务卸载成功")
    }

    /// 启动系统服务。
    fn start(&self, out: &mut dyn Write) -> Result<(), String> {
        self.service_control("start")
            .map_err(|e| format!("启动服务失败: {e}"))?;
        write_line(out, "✅ SV 系统服务启动成功")
    }

    /// 停止系统服务。
    fn stop(&self, out: &mut dyn Write) -> Result<(), String> {
        self.service_control("stop")
            .map_err(|e| format!("停止服务失败: {e}"))?;
        write_line(out, "✅ SV 系统服务停止成功")
    }

    /// 重启系统服务。
    fn restart(&self, out: &mut dyn Write) -> Result<(), String> {
        self.service_control("restart")
            .map_err(|e| format!("重启服务失败: {e}"))?;
        write_line(out, "✅ SV 系统服务重启成功")
    }

    /// 查询系统服务状态。
    fn status(&self, out: &mut dyn Write) -> Result<(), String> {
        let status = match self.backend {
            ServiceBackend::Systemd => {
                let active = self
                    .run_systemctl(&["is-active", &self.unit_file_name()])
                    .map_err(|e| format!("获取服务状态失败: {e}"))?;
                match active.trim() {
                    "active" => ServiceStatus::Running,
                    "inactive" => ServiceStatus::Stopped,
                    _ => ServiceStatus::Unknown,
                }
            }
            ServiceBackend::SysV => {
                let outcome = run_command(
                    self.init_script_path.to_str().unwrap_or_default(),
                    &[String::from("status")],
                    Duration::from_secs(15),
                )
                .map_err(|e| format!("获取服务状态失败: {e}"))?;
                match outcome.code {
                    Some(0) => ServiceStatus::Running,
                    Some(3) => ServiceStatus::Stopped,
                    Some(_) => ServiceStatus::Unknown,
                    None => return Err("获取服务状态失败: signal: killed".to_string()),
                }
            }
        };
        let text = match status {
            ServiceStatus::Running => "✅ 运行中",
            ServiceStatus::Stopped => "⏸️ 已停止",
            ServiceStatus::Unknown => "❓ 未知状态",
        };
        write_line(out, &format!("SV 系统服务状态: {text}"))
    }

    /// 以服务守护进程方式常驻,响应 SIGTERM/SIGINT 优雅退出。
    pub fn run_daemon(&self, out: &mut dyn Write) -> Result<(), String> {
        if self.executable.is_empty() {
            return Err("服务程序未初始化".to_string());
        }
        write_line(out, "SV服务已启动，正在后台运行...")?;
        wait_for_signal()?;
        write_line(out, "SV服务已停止")
    }

    fn unit_file_name(&self) -> String {
        self.unit_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("{SERVICE_NAME}.service"))
    }

    fn write_unit_file(&self) -> Result<(), String> {
        let directory = self.unit_path.parent().ok_or("unit目录缺失")?;
        fs::create_dir_all(directory).map_err(|e| io_context("创建服务目录", e))?;
        let content = systemd_unit_text(&self.executable);
        fs::write(&self.unit_path, content).map_err(|e| io_context("写入服务单元文件", e))
    }

    fn write_init_script(&self) -> Result<(), String> {
        let directory = self.init_script_path.parent().ok_or("init.d目录缺失")?;
        fs::create_dir_all(directory).map_err(|e| io_context("创建服务目录", e))?;
        let content = sysv_init_text(&self.executable);
        fs::write(&self.init_script_path, content).map_err(|e| io_context("写入服务脚本", e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.init_script_path, fs::Permissions::from_mode(0o755))
                .map_err(|e| io_context("设置服务脚本权限", e))?;
        }
        Ok(())
    }

    /// 执行 systemctl 子命令;非零退出视为失败。
    fn run_systemctl(&self, args: &[&str]) -> Result<String, String> {
        let arguments: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let outcome = run_command("systemctl", &arguments, Duration::from_secs(30))
            .map_err(|e| format!("systemctl: {e}"))?;
        let text = outcome.combined_text();
        match outcome.code {
            Some(0) => Ok(text),
            Some(code) => Err(format!(
                "systemctl {}: exit status {code} ({})",
                arguments.join(" "),
                text.trim()
            )),
            None => Err(format!("systemctl {}: signal: killed", arguments.join(" "))),
        }
    }

    fn service_control(&self, action: &str) -> Result<(), String> {
        match self.backend {
            ServiceBackend::Systemd => self
                .run_systemctl(&[action, &self.unit_file_name()])
                .map(|_| ()),
            ServiceBackend::SysV => {
                let script = self.init_script_path.to_str().unwrap_or_default();
                let outcome = run_command(script, &[action.to_string()], Duration::from_secs(30))
                    .map_err(|e| format!("{script} {action}: {e}"))?;
                let text = outcome.combined_text();
                match outcome.code {
                    Some(0) => Ok(()),
                    Some(code) => Err(format!("exit status {code} ({})", text.trim())),
                    None => Err(format!("signal: killed ({})", text.trim())),
                }
            }
        }
    }

    /// 创建命令软链接(镜像 Go `createSymlink`)。
    fn create_symlink(&self, out: &mut dyn Write) -> Result<(), String> {
        self.require_executable()?;
        let directory = self
            .symlink_path
            .parent()
            .ok_or_else(|| "软链接路径不能为空".to_string())?;
        fs::create_dir_all(directory).map_err(|e| io_context("创建软链接目录", e))?;

        match fs::symlink_metadata(&self.symlink_path) {
            Ok(info) => {
                if !info.file_type().is_symlink() {
                    return Err(format!(
                        "目标路径已存在且不是软链接: {}",
                        self.symlink_path.display()
                    ));
                }
                let resolved_target = fs::canonicalize(&self.symlink_path).ok();
                let resolved_executable = fs::canonicalize(&self.executable).ok();
                if resolved_target == resolved_executable {
                    return Ok(());
                }
                Err(format!(
                    "目标软链接已存在且指向其他文件: {}",
                    self.symlink_path.display()
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                make_symlink(Path::new(&self.executable), &self.symlink_path)
                    .map_err(|e| io_context("创建软链接", e))?;
                write_line(
                    out,
                    &format!(
                        "🔗 已创建软链接: {} -> {}",
                        self.symlink_path.display(),
                        self.executable
                    ),
                )
            }
            Err(error) => Err(io_context("检查软链接目标", error)),
        }
    }

    /// 移除由本程序创建的软链接(镜像 Go `removeSymlink`)。
    fn remove_symlink(&self, out: &mut dyn Write) -> Result<(), String> {
        self.require_executable()?;
        match fs::symlink_metadata(&self.symlink_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_context("检查软链接", error)),
            Ok(info) => {
                if !info.file_type().is_symlink() {
                    return Err(format!(
                        "拒绝删除非软链接路径: {}",
                        self.symlink_path.display()
                    ));
                }
                let resolved_target = fs::canonicalize(&self.symlink_path)
                    .map_err(|e| io_context("解析软链接", e))?;
                let resolved_executable = fs::canonicalize(&self.executable)
                    .map_err(|e| io_context("解析可执行文件", e))?;
                if resolved_target != resolved_executable {
                    return Err(format!(
                        "拒绝删除指向其他文件的软链接: {}",
                        self.symlink_path.display()
                    ));
                }
                fs::remove_file(&self.symlink_path).map_err(|e| io_context("删除软链接", e))?;
                write_line(
                    out,
                    &format!("✅ 已删除软链接: {}", self.symlink_path.display()),
                )
            }
        }
    }

    fn require_executable(&self) -> Result<(), String> {
        if !self.executable.is_empty() {
            return Ok(());
        }
        let executable = resolve_own_executable();
        if executable.is_empty() {
            return Err("获取可执行文件路径失败".to_string());
        }
        let info = fs::metadata(&executable).map_err(|e| io_context("获取可执行文件状态", e))?;
        if info.is_dir() {
            return Err(format!("可执行文件路径指向目录: {executable}"));
        }
        Ok(())
    }
}

impl Default for ServiceManager {
    fn default() -> Self {
        Self::new()
    }
}

/// 打印服务子命令用法(镜像 Go `printServiceUsage`)。
fn print_service_usage(out: &mut dyn Write) -> Result<(), String> {
    write_line(
        out,
        "用法: sv service <action>\n\n可用操作:\n  install   安装 sv 为系统服务\n  uninstall 卸载 sv 系统服务\n  start     启动 sv 系统服务\n  stop      停止 sv 系统服务\n  restart   重启 sv 系统服务\n  status    查看 sv 服务状态",
    )
}

fn write_line(out: &mut dyn Write, text: &str) -> Result<(), String> {
    out.write_all(text.as_bytes())
        .and_then(|_| out.write_all(b"\n"))
        .map_err(|e| io_context("写入服务输出", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 在临时目录内生成一个真实可执行文件(镜像 std::env::current_exe 解析结果),
    /// 避免 canonicalize 指向不存在的路径。
    fn real_executable(dir: &Path) -> String {
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let file = bin.join("sv-real");
        fs::write(&file, "#!/bin/sh\n").unwrap();
        fs::canonicalize(&file)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    fn temp_manager(backend: ServiceBackend) -> (tempfile::TempDir, ServiceManager, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let executable = real_executable(dir.path());
        let symlink = PathBuf::from(dir.path()).join("bin").join("sv");
        let unit = dir
            .path()
            .join("systemd")
            .join(format!("{SERVICE_NAME}.service"));
        let script = dir.path().join("init.d").join(SERVICE_NAME);
        let manager =
            ServiceManager::for_testing(executable, symlink.clone(), unit, script, backend);
        (dir, manager, symlink)
    }

    fn capture(out: &mut Vec<u8>) -> &mut dyn Write {
        out
    }

    #[test]
    fn test_usage_and_unknown_operation() {
        let (_dir, manager, _) = temp_manager(ServiceBackend::SysV);
        let mut out = Vec::new();
        let error = manager.handle_command(&[], capture(&mut out)).unwrap_err();
        assert_eq!(error, "缺少服务操作");
        assert!(String::from_utf8_lossy(&out).contains("用法: sv service <action>"));

        let mut out = Vec::new();
        let error = manager
            .handle_command(
                &["install".to_string(), "extra".to_string()],
                capture(&mut out),
            )
            .unwrap_err();
        assert_eq!(error, "服务操作不接受额外参数: extra");

        let mut out = Vec::new();
        let error = manager
            .handle_command(&["bogus".to_string()], capture(&mut out))
            .unwrap_err();
        assert_eq!(error, "未知服务操作: bogus");
    }

    #[test]
    fn test_install_uninstall_sysv_with_symlink() {
        let (_dir, manager, symlink) = temp_manager(ServiceBackend::SysV);
        let mut out = Vec::new();

        manager.install(capture(&mut out)).unwrap();
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("✅ SV 系统服务安装成功"), "{text}");
        assert!(manager.init_script_path.exists());
        assert!(symlink.symlink_metadata().unwrap().file_type().is_symlink());

        let mut out = Vec::new();
        manager.uninstall(capture(&mut out)).unwrap();
        assert!(!manager.init_script_path.exists());
        assert!(!symlink.exists());
    }

    #[test]
    fn test_sysv_script_runs_and_status() {
        let (_dir, manager, _) = temp_manager(ServiceBackend::SysV);
        manager.write_init_script().unwrap();
        // 直接运行脚本(真实进程)验证可执行与 stop 幂等。
        let outcome = run_command(
            manager.init_script_path.to_str().unwrap(),
            &["status".to_string()],
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(outcome.code, Some(3));
        assert!(outcome.stdout_text().contains("not running"));
    }

    #[test]
    fn test_symlink_refuses_non_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let executable = real_executable(dir.path());
        // 占用软链接路径写入一个普通文件。
        let symlink = dir.path().join("occupied");
        fs::write(&symlink, "plain file").unwrap();
        let manager = ServiceManager::for_testing(
            executable,
            symlink,
            dir.path().join("unit.service"),
            dir.path().join("init"),
            ServiceBackend::SysV,
        );
        let mut out = Vec::new();
        let error = manager.create_symlink(capture(&mut out)).unwrap_err();
        assert!(error.contains("目标路径已存在且不是软链接"), "{error}");
    }

    #[test]
    fn test_remove_symlink_refuses_foreign() {
        let dir = tempfile::tempdir().unwrap();
        let executable = real_executable(dir.path());
        let symlink = dir.path().join("sv");
        let manager = ServiceManager::for_testing(
            executable,
            symlink.clone(),
            dir.path().join("unit.service"),
            dir.path().join("init"),
            ServiceBackend::SysV,
        );
        // 软链接指向真实可执行文件以外的文件,应拒绝删除。
        let other = dir.path().join("other");
        fs::write(&other, "x").unwrap();
        make_symlink(&other, &symlink).unwrap();
        let mut out = Vec::new();
        let error = manager.remove_symlink(capture(&mut out)).unwrap_err();
        assert!(error.contains("拒绝删除指向其他文件的软链接"), "{error}");
    }
}
