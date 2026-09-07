use super::*;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use crate::service_files::{
    systemd_enable_link_path, systemd_unit_text, sysv_init_text, sysv_runlevel_link_paths,
    validate_systemd_enable_link,
};

fn temp_cleanup() -> (tempfile::TempDir, LegacyServiceCleanup) {
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("bin").join("sv-rs");
    fs::create_dir_all(executable.parent().unwrap()).unwrap();
    fs::write(&executable, "#!/bin/sh\n").unwrap();
    let executable = fs::canonicalize(executable).unwrap();
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

fn write_unit(manager: &LegacyServiceCleanup, executable: &str) {
    fs::create_dir_all(manager.unit_path.parent().unwrap()).unwrap();
    fs::write(&manager.unit_path, systemd_unit_text(executable)).unwrap();
}

fn write_init(manager: &LegacyServiceCleanup, executable: &str) {
    fs::create_dir_all(manager.init_script_path.parent().unwrap()).unwrap();
    fs::write(&manager.init_script_path, sysv_init_text(executable)).unwrap();
    fs::set_permissions(&manager.init_script_path, PermissionsExt::from_mode(0o755)).unwrap();
}

fn create_sysv_links(init_script: &Path) -> Vec<PathBuf> {
    let links = sysv_runlevel_link_paths(init_script);
    for link in &links {
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(init_script, link).unwrap();
    }
    links
}

fn create_owned_symlink(manager: &LegacyServiceCleanup) {
    fs::create_dir_all(manager.symlink_path.parent().unwrap()).unwrap();
    symlink(Path::new(&manager.executable), &manager.symlink_path).unwrap();
}

#[test]
fn test_uninstall_removes_legacy_sysv_assets() {
    let (_dir, manager) = temp_cleanup();
    write_init(&manager, &manager.executable);
    let links = create_sysv_links(&manager.init_script_path);
    create_owned_symlink(&manager);

    let mut out = Vec::new();
    manager.uninstall(capture(&mut out)).unwrap();

    assert!(!manager.init_script_path.exists());
    for link in links {
        assert!(link.symlink_metadata().is_err(), "{}", link.display());
    }
    assert!(manager.symlink_path.symlink_metadata().is_err());
    assert!(String::from_utf8_lossy(&out).contains("SV 系统服务卸载成功"));
}

#[test]
fn test_uninstall_removes_legacy_systemd_assets_without_runtime() {
    let (_dir, manager) = temp_cleanup();
    write_unit(&manager, &manager.executable);
    let enable_link = systemd_enable_link_path(&manager.unit_path).unwrap();
    fs::create_dir_all(enable_link.parent().unwrap()).unwrap();
    symlink(&manager.unit_path, &enable_link).unwrap();
    create_owned_symlink(&manager);

    let mut out = Vec::new();
    manager.uninstall(capture(&mut out)).unwrap();

    assert!(!manager.unit_path.exists());
    assert!(enable_link.symlink_metadata().is_err());
    assert!(manager.symlink_path.symlink_metadata().is_err());
}

#[test]
fn test_uninstall_removes_both_legacy_backends() {
    let (_dir, manager) = temp_cleanup();
    write_unit(&manager, &manager.executable);
    write_init(&manager, &manager.executable);
    let links = create_sysv_links(&manager.init_script_path);
    create_owned_symlink(&manager);

    let mut out = Vec::new();
    manager.uninstall(capture(&mut out)).unwrap();

    assert!(!manager.unit_path.exists());
    assert!(!manager.init_script_path.exists());
    for link in links {
        assert!(link.symlink_metadata().is_err(), "{}", link.display());
    }
    assert!(manager.symlink_path.symlink_metadata().is_err());
}

#[test]
fn test_uninstall_cleans_residual_links_without_service_files() {
    let (_dir, manager) = temp_cleanup();
    let sysv_links = sysv_runlevel_link_paths(&manager.init_script_path);
    for link in &sysv_links {
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&manager.init_script_path, link).unwrap();
    }
    let systemd_link = systemd_enable_link_path(&manager.unit_path).unwrap();
    fs::create_dir_all(systemd_link.parent().unwrap()).unwrap();
    symlink(&manager.unit_path, &systemd_link).unwrap();
    create_owned_symlink(&manager);

    let mut out = Vec::new();
    manager.uninstall(capture(&mut out)).unwrap();

    for link in sysv_links {
        assert!(link.symlink_metadata().is_err(), "{}", link.display());
    }
    assert!(systemd_link.symlink_metadata().is_err());
    assert!(manager.symlink_path.symlink_metadata().is_err());
    assert!(String::from_utf8_lossy(&out).contains("已清理残留"));
}

#[test]
fn test_uninstall_is_idempotent_after_cleanup() {
    let (_dir, manager) = temp_cleanup();
    write_init(&manager, &manager.executable);
    create_sysv_links(&manager.init_script_path);
    create_owned_symlink(&manager);

    manager.uninstall(&mut Vec::new()).unwrap();
    let mut out = Vec::new();
    manager.uninstall(capture(&mut out)).unwrap();
    assert!(String::from_utf8_lossy(&out).contains("服务未安装，无需卸载"));
}

#[test]
fn test_uninstall_removes_dangling_symlink_recorded_by_legacy_script() {
    let (dir, manager) = temp_cleanup();
    let old_executable = dir.path().join("old").join("sv-rs");
    write_init(&manager, &old_executable.to_string_lossy());
    fs::create_dir_all(manager.symlink_path.parent().unwrap()).unwrap();
    symlink(&old_executable, &manager.symlink_path).unwrap();

    manager.uninstall(&mut Vec::new()).unwrap();
    assert!(!manager.init_script_path.exists());
    assert!(manager.symlink_path.symlink_metadata().is_err());
}

#[test]
fn test_uninstall_refuses_foreign_service_file() {
    let (_dir, manager) = temp_cleanup();
    write_unit(&manager, "/usr/bin/not-sv");
    create_owned_symlink(&manager);

    let mut out = Vec::new();
    let error = manager.uninstall(capture(&mut out)).unwrap_err();
    assert!(error.contains("拒绝卸载非本程序注册的服务文件"), "{error}");
    assert!(manager.unit_path.exists());
    assert!(manager.symlink_path.symlink_metadata().is_ok());
}

#[test]
fn test_uninstall_refuses_service_path_symlink() {
    let (_dir, manager) = temp_cleanup();
    let foreign = manager.unit_path.parent().unwrap().join("foreign.service");
    fs::create_dir_all(foreign.parent().unwrap()).unwrap();
    fs::write(&foreign, "foreign").unwrap();
    symlink(&foreign, &manager.unit_path).unwrap();

    let error = manager.uninstall(&mut Vec::new()).unwrap_err();
    assert!(error.contains("拒绝卸载服务路径软链接"), "{error}");
    assert!(foreign.exists());
}

#[test]
fn test_uninstall_refuses_foreign_enable_link() {
    let (_dir, manager) = temp_cleanup();
    let link = systemd_enable_link_path(&manager.unit_path).unwrap();
    let foreign = manager.unit_path.parent().unwrap().join("foreign.service");
    fs::create_dir_all(foreign.parent().unwrap()).unwrap();
    fs::write(&foreign, "foreign").unwrap();
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink(&foreign, &link).unwrap();

    let error = manager.uninstall(&mut Vec::new()).unwrap_err();
    assert!(error.contains("systemd enable 链接"), "{error}");
    assert!(link.symlink_metadata().is_ok());
}

#[test]
fn test_uninstall_refuses_foreign_command_path_before_cleanup() {
    let (_dir, manager) = temp_cleanup();
    write_init(&manager, &manager.executable);
    let links = create_sysv_links(&manager.init_script_path);
    let foreign = manager.symlink_path.parent().unwrap().join("foreign");
    fs::write(&foreign, "foreign").unwrap();
    fs::create_dir_all(manager.symlink_path.parent().unwrap()).unwrap();
    symlink(&foreign, &manager.symlink_path).unwrap();

    let error = manager.uninstall(&mut Vec::new()).unwrap_err();
    assert!(error.contains("卸载前检查命令软链接失败"), "{error}");
    assert!(manager.init_script_path.exists());
    for link in links {
        assert!(link.symlink_metadata().is_ok(), "{}", link.display());
    }
}

#[test]
fn test_uninstall_refuses_regular_command_path_before_cleanup() {
    let (_dir, manager) = temp_cleanup();
    write_init(&manager, &manager.executable);
    create_sysv_links(&manager.init_script_path);
    fs::create_dir_all(manager.symlink_path.parent().unwrap()).unwrap();
    fs::write(&manager.symlink_path, "foreign").unwrap();

    let error = manager.uninstall(&mut Vec::new()).unwrap_err();
    assert!(error.contains("卸载前检查命令软链接失败"), "{error}");
    assert!(manager.init_script_path.exists());
}

#[test]
fn test_uninstall_refuses_foreign_sysv_link() {
    let (_dir, manager) = temp_cleanup();
    write_init(&manager, &manager.executable);
    let links = create_sysv_links(&manager.init_script_path);
    let foreign = manager.init_script_path.parent().unwrap().join("foreign");
    fs::write(&foreign, "foreign").unwrap();
    fs::remove_file(&links[0]).unwrap();
    symlink(&foreign, &links[0]).unwrap();
    create_owned_symlink(&manager);

    let error = manager.uninstall(&mut Vec::new()).unwrap_err();
    assert!(error.contains("指向其他文件"), "{error}");
    assert!(manager.init_script_path.exists());
    assert!(manager.symlink_path.symlink_metadata().is_ok());
}

#[test]
fn test_validate_systemd_enable_link_accepts_owned_link() {
    let (_dir, manager) = temp_cleanup();
    let link = systemd_enable_link_path(&manager.unit_path).unwrap();
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink(&manager.unit_path, &link).unwrap();
    assert!(validate_systemd_enable_link(&manager.unit_path).unwrap());
}
