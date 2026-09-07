use super::*;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

fn runlevel_links(init_script: &Path) -> Vec<PathBuf> {
    sysv_runlevel_link_paths(init_script)
}

fn create_owned_runlevel_links(init_script: &Path) -> Vec<PathBuf> {
    fs::create_dir_all(init_script.parent().unwrap()).unwrap();
    fs::write(init_script, "#!/bin/sh\n").unwrap();
    let links = runlevel_links(init_script);
    for link in &links {
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(init_script, link).unwrap();
    }
    links
}

#[test]
fn test_systemd_unit_text_is_recognizable() {
    let text = systemd_unit_text("/opt/sv");
    assert!(text.starts_with("[Unit]"));
    assert!(text.contains("ExecStart=/opt/sv daemon"));
    assert!(text.contains("Restart=always"));
    assert!(text.contains("WantedBy=multi-user.target"));
}

#[test]
fn test_sysv_script_text_is_recognizable() {
    let text = sysv_init_text("/usr/bin/sv");
    assert!(text.starts_with("#!/bin/sh"));
    assert!(text.contains("Provides:          sv-supervisor-manager"));
    assert!(text.contains("exe='/usr/bin/sv'"));
    assert!(text.contains("pidfile=/run/$name.pid"));
}

#[test]
fn test_systemd_unit_executable_roundtrip() {
    for path in ["/opt/sv", "/opt/my app/sv-rs", "/usr/bin/sv'rs"] {
        let text = systemd_unit_text(path);
        assert_eq!(systemd_unit_executable(&text).as_deref(), Some(path));
    }
}

#[test]
fn test_systemd_unit_executable_rejects_invalid_text() {
    assert_eq!(
        systemd_unit_executable("[Service]\nExecStart=relative/sv daemon\n"),
        None
    );
    assert_eq!(systemd_unit_executable("[Unit]\nDescription=x\n"), None);
}

#[test]
fn test_sysv_init_executable_roundtrip() {
    for path in ["/usr/bin/sv", "/opt/my app/sv'rs", "/opt/it's sv"] {
        let text = sysv_init_text(path);
        assert_eq!(sysv_init_executable(&text).as_deref(), Some(path));
    }
}

#[test]
fn test_sysv_init_executable_rejects_invalid_text() {
    assert_eq!(sysv_init_executable("exe=\"plain\"\n"), None);
    assert_eq!(sysv_init_executable("name=sv\n"), None);
    assert_eq!(sysv_init_executable("exe=''\n"), None);
}

#[test]
fn test_remove_sysv_runlevel_links_removes_owned_links() {
    let dir = tempfile::tempdir().unwrap();
    let init_script = dir.path().join("init.d").join(SERVICE_NAME);
    let links = create_owned_runlevel_links(&init_script);

    remove_sysv_runlevel_links(&init_script).unwrap();
    for link in links {
        assert!(link.symlink_metadata().is_err(), "{}", link.display());
    }
    assert!(init_script.exists());
}

#[test]
fn test_remove_sysv_runlevel_links_supports_relative_and_dangling_links() {
    let dir = tempfile::tempdir().unwrap();
    let init_script = dir.path().join("init.d").join(SERVICE_NAME);
    fs::create_dir_all(init_script.parent().unwrap()).unwrap();
    fs::write(&init_script, "#!/bin/sh\n").unwrap();
    let links = runlevel_links(&init_script);
    for link in &links {
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        let relative = pathdiff(link.parent().unwrap(), &init_script);
        symlink(relative, link).unwrap();
    }
    fs::remove_file(&init_script).unwrap();

    remove_sysv_runlevel_links(&init_script).unwrap();
    for link in links {
        assert!(link.symlink_metadata().is_err(), "{}", link.display());
    }
}

#[test]
fn test_remove_sysv_runlevel_links_refuses_foreign_link_without_partial_removal() {
    let dir = tempfile::tempdir().unwrap();
    let init_script = dir.path().join("init.d").join(SERVICE_NAME);
    let links = create_owned_runlevel_links(&init_script);
    let foreign = dir.path().join("foreign-service");
    fs::write(&foreign, "foreign").unwrap();
    fs::remove_file(&links[0]).unwrap();
    symlink(&foreign, &links[0]).unwrap();

    let error = remove_sysv_runlevel_links(&init_script).unwrap_err();
    assert!(error.contains("指向其他文件"), "{error}");
    for link in links {
        assert!(link.symlink_metadata().is_ok(), "{}", link.display());
    }
}

#[test]
fn test_remove_sysv_runlevel_links_refuses_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let init_script = dir.path().join("init.d").join(SERVICE_NAME);
    let links = create_owned_runlevel_links(&init_script);
    fs::remove_file(&links[0]).unwrap();
    fs::write(&links[0], "not a link").unwrap();

    let error = remove_sysv_runlevel_links(&init_script).unwrap_err();
    assert!(error.contains("非软链接"), "{error}");
    for link in links {
        assert!(link.symlink_metadata().is_ok(), "{}", link.display());
    }
}

#[test]
fn test_systemd_enable_link_validation_and_removal() {
    let dir = tempfile::tempdir().unwrap();
    let unit = dir.path().join("systemd").join("sv.service");
    let link = systemd_enable_link_path(&unit).unwrap();
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink(&unit, &link).unwrap();

    assert!(validate_systemd_enable_link(&unit).unwrap());
    assert!(remove_systemd_enable_link(&unit).unwrap());
    assert!(link.symlink_metadata().is_err());
}

#[test]
fn test_systemd_enable_link_validation_rejects_foreign_target() {
    let dir = tempfile::tempdir().unwrap();
    let unit = dir.path().join("systemd").join("sv.service");
    let foreign = dir.path().join("foreign.service");
    let link = systemd_enable_link_path(&unit).unwrap();
    fs::write(&foreign, "foreign").unwrap();
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    symlink(&foreign, &link).unwrap();

    let error = validate_systemd_enable_link(&unit).unwrap_err();
    assert!(error.contains("指向其他文件"), "{error}");
    assert!(link.symlink_metadata().is_ok());
}

#[test]
fn test_systemd_enable_link_validation_rejects_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let unit = dir.path().join("systemd").join("sv.service");
    let link = systemd_enable_link_path(&unit).unwrap();
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    fs::write(&link, "foreign").unwrap();

    let error = validate_systemd_enable_link(&unit).unwrap_err();
    assert!(error.contains("非软链接"), "{error}");
}

#[test]
fn test_resolve_own_executable_is_absolute() {
    let executable = resolve_own_executable();
    assert!(!executable.is_empty());
    assert!(Path::new(&executable).is_absolute());
}

fn pathdiff(base: &Path, target: &Path) -> PathBuf {
    let base = base.components().collect::<Vec<_>>();
    let target = target.components().collect::<Vec<_>>();
    let common = base
        .iter()
        .zip(&target)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in common..base.len() {
        relative.push("..");
    }
    for component in &target[common..] {
        relative.push(component.as_os_str());
    }
    relative
}
