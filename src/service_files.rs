//! systemd / SysV 服务资产:后端探测、unit 与 init 脚本文本、软链接原语、
//! 信号等待。均为纯函数或系统探测,不触碰落地目录,便于独立测试。

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::util::command_available;

/// 服务标识(镜像 Go `serviceName`)。
pub const SERVICE_NAME: &str = "sv-supervisor-manager";

/// 创建符号链接(Linux 原生实现)。
pub fn make_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

const SYSV_START_RUNLEVELS: [&str; 4] = ["2", "3", "4", "5"];
const SYSV_STOP_RUNLEVELS: [&str; 3] = ["0", "1", "6"];

/// 返回指向指定 init 脚本的 SysV runlevel 链接路径列表(S50/K02 命名)。
pub fn sysv_runlevel_link_paths(init_script: &Path) -> Vec<PathBuf> {
    let root = init_script
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut paths = Vec::with_capacity(SYSV_START_RUNLEVELS.len() + SYSV_STOP_RUNLEVELS.len());
    for runlevel in SYSV_START_RUNLEVELS {
        paths.push(root.join(format!("rc{runlevel}.d/S50{SERVICE_NAME}")));
    }
    for runlevel in SYSV_STOP_RUNLEVELS {
        paths.push(root.join(format!("rc{runlevel}.d/K02{SERVICE_NAME}")));
    }
    paths
}

/// 清理指向指定 init 脚本的 SysV runlevel 链接。
///
/// 只移除目标完全匹配的符号链接,并在发现外部路径时先整体拒绝,避免部分清理。
pub fn remove_sysv_runlevel_links(init_script: &Path) -> Result<(), String> {
    let paths = sysv_runlevel_link_paths(init_script);

    let expected = clean_path(init_script);
    let mut removable = Vec::with_capacity(paths.len());
    for path in paths {
        let info = match fs::symlink_metadata(&path) {
            Ok(info) => info,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("检查 SysV 启动链接失败: {error}")),
        };
        if !info.file_type().is_symlink() {
            return Err(format!("拒绝删除非软链接路径: {}", path.display()));
        }
        let target =
            fs::read_link(&path).map_err(|error| format!("读取 SysV 启动链接失败: {error}"))?;
        let resolved = if target.is_absolute() {
            target
        } else {
            path.parent().unwrap_or(Path::new(".")).join(target)
        };
        if clean_path(&resolved) != expected {
            return Err(format!(
                "拒绝删除指向其他文件的 SysV 启动链接: {}",
                path.display()
            ));
        }
        removable.push(path);
    }
    for path in removable {
        fs::remove_file(&path).map_err(|error| format!("删除 SysV 启动链接失败: {error}"))?;
    }
    Ok(())
}

pub fn clean_path(path: &Path) -> PathBuf {
    let mut cleaned = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                cleaned.pop();
            }
            other => cleaned.push(other.as_os_str()),
        }
    }
    cleaned
}

/// systemd unit 里 ExecStart 可执行路径的转义:空格写作 \x20,避免被当作参数分隔。
fn systemd_escape_executable(executable: &str) -> String {
    executable.replace(' ', r"\x20")
}

/// shell 单引号转义,供 SysV 脚本内嵌可执行路径使用。
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// 从 systemd unit 文本解析回 ExecStart 的可执行路径(把 \x20 还原为空格)。
/// 只接受无前缀、绝对路径的第一段;解析不到返回 None。
pub fn systemd_unit_executable(text: &str) -> Option<String> {
    let line = text
        .lines()
        .map(str::trim_end)
        .find(|line| line.starts_with("ExecStart="))?;
    let rest = line.trim_start_matches("ExecStart=").trim();
    let first = rest.split_whitespace().next()?;
    if !first.starts_with('/') {
        return None;
    }
    Some(first.replace(r"\x20", " "))
}

/// 从 SysV init 脚本文本解析回 exe 记录的单引号值(反转 shell_quote)。
/// shell_quote 把值里的 ' 编码为 '\'';整行以单引号开头结尾,剥掉外层后再把
/// '\'' 还原为 ' 即得原值。只接受单引号包裹且非空的行;解析不到返回 None。
pub fn sysv_init_executable(text: &str) -> Option<String> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("exe="))?;
    let value = line.trim_start_matches("exe=");
    if !value.starts_with('\'') || !value.ends_with('\'') || value.len() < 2 {
        return None;
    }
    let unquoted = value[1..value.len() - 1].replace(r"'\''", "'");
    if unquoted.is_empty() {
        return None;
    }
    Some(unquoted)
}

/// 创建指向指定 init 脚本的 SysV runlevel 链接(与 Go 依赖库的 S50/K02 命名一致)。
/// 已存在且指向本脚本时视为成功;占用为普通文件或指向其他目标时拒绝,避免覆盖。
/// 任何一步失败都会回滚本次已创建的链接,不留半成品。
pub fn create_sysv_runlevel_links(init_script: &Path) -> Result<(), String> {
    let root = init_script
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "init.d目录缺失".to_string())?;
    let mut links = Vec::with_capacity(SYSV_START_RUNLEVELS.len() + SYSV_STOP_RUNLEVELS.len());
    for runlevel in SYSV_START_RUNLEVELS {
        links.push(root.join(format!("rc{runlevel}.d/S50{SERVICE_NAME}")));
    }
    for runlevel in SYSV_STOP_RUNLEVELS {
        links.push(root.join(format!("rc{runlevel}.d/K02{SERVICE_NAME}")));
    }

    let expected = clean_path(init_script);
    let mut created = Vec::with_capacity(links.len());
    let result = create_sysv_runlevel_links_inner(&links, init_script, &expected, &mut created);
    if result.is_err() {
        for link in created {
            let _ = fs::remove_file(link);
        }
    }
    result
}

fn create_sysv_runlevel_links_inner(
    links: &[PathBuf],
    init_script: &Path,
    expected: &Path,
    created: &mut Vec<PathBuf>,
) -> Result<(), String> {
    for link in links {
        match fs::symlink_metadata(link) {
            Ok(info) => {
                if !info.file_type().is_symlink() {
                    return Err(format!("目标路径已存在且不是软链接: {}", link.display()));
                }
                let target = fs::read_link(link)
                    .map_err(|error| format!("读取 SysV 启动链接失败: {error}"))?;
                let resolved = if target.is_absolute() {
                    target
                } else {
                    link.parent().unwrap_or(Path::new(".")).join(target)
                };
                if clean_path(&resolved) != expected {
                    return Err(format!(
                        "目标软链接已存在且指向其他文件: {}",
                        link.display()
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = link.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|error| format!("创建 SysV 启动链接目录失败: {error}"))?;
                }
                make_symlink(init_script, link)
                    .map_err(|error| format!("创建 SysV 启动链接失败: {error}"))?;
                created.push(link.clone());
            }
            Err(error) => return Err(format!("检查 SysV 启动链接失败: {error}")),
        }
    }
    Ok(())
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
[Service]\nType=simple\nExecStart={} daemon\nRestart=always\n\n\
[Install]\nWantedBy=multi-user.target\n",
        systemd_escape_executable(executable)
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
exe={}\n\
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
    pid=\"$(cat \"$pidfile\" 2>/dev/null)\" || exit 1\n\
    if ! kill -0 \"$pid\" 2>/dev/null; then\n\
      rm -f \"$pidfile\"\n\
      exit 0\n\
    fi\n\
    kill \"$pid\" 2>/dev/null || exit 1\n\
    for i in $(seq 1 10)\n\
    do\n\
      if ! kill -0 \"$pid\" 2>/dev/null; then\n\
        rm -f \"$pidfile\"\n\
        exit 0\n\
      fi\n\
      sleep 1\n\
    done\n\
    echo \"not stopped\" >&2\n\
    exit 1\n\
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
esac\n",
        shell_quote(executable)
    )
}

#[cfg(test)]
mod text_tests {
    use super::*;

    #[test]
    fn test_systemd_unit_escapes_space_in_executable() {
        let text = systemd_unit_text("/opt/my app/sv-rs");
        assert!(
            text.contains("ExecStart=/opt/my\\x20app/sv-rs daemon"),
            "{text}"
        );
    }

    #[test]
    fn test_sysv_init_quotes_executable() {
        let text = sysv_init_text("/opt/my app/sv'rs");
        assert!(text.contains("exe='/opt/my app/sv'\\''rs'"), "{text}");
    }
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
#[path = "service_files_tests.rs"]
mod tests;
