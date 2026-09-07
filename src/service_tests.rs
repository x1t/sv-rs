use super::*;
use std::io::Write;
use std::path::{Path, PathBuf};

fn temp_cleanup() -> (tempfile::TempDir, LegacyServiceCleanup) {
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("bin").join("sv-rs");
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(&executable, "#!/bin/sh\n").unwrap();
    let executable = std::fs::canonicalize(executable).unwrap();
    let manager = LegacyServiceCleanup::for_testing(
        executable.to_string_lossy().into_owned(),
        dir.path().join("bin").join("sv"),
        dir.path()
            .join("systemd")
            .join(format!("{SERVICE_NAME}.service")),
        dir.path().join("init.d").join(SERVICE_NAME),
    );
    (dir, manager)
}

fn capture(out: &mut Vec<u8>) -> &mut dyn Write {
    out
}

#[test]
fn test_usage_and_unknown_operation() {
    let (_dir, manager) = temp_cleanup();
    let mut out = Vec::new();
    let error = manager.handle_command(&[], capture(&mut out)).unwrap_err();
    assert_eq!(error, "缺少服务操作");
    assert!(String::from_utf8_lossy(&out).contains("sv service uninstall"));

    let mut out = Vec::new();
    let error = manager
        .handle_command(&["bogus".to_string()], capture(&mut out))
        .unwrap_err();
    assert_eq!(error, "未知服务操作: bogus");
    assert!(String::from_utf8_lossy(&out).contains("sv service uninstall"));
}

#[test]
fn test_removed_service_actions_report_migration_guidance() {
    let (_dir, manager) = temp_cleanup();
    for action in ["install", "start", "stop", "restart", "status"] {
        let error = manager
            .handle_command(&[action.to_string()], &mut Vec::new())
            .unwrap_err();
        assert!(error.contains("不需要常驻运行或安装为系统服务"), "{error}");
    }
}

#[test]
fn test_uninstall_without_assets_is_idempotent() {
    let (_dir, manager) = temp_cleanup();
    let mut first = Vec::new();
    manager.uninstall(capture(&mut first)).unwrap();
    assert!(String::from_utf8_lossy(&first).contains("服务未安装，无需卸载"));

    let mut second = Vec::new();
    manager.uninstall(capture(&mut second)).unwrap();
    assert!(String::from_utf8_lossy(&second).contains("服务未安装，无需卸载"));
}

#[test]
fn test_resolve_relative_link_target() {
    let link = Path::new("/etc/rc2.d/S50sv-supervisor-manager");
    let target = PathBuf::from("../init.d/sv-supervisor-manager");
    assert_eq!(
        resolve_link_target(link, &target),
        PathBuf::from("/etc/init.d/sv-supervisor-manager")
    );
}

#[test]
fn test_unit_file_name_uses_basename() {
    let (_dir, manager) = temp_cleanup();
    assert_eq!(manager.unit_file_name(), "sv-supervisor-manager.service");
}
