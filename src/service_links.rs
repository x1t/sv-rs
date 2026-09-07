use std::fs;
use std::io::Write;
use std::path::Path;

use crate::service::{ServiceManager, resolve_link_target, write_line};
use crate::service_files::{
    ServiceBackend, clean_path, make_symlink, resolve_own_executable, systemd_unit_executable,
    sysv_init_executable,
};
use crate::util::io_context;

impl ServiceManager {
    /// 创建命令软链接(镜像 Go `createSymlink`)。
    pub(crate) fn create_symlink(&self, out: &mut dyn Write) -> Result<(), String> {
        self.require_executable()?;
        let directory = self
            .symlink_path
            .parent()
            .ok_or_else(|| "软链接路径不能为空".to_string())?;
        fs::create_dir_all(directory).map_err(|e| io_context("创建软链接目录", e))?;

        match fs::symlink_metadata(&self.symlink_path) {
            Ok(info) => {
                if !info.file_type().is_symlink() {
                    return Err(format!(
                        "目标路径已存在且不是软链接: {}",
                        self.symlink_path.display()
                    ));
                }
                let resolved_target = fs::canonicalize(&self.symlink_path).ok();
                let resolved_executable = fs::canonicalize(&self.executable).ok();
                if resolved_target == resolved_executable {
                    return Ok(());
                }
                Err(format!(
                    "目标软链接已存在且指向其他文件: {}",
                    self.symlink_path.display()
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                make_symlink(Path::new(&self.executable), &self.symlink_path)
                    .map_err(|e| io_context("创建软链接", e))?;
                write_line(
                    out,
                    &format!(
                        "🔗 已创建软链接: {} -> {}",
                        self.symlink_path.display(),
                        self.executable
                    ),
                )
            }
            Err(error) => Err(io_context("检查软链接目标", error)),
        }
    }

    /// 移除命令软链接。`owned_paths` 是判定归属的可执行路径集合:
    /// 服务文件里记录的可执行路径 + 当前进程可执行路径任一匹配即删。
    /// 用 readlink 的原始文本比较,允许目标已被删除(dangling 软链)——
    /// 因为升级后旧 exe 可能已不存在,但软链仍指向服务文件记录的那条路径。
    pub(crate) fn remove_symlink(
        &self,
        out: &mut dyn Write,
        owned_paths: &[String],
    ) -> Result<(), String> {
        match fs::symlink_metadata(&self.symlink_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
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
                fs::remove_file(&self.symlink_path).map_err(|e| io_context("删除软链接", e))?;
                write_line(
                    out,
                    &format!("✅ 已删除软链接: {}", self.symlink_path.display()),
                )
            }
        }
    }

    /// 卸载时用于判定命令软链归属的候选路径:服务文件记录的可执行路径在前,
    /// 当前运行的可执行路径兜底(两者都可能为空则返回空集)。
    pub(crate) fn uninstall_symlink_owned(&self) -> Vec<String> {
        let mut owned = Vec::new();
        if let Some(recorded) = self.recorded_executable() {
            owned.push(recorded);
        }
        if !self.executable.is_empty() {
            owned.push(self.executable.clone());
        }
        owned
    }

    /// 从服务注册文件解析安装时记录的可执行路径(卸载前读取,删除文件前调用)。
    pub(crate) fn recorded_executable(&self) -> Option<String> {
        match self.backend {
            ServiceBackend::Systemd => fs::read_to_string(&self.unit_path)
                .ok()
                .as_deref()
                .and_then(systemd_unit_executable),
            ServiceBackend::SysV => fs::read_to_string(&self.init_script_path)
                .ok()
                .as_deref()
                .and_then(sysv_init_executable),
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

    fn require_executable(&self) -> Result<(), String> {
        if !self.executable.is_empty() {
            return Ok(());
        }
        let executable = resolve_own_executable();
        if executable.is_empty() {
            return Err("获取可执行文件路径失败".to_string());
        }
        let info = fs::metadata(&executable).map_err(|e| io_context("获取可执行文件状态", e))?;
        if info.is_dir() {
            return Err(format!("可执行文件路径指向目录: {executable}"));
        }
        Ok(())
    }
}
