//! systemd / SysV 服务资产:后端探测、unit 与 init 脚本文本、软链接原语、
//! 信号等待。均为纯函数或系统探测,不触碰落地目录,便于独立测试。

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::util::command_available;

/// 服务标识(镜像 Go `serviceName`)。
pub const SERVICE_NAME: &str = "sv-supervisor-manager";

/// 创建符号链接(Linux 原生实现)。
pub fn make_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// 可用的服务后端。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ServiceBackend {
    Systemd,
    SysV,
}

/// 服务运行状态(镜像 Go `service.Status`)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ServiceStatus {
    Running,
    Stopped,
    Unknown,
}

/// 探测当前后端:systemd 活跃且 systemctl 可用时为 Systemd,否则 SysV。
pub fn detect_backend() -> ServiceBackend {
    let systemd_running = Path::new("/run/systemd/system").exists()
        || fs::read_to_string("/proc/1/comm")
            .map(|name| name.trim() == "systemd")
            .unwrap_or(false);
    if systemd_running && command_available("systemctl") {
        ServiceBackend::Systemd
    } else {
        ServiceBackend::SysV
    }
}

/// 解析当前可执行文件的绝对路径;失败时返回空串。
pub fn resolve_own_executable() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| fs::canonicalize(path).ok())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// systemd unit 文件内容。
pub fn systemd_unit_text(executable: &str) -> String {
    format!(
        "[Unit]\nDescription=SV Supervisor Manager\nAfter=network.target\n\n\
[Service]\nType=simple\nExecStart={executable} daemon\nRestart=always\n\n\
[Install]\nWantedBy=multi-user.target\n"
    )
}

/// SysV init.d 脚本内容。
pub fn sysv_init_text(executable: &str) -> String {
    format!(
        "#!/bin/sh\n\
### BEGIN INIT INFO\n\
# Provides:          {SERVICE_NAME}\n\
# Required-Start:    $network\n\
# Required-Stop:     $network\n\
# Default-Start:     2 3 4 5\n\
# Default-Stop:      0 1 6\n\
# Short-Description: SV Supervisor Manager\n\
### END INIT INFO\n\n\
name={SERVICE_NAME}\n\
exe=\"{executable}\"\n\
pidfile=/run/$name.pid\n\n\
case \"$1\" in\n\
  start)\n\
    if [ -f \"$pidfile\" ] && kill -0 \"$(cat \"$pidfile\" 2>/dev/null)\" 2>/dev/null; then\n\
      exit 0\n\
    fi\n\
    \"$exe\" daemon >>/var/log/$name.log 2>&1 &\n\
    echo $! > \"$pidfile\"\n\
    ;;\n\
  stop)\n\
    [ -f \"$pidfile\" ] || exit 0\n\
    kill \"$(cat \"$pidfile\" 2>/dev/null)\" 2>/dev/null\n\
    rm -f \"$pidfile\"\n\
    ;;\n\
  restart)\n\
    $0 stop\n\
    $0 start\n\
    ;;\n\
  status)\n\
    if [ -f \"$pidfile\" ] && kill -0 \"$(cat \"$pidfile\" 2>/dev/null)\" 2>/dev/null; then\n\
      echo \"running\"\n\
      exit 0\n\
    fi\n\
    echo \"not running\"\n\
    exit 3\n\
    ;;\n\
  *)\n\
    echo \"Usage: $0 {{start|stop|restart|status}}\" >&2\n\
    exit 1\n\
    ;;\n\
esac\n"
    )
}

/// 等待 SIGTERM/SIGINT;收到信号后返回。供守护进程优雅退出。
pub fn wait_for_signal() -> Result<(), String> {
    static STOP: AtomicBool = AtomicBool::new(false);
    extern "C" fn on_signal(_signal: i32) {
        STOP.store(true, Ordering::SeqCst);
    }
    #[cfg(unix)]
    {
        unsafe {
            libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        }
    }
    while !STOP.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_systemd_unit_content() {
        let text = systemd_unit_text("/opt/sv");
        assert!(text.starts_with("[Unit]"));
        assert!(text.contains("ExecStart=/opt/sv daemon"));
        assert!(text.contains("Restart=always"));
        assert!(text.contains("WantedBy=multi-user.target"));
    }

    #[test]
    fn test_sysv_script_content() {
        let text = sysv_init_text("/usr/bin/sv");
        assert!(text.starts_with("#!/bin/sh"));
        assert!(text.contains("Provides:          sv-supervisor-manager"));
        assert!(text.contains("exe=\"/usr/bin/sv\""));
        assert!(text.contains("Usage: $0 {start|stop|restart|status}"));
    }

    #[test]
    fn test_make_symlink_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real");
        let link = dir.path().join("link");
        std::fs::write(&target, "x").unwrap();
        make_symlink(&target, &link).unwrap();
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::canonicalize(&link).unwrap(),
            std::fs::canonicalize(&target).unwrap()
        );
    }

    #[test]
    fn test_resolve_own_executable_is_absolute() {
        let executable = resolve_own_executable();
        assert!(!executable.is_empty());
        assert!(Path::new(&executable).is_absolute());
    }
}
