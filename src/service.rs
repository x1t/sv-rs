//! Linux 系统服务管理(镜像 Go `pkg/supervisor/service_manager.go`,去掉跨平台层)。
//!
//! 双后端:优先 systemd(unit 文件 + systemctl),否则回退 SysV(/etc/init.d 脚本)。
//! 目录与后端均可在测试中注入,避免单元测试触碰真实系统路径。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::service_files::{
    SERVICE_NAME, ServiceBackend, ServiceStatus, clean_path, create_sysv_runlevel_links,
    detect_backend, remove_sysv_runlevel_links, resolve_own_executable, systemd_unit_text,
    sysv_init_text, sysv_runlevel_link_paths, wait_for_signal,
};
use crate::util::{io_context, run_command};

const SYMLINK_DEFAULT: &str = "/usr/local/bin/sv";
const SYSTEMD_DIR_DEFAULT: &str = "/etc/systemd/system";
const INIT_DIR_DEFAULT: &str = "/etc/init.d";

/// 安装过程中创建的落地资产,用于失败回滚。
enum ServiceAsset {
    Unit(PathBuf),
    Init(PathBuf),
}

/// 服务管理器:持有可执行文件、软链接与后端的落地路径。
/// 字段以 pub(crate) 暴露,供同 crate 的 service_links 扩展 impl 使用。
pub struct ServiceManager {
    pub(crate) executable: String,
    pub(crate) symlink_path: PathBuf,
    pub(crate) unit_path: PathBuf,
    pub(crate) init_script_path: PathBuf,
    pub(crate) backend: ServiceBackend,
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

    /// 安装系统服务并创建命令软链接。任何已存在的服务文件或软链接都会拒绝覆盖。
    fn install(&self, out: &mut dyn Write) -> Result<(), String> {
        write_line(out, "🔧 正在安装 SV 系统服务...")?;
        let mut created_assets = Vec::new();
        match self.backend {
            ServiceBackend::Systemd => {
                self.write_unit_file()
                    .map_err(|e| format!("安装服务失败: {e}"))?;
                created_assets.push(ServiceAsset::Unit(self.unit_path.clone()));
                if let Err(error) = self.run_systemctl(&["daemon-reload"]) {
                    self.rollback_assets(&created_assets);
                    return Err(format!("安装服务失败: {error}"));
                }
                if let Err(error) = self.run_systemctl(&["enable", &self.unit_file_name()]) {
                    self.rollback_assets(&created_assets);
                    return Err(format!("安装服务失败: {error}"));
                }
            }
            ServiceBackend::SysV => {
                self.write_init_script()
                    .map_err(|e| format!("安装服务失败: {e}"))?;
                created_assets.push(ServiceAsset::Init(self.init_script_path.clone()));
                if let Err(error) = create_sysv_runlevel_links(&self.init_script_path) {
                    self.rollback_assets(&created_assets);
                    return Err(format!("安装服务失败: {error}"));
                }
            }
        }
        match self.create_symlink(out) {
            Ok(()) => write_line(out, "✅ SV 系统服务安装成功"),
            Err(error) => {
                self.rollback_assets(&created_assets);
                Err(format!("服务已安装，但创建命令软链接失败: {error}"))
            }
        }
    }

    /// 回滚本次安装已创建的服务资产(不触碰先前已存在的路径)。
    /// systemd 需先 disable(清除 .wants 链接)、删除 unit 后再 daemon-reload,
    /// 才能与 uninstall 一致地恢复原状;SysV 需清理 runlevel 链接与脚本。
    fn rollback_assets(&self, created: &[ServiceAsset]) {
        if self.backend == ServiceBackend::Systemd {
            let _ = self.run_systemctl(&["disable", &self.unit_file_name()]);
        }
        for asset in created {
            match asset {
                ServiceAsset::Unit(path) | ServiceAsset::Init(path) => {
                    let _ = fs::remove_file(path);
                }
            }
        }
        if self.backend == ServiceBackend::SysV {
            let _ = remove_sysv_runlevel_links(&self.init_script_path);
        }
        if self.backend == ServiceBackend::Systemd {
            let _ = self.run_systemctl(&["daemon-reload"]);
        }
        // 回滚只清理本次创建的软链:以当前可执行路径为候选,指向他处的一律不动。
        let candidates = if self.executable.is_empty() {
            Vec::new()
        } else {
            vec![self.executable.clone()]
        };
        let _ = self.remove_symlink(&mut std::io::sink(), &candidates);
    }

    /// 卸载系统服务并移除本程序创建的软链接。
    ///
    /// 幂等:服务本就未安装(unit/init 均不存在)时,若有残留命令软链则尽力清理,
    /// 否则输出「服务未安装,无需卸载」并返回成功。
    fn uninstall(&self, out: &mut dyn Write) -> Result<(), String> {
        let service_file_present = match self.backend {
            ServiceBackend::Systemd => path_lstat(&self.unit_path).is_some(),
            ServiceBackend::SysV => path_lstat(&self.init_script_path).is_some(),
        };

        if !service_file_present {
            // 未注册:仍清理本程序可能残留的 rc 启停链接与命令软链;全无则 no-op。
            let mut cleaned_anything = false;
            if self.backend == ServiceBackend::SysV
                && sysv_runlevel_link_paths(&self.init_script_path)
                    .iter()
                    .any(|link| path_lstat(link).is_some())
                && remove_sysv_runlevel_links(&self.init_script_path).is_ok()
            {
                cleaned_anything = true;
            }
            let owned = self.uninstall_symlink_owned();
            match fs::symlink_metadata(&self.symlink_path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(io_context("检查软链接", error)),
                Ok(_) => {
                    self.remove_symlink(out, &owned)?;
                    cleaned_anything = true;
                }
            }
            if cleaned_anything {
                write_line(out, "✅ 服务未安装，已清理残留文件/软链接")
            } else {
                write_line(out, "服务未安装，无需卸载")
            }
        } else {
            // 先记录服务文件里的可执行路径,再删除服务文件,供命令软链归属判定。
            let owned = self.uninstall_symlink_owned();
            match self.backend {
                ServiceBackend::Systemd => {
                    self.run_systemctl(&["stop", &self.unit_file_name()])
                        .map_err(|e| format!("卸载服务失败: {e}"))?;
                    self.run_systemctl(&["disable", &self.unit_file_name()])
                        .map_err(|e| format!("卸载服务失败: {e}"))?;
                    if let Some(info) = path_lstat(&self.unit_path) {
                        if info.is_dir() {
                            return Err(format!("拒绝删除目录: {}", self.unit_path.display()));
                        }
                        fs::remove_file(&self.unit_path)
                            .map_err(|e| io_context("卸载服务失败", e))?;
                    }
                    self.run_systemctl(&["daemon-reload"])
                        .map_err(|e| format!("卸载服务失败: {e}"))?;
                }
                ServiceBackend::SysV => {
                    if path_lstat(&self.init_script_path).is_some() {
                        self.service_control("stop")
                            .map_err(|e| format!("卸载服务失败: {e}"))?;
                    }
                    remove_sysv_runlevel_links(&self.init_script_path)
                        .map_err(|e| format!("卸载服务失败: {e}"))?;
                    if let Some(info) = path_lstat(&self.init_script_path) {
                        if info.is_dir() {
                            return Err(format!(
                                "拒绝删除目录: {}",
                                self.init_script_path.display()
                            ));
                        }
                        fs::remove_file(&self.init_script_path)
                            .map_err(|e| io_context("卸载服务失败", e))?;
                    }
                }
            }
            self.remove_symlink(out, &owned)
                .map_err(|e| format!("服务已卸载，但移除命令软链接失败: {e}"))?;
            write_line(out, "✅ SV 系统服务卸载成功")
        }
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

    /// 拒绝写入已存在的路径(含软链接),避免覆盖既有服务文件或跟随软链接写向别处。
    fn ensure_absent(path: &Path) -> Result<(), String> {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_context("检查目标路径", error)),
            Ok(_) => Err(format!("目标路径已存在: {}", path.display())),
        }
    }

    fn write_unit_file(&self) -> Result<(), String> {
        Self::ensure_absent(&self.unit_path)?;
        let directory = self.unit_path.parent().ok_or("unit目录缺失")?;
        fs::create_dir_all(directory).map_err(|e| io_context("创建服务目录", e))?;
        let content = systemd_unit_text(&self.executable);
        fs::write(&self.unit_path, content).map_err(|e| io_context("写入服务单元文件", e))
    }

    fn write_init_script(&self) -> Result<(), String> {
        Self::ensure_absent(&self.init_script_path)?;
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
}

/// 打印服务子命令用法(镜像 Go `printServiceUsage`)。
pub(crate) fn print_service_usage(out: &mut dyn Write) -> Result<(), String> {
    write_line(
        out,
        "用法: sv service <action>\n\n可用操作:\n  install   安装 sv 为系统服务\n  uninstall 卸载 sv 系统服务\n  start     启动 sv 系统服务\n  stop      停止 sv 系统服务\n  restart   重启 sv 系统服务\n  status    查看 sv 服务状态",
    )
}

impl Default for ServiceManager {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn write_line(out: &mut dyn Write, text: &str) -> Result<(), String> {
    out.write_all(text.as_bytes())
        .and_then(|_| out.write_all(b"\n"))
        .map_err(|e| io_context("写入服务输出", e))
}

/// 返回路径的 symlink 元数据(不跟随软链);不存在返回 None。
pub(crate) fn path_lstat(path: &Path) -> Option<std::fs::Metadata> {
    fs::symlink_metadata(path).ok()
}

/// 把软链目标的原始文本解析成绝对化、规范化的路径,允许目标已删除(dangling)。
pub(crate) fn resolve_link_target(link: &Path, target: &Path) -> PathBuf {
    let joined = if target.is_absolute() {
        target.to_path_buf()
    } else {
        link.parent().unwrap_or(Path::new(".")).join(target)
    };
    clean_path(&joined)
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;

#[path = "service_links.rs"]
mod service_links;
