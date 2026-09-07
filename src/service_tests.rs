use super::*;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::service_files::make_symlink;

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
    let manager = ServiceManager::for_testing(executable, symlink.clone(), unit, script, backend);
    (dir, manager, symlink)
}

fn capture(out: &mut Vec<u8>) -> &mut dyn Write {
    out
}

fn sysv_runlevel_links(init_script: &Path) -> Vec<PathBuf> {
    let root = init_script.parent().unwrap().parent().unwrap();
    let mut links = Vec::new();
    for runlevel in ["2", "3", "4", "5"] {
        links.push(root.join(format!("rc{runlevel}.d/S50{SERVICE_NAME}")));
    }
    for runlevel in ["0", "1", "6"] {
        links.push(root.join(format!("rc{runlevel}.d/K02{SERVICE_NAME}")));
    }
    links
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
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
    let links = sysv_runlevel_links(&manager.init_script_path);
    let mut out = Vec::new();

    manager.install(capture(&mut out)).unwrap();
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("✅ SV 系统服务安装成功"), "{text}");
    assert!(manager.init_script_path.exists());
    for link in &links {
        assert!(
            link.symlink_metadata().unwrap().file_type().is_symlink(),
            "{}",
            link.display()
        );
    }
    assert!(symlink.symlink_metadata().unwrap().file_type().is_symlink());

    let mut out = Vec::new();
    manager.uninstall(capture(&mut out)).unwrap();
    assert!(!manager.init_script_path.exists());
    for link in links {
        assert!(link.symlink_metadata().is_err(), "{}", link.display());
    }
    assert!(!symlink.exists());
}

#[test]
fn test_install_sysv_uninstall_is_idempotent_second_removes_nothing() {
    let (_dir, manager, symlink) = temp_manager(ServiceBackend::SysV);
    let mut out = Vec::new();
    manager.install(capture(&mut out)).unwrap();
    manager.uninstall(capture(&mut out)).unwrap();
    assert!(!manager.init_script_path.exists());
    assert!(!symlink.exists());
}

#[test]
fn test_uninstall_removes_dangling_command_symlink_after_upgrade() {
    let (dir, manager, symlink) = temp_manager(ServiceBackend::SysV);
    let mut out = Vec::new();
    manager.install(capture(&mut out)).unwrap();
    // 升级场景:命令软链仍指向 init 脚本记录的可执行路径,但该旧 exe 已被删除
    // (dangling)。uninstall 应依据 init 脚本里的记录,仍把软链清理干净。
    fs::remove_file(&manager.executable).unwrap();
    assert!(symlink.symlink_metadata().unwrap().file_type().is_symlink());

    let mut out = Vec::new();
    manager.uninstall(capture(&mut out)).unwrap();
    assert!(!manager.init_script_path.exists());
    assert!(symlink.symlink_metadata().is_err(), "dangling 软链应被清理");
    drop(dir);
}

#[test]
fn test_install_refuses_overwrite_of_existing_script() {
    let (_dir, manager, _) = temp_manager(ServiceBackend::SysV);
    fs::create_dir_all(manager.init_script_path.parent().unwrap()).unwrap();
    fs::write(&manager.init_script_path, "#!/bin/sh\n# mine\n").unwrap();

    let mut out = Vec::new();
    let error = manager.install(capture(&mut out)).unwrap_err();
    assert!(error.contains("目标路径已存在"), "{error}");
    assert_eq!(
        fs::read_to_string(&manager.init_script_path).unwrap(),
        "#!/bin/sh\n# mine\n"
    );
}

#[test]
fn test_install_sysv_creates_runlevel_links() {
    let (_dir, manager, symlink) = temp_manager(ServiceBackend::SysV);
    let links = sysv_runlevel_links(&manager.init_script_path);

    let mut out = Vec::new();
    manager.install(capture(&mut out)).unwrap();
    assert!(manager.init_script_path.exists());
    for link in &links {
        assert!(
            link.symlink_metadata().unwrap().file_type().is_symlink(),
            "{}",
            link.display()
        );
    }
    assert!(symlink.symlink_metadata().unwrap().file_type().is_symlink());
}

#[test]
fn test_install_rolls_back_assets_when_command_symlink_blocked() {
    let (_dir, manager, _) = temp_manager(ServiceBackend::SysV);
    let links = sysv_runlevel_links(&manager.init_script_path);
    // 命令软链接路径被一个普通文件占用,create_symlink 应失败。
    fs::create_dir_all(manager.symlink_path.parent().unwrap()).unwrap();
    fs::write(&manager.symlink_path, "occupied").unwrap();

    let mut out = Vec::new();
    let error = manager.install(capture(&mut out)).unwrap_err();
    assert!(error.contains("创建命令软链接失败"), "{error}");
    assert!(!manager.init_script_path.exists());
    for link in &links {
        assert!(link.symlink_metadata().is_err(), "{}", link.display());
    }
    assert_eq!(
        fs::read_to_string(&manager.symlink_path).unwrap(),
        "occupied"
    );
}

#[test]
fn test_systemd_install_rolls_back_unit_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let executable = real_executable(dir.path());
    // 用不存在的唯一 unit 名触发 systemctl enable 失败,验证已写 unit 被回滚。
    let unit = dir
        .path()
        .join(format!("sv-install-{}.service", std::process::id()));
    let manager = ServiceManager::for_testing(
        executable,
        dir.path().join("bin/sv"),
        unit.clone(),
        dir.path().join("init.d").join(SERVICE_NAME),
        ServiceBackend::Systemd,
    );
    let mut out = Vec::new();
    let error = manager.install(capture(&mut out)).unwrap_err();
    assert!(error.contains("安装服务失败"), "{error}");
    assert!(!unit.exists(), "enable 失败后应回滚 unit 文件");
}

#[test]
fn test_uninstall_stops_sysv_service_before_removal() {
    let (dir, manager, symlink) = temp_manager(ServiceBackend::SysV);
    fs::create_dir_all(manager.init_script_path.parent().unwrap()).unwrap();
    let marker = dir.path().join("stop-called");
    let script = format!("#!/bin/sh\nprintf stopped > {}\n", shell_quote(&marker));
    fs::write(&manager.init_script_path, script).unwrap();
    fs::set_permissions(
        &manager.init_script_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    make_symlink(Path::new(&manager.executable), &symlink).unwrap();

    let mut out = Vec::new();
    manager.uninstall(capture(&mut out)).unwrap();
    assert_eq!(fs::read_to_string(marker).unwrap(), "stopped");
    assert!(!manager.init_script_path.exists());
    assert!(!symlink.exists());
}

#[test]
fn test_uninstall_removes_sysv_runlevel_links() {
    let (_dir, manager, symlink) = temp_manager(ServiceBackend::SysV);
    manager.write_init_script().unwrap();
    make_symlink(Path::new(&manager.executable), &symlink).unwrap();
    let links = sysv_runlevel_links(&manager.init_script_path);
    for link in &links {
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        make_symlink(&manager.init_script_path, link).unwrap();
    }

    let mut out = Vec::new();
    manager.uninstall(capture(&mut out)).unwrap();
    for link in links {
        assert!(link.symlink_metadata().is_err(), "{}", link.display());
    }
    assert!(!manager.init_script_path.exists());
}

#[test]
fn test_systemd_uninstall_when_not_installed_is_noop() {
    // unit 文件不存在即视为未安装:不触发 systemctl,直接成功 no-op。
    let dir = tempfile::tempdir().unwrap();
    let executable = real_executable(dir.path());
    let unit = dir
        .path()
        .join(format!("sv-uninstall-{}.service", std::process::id()));
    let manager = ServiceManager::for_testing(
        executable,
        dir.path().join("bin/sv"),
        unit,
        dir.path().join("init.d").join(SERVICE_NAME),
        ServiceBackend::Systemd,
    );
    let mut out = Vec::new();
    manager.uninstall(capture(&mut out)).unwrap();
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("服务未安装，无需卸载"), "{text}");
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
        executable.clone(),
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
    let error = manager
        .remove_symlink(capture(&mut out), &[executable])
        .unwrap_err();
    assert!(error.contains("拒绝删除指向其他文件的软链接"), "{error}");
}

#[test]
fn test_remove_symlink_removes_dangling_to_recorded_executable() {
    let dir = tempfile::tempdir().unwrap();
    let executable = real_executable(dir.path());
    let recorded = dir.path().join("old-install").join("sv");
    let symlink = dir.path().join("bin").join("sv");
    let manager = ServiceManager::for_testing(
        executable,
        symlink.clone(),
        dir.path().join("unit.service"),
        dir.path().join("init"),
        ServiceBackend::SysV,
    );
    // 升级场景:命令软链仍指向旧安装路径,且该旧路径已删除(dangling)。
    fs::create_dir_all(symlink.parent().unwrap()).unwrap();
    make_symlink(&recorded, &symlink).unwrap();
    let mut out = Vec::new();
    manager
        .remove_symlink(
            capture(&mut out),
            &[recorded.to_string_lossy().into_owned()],
        )
        .unwrap();
    assert!(symlink.symlink_metadata().is_err());
}

#[test]
fn test_remove_symlink_keeps_symlink_pointing_to_recorded_only() {
    // 记录路径与软链目标一致时应删除(读记录,不要求目标存在)。
    let dir = tempfile::tempdir().unwrap();
    let executable = real_executable(dir.path());
    let recorded = dir.path().join("bin").join("sv");
    let symlink = dir.path().join("usr-local-bin").join("sv");
    let manager = ServiceManager::for_testing(
        executable,
        symlink.clone(),
        dir.path().join("unit.service"),
        dir.path().join("init"),
        ServiceBackend::SysV,
    );
    fs::create_dir_all(symlink.parent().unwrap()).unwrap();
    make_symlink(&recorded, &symlink).unwrap();
    let mut out = Vec::new();
    manager
        .remove_symlink(
            capture(&mut out),
            &[recorded.to_string_lossy().into_owned()],
        )
        .unwrap();
    assert!(symlink.symlink_metadata().is_err());
}

#[test]
fn test_uninstall_when_never_installed_is_noop() {
    let (_dir, manager, _) = temp_manager(ServiceBackend::SysV);
    let mut out = Vec::new();
    manager.uninstall(capture(&mut out)).unwrap();
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("服务未安装，无需卸载"), "{text}");
}

#[test]
fn test_uninstall_cleans_leftover_command_symlink_only() {
    let (dir, manager, _) = temp_manager(ServiceBackend::SysV);
    // 服务文件不存在,但命令软链残留且指向当前可执行文件:应清理并报告。
    let symlink = dir.path().join("bin").join("sv");
    fs::create_dir_all(symlink.parent().unwrap()).unwrap();
    make_symlink(Path::new(&manager.executable), &symlink).unwrap();

    let mut out = Vec::new();
    manager.uninstall(capture(&mut out)).unwrap();
    assert!(symlink.symlink_metadata().is_err(), "残留软链应被清理");
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("已清理残留"), "{text}");
}
