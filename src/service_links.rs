use std::fs;
use std::io::Write;
use std::path::Path;

use crate::service::{LegacyServiceCleanup, resolve_link_target, write_line};
use crate::service_files::{
    ServiceBackend, clean_path, systemd_unit_executable, systemd_unit_text, sysv_init_executable,
    sysv_init_text,
};
use crate::util::io_context;

impl LegacyServiceCleanup {
    /// 预检命令软链接是否存在且明确指向本程序资产,不修改文件系统。
    /// 用 readlink 的原始文本比较,允许目标已被删除(dangling 软链)——
    /// 因为升级后旧 exe 可能已不存在,但软链仍指向服务文件记录的那条路径。
    pub(crate) fn validate_symlink_owned(&self, owned_paths: &[String]) -> Result<bool, String> {
        match fs::symlink_metadata(&self.symlink_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(io_context("检查软链接", error)),
            Ok(info) => {
                if !info.file_type().is_symlink() {
                    return Err(format!(
                        "拒绝删除非软链接路径: {}",
                        self.symlink_path.display()
                    ));
                }
                let target = fs::read_link(&self.symlink_path)
                    .map_err(|e| io_context("读取软链接目标", e))?;
                let resolved = resolve_link_target(&self.symlink_path, &target);
                if !owned_paths
                    .iter()
                    .map(|path| clean_path(Path::new(path)))
                    .any(|candidate| candidate == resolved)
                {
                    return Err(format!(
                        "拒绝删除指向其他文件的软链接: {}",
                        self.symlink_path.display()
                    ));
                }
                Ok(true)
            }
        }
    }

    /// 移除命令软链接。删除前再次执行归属校验,防止预检后软链接被替换。
    pub(crate) fn remove_symlink(
        &self,
        out: &mut dyn Write,
        owned_paths: &[String],
    ) -> Result<(), String> {
        if !self.validate_symlink_owned(owned_paths)? {
            return Ok(());
        }
        fs::remove_file(&self.symlink_path).map_err(|e| io_context("删除软链接", e))?;
        write_line(
            out,
            &format!("✅ 已删除软链接: {}", self.symlink_path.display()),
        )
    }

    /// 卸载时用于判定命令软链归属的候选路径:合并两套服务文件记录与当前 exe。
    pub(crate) fn uninstall_symlink_owned_all(&self) -> Vec<String> {
        let mut owned = Vec::new();
        for backend in [ServiceBackend::Systemd, ServiceBackend::SysV] {
            if let Some(recorded) = self.recorded_executable_for(backend)
                && !owned.contains(&recorded)
            {
                owned.push(recorded);
            }
        }
        if !self.executable.is_empty() && !owned.contains(&self.executable) {
            owned.push(self.executable.clone());
        }
        owned
    }

    /// 从指定服务注册文件解析安装时记录的可执行路径。
    pub(crate) fn recorded_executable_for(&self, backend: ServiceBackend) -> Option<String> {
        let path = match backend {
            ServiceBackend::Systemd => &self.unit_path,
            ServiceBackend::SysV => &self.init_script_path,
        };
        let info = fs::symlink_metadata(path).ok()?;
        if !info.file_type().is_file() {
            return None;
        }
        let content = fs::read_to_string(path).ok()?;
        match backend {
            ServiceBackend::Systemd => systemd_unit_executable(&content),
            ServiceBackend::SysV => sysv_init_executable(&content),
        }
    }

    /// 判断服务注册文件是否为 sv 安装的资产(防止 uninstall 误删占用本服务名的
    /// 异源文件)。归属证据二选一:记录的可执行路径等于当前 sv(含升级后仍用同一
    /// 路径的情形),或等于命令软链当前指向的路径(覆盖升级换路径/旧 exe 已删除,
    /// 软链 readlink 原文匹配即可,允许 dangling)。
    pub(crate) fn service_file_owned(&self, recorded: &str) -> bool {
        if recorded.is_empty() {
            return false;
        }
        let recorded = clean_path(Path::new(recorded));
        if !self.executable.is_empty() && clean_path(Path::new(&self.executable)) == recorded {
            return true;
        }
        match fs::symlink_metadata(&self.symlink_path) {
            Ok(info) if info.file_type().is_symlink() => fs::read_link(&self.symlink_path)
                .map(|target| resolve_link_target(&self.symlink_path, &target) == recorded)
                .unwrap_or(false),
            _ => false,
        }
    }

    /// 校验服务文件仍匹配当前或旧版生成模板,避免仅凭 exe 路径删除异源配置。
    pub(crate) fn service_file_owned_for(&self, backend: ServiceBackend, recorded: &str) -> bool {
        if !self.service_file_owned(recorded) {
            return false;
        }
        let path = match backend {
            ServiceBackend::Systemd => &self.unit_path,
            ServiceBackend::SysV => &self.init_script_path,
        };
        let expected = match backend {
            ServiceBackend::Systemd => systemd_unit_text(recorded),
            ServiceBackend::SysV => sysv_init_text(recorded),
        };
        fs::read_to_string(path)
            .map(|content| content == expected)
            .unwrap_or(false)
    }
}
