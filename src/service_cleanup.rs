//! 跨后端服务卸载与残留资产清理。
//!
//! 安装时只使用当前探测到的后端;卸载则检查两套已知资产,避免运行环境变化后
//! 遗留另一套 service 文件、启停链接或 systemd enable 链接。

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use crate::service::{LegacyServiceCleanup, path_lstat, write_line};
use crate::service_files::{
    ServiceBackend, remove_systemd_enable_link, remove_sysv_runlevel_links,
    systemd_enable_link_path, sysv_runlevel_link_paths, validate_systemd_enable_link,
    validate_sysv_runlevel_links,
};
use crate::util::{command_available, io_context, run_command};

const ALL_BACKENDS: [ServiceBackend; 2] = [ServiceBackend::Systemd, ServiceBackend::SysV];

impl LegacyServiceCleanup {
    /// 跨 systemd/SysV 卸载所有本程序可证明归属的服务资产。
    pub(crate) fn uninstall_all(&self, out: &mut dyn Write) -> Result<(), String> {
        let installed = self.collect_owned_services()?;
        self.validate_backend_links()?;
        self.ensure_systemd_control_available(&installed)?;
        let owned_paths = self.uninstall_symlink_owned_all();
        let symlink_present = self
            .validate_symlink_owned(&owned_paths)
            .map_err(|error| format!("卸载前检查命令软链接失败: {error}"))?;
        let mut cleaned_anything = false;

        for backend in &installed {
            self.remove_backend(*backend)?;
            cleaned_anything = true;
        }
        cleaned_anything |= self.clean_residual_links()?;

        let symlink_exists = path_lstat(&self.symlink_path)
            .map_err(|error| format!("卸载前检查命令软链接失败: {error}"))?
            .is_some();
        if symlink_present || symlink_exists {
            self.remove_symlink(out, &owned_paths)
                .map_err(|error| format!("服务已卸载，但移除命令软链接失败: {error}"))?;
            cleaned_anything = true;
        }

        if !installed.is_empty() {
            write_line(out, "✅ SV 系统服务卸载成功")
        } else if cleaned_anything {
            write_line(out, "✅ 服务未安装，已清理残留文件/软链接")
        } else {
            write_line(out, "服务未安装，无需卸载")
        }
    }

    fn collect_owned_services(&self) -> Result<Vec<ServiceBackend>, String> {
        let mut installed = Vec::new();
        for backend in ALL_BACKENDS {
            let path = self.service_path(backend);
            if !validate_service_file_path(path)? {
                continue;
            }
            let recorded = self.recorded_executable_for(backend).unwrap_or_default();
            if !self.service_file_owned_for(backend, &recorded) {
                return Err(format!(
                    "拒绝卸载非本程序注册的服务文件: {}",
                    path.display()
                ));
            }
            installed.push(backend);
        }
        Ok(installed)
    }

    fn ensure_systemd_control_available(&self, installed: &[ServiceBackend]) -> Result<(), String> {
        if !self.systemd_active {
            return Ok(());
        }
        let has_unit = installed.contains(&ServiceBackend::Systemd);
        let enable_link = systemd_enable_link_path(&self.unit_path);
        let has_enable_link = enable_link
            .as_deref()
            .map(path_lstat)
            .transpose()
            .map_err(|error| format!("卸载服务失败，检查 systemd 启动链接: {error}"))?
            .flatten()
            .is_some();
        if (has_unit || has_enable_link) && !command_available(&self.systemctl_path) {
            return Err(format!(
                "卸载服务失败: systemd 正在运行但找不到 systemctl: {}",
                self.systemctl_path
            ));
        }
        Ok(())
    }

    fn validate_backend_links(&self) -> Result<(), String> {
        for backend in ALL_BACKENDS {
            match backend {
                ServiceBackend::Systemd => {
                    validate_systemd_enable_link(&self.unit_path)
                        .map_err(|error| format!("卸载服务失败: {error}"))?;
                }
                ServiceBackend::SysV => {
                    validate_sysv_runlevel_links(&self.init_script_path)
                        .map_err(|error| format!("卸载服务失败: {error}"))?;
                }
            }
        }
        Ok(())
    }

    fn remove_backend(&self, backend: ServiceBackend) -> Result<(), String> {
        let should_control = match backend {
            ServiceBackend::Systemd => self.systemd_active,
            ServiceBackend::SysV => true,
        };
        if should_control {
            let path = self.service_path(backend);
            if !validate_service_file_path(path)? {
                return Err(format!(
                    "卸载服务失败，服务文件在控制前消失: {}",
                    path.display()
                ));
            }
            self.service_control_for(backend, "stop")
                .map_err(|error| format!("卸载服务失败: {error}"))?;
        }

        match backend {
            ServiceBackend::Systemd => {
                remove_systemd_enable_link(&self.unit_path)
                    .map_err(|error| format!("卸载服务失败: {error}"))?;
                remove_systemd_unit(&self.unit_path)?;
                if self.systemd_active {
                    self.run_systemctl(&["daemon-reload"])
                        .map_err(|error| format!("卸载服务失败: {error}"))?;
                }
            }
            ServiceBackend::SysV => {
                remove_sysv_runlevel_links(&self.init_script_path)
                    .map_err(|error| format!("卸载服务失败: {error}"))?;
                remove_service_file(&self.init_script_path)?;
            }
        }
        Ok(())
    }

    fn clean_residual_links(&self) -> Result<bool, String> {
        let mut cleaned = false;
        let has_sysv_links = sysv_runlevel_link_paths(&self.init_script_path)
            .iter()
            .map(|link| {
                path_lstat(link)
                    .map(|metadata| metadata.is_some())
                    .map_err(|error| io_context("检查 SysV 启动链接", error))
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .any(|present| present);
        if has_sysv_links {
            remove_sysv_runlevel_links(&self.init_script_path)
                .map_err(|error| format!("卸载服务失败: {error}"))?;
            cleaned = true;
        }

        let has_systemd_link = match systemd_enable_link_path(&self.unit_path) {
            Some(path) => path_lstat(&path)
                .map(|metadata| metadata.is_some())
                .map_err(|error| io_context("检查 systemd 启动链接", error))?,
            None => false,
        };
        if has_systemd_link
            && remove_systemd_enable_link(&self.unit_path)
                .map_err(|error| format!("卸载服务失败: {error}"))?
        {
            cleaned = true;
            if self.systemd_active {
                self.run_systemctl(&["daemon-reload"])
                    .map_err(|error| format!("卸载服务失败: {error}"))?;
            }
        }
        Ok(cleaned)
    }

    fn service_path(&self, backend: ServiceBackend) -> &std::path::Path {
        match backend {
            ServiceBackend::Systemd => &self.unit_path,
            ServiceBackend::SysV => &self.init_script_path,
        }
    }

    /// 按指定后端执行 stop,供跨后端卸载使用。
    pub(crate) fn service_control_for(
        &self,
        backend: ServiceBackend,
        action: &str,
    ) -> Result<(), String> {
        match backend {
            ServiceBackend::Systemd => self
                .run_systemctl(&[action, &self.unit_file_name()])
                .map(|_| ()),
            ServiceBackend::SysV => {
                let script = self.init_script_path.to_str().unwrap_or_default();
                let outcome = run_command(script, &[action.to_string()], Duration::from_secs(30))
                    .map_err(|error| format!("{script} {action}: {error}"))?;
                let text = outcome.combined_text();
                match outcome.code {
                    Some(0) => Ok(()),
                    Some(code) => Err(format!("exit status {code} ({})", text.trim())),
                    None => Err(format!("signal: killed ({})", text.trim())),
                }
            }
        }
    }
}

fn validate_service_file_path(path: &Path) -> Result<bool, String> {
    let Some(info) = path_lstat(path).map_err(|error| format!("检查服务文件失败: {error}"))?
    else {
        return Ok(false);
    };
    if info.file_type().is_symlink() {
        return Err(format!("拒绝卸载服务路径软链接: {}", path.display()));
    }
    if !info.file_type().is_file() {
        return Err(format!("拒绝卸载非普通服务文件: {}", path.display()));
    }
    Ok(true)
}

fn remove_service_file(path: &Path) -> Result<(), String> {
    if validate_service_file_path(path)? {
        fs::remove_file(path).map_err(|error| format!("卸载服务失败: {error}"))?;
    }
    Ok(())
}

fn remove_systemd_unit(path: &Path) -> Result<(), String> {
    remove_service_file(path)
}
