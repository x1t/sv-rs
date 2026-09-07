use super::*;
use std::io::{BufRead as _, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;

/// 用真实 TCP 服务器回放一段 HTTP 响应,返回其地址。
fn serve_once(status: u16, body: &str) -> String {
    serve_delayed(status, body, 0)
}

/// 同 serve_once,但延迟 delay_ms 毫秒后才回包(用于区分长短超时连接)。
fn serve_delayed(status: u16, body: &str, delay_ms: u64) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定测试端口");
    let address = listener.local_addr().unwrap();
    let body = body.to_string();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("接收请求");
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut head = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            if line == "\r\n" {
                break;
            }
            head.push_str(&line);
        }
        let content_length = head
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap_or(0))
            })
            .unwrap_or(0);
        let mut request_body = vec![0u8; content_length];
        if content_length > 0 {
            reader.read_exact(&mut request_body).unwrap();
        }
        if delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
        let response = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: text/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut writer = stream;
        writer.write_all(response.as_bytes()).unwrap();
    });
    format!("http://{address}/RPC2")
}

fn boolean_body(success: bool) -> String {
    let value = if success { 1 } else { 0 };
    format!(
        "<?xml version=\"1.0\"?><methodResponse><params><param><value><boolean>{value}</boolean></value></param></params></methodResponse>"
    )
}

fn fault_body(message: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?><methodResponse><fault><value><struct><member><name>faultString</name><value><string>{message}</string></value></member></struct></value></fault></methodResponse>"
    )
}

/// 依请求顺序为每次连接回包(每次 accept 一个请求),用于多步控制类 RPC 测试。
fn serve_in_order(bodies: &[String]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定测试端口");
    let address = listener.local_addr().unwrap();
    let bodies = bodies.to_vec();
    std::thread::spawn(move || {
        for body in bodies {
            let (stream, _) = listener.accept().expect("接收请求");
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let mut writer = stream;
            writer.write_all(response.as_bytes()).unwrap();
        }
    });
    format!("http://{address}/RPC2")
}

#[test]
fn test_validate_endpoint() {
    assert!(validate_endpoint("http://localhost:9001/RPC2").is_ok());
    assert!(validate_endpoint("https://sv.example.com/RPC2").is_ok());
    let error = validate_endpoint("ftp://localhost/RPC2").unwrap_err();
    assert_eq!(error, "Supervisor地址必须使用http或https");
    let error = validate_endpoint("localhost:9001/RPC2").unwrap_err();
    assert_eq!(error, "Supervisor地址必须使用http或https");
    let error = validate_endpoint("http://").unwrap_err();
    assert!(error.contains("必须包含主机"));
    let error = validate_endpoint("http://user:pass@localhost:9001/RPC2").unwrap_err();
    assert!(error.contains("认证信息"));
}

#[test]
fn test_call_returns_boolean() {
    let host = serve_once(200, &boolean_body(true));
    let client = RpcClient::new(&host, "", "");
    let value = client.call("supervisor.startProcess", &[]).unwrap();
    assert_eq!(value, Value::Bool(true));
}

#[test]
fn test_call_reports_http_status() {
    let host = serve_once(500, "boom");
    let client = RpcClient::new(&host, "", "");
    let error = client.call("supervisor.startProcess", &[]).unwrap_err();
    assert!(error.contains("Supervisor返回HTTP 500: boom"), "{error}");
}

#[test]
fn test_call_reports_fault() {
    let body = "<?xml version=\"1.0\"?><methodResponse><fault><value><struct><member><name>faultString</name><value><string>BAD_NAME</string></value></member></struct></value></fault></methodResponse>";
    let host = serve_once(200, body);
    let client = RpcClient::new(&host, "", "");
    let error = client.call("supervisor.startProcess", &[]).unwrap_err();
    assert_eq!(error, "XML-RPC错误: BAD_NAME");
}

#[test]
fn test_control_process_rejected() {
    let host = serve_once(200, &boolean_body(false));
    let client = RpcClient::new(&host, "", "");
    let error = client.control_process("stop", "web:api").unwrap_err();
    assert!(error.contains("Supervisor拒绝了操作"), "{error}");
}

fn fake_script(dir: &tempfile::TempDir, stdout: &str, exit_code: i32) -> String {
    let script = dir.path().join("fake-supervisorctl");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nprintf '%s' '{stdout}'\nexit {exit_code}\n"),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script.to_string_lossy().into_owned()
}

/// 白盒直测 status 命令回退解析(真实子进程输出,不依赖本机 9001 是否在跑)。
#[test]
fn test_supervisorctl_status_parses_script_output() {
    let dir = tempfile::tempdir().unwrap();
    let script = fake_script(
        &dir,
        "app RUNNING pid 42, uptime 0:01:05\nidle STOPPED Not started\n",
        0,
    );
    let mut client = RpcClient::new(DEFAULT_SUPERVISOR_HOST, "", "");
    client.command_path = script;
    let processes = client.get_all_processes_via_command().unwrap();
    assert_eq!(processes.len(), 2);
    assert_eq!(processes[0].name, "app");
    assert_eq!(processes[1].name, "idle");
    assert_eq!(processes[0].uptime, "1分05秒");
}

#[test]
fn test_supervisorctl_control_error_keyword() {
    let dir = tempfile::tempdir().unwrap();
    let script = fake_script(&dir, "ERROR: no such process: web:nope", 0);
    let mut client = RpcClient::new(DEFAULT_SUPERVISOR_HOST, "", "");
    client.command_path = script;
    let error = client
        .control_process_via_command("start", "web:nope")
        .unwrap_err();
    assert!(
        error.contains("start进程失败: ERROR: no such process"),
        "{error}"
    );
}

#[test]
fn test_supervisorctl_control_success() {
    let dir = tempfile::tempdir().unwrap();
    let script = fake_script(&dir, "web:api: started", 0);
    let mut client = RpcClient::new(DEFAULT_SUPERVISOR_HOST, "", "");
    client.command_path = script;
    assert!(
        client
            .control_process_via_command("start", "web:api")
            .is_ok()
    );
}

/// 回退仅对默认本地端点且无认证开启(与 RPC 是否可达解耦,纯字符串判定)。
#[test]
fn test_command_fallback_gate() {
    let local = RpcClient::new(DEFAULT_SUPERVISOR_HOST, "", "");
    assert!(local.can_use_command_fallback());

    let remote = RpcClient::new("http://127.0.0.1:9/RPC2", "", "");
    assert!(!remote.can_use_command_fallback());

    let authed = RpcClient::new(DEFAULT_SUPERVISOR_HOST, "user", "pass");
    assert!(!authed.can_use_command_fallback());
}

#[test]
fn test_remote_failure_never_falls_back() {
    let host = serve_once(200, &boolean_body(false));
    let client = RpcClient::new(&host, "", "");
    let error = client.control_process("start", "web:api").unwrap_err();
    assert!(error.contains("start进程失败"), "{error}");
}

#[test]
fn test_read_body_limit_rejects_oversize() {
    struct Fake {
        total: u64,
        sent: u64,
    }
    impl Read for Fake {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.sent >= self.total {
                return Ok(0);
            }
            let count = buffer.len().min((self.total - self.sent) as usize);
            self.sent += count as u64;
            Ok(count)
        }
    }
    let error = read_body_limited(Fake {
        total: MAX_RPC_RESPONSE + 5,
        sent: 0,
    })
    .unwrap_err();
    assert!(error.to_string().contains("字节限制"));
}

#[test]
fn test_parse_control_timeout() {
    assert_eq!(parse_control_timeout(None), CONTROL_TIMEOUT);
    assert_eq!(parse_control_timeout(Some("")), CONTROL_TIMEOUT);
    assert_eq!(parse_control_timeout(Some("abc")), CONTROL_TIMEOUT);
    assert_eq!(parse_control_timeout(Some("0")), CONTROL_TIMEOUT);
    assert_eq!(parse_control_timeout(Some("-3")), CONTROL_TIMEOUT);
    assert_eq!(parse_control_timeout(Some("45")), Duration::from_secs(45));
    assert_eq!(parse_control_timeout(Some(" 45 ")), Duration::from_secs(45));
}

/// 控制操作必须走 op_agent(长超时):查询 agent 仅 40ms,若误走会立刻超时。
#[test]
fn test_control_process_uses_op_agent() {
    let host = serve_delayed(200, &boolean_body(true), 250);
    let mut client = RpcClient::new(&host, "", "");
    client.op_agent = agent_with_timeout(Duration::from_secs(3));
    client.agent = agent_with_timeout(Duration::from_millis(40));
    client.control_process("start", "web:api").unwrap();
}

/// 反向:op_agent 过小(40ms)时控制操作应超时失败,而非落到查询 agent(3s)成功。
#[test]
fn test_control_process_does_not_use_query_agent() {
    let host = serve_delayed(200, &boolean_body(true), 250);
    let mut client = RpcClient::new(&host, "", "");
    client.op_agent = agent_with_timeout(Duration::from_millis(40));
    client.agent = agent_with_timeout(Duration::from_secs(3));
    let error = client.control_process("start", "web:api").unwrap_err();
    assert!(error.contains("请求Supervisor失败"), "{error}");
}

/// restart 对已停止/未运行进程:stop 报 NOT_RUNNING 应跳过并继续启动。
#[test]
fn test_restart_skips_not_running_stop() {
    let host = serve_in_order(&[fault_body("NOT_RUNNING: web:api"), boolean_body(true)]);
    let client = RpcClient::new(&host, "", "");
    client.control_process("restart", "web:api").unwrap();
}

/// restart 停止阶段遇到非 NOT_RUNNING 的硬错误应失败。
#[test]
fn test_restart_hard_stop_error_fails() {
    let host = serve_in_order(&[fault_body("ALREADY_STARTED: web:api")]);
    let client = RpcClient::new(&host, "", "");
    let error = client.control_process("restart", "web:api").unwrap_err();
    assert!(error.contains("重启进程失败（停止阶段）"), "{error}");
}
