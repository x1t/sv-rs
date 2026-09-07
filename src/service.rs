//! 旧版 sv-rs 常驻服务资产清理。
//!
//! sv-rs 本身是 Supervisor 的短生命周期控制面 CLI,不再安装或运行自身服务。
//! 本模块仅保留对旧版本 systemd/SysV 资产的安全、幂等卸载兼容入口。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::service_files::{SERVICE_NAME, systemd_runtime_active};
use crate::util::{io_context, run_command};

const SYMLINK_DEFAULT: &str = "/usr/local/bin/sv";
const SYSTEMD_DIR_DEFAULT: &str = "/etc/systemd/system";
const INIT_DIR_DEFAULT: &str = "/etc/init.d";

/// 兼容清理旧版 sv-rs 常驻服务的资产定位器。
///
/// 该类型不负责安装、启动或管理 sv-rs 服务,只在用户显式执行
/// `sv service uninstall` 时清理旧版本留下的注册资产。
pub struct LegacyServiceCleanup {
    pub(crate) executable: String,
    pub(crate) symlink_path: PathBuf,
    pub(crate) unit_path: PathBuf,
    pub(crate) init_script_path: PathBuf,
    pub(crate) systemctl_path: String,
    pub(crate) systemd_active: bool,
}

impl LegacyServiceCleanup {
    /// 使用真实环境构造旧资产清理器。
    pub fn new() -> Self {
        LegacyServiceCleanup {
            executable: crate::service_files::resolve_own_executable(),
            symlink_path: PathBuf::from(SYMLINK_DEFAULT),
            unit_path: PathBuf::from(SYSTEMD_DIR_DEFAULT).join(format!("{SERVICE_NAME}.service")),
            init_script_path: PathBuf::from(INIT_DIR_DEFAULT).join(SERVICE_NAME),
            systemctl_path: "systemctl".to_string(),
            systemd_active: systemd_runtime_active(),
        }
    }

    /// 为测试注入资产路径,避免触碰真实系统目录。
    #[cfg(test)]
    pub fn for_testing(
        executable: String,
        symlink_path: PathBuf,
        unit_path: PathBuf,
        init_script_path: PathBuf,
    ) -> Self {
        LegacyServiceCleanup {
            executable,
            symlink_path,
            unit_path,
            init_script_path,
            systemctl_path: "systemctl".to_string(),
            systemd_active: false,
        }
    }

    /// 处理兼容命令。只有旧版服务资产清理仍然有效。
    pub fn handle_command(&self, args: &[String], out: &mut dyn Write) -> Result<(), String> {
        if args.is_empty() {
            print_service_usage(out)?;
            return Err("缺少服务操作".to_string());
        }
        if args.len() > 1 {
            return Err(format!("服务操作不接受额外参数: {}", args[1..].join(" ")));
        }
        match args[0].as_str() {
            "uninstall" => self.uninstall(out),
            "install" | "start" | "stop" | "restart" | "status" => Err(
                "sv-rs 不需要常驻运行或安装为系统服务，请直接安装二进制并使用 status/start/stop/restart"
                    .to_string(),
            ),
            other => {
                print_service_usage(out)?;
                Err(format!("未知服务操作: {other}"))
            }
        }
    }

    fn uninstall(&self, out: &mut dyn Write) -> Result<(), String> {
        self.uninstall_all(out)
    }

    pub(crate) fn unit_file_name(&self) -> String {
        self.unit_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("{SERVICE_NAME}.service"))
    }

    /// 执行 systemctl 子命令;非零退出视为失败。
    pub(crate) fn run_systemctl(&self, args: &[&str]) -> Result<String, String> {
        let arguments: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let outcome = run_command(&self.systemctl_path, &arguments, Duration::from_secs(30))
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
}

/// 打印旧版服务资产清理用法。
pub(crate) fn print_service_usage(out: &mut dyn Write) -> Result<(), String> {
    write_line(
        out,
        "用法: sv service uninstall\n\n该命令仅用于清理旧版 sv-rs 常驻服务资产;sv-rs 本身不需要安装为系统服务",
    )
}

impl Default for LegacyServiceCleanup {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn write_line(out: &mut dyn Write, text: &str) -> Result<(), String> {
    out.write_all(text.as_bytes())
        .and_then(|_| out.write_all(b"\n"))
        .map_err(|e| io_context("写入服务输出", e))
}

/// 返回路径的 symlink 元数据(不跟随软链);不存在返回 None,其他 I/O 错误向上返回。
pub(crate) fn path_lstat(path: &Path) -> std::io::Result<Option<std::fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// 把软链目标的原始文本解析成绝对化、规范化的路径,允许目标已删除(dangling)。
pub(crate) fn resolve_link_target(link: &Path, target: &Path) -> PathBuf {
    let joined = if target.is_absolute() {
        target.to_path_buf()
    } else {
        link.parent().unwrap_or(Path::new(".")).join(target)
    };
    crate::service_files::clean_path(&joined)
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;

#[path = "service_links.rs"]
mod service_links;

#[path = "service_cleanup.rs"]
mod service_cleanup;

#[cfg(test)]
#[path = "service_cleanup_tests.rs"]
mod cleanup_tests;
