//! CLI 分发与渲染(镜像 Go `pkg/cli/app.go` 与 `pkg/cli/renderer.go`)。
//!
//! 所有业务输出只写 `out`,错误经 `Result` 返回由 `main` 统一以 `❌ ` 前缀打到 stderr,
//! 与 Go `main.go` 的 `fmt.Fprintf(os.Stderr, "❌ %v\n", err); os.Exit(1)` 语义一致。

use std::io::Write;

use crate::client::RpcClient;
use crate::config::ConfigDetector;
use crate::parse::{ProcessInfo, action_icon, color_by_state};
use crate::render::{NO_PROCESSES_TEXT, render_table};
use crate::service::ServiceManager;
use crate::spec;
use crate::util::io_context;

/// 完整使用说明(与 Go `PrintUsage` 逐字一致)。
const USAGE: &str = "sv - Supervisor进程管理工具

用法:
  sv status                    # 显示所有进程状态
  sv list                      # 显示所有进程状态（同 status）
  sv ls                        # list 的简写
  sv start <进程>              # 启动进程
  sv stop <进程>               # 停止进程
  sv restart <进程>            # 重启进程
  sv configure rpc             # 检查并补齐 RPC 配置
  sv configure rpc --dry-run   # 只预览配置变更
  sv configure rpc --restart   # 配置后显式重启 Supervisor
  sv service <action>          # 服务管理
  sv daemon                    # 运行服务守护进程

进程参数支持:
  序号      sv restart 1
  名称      sv restart myapp
  多个      sv restart 1 3 5
  范围      sv restart 1-5

服务操作:
  install   安装 sv 为系统服务
  uninstall 卸载 sv 系统服务
  start     启动 sv 系统服务
  stop      停止 sv 系统服务
  restart   重启 sv 系统服务
  status    查看 sv 服务状态

环境变量:
  SUPERVISOR_HOST              # Supervisor RPC 地址（默认: http://localhost:9001/RPC2）
  SUPERVISOR_USER              # 用户名（可选）
  SUPERVISOR_PASSWORD          # 密码（可选）";

fn write_text(out: &mut dyn Write, text: &str) -> Result<(), String> {
    out.write_all(text.as_bytes())
        .map_err(|e| io_context("写入输出失败", e))
}

/// 打印使用说明(末尾带一个换行)。
fn print_usage(out: &mut dyn Write) -> Result<(), String> {
    write_text(out, USAGE).and_then(|_| write_text(out, "\n"))
}

/// 依据 `SUPERVISOR_*` 环境变量构造 RPC 客户端。
fn new_client() -> RpcClient {
    let (host, username, password) = ConfigDetector::read_env_config();
    RpcClient::new(&host, &username, &password)
}

/// 进程状态表格的每一行;状态列按 `color` 决定是否加 ANSI 颜色。
fn status_rows(processes: &[ProcessInfo], color: bool) -> Vec<Vec<String>> {
    processes
        .iter()
        .map(|process| {
            let pid = if process.pid > 0 {
                process.pid.to_string()
            } else {
                "-".to_string()
            };
            let mut state = process.state_name.trim().to_string();
            if state.is_empty() {
                state = "UNKNOWN".to_string();
            }
            if color {
                state = format!("{}{}\x1b[0m", color_by_state(process.state), state);
            }
            vec![
                process.index.to_string(),
                process.name.clone(),
                state,
                pid,
                process.uptime.clone(),
            ]
        })
        .collect()
}

/// 显示进程状态。
fn show_status(out: &mut dyn Write, color: bool) -> Result<(), String> {
    let client = new_client();
    let processes = client
        .get_all_processes()
        .map_err(|e| format!("获取进程状态失败: {e}"))?;

    write_text(
        out,
        &format!("\n🔍 Supervisor进程状态 (共{}个进程)\n", processes.len()),
    )?;
    if processes.is_empty() {
        write_text(out, NO_PROCESSES_TEXT)?;
        write_text(out, "\n")?;
    } else {
        let headers = ["序号", "名称", "状态", "PID", "运行时间"].map(|header| header.to_string());
        let rows = status_rows(&processes, color);
        let table = render_table(&headers, &rows);
        write_text(out, &table)?;
        write_text(out, "\n")?;
    }
    write_text(
        out,
        "\n💡 提示: 使用 'sv start/stop/restart <序号>' 来控制进程\n",
    )
}

/// 对多个进程执行启动/停止/重启。
fn control_processes(args: &[String], action: &str, out: &mut dyn Write) -> Result<(), String> {
    let client = new_client();
    let processes = client
        .get_all_processes()
        .map_err(|e| format!("获取进程信息失败: {e}"))?;
    let process_names = spec::parse_process_indices(args, &processes)
        .map_err(|e| format!("解析进程参数失败: {e}"))?;

    write_text(out, &format!("🎯 正在执行 '{action}' 操作...\n"))?;
    let mut failures = Vec::new();
    for name in &process_names {
        write_text(out, &format!("  {} 进程 {name} ... ", action_icon(action)))?;
        match client.control_process(action, name) {
            Ok(()) => write_text(out, "✅ 成功\n")?,
            Err(error) => {
                failures.push(format!("{name}: {error}"));
                write_text(out, "❌ 失败\n")?;
            }
        }
    }

    let success_count = process_names.len() - failures.len();
    write_text(
        out,
        &format!(
            "\n📊 操作完成: 成功 {success_count} 个，失败 {} 个\n",
            failures.len()
        ),
    )?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("部分进程操作失败: {}", failures.join("; ")))
    }
}

/// 解析 `configure rpc` 参数并执行(镜像 Go `app.configure`)。
fn configure(args: &[String], out: &mut dyn Write) -> Result<(), String> {
    if args.is_empty() || !args[0].eq_ignore_ascii_case("rpc") {
        return Err("用法: sv configure rpc [--dry-run] [--restart]".to_string());
    }
    let mut dry_run = false;
    let mut restart = false;
    for argument in &args[1..] {
        match argument.as_str() {
            "--dry-run" => dry_run = true,
            "--restart" => restart = true,
            other => return Err(format!("未知配置参数: {other}")),
        }
    }
    if dry_run && restart {
        return Err("--dry-run不能与--restart同时使用".to_string());
    }

    let detector = ConfigDetector::new();
    let message = detector.configure_rpc(dry_run)?;
    write_text(out, &message)?;
    write_text(out, "\n")?;
    if restart {
        detector.restart_supervisor()?;
        write_text(out, "Supervisor 已重启，RPC 配置已生效\n")?;
    }
    Ok(())
}

/// 按参数分发命令(镜像 Go `app.RunArgs`)。
pub fn run(args: &[String], out: &mut dyn Write, color: bool) -> Result<(), String> {
    if args.is_empty() {
        return print_usage(out);
    }

    let command = args[0].trim().to_lowercase();
    let command_args = &args[1..];
    match command.as_str() {
        "help" | "-h" | "--help" => print_usage(out),
        "service" => ServiceManager::new().handle_command(command_args, out),
        "configure" => configure(command_args, out),
        "daemon" => {
            if !command_args.is_empty() {
                return Err("daemon不接受额外参数".to_string());
            }
            ServiceManager::new().run_daemon(out)
        }
        "status" | "list" | "ls" => {
            if !command_args.is_empty() {
                return Err(format!("{command}不接受额外参数"));
            }
            show_status(out, color)
        }
        "start" | "stop" | "restart" => {
            if command_args.is_empty() {
                return Err(format!("用法: sv {command} <进程序号|进程名称|范围>"));
            }
            control_processes(command_args, &command, out)
        }
        _ => {
            print_usage(out)?;
            Err(format!("未知命令: {command}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_capture(args: &[&str]) -> (Result<(), String>, String) {
        let mut out = Vec::new();
        let owned: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let result = run(&owned, &mut out, false);
        (result, String::from_utf8(out).unwrap())
    }

    #[test]
    fn test_no_args_prints_usage() {
        let (result, out) = run_capture(&[]);
        assert!(result.is_ok());
        assert!(out.starts_with("sv - Supervisor进程管理工具\n"));
        assert!(out.trim_end().ends_with("密码（可选）"));
    }

    #[test]
    fn test_help_prints_usage() {
        for flag in ["help", "-h", "--help"] {
            let (result, out) = run_capture(&[flag]);
            assert!(result.is_ok(), "{flag}");
            assert!(out.contains("sv restart <进程>"));
        }
    }

    #[test]
    fn test_unknown_command_prints_usage_and_error() {
        let (result, out) = run_capture(&["nosuchcmd"]);
        let error = result.unwrap_err();
        assert_eq!(error, "未知命令: nosuchcmd");
        assert!(out.starts_with("sv - Supervisor进程管理工具"));
    }

    #[test]
    fn test_restart_without_target_errors() {
        let (result, out) = run_capture(&["restart"]);
        let error = result.unwrap_err();
        assert_eq!(error, "用法: sv restart <进程序号|进程名称|范围>");
        assert!(out.is_empty());
    }

    #[test]
    fn test_list_with_extra_args_errors() {
        let (result, out) = run_capture(&["list", "extra"]);
        let error = result.unwrap_err();
        assert_eq!(error, "list不接受额外参数");
        assert!(out.is_empty());
    }

    #[test]
    fn test_daemon_with_extra_args_errors() {
        let (result, out) = run_capture(&["daemon", "extra"]);
        let error = result.unwrap_err();
        assert_eq!(error, "daemon不接受额外参数");
        assert!(out.is_empty());
    }

    #[test]
    fn test_service_usage_and_errors() {
        let (result, out) = run_capture(&["service"]);
        assert_eq!(result.unwrap_err(), "缺少服务操作");
        assert!(out.contains("用法: sv service <action>"));

        let (result, out) = run_capture(&["service", "bogus"]);
        assert_eq!(result.unwrap_err(), "未知服务操作: bogus");
        assert!(out.contains("用法: sv service <action>"));

        let (result, _) = run_capture(&["service", "install", "extra"]);
        assert_eq!(result.unwrap_err(), "服务操作不接受额外参数: extra");
    }

    #[test]
    fn test_configure_argument_validation() {
        let (result, _) = run_capture(&["configure"]);
        assert_eq!(
            result.unwrap_err(),
            "用法: sv configure rpc [--dry-run] [--restart]"
        );

        let (result, _) = run_capture(&["configure", "rpc", "--bogus"]);
        assert_eq!(result.unwrap_err(), "未知配置参数: --bogus");

        let (result, _) = run_capture(&["configure", "rpc", "--dry-run", "--restart"]);
        assert_eq!(result.unwrap_err(), "--dry-run不能与--restart同时使用");
    }
}
