//! 端到端 golden 测试:把编译出的 `sv-rs` 当作真实进程运行,喂以本地真实 HTTP
//! XML-RPC 服务端,再把 stdout / stderr 与 `tests/golden/` 下由 Go 版 `sv` 捕获的
//! 输出做逐字节比对。服务端不是 mock:它是一个在真实端口上监听、说 XML-RPC 协议
//! 的最小 HTTP 服务器(与生成 golden 所用的 `golden_stub.py` 同思路)。

use std::io::{BufRead as _, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Output;
use std::thread;

use assert_cmd::Command;

/// 返回 `supervisor.getAllProcessInfo` 两个进程的响应。
const TWO_PROCS: &str = "<?xml version=\"1.0\"?><methodResponse><params><param><value><array><data>
<value><struct>
<member><name>name</name><value><string>api</string></value></member>
<member><name>group</name><value><string>web</string></value></member>
<member><name>start</name><value><double>1000</double></value></member>
<member><name>stop</name><value><double>0</double></value></member>
<member><name>now</name><value><double>18406</double></value></member>
<member><name>state</name><value><int>20</int></value></member>
<member><name>statename</name><value><string>RUNNING</string></value></member>
<member><name>spawnerr</name><value><string></string></value></member>
<member><name>exitstatus</name><value><int>0</int></value></member>
<member><name>logfile</name><value><string>/var/log/web.log</string></value></member>
<member><name>stdout_logfile</name><value><string>/var/log/web.out</string></value></member>
<member><name>stderr_logfile</name><value><string>/var/log/web.err</string></value></member>
<member><name>pid</name><value><int>1234</int></value></member>
<member><name>description</name><value><string>API process</string></value></member>
</struct></value>
<value><struct>
<member><name>name</name><value><string>worker</string></value></member>
<member><name>group</name><value><string>web</string></value></member>
<member><name>start</name><value><double>0</double></value></member>
<member><name>stop</name><value><double>0</double></value></member>
<member><name>now</name><value><double>18406</double></value></member>
<member><name>state</name><value><int>0</int></value></member>
<member><name>statename</name><value><string>STOPPED</string></value></member>
<member><name>spawnerr</name><value><string></string></value></member>
<member><name>exitstatus</name><value><int>0</int></value></member>
<member><name>logfile</name><value><string></string></value></member>
<member><name>stdout_logfile</name><value><string>/var/log/web.out</string></value></member>
<member><name>stderr_logfile</name><value><string>/var/log/web.err</string></value></member>
<member><name>pid</name><value><int>0</int></value></member>
<member><name>description</name><value><string></string></value></member>
</struct></value>
</data></array></value></param></params></methodResponse>";

/// 返回空进程列表的响应。
const EMPTY_PROCS: &str = "<?xml version=\"1.0\"?><methodResponse><params><param><value><array><data></data></array></value></param></params></methodResponse>";

const BOOL_TRUE: &str = "<?xml version=\"1.0\"?><methodResponse><params><param><value><boolean>1</boolean></value></param></params></methodResponse>";
const BOOL_FALSE: &str = "<?xml version=\"1.0\"?><methodResponse><params><param><value><boolean>0</boolean></value></param></params></methodResponse>";

#[derive(Clone, Copy)]
enum StubMode {
    Two,
    Empty,
}

/// 从请求体里提取 `<methodName>`(失败时返回空串)。
fn method_of(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let start = text.find("<methodName>").map(|i| i + "<methodName>".len());
    let Some(start) = start else {
        return String::new();
    };
    let end = text[start..].find("</methodName>").map(|i| start + i);
    match end {
        Some(end) => text[start..end].to_string(),
        None => String::new(),
    }
}

fn pick_payload(mode: StubMode, method: &str, body: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(body);
    if method == "supervisor.getAllProcessInfo" {
        return match mode {
            StubMode::Two => TWO_PROCS,
            StubMode::Empty => EMPTY_PROCS,
        };
    }
    if method == "supervisor.startProcess" && text.contains("web:worker") {
        return BOOL_FALSE;
    }
    BOOL_TRUE
}

fn handle_connection(stream: &mut TcpStream, mode: StubMode) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        let lowered = line.to_ascii_lowercase();
        if let Some(rest) = lowered.strip_prefix("content-length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    let mut filled = 0usize;
    while filled < content_length {
        let read = reader.read(&mut body[filled..]).unwrap_or(0);
        if read == 0 {
            break;
        }
        filled += read;
    }
    body.truncate(filled);

    let method = method_of(&body);
    let payload = pick_payload(mode, &method, &body);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// 在真实端口上监听一定请求次数的 XML-RPC 服务器。
struct Stub {
    url: String,
    join: Option<thread::JoinHandle<()>>,
}

impl Stub {
    fn start(mode: StubMode, expected_requests: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定测试端口");
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}/RPC2");
        let join = thread::spawn(move || {
            for _ in 0..expected_requests {
                match listener.accept() {
                    Ok((mut stream, _)) => handle_connection(&mut stream, mode),
                    Err(_) => break,
                }
            }
        });
        Stub {
            url,
            join: Some(join),
        }
    }

    fn join(mut self) {
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// 读取 golden 文件字节。
fn golden(name: &str) -> Vec<u8> {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "golden", name]
        .iter()
        .collect();
    std::fs::read(&path).unwrap_or_else(|e| panic!("读取 golden {name}: {e}"))
}

fn run_bin(args: &[&str]) -> Output {
    Command::cargo_bin("sv-rs")
        .expect("定位 sv-rs 二进制")
        .args(args)
        .output()
        .expect("运行 sv-rs")
}

fn run_bin_with_host(args: &[&str], host: &str) -> Output {
    Command::cargo_bin("sv-rs")
        .expect("定位 sv-rs 二进制")
        .env("SUPERVISOR_HOST", host)
        .args(args)
        .output()
        .expect("运行 sv-rs")
}

#[test]
fn e2e_help_matches_golden() {
    for args in [&[][..], &["--help"][..], &["-h"][..]] {
        let output = run_bin(args);
        assert_eq!(output.status.code(), Some(0));
        assert_eq!(output.stdout, golden("help.txt"), "args={args:?}");
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn e2e_unknown_command_matches_golden() {
    let output = run_bin(&["nosuchcmd"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, golden("err_unknown.out.txt"));
    assert_eq!(output.stderr, golden("err_unknown.err.txt"));
}

#[test]
fn e2e_restart_without_target_matches_golden() {
    let output = run_bin(&["restart"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, golden("err_restart_noargs.err.txt"));
}

#[test]
fn e2e_list_with_extra_args_matches_golden() {
    let output = run_bin(&["list", "extra"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(output.stderr, golden("err_list_extra.err.txt"));
}

#[test]
fn e2e_service_command_argument_errors() {
    let output = run_bin(&["service", "install", "extra"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8_lossy(&output.stderr);
    assert_eq!(error.trim_end(), "❌ 服务操作不接受额外参数: extra");
}

#[test]
fn e2e_status_two_matches_golden() {
    let stub = Stub::start(StubMode::Two, 1);
    let output = run_bin_with_host(&["status"], &stub.url);
    stub.join();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, golden("status_two.txt"));
    assert!(output.stderr.is_empty());
}

#[test]
fn e2e_status_empty_matches_golden() {
    let stub = Stub::start(StubMode::Empty, 1);
    let output = run_bin_with_host(&["status"], &stub.url);
    stub.join();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, golden("status_empty.txt"));
    assert!(output.stderr.is_empty());
}

#[test]
fn e2e_restart_process_matches_golden() {
    let stub = Stub::start(StubMode::Two, 3);
    let output = run_bin_with_host(&["restart", "1"], &stub.url);
    stub.join();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, golden("control_restart_ok.txt"));
    assert!(output.stderr.is_empty());
}

#[test]
fn e2e_start_failure_matches_golden() {
    let stub = Stub::start(StubMode::Two, 2);
    let output = run_bin_with_host(&["start", "web:worker"], &stub.url);
    stub.join();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, golden("control_start_fail.out.txt"));
    assert_eq!(output.stderr, golden("control_start_fail.err.txt"));
}

#[test]
fn e2e_configure_dry_run_missing_section_matches_golden_form() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("supervisord.conf");
    std::fs::write(
        &config_path,
        "[supervisord]\nlogfile=/var/log/supervisord.log\n",
    )
    .unwrap();
    let path_text = config_path.to_string_lossy();

    let output = Command::cargo_bin("sv-rs")
        .unwrap()
        .env("SUPERVISOR_CONFIG", config_path.as_os_str())
        .args(["configure", "rpc", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let expected =
        format!("将更新 {path_text}，新增配置段: inet_http_server, rpcinterface:supervisor\n");
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
    assert!(output.stderr.is_empty());
}

#[test]
fn e2e_configure_dry_run_existing_section_matches_golden_form() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("supervisord.conf");
    std::fs::write(
        &config_path,
        "[inet_http_server]\nport=127.0.0.1:9001\n\n\
         [rpcinterface:supervisor]\n\
         supervisor.rpcinterface_factory = supervisor.rpcinterface:make_main_rpcinterface\n",
    )
    .unwrap();
    let path_text = config_path.to_string_lossy();

    let output = Command::cargo_bin("sv-rs")
        .unwrap()
        .env("SUPERVISOR_CONFIG", config_path.as_os_str())
        .args(["configure", "rpc", "--dry-run"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let expected = format!("RPC配置已存在: {path_text}\n");
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
    assert!(output.stderr.is_empty());
}

#[test]
fn e2e_configure_rejects_non_file_config_path() {
    // 指向目录是确定性场景(缺文件可能回退到 /etc 默认配置,语义同 Go)。
    let dir = tempfile::tempdir().unwrap();
    let output = Command::cargo_bin("sv-rs")
        .unwrap()
        .env("SUPERVISOR_CONFIG", dir.path())
        .args(["configure", "rpc"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("配置路径不是普通文件"), "{error}");
}
