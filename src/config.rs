//! Supervisor 配置检测与自动补齐(逐点对齐 Go `pkg/supervisor/config_detector.go`)。
//!
//! 负责定位配置文件、检查缺失的 RPC 段、原子地追加配置(带备份)以及探测重启命令。

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use crate::util::{command_available, io_context, run_command};

/// 默认 Supervisor RPC 端点。
pub const DEFAULT_HOST: &str = "http://localhost:9001/RPC2";

/// 缺失时自动追加的 inet_http_server 段。
const INET_HTTP_SERVER_CONFIG: &str = "[inet_http_server]\nport=127.0.0.1:9001\n";
/// 缺失时自动追加的 rpcinterface 段。
const RPC_INTERFACE_CONFIG: &str = "[rpcinterface:supervisor]\nsupervisor.rpcinterface_factory = supervisor.rpcinterface:make_main_rpcinterface\n";

/// 重启 Supervisor 时依次尝试的命令列表(镜像 Go)。
const RESTART_COMMANDS: [&[&str]; 6] = [
    &["systemctl", "restart", "supervisor"],
    &["systemctl", "restart", "supervisord"],
    &["service", "supervisor", "restart"],
    &["service", "supervisord", "restart"],
    &["/etc/init.d/supervisor", "restart"],
    &["/etc/init.d/supervisord", "restart"],
];

/// 配置检测器:持有候选配置文件路径。
#[derive(Debug, Clone)]
pub struct ConfigDetector {
    config_paths: Vec<String>,
}

impl ConfigDetector {
    /// 使用显式路径列表创建检测器(供测试与定制)。
    #[cfg(test)]
    pub fn new_with_paths(paths: Vec<String>) -> Self {
        ConfigDetector {
            config_paths: paths,
        }
    }

    /// 读取 Supervisor 连接配置:优先使用环境变量,否则使用默认端点。
    pub fn read_env_config() -> (String, String, String) {
        let host = match std::env::var("SUPERVISOR_HOST") {
            Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
            _ => DEFAULT_HOST.to_string(),
        };
        let username = std::env::var("SUPERVISOR_USER").unwrap_or_default();
        let username = username.trim().to_string();
        let password = std::env::var("SUPERVISOR_PASSWORD").unwrap_or_default();
        (host, username, password)
    }

    /// 按环境变量与默认路径构造检测器。
    pub fn new() -> Self {
        let mut paths = Vec::new();
        if let Ok(configured) = std::env::var("SUPERVISOR_CONFIG") {
            let configured = configured.trim().to_string();
            if !configured.is_empty() {
                paths.push(configured);
            }
        }
        paths.push("/etc/supervisor/supervisord.conf".to_string());
        paths.push("/etc/supervisord.conf".to_string());
        ConfigDetector {
            config_paths: paths,
        }
    }

    /// 返回第一个存在且为普通文件的候选路径。
    pub fn find_config_path(&self) -> Result<String, String> {
        for path in &self.config_paths {
            match std::fs::symlink_metadata(path) {
                Ok(meta) if meta.is_file() => return Ok(path.clone()),
                Ok(_) => return Err(format!("配置路径不是普通文件: {path}")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(io_context(&format!("检查配置文件 {path}"), error)),
            }
        }
        Err("未找到 Supervisor 配置文件，请设置 SUPERVISOR_CONFIG".to_string())
    }

    /// 读取配置并补齐缺失的 RPC 段;`dry_run` 为真时只预览不写入。
    pub fn configure_rpc(&self, dry_run: bool) -> Result<String, String> {
        let config_path = self.find_config_path()?;
        let content =
            std::fs::read_to_string(&config_path).map_err(|e| io_context("读取配置文件", e))?;

        let mut updated = content.clone();
        let mut changes = Vec::new();
        if !has_section_key(&updated, "inet_http_server", "port") {
            updated = append_config_section(&updated, INET_HTTP_SERVER_CONFIG);
            changes.push("inet_http_server");
        }
        if !has_section_key(
            &updated,
            "rpcinterface:supervisor",
            "supervisor.rpcinterface_factory",
        ) {
            updated = append_config_section(&updated, RPC_INTERFACE_CONFIG);
            changes.push("rpcinterface:supervisor");
        }
        if changes.is_empty() {
            return Ok(format!("RPC配置已存在: {config_path}"));
        }
        if dry_run {
            return Ok(format!(
                "将更新 {config_path}，新增配置段: {}",
                changes.join(", ")
            ));
        }

        let backup_path = backup_and_write_config(&config_path, &updated)?;
        Ok(format!(
            "配置已更新: {config_path}；备份: {backup_path}；请使用 --restart 或手动重启 Supervisor"
        ))
    }

    /// 检查配置文件是否包含 inet_http_server.port 有效配置。
    #[cfg(test)]
    pub fn has_inet_http_server(&self, config_path: &str) -> Result<bool, String> {
        let content =
            std::fs::read_to_string(config_path).map_err(|e| io_context("读取配置文件", e))?;
        Ok(has_section_key(&content, "inet_http_server", "port"))
    }

    /// 检查配置文件是否包含 Supervisor RPC 工厂配置。
    #[cfg(test)]
    pub fn has_rpc_interface(&self, config_path: &str) -> Result<bool, String> {
        let content =
            std::fs::read_to_string(config_path).map_err(|e| io_context("读取配置文件", e))?;
        Ok(has_section_key(
            &content,
            "rpcinterface:supervisor",
            "supervisor.rpcinterface_factory",
        ))
    }

    /// 依次尝试重启 Supervisor,直到某个命令成功。
    pub fn restart_supervisor(&self) -> Result<(), String> {
        run_restart(&RESTART_COMMANDS)
    }
}

impl Default for ConfigDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// 判断命令首领(带斜杠则按路径,否则按 PATH)是否可用。
fn command_leader_available(leader: &str) -> bool {
    if leader.contains('/') {
        Path::new(leader).exists()
    } else {
        command_available(leader)
    }
}

/// 依次尝试给定命令(镜像 Go):不可用的命令跳过,任一成功即返回,
/// 否则聚合错误;一个命令都没执行到时返回"未找到可用"。
fn run_restart(commands: &[&[&str]]) -> Result<(), String> {
    let timeout = Duration::from_secs(15);
    let mut errors = Vec::new();
    for command in commands {
        if !command_leader_available(command[0]) {
            continue;
        }
        let joined = command.join(" ");
        let args: Vec<String> = command[1..].iter().map(|s| s.to_string()).collect();
        let outcome = match run_command(command[0], &args, timeout) {
            Ok(outcome) => outcome,
            Err(error) => {
                errors.push(format!("{joined}: {error} ()"));
                continue;
            }
        };
        if outcome.code == Some(0) {
            return Ok(());
        }
        let status = match outcome.code {
            Some(code) => format!("exit status {code}"),
            None => "signal: killed".to_string(),
        };
        errors.push(format!(
            "{joined}: {status} ({})",
            outcome.combined_text().trim()
        ));
    }
    if errors.is_empty() {
        return Err("未找到可用的 Supervisor 服务管理命令".to_string());
    }
    Err(format!("重启 Supervisor 失败: {}", errors.join("; ")))
}

/// 判断配置文本中指定段是否包含给定键(带注释剔除)。
fn has_section_key(content: &str, expected_section: &str, expected_key: &str) -> bool {
    let mut section = String::new();
    for raw_line in content.split('\n') {
        let line = active_config_line(raw_line);
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_string();
            continue;
        }
        if section.as_str() != expected_section {
            continue;
        }
        if let Some((key, value)) = line.split_once('=')
            && key.trim().eq_ignore_ascii_case(expected_key)
            && !value.trim().is_empty()
        {
            return true;
        }
    }
    false
}

/// 去除非配置行:空白、整行注释与行内注释。
fn active_config_line(raw_line: &str) -> String {
    let mut line = raw_line.trim().to_string();
    if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
        return String::new();
    }
    for marker in ['#', ';'] {
        if let Some(index) = line.find(marker) {
            line = line[..index].trim().to_string();
        }
    }
    line
}

/// 在文本末尾追加一个配置段(保证换行整洁)。
fn append_config_section(content: &str, section: &str) -> String {
    let mut updated = content.trim_end_matches(['\r', '\n']).to_string();
    if !updated.is_empty() {
        updated.push_str("\n\n");
    }
    updated.push_str(section.trim_end_matches(['\r', '\n']));
    updated.push('\n');
    updated
}

/// 先创建备份文件,再把内容原子写入原文件。
fn backup_and_write_config(config_path: &str, content: &str) -> Result<String, String> {
    let meta = std::fs::symlink_metadata(config_path).map_err(|e| io_context("检查配置文件", e))?;
    if !meta.is_file() {
        return Err(format!("配置文件必须是普通文件: {config_path}"));
    }
    if meta.permissions().mode() & 0o200 == 0 {
        return Err(format!("配置文件不可写: {config_path}"));
    }
    let mode = meta.permissions().mode();

    let backup_path = format!(
        "{config_path}.bak.{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let mut backup = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&backup_path)
    {
        Ok(file) => file,
        Err(error) => {
            let _ = std::fs::remove_file(&backup_path);
            return Err(io_context("创建配置备份", error));
        }
    };
    let mut original = match std::fs::File::open(config_path) {
        Ok(file) => file,
        Err(error) => {
            let _ = std::fs::remove_file(&backup_path);
            return Err(io_context("打开配置文件", error));
        }
    };
    let copy_result = std::io::copy(&mut original, &mut backup);
    if let Err(error) = copy_result {
        let _ = std::fs::remove_file(&backup_path);
        return Err(io_context("写入配置备份", error));
    }
    if let Err(error) = backup.sync_all() {
        let _ = std::fs::remove_file(&backup_path);
        return Err(io_context("同步配置备份", error));
    }
    drop(backup);

    let parent = Path::new(config_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let base = Path::new(config_path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config".to_string());
    let temp_path = parent.join(format!(".{base}.tmp-{}", std::process::id()));

    let result = write_atomic(config_path, &temp_path, content, mode);
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result?;
    Ok(backup_path)
}

fn write_atomic(
    config_path: &str,
    temp_path: &std::path::Path,
    content: &str,
    mode: u32,
) -> Result<(), String> {
    let mut temp = match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(temp_path)
    {
        Ok(file) => file,
        Err(error) => return Err(io_context("创建临时配置文件", error)),
    };
    if let Err(error) = std::fs::set_permissions(temp_path, std::fs::Permissions::from_mode(mode)) {
        return Err(io_context("设置临时配置权限", error));
    }
    if let Err(error) = temp.write_all(content.as_bytes()) {
        return Err(io_context("写入临时配置", error));
    }
    if let Err(error) = temp.sync_all() {
        return Err(io_context("同步临时配置", error));
    }
    drop(temp);
    std::fs::rename(temp_path, config_path).map_err(|e| io_context("原子替换配置文件", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp_config(content: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("创建临时目录");
        let path = dir.path().join("supervisord.conf");
        std::fs::write(&path, content).expect("写入临时配置");
        (dir, path.to_string_lossy().into_owned())
    }

    #[test]
    fn test_detect_missing_and_patch() {
        let (_dir, path) = write_temp_config("[supervisord]\nlogfile=/var/log/supervisord.log\n");
        let detector = ConfigDetector::new_with_paths(vec![path.clone()]);

        let message = detector.configure_rpc(true).expect("dry-run 应成功");
        assert!(message.contains("将更新"));
        assert!(message.contains("inet_http_server, rpcinterface:supervisor"));
        assert!(std::fs::read_to_string(&path).unwrap().contains("logfile"));

        let _ = detector.configure_rpc(false).expect("写回应成功");
        let updated = std::fs::read_to_string(&path).unwrap();
        assert!(updated.contains("[inet_http_server]"));
        assert!(updated.contains("[rpcinterface:supervisor]"));
        assert!(detector.has_inet_http_server(&path).unwrap());
        assert!(detector.has_rpc_interface(&path).unwrap());

        let backups: Vec<_> = glob_backups(&path);
        assert_eq!(backups.len(), 1);

        let message = detector.configure_rpc(false).expect("二次执行应成功");
        assert!(message.contains("已存在"));
    }

    fn glob_backups(path: &str) -> Vec<String> {
        std::fs::read_dir(Path::new(path).parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(&format!("{path}.bak.")))
            .collect()
    }

    #[test]
    fn test_dry_run_keeps_file_unchanged() {
        let (_dir, path) = write_temp_config("[supervisord]\n");
        let initial = std::fs::read_to_string(&path).unwrap();
        let detector = ConfigDetector::new_with_paths(vec![path.clone()]);
        let _ = detector.configure_rpc(true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), initial);
    }

    #[test]
    fn test_find_config_missing() {
        let detector = ConfigDetector::new_with_paths(vec!["/nonexistent/a.conf".to_string()]);
        let error = detector.find_config_path().unwrap_err();
        assert!(error.contains("未找到 Supervisor 配置文件"));
    }

    #[test]
    fn test_section_scan() {
        let content = "# 注释\n[inet_http_server]\nport = 127.0.0.1:9001  ; 行内注释\n";
        assert!(has_section_key(content, "inet_http_server", "port"));
        assert!(!has_section_key(content, "inet_http_server", "username"));
    }

    #[test]
    fn test_restart_none_available() {
        let empty: [&[&str]; 0] = [];
        let error = run_restart(&empty).unwrap_err();
        assert!(error.contains("未找到可用的 Supervisor 服务管理命令"));
    }

    #[test]
    fn test_restart_all_failed() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fail-restart");
        std::fs::write(&script, "#!/bin/sh\nprintf 'boom' >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let binding = [script.to_str().unwrap(), "restart"];
        let commands: Vec<&[&str]> = vec![&binding];
        let error = run_restart(&commands).unwrap_err();
        assert!(error.contains("重启 Supervisor 失败"));
        assert!(error.contains("boom"));
        assert!(error.contains("exit status 1"));
    }

    #[test]
    fn test_restart_success() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("ok-restart");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let binding = [script.to_str().unwrap(), "restart"];
        let commands: Vec<&[&str]> = vec![&binding];
        assert!(run_restart(&commands).is_ok());
    }
}
