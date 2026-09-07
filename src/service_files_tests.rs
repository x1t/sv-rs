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
    assert!(text.contains("exe='/usr/bin/sv'"));
    assert!(text.contains("pid=\"$(cat \"$pidfile\""));
    assert!(text.contains("not stopped"));
    assert!(text.contains("Usage: $0 {start|stop|restart|status}"));
}

#[test]
fn test_create_sysv_runlevel_links_creates_all_links() {
    let dir = tempfile::tempdir().unwrap();
    let init_script = dir.path().join("init.d").join(SERVICE_NAME);
    fs::create_dir_all(init_script.parent().unwrap()).unwrap();
    fs::write(&init_script, "#!/bin/sh\n").unwrap();

    create_sysv_runlevel_links(&init_script).unwrap();
    for link in runlevel_links(&init_script) {
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            fs::read_link(&link).unwrap(),
            init_script,
            "{}",
            link.display()
        );
    }
}

#[test]
fn test_create_sysv_runlevel_links_idempotent_for_own_links() {
    let dir = tempfile::tempdir().unwrap();
    let init_script = dir.path().join("init.d").join(SERVICE_NAME);
    let links = create_runlevel_links(&init_script);

    create_sysv_runlevel_links(&init_script).unwrap();
    for link in links {
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    }
}

#[test]
fn test_create_sysv_runlevel_links_refuses_foreign_link_and_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let init_script = dir.path().join("init.d").join(SERVICE_NAME);
    fs::create_dir_all(init_script.parent().unwrap()).unwrap();
    fs::write(&init_script, "#!/bin/sh\n").unwrap();
    let links = runlevel_links(&init_script);
    let other = dir.path().join("other-service");
    fs::write(&other, "other").unwrap();

    // 先占用其中一个启动链接为指向其他文件,创建应失败并回滚已建链接。
    fs::create_dir_all(links[1].parent().unwrap()).unwrap();
    make_symlink(&other, &links[1]).unwrap();
    let error = create_sysv_runlevel_links(&init_script).unwrap_err();
    assert!(error.contains("指向其他文件"), "{error}");
    assert!(
        links[0].symlink_metadata().is_err(),
        "应回滚 {}",
        links[0].display()
    );
    assert!(links[1].symlink_metadata().is_ok());
}

#[test]
fn test_systemd_unit_executable_roundtrip() {
    for path in ["/opt/sv", "/opt/my app/sv-rs", "/usr/bin/sv'rs"] {
        let text = systemd_unit_text(path);
        assert_eq!(
            systemd_unit_executable(&text).as_deref(),
            Some(path),
            "path={path:?}"
        );
    }
}

#[test]
fn test_systemd_unit_executable_rejects_non_absolute_or_missing() {
    assert_eq!(
        systemd_unit_executable("[Service]\nExecStart=rel/sv daemon\n"),
        None
    );
    assert_eq!(systemd_unit_executable("[Unit]\nDescription=x\n"), None);
}

#[test]
fn test_sysv_init_executable_roundtrip() {
    for path in ["/usr/bin/sv", "/opt/my app/sv'rs", "/opt/it's sv"] {
        let text = sysv_init_text(path);
        assert_eq!(
            sysv_init_executable(&text).as_deref(),
            Some(path),
            "path={path:?}"
        );
    }
}

#[test]
fn test_sysv_init_executable_rejects_malformed_or_missing() {
    assert_eq!(sysv_init_executable("exe=\"plain\"\n"), None);
    assert_eq!(sysv_init_executable("name=sv\n"), None);
    assert_eq!(sysv_init_executable("exe=''\n"), None);
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

fn runlevel_links(init_script: &Path) -> Vec<PathBuf> {
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

fn create_runlevel_links(init_script: &Path) -> Vec<PathBuf> {
    fs::create_dir_all(init_script.parent().unwrap()).unwrap();
    fs::write(init_script, "#!/bin/sh\n").unwrap();
    let links = runlevel_links(init_script);
    for link in &links {
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        make_symlink(init_script, link).unwrap();
    }
    links
}

#[test]
fn test_remove_sysv_runlevel_links_removes_owned_links() {
    let dir = tempfile::tempdir().unwrap();
    let init_script = dir.path().join("init.d").join(SERVICE_NAME);
    let links = create_runlevel_links(&init_script);

    remove_sysv_runlevel_links(&init_script).unwrap();
    for link in links {
        assert!(link.symlink_metadata().is_err(), "{}", link.display());
    }
}

#[test]
fn test_remove_sysv_runlevel_links_supports_dangling_links() {
    let dir = tempfile::tempdir().unwrap();
    let init_script = dir.path().join("init.d").join(SERVICE_NAME);
    let links = create_runlevel_links(&init_script);
    fs::remove_file(&init_script).unwrap();

    remove_sysv_runlevel_links(&init_script).unwrap();
    for link in links {
        assert!(link.symlink_metadata().is_err(), "{}", link.display());
    }
}

#[test]
fn test_remove_sysv_runlevel_links_refuses_foreign_link() {
    let dir = tempfile::tempdir().unwrap();
    let init_script = dir.path().join("init.d").join(SERVICE_NAME);
    let links = create_runlevel_links(&init_script);
    let other = dir.path().join("other-service");
    fs::write(&other, "other").unwrap();
    fs::remove_file(&links[0]).unwrap();
    make_symlink(&other, &links[0]).unwrap();

    let error = remove_sysv_runlevel_links(&init_script).unwrap_err();
    assert!(error.contains("指向其他文件"), "{error}");
    for link in links {
        assert!(link.symlink_metadata().is_ok(), "{}", link.display());
    }
}

#[test]
fn test_resolve_own_executable_is_absolute() {
    let executable = resolve_own_executable();
    assert!(!executable.is_empty());
    assert!(Path::new(&executable).is_absolute());
}
