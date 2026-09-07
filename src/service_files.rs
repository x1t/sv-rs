//! 旧版 systemd / SysV 服务资产的解析与清理原语。
//!
//! sv-rs 不再安装或运行自身服务;这些函数只服务于兼容卸载,不主动触碰系统路径。

use std::fs;
use std::path::{Component, Path, PathBuf};

/// 旧版服务标识(镜像 Go `serviceName`)。
pub const SERVICE_NAME: &str = "sv-supervisor-manager";

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

/// 校验并收集指向指定 init 脚本的现有 SysV runlevel 链接。
///
/// 缺失链接允许存在,但任何普通文件或异源软链接都会整体拒绝,避免误删。
fn collect_sysv_runlevel_links(init_script: &Path) -> Result<Vec<PathBuf>, String> {
    let expected = clean_path(init_script);
    let mut removable = Vec::new();
    for path in sysv_runlevel_link_paths(init_script) {
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
    Ok(removable)
}

/// 仅校验现有 SysV 链接归属,允许旧安装缺失部分链接。
pub fn validate_sysv_runlevel_links(init_script: &Path) -> Result<(), String> {
    collect_sysv_runlevel_links(init_script).map(|_| ())
}

/// 清理指向指定 init 脚本的 SysV runlevel 链接。
pub fn remove_sysv_runlevel_links(init_script: &Path) -> Result<(), String> {
    for path in collect_sysv_runlevel_links(init_script)? {
        fs::remove_file(&path).map_err(|error| format!("删除 SysV 启动链接失败: {error}"))?;
    }
    Ok(())
}

/// 返回 systemd unit 对应的 multi-user.target enable 链接路径。
pub fn systemd_enable_link_path(unit_path: &Path) -> Option<PathBuf> {
    let directory = unit_path.parent()?;
    let name = unit_path.file_name()?;
    Some(directory.join("multi-user.target.wants").join(name))
}

/// 校验 systemd enable 链接是否存在且明确指向指定 unit。
pub fn validate_systemd_enable_link(unit_path: &Path) -> Result<bool, String> {
    let Some(link) = systemd_enable_link_path(unit_path) else {
        return Ok(false);
    };
    let info = match fs::symlink_metadata(&link) {
        Ok(info) => info,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("检查 systemd enable 链接失败: {error}")),
    };
    if !info.file_type().is_symlink() {
        return Err(format!("拒绝删除非软链接路径: {}", link.display()));
    }
    let target =
        fs::read_link(&link).map_err(|error| format!("读取 systemd enable 链接失败: {error}"))?;
    let resolved = if target.is_absolute() {
        target
    } else {
        link.parent().unwrap_or(Path::new(".")).join(target)
    };
    if clean_path(&resolved) != clean_path(unit_path) {
        return Err(format!(
            "拒绝删除指向其他文件的 systemd enable 链接: {}",
            link.display()
        ));
    }
    Ok(true)
}

/// 删除明确属于指定 unit 的 systemd enable 链接。
pub fn remove_systemd_enable_link(unit_path: &Path) -> Result<bool, String> {
    if !validate_systemd_enable_link(unit_path)? {
        return Ok(false);
    }
    let link = systemd_enable_link_path(unit_path).ok_or("systemd enable 链接路径缺失")?;
    fs::remove_file(&link).map_err(|error| format!("删除 systemd enable 链接失败: {error}"))?;
    Ok(true)
}

/// 对绝对路径做不跟随软链的词法规范化。
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

/// systemd unit 里 ExecStart 可执行路径的转义:空格写作 \x20。
fn systemd_escape_executable(executable: &str) -> String {
    executable.replace(' ', r"\x20")
}

/// shell 单引号转义,用于解析旧版 SysV init 脚本中的可执行路径。
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// 从旧版 systemd unit 文本解析回 ExecStart 的可执行路径。
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

/// 从旧版 SysV init 脚本文本解析回 exe 记录的单引号值。
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

/// 旧版服务后端,仅用于卸载时遍历已知资产。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ServiceBackend {
    Systemd,
    SysV,
}

/// 判断当前系统是否由 systemd 作为服务管理器运行。
pub fn systemd_runtime_active() -> bool {
    Path::new("/run/systemd/system").exists()
        || fs::read_to_string("/proc/1/comm")
            .map(|name| name.trim() == "systemd")
            .unwrap_or(false)
}

/// 解析当前可执行文件的绝对路径;失败时返回空串。
pub fn resolve_own_executable() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| fs::canonicalize(path).ok())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// 旧版 systemd unit 模板,仅用于卸载时确认资产归属。
pub fn systemd_unit_text(executable: &str) -> String {
    format!(
        "[Unit]\nDescription=SV Supervisor Manager\nAfter=network.target\n\n\
[Service]\nType=simple\nExecStart={} daemon\nRestart=always\n\n\
[Install]\nWantedBy=multi-user.target\n",
        systemd_escape_executable(executable)
    )
}

/// 旧版 SysV init 脚本模板,仅用于卸载时确认资产归属。
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
  ;;\
  *)\n\
    echo \"Usage: $0 {{start|stop|restart|status}}\" >&2\n\
    exit 1\n\
  ;;\n\
esac\n",
        shell_quote(executable)
    )
}

#[cfg(test)]
#[path = "service_files_tests.rs"]
mod tests;
