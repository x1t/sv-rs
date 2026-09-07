//! Supervisor XML-RPC 客户端与 supervisorctl 命令回退(镜像 Go `pkg/supervisor/rpc_client.go`)。
//!
//! RPC 优先,仅在「默认本地端点且无认证」时允许回退到 `supervisorctl`,
//! 避免远端失败意外影响本地 Supervisor。

use std::io::Read;
use std::time::Duration;

use base64::Engine as _;

use crate::client_map::parse_process_list;
use crate::parse::{self, ProcessInfo};
use crate::spec;
use crate::util::run_command;
use crate::xmlrpc::{Value, method_call_xml, parse_method_response};

/// 默认 Supervisor XML-RPC 端点。
pub const DEFAULT_SUPERVISOR_HOST: &str = "http://localhost:9001/RPC2";

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
/// 同步控制操作(start/stop/restart)超时:等待 supervisor 完成优雅停止与启动判定,
/// 慢服务常超 10s,故放宽到 120s,可用 SUPERVISOR_TIMEOUT(秒)覆盖。
const CONTROL_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_RPC_RESPONSE: u64 = 16 << 20;

/// 以秒覆盖同步控制操作超时的环境变量名。
pub const CONTROL_TIMEOUT_ENV: &str = "SUPERVISOR_TIMEOUT";

/// 一个 Supervisor 客户端:查询与控制各持一个 HTTP Agent(不同超时)与回退命令参数。
pub struct RpcClient {
    host: String,
    username: String,
    password: String,
    agent: ureq::Agent,    // 查询用,默认 HTTP_TIMEOUT
    op_agent: ureq::Agent, // 控制操作用,默认 CONTROL_TIMEOUT
    command_path: String,
    command_timeout: Duration,    // 查询命令回退超时
    op_command_timeout: Duration, // 控制命令回退超时
}

/// 构造带全局超时的 ureq Agent;对齐 Go http.Client:非 2xx 也返回响应,由调用方检查。
fn agent_with_timeout(timeout: Duration) -> ureq::Agent {
    ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .max_redirects(0)
            .http_status_as_error(false)
            .build(),
    )
}

/// 解析 SUPERVISOR_TIMEOUT(单位秒)作为同步控制操作的超时。
fn control_timeout() -> Duration {
    parse_control_timeout(std::env::var(CONTROL_TIMEOUT_ENV).ok().as_deref())
}

/// 纯解析:缺省、非法或小于 1 时回退到 CONTROL_TIMEOUT。
fn parse_control_timeout(raw: Option<&str>) -> Duration {
    let seconds = match raw.map(str::trim) {
        Some(text) if !text.is_empty() => text.parse::<u64>().unwrap_or(0),
        _ => 0,
    };
    if seconds < 1 {
        CONTROL_TIMEOUT
    } else {
        Duration::from_secs(seconds)
    }
}

impl RpcClient {
    /// 创建客户端;空 host 使用本地默认端点。
    pub fn new(host: &str, username: &str, password: &str) -> Self {
        let host = if host.trim().is_empty() {
            DEFAULT_SUPERVISOR_HOST.to_string()
        } else {
            host.to_string()
        };
        let control_timeout = control_timeout();
        RpcClient {
            host,
            username: username.to_string(),
            password: password.to_string(),
            agent: agent_with_timeout(HTTP_TIMEOUT),
            op_agent: agent_with_timeout(control_timeout),
            command_path: "supervisorctl".to_string(),
            command_timeout: COMMAND_TIMEOUT,
            op_command_timeout: control_timeout,
        }
    }

    /// 是否为默认本地端点且无认证(可安全回退命令)。
    fn can_use_command_fallback(&self) -> bool {
        self.host.trim_end_matches('/') == DEFAULT_SUPERVISOR_HOST.trim_end_matches('/')
            && self.username.is_empty()
            && self.password.is_empty()
    }

    /// 只读查询 RPC,走默认(10s)查询连接。
    fn call(&self, method: &str, params: &[Value]) -> Result<Value, String> {
        self.call_on(&self.agent, method, params)
    }

    /// 同步控制 RPC,走长超时控制连接。
    fn control_call(&self, method: &str, params: &[Value]) -> Result<Value, String> {
        self.call_on(&self.op_agent, method, params)
    }

    /// 调用一个 XML-RPC 方法,返回响应值或错误文案。
    fn call_on(
        &self,
        agent: &ureq::Agent,
        method: &str,
        params: &[Value],
    ) -> Result<Value, String> {
        if method.trim().is_empty() {
            return Err("XML-RPC方法名不能为空".to_string());
        }
        validate_endpoint(&self.host)?;
        let body = method_call_xml(method, params);

        let mut builder = agent
            .post(&self.host)
            .header("Accept", "text/xml")
            .header("Content-Type", "text/xml; charset=utf-8")
            .header("User-Agent", "sv-supervisor-client/1.0");
        if !self.username.is_empty() {
            let token = format!("{}:{}", self.username, self.password);
            let encoded = base64::engine::general_purpose::STANDARD.encode(token.as_bytes());
            builder = builder.header("Authorization", &format!("Basic {encoded}"));
        }

        let response = builder
            .send(body.as_bytes())
            .map_err(|error| format!("请求Supervisor失败: {error}"))?;
        let status = response.status().as_u16();
        let body = read_body_limited(response.into_body().into_reader())
            .map_err(|error| format!("读取Supervisor响应失败: {error}"))?;
        if status != 200 {
            return Err(format!(
                "Supervisor返回HTTP {status}: {}",
                String::from_utf8_lossy(&body).trim()
            ));
        }
        parse_method_response(&String::from_utf8_lossy(&body))
    }

    /// 取得全部进程信息;RPC 失败时仅在默认本地端点回退 supervisorctl。
    pub fn get_all_processes(&self) -> Result<Vec<ProcessInfo>, String> {
        match self.call("supervisor.getAllProcessInfo", &[]) {
            Ok(value) => parse_process_list(&value),
            Err(error) => {
                if !self.can_use_command_fallback() {
                    return Err(format!("RPC获取进程失败: {error}"));
                }
                match self.get_all_processes_via_command() {
                    Ok(processes) => Ok(processes),
                    Err(fallback_error) => Err(format!(
                        "RPC获取进程失败: {error}；supervisorctl回退失败: {fallback_error}"
                    )),
                }
            }
        }
    }

    /// 对单个进程执行 start / stop / restart(restart 为 stop+start 两阶段)。
    pub fn control_process(&self, action: &str, process_name: &str) -> Result<(), String> {
        let action = action.trim().to_lowercase();
        spec::validate_action(&action)?;
        spec::validate_process_name(process_name)?;

        if action == "restart" {
            // 目标本来就未在运行(已停止/FATAL)时 stop 会报 NOT_RUNNING:跳过停止
            // 阶段直接启动,与 supervisorctl restart 语义保持一致。
            if let Err(error) = self.control_process_rpc("supervisor.stopProcess", process_name)
                && !is_not_running_error(&error)
            {
                if self.can_use_command_fallback() {
                    return self.control_process_via_command(&action, process_name);
                }
                return Err(format!("重启进程失败（停止阶段）: {error}"));
            }
            if let Err(error) = self.control_process_rpc("supervisor.startProcess", process_name) {
                return Err(format!("重启进程失败（启动阶段）: {error}"));
            }
            return Ok(());
        }

        let method = format!("supervisor.{action}Process");
        match self.control_process_rpc(&method, process_name) {
            Ok(()) => Ok(()),
            Err(_) if self.can_use_command_fallback() => {
                self.control_process_via_command(&action, process_name)
            }
            Err(error) => Err(format!("{action}进程失败: {error}")),
        }
    }

    /// 执行一次 RPC 进程操作;Supervisor 返回 false 时视为拒绝。
    fn control_process_rpc(&self, method: &str, process_name: &str) -> Result<(), String> {
        let result = self.control_call(
            method,
            &[Value::Str(process_name.to_string()), Value::Bool(true)],
        )?;
        if let Value::Bool(success) = result
            && !success
        {
            return Err("Supervisor拒绝了操作".to_string());
        }
        Ok(())
    }

    /// 通过 supervisorctl status 回退获取进程列表。
    fn get_all_processes_via_command(&self) -> Result<Vec<ProcessInfo>, String> {
        let (output, error) = self.run_supervisorctl(&["status"]);
        let text = String::from_utf8_lossy(&output);
        let processes = parse::parse_supervisorctl_output(&text);
        if let Some(reason) = error
            && processes.is_empty()
        {
            return Err(format!(
                "supervisorctl status失败: {reason}; 输出: {}",
                text.trim()
            ));
        }
        if processes.is_empty() && text.trim().is_empty() {
            return Err("supervisorctl未返回进程信息".to_string());
        }
        Ok(processes)
    }

    /// 通过 supervisorctl 执行单个进程操作(控制命令,长超时)。
    fn control_process_via_command(&self, action: &str, process_name: &str) -> Result<(), String> {
        let (output, error) = self.run_supervisorctl_control(&[action, process_name]);
        let text = String::from_utf8_lossy(&output);
        if let Some(reason) = error {
            return Err(format!("{action}进程失败: {reason}, 输出: {}", text.trim()));
        }
        if text.to_uppercase().contains("ERROR") {
            return Err(format!("{action}进程失败: {}", text.trim()));
        }
        Ok(())
    }

    /// 只读查询命令(status),用查询命令超时。
    fn run_supervisorctl(&self, args: &[&str]) -> (Vec<u8>, Option<String>) {
        self.run_supervisorctl_with(args, self.command_timeout, COMMAND_TIMEOUT)
    }

    /// 控制命令(start/stop/restart),用控制命令超时。
    fn run_supervisorctl_control(&self, args: &[&str]) -> (Vec<u8>, Option<String>) {
        self.run_supervisorctl_with(args, self.op_command_timeout, CONTROL_TIMEOUT)
    }

    /// 运行 supervisorctl 子命令,返回(合并输出, 失败原因);timeout 为 0 时回退。
    fn run_supervisorctl_with(
        &self,
        args: &[&str],
        timeout: Duration,
        fallback: Duration,
    ) -> (Vec<u8>, Option<String>) {
        let timeout = if timeout.is_zero() { fallback } else { timeout };
        let command_args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let outcome = match run_command(&self.command_path, &command_args, timeout) {
            Ok(outcome) => outcome,
            Err(error) => return (Vec::new(), Some(error.to_string())),
        };
        let error = match outcome.code {
            Some(0) => None,
            Some(code) => Some(format!("exit status {code}")),
            None => Some("signal: killed".to_string()),
        };
        (outcome.combined(), error)
    }
}

/// 读取响应体,最多多读 1 字节以探测超限。
fn read_body_limited(mut reader: impl Read) -> Result<Vec<u8>, std::io::Error> {
    let mut body = Vec::new();
    reader
        .by_ref()
        .take(MAX_RPC_RESPONSE + 1)
        .read_to_end(&mut body)?;
    if body.len() as u64 > MAX_RPC_RESPONSE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Supervisor响应超过{}字节限制", MAX_RPC_RESPONSE),
        ));
    }
    Ok(body)
}

/// 校验端点:http/https、含主机、URL 中不内嵌认证信息。
fn validate_endpoint(raw_url: &str) -> Result<(), String> {
    let text = raw_url.trim();
    let (scheme, rest) = match text.split_once("://") {
        Some(pair) => pair,
        None => return Err("Supervisor地址必须使用http或https".to_string()),
    };
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return Err("Supervisor地址必须使用http或https".to_string());
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err("Supervisor地址必须包含主机且不能在URL中嵌入认证信息".to_string());
    }
    Ok(())
}

/// 错误是否源于 supervisor 的 NOT_RUNNING(目标进程本来就未在运行)。
fn is_not_running_error(error: &str) -> bool {
    error.to_uppercase().contains("NOT_RUNNING")
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
