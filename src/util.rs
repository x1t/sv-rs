//! 通用工具:io 错误文案、可执行文件探测、带超时的子进程执行器。
//!
//! 对应 Go 里 `os/exec.CommandContext` 的超时与输出合并语义。

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 把 io 错误包装为带上下文的文案,替代 Go 的 `fmt.Errorf("%s: %w", ctx, err)`。
pub fn io_context(context: &str, error: std::io::Error) -> String {
    format!("{context}: {error}")
}

/// 判断路径是否存在。
pub fn path_exists(path: &str) -> bool {
    std::fs::metadata(path).is_ok()
}

/// 在 PATH 中查找可执行文件(镜像 Go `exec.LookPath`)。
pub fn command_available(name: &str) -> bool {
    if name.contains('/') {
        return path_exists(name);
    }
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if let Ok(meta) = std::fs::metadata(&candidate)
            && meta.is_file()
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o111 != 0 {
                    return true;
                }
            }
            #[cfg(not(unix))]
            {
                return true;
            }
        }
    }
    false
}

/// 子进程执行结果。
#[derive(Debug)]
pub struct RunOutcome {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub code: Option<i32>,
}

impl RunOutcome {
    /// 合并输出,近似 Go `CombinedOutput` 的语义(stdout 在前)。
    pub fn combined(&self) -> Vec<u8> {
        let mut buffer = self.stdout.clone();
        buffer.extend_from_slice(&self.stderr);
        buffer
    }

    /// 合并输出并转为 UTF-8 文本(非法字节以替换符替代)。
    pub fn combined_text(&self) -> String {
        String::from_utf8_lossy(&self.combined()).into_owned()
    }

    #[cfg(test)]
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// 子进程执行失败原因。
#[derive(Debug)]
pub enum RunError {
    Io(std::io::Error),
    Timeout(Duration),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Io(error) => write!(formatter, "{error}"),
            RunError::Timeout(limit) => write!(formatter, "命令执行超时({limit:?})"),
        }
    }
}

/// 向子进程所在进程组发送 SIGKILL(子进程以组首身份启动,其 PID 即 PGID)。
#[cfg(unix)]
fn kill_process_group(child_id: u32) {
    unsafe {
        libc::kill(-(child_id as libc::pid_t), libc::SIGKILL);
    }
}

/// 执行命令并等待其结束,超过 `timeout` 则杀掉整个进程组并返回错误。
/// 输出被完整缓存,避免管道阻塞;子进程随父进程组一同回收。
pub fn run_command(path: &str, args: &[String], timeout: Duration) -> Result<RunOutcome, RunError> {
    let mut command = Command::new(path);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(RunError::Io)?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // 用独立线程把管道读尽,避免写满后子进程阻塞;句柄保留以便结束时 join。
    let out_buffer = Arc::new(Mutex::new(Vec::new()));
    let err_buffer = Arc::new(Mutex::new(Vec::new()));
    let stdout_thread = stdout.map(|mut pipe| {
        let target = Arc::clone(&out_buffer);
        std::thread::spawn(move || {
            let _ = pipe.read_to_end(&mut target.lock().unwrap());
        })
    });
    let stderr_thread = stderr.map(|mut pipe| {
        let target = Arc::clone(&err_buffer);
        std::thread::spawn(move || {
            let _ = pipe.read_to_end(&mut target.lock().unwrap());
        })
    });

    // 等待子进程结束:先回收进程,再 join 读取线程确保输出完整。
    fn take_output(
        stdout_thread: Option<std::thread::JoinHandle<()>>,
        stderr_thread: Option<std::thread::JoinHandle<()>>,
        out_buffer: &Arc<Mutex<Vec<u8>>>,
        err_buffer: &Arc<Mutex<Vec<u8>>>,
    ) -> RunOutcome {
        if let Some(handle) = stdout_thread {
            let _ = handle.join();
        }
        if let Some(handle) = stderr_thread {
            let _ = handle.join();
        }
        RunOutcome {
            stdout: out_buffer.lock().unwrap().clone(),
            stderr: err_buffer.lock().unwrap().clone(),
            code: None,
        }
    }

    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(RunError::Io)? {
            let mut outcome = take_output(stdout_thread, stderr_thread, &out_buffer, &err_buffer);
            outcome.code = status.code();
            return Ok(outcome);
        }
        if started.elapsed() >= timeout {
            #[cfg(unix)]
            kill_process_group(child.id());
            #[cfg(not(unix))]
            let _ = child.kill();
            let _ = child.wait();
            let _ = take_output(stdout_thread, stderr_thread, &out_buffer, &err_buffer);
            return Err(RunError::Timeout(timeout));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_available() {
        assert!(command_available("sh"));
        assert!(!command_available("sv-definitely-not-a-real-cmd-xyz"));
        assert!(command_available("/bin/sh"));
    }

    #[test]
    fn test_run_command_success() {
        let outcome = run_command(
            "sh",
            &[
                "-c".to_string(),
                "printf hello && printf err >&2".to_string(),
            ],
            Duration::from_secs(5),
        )
        .expect("命令应成功执行");
        assert_eq!(outcome.code, Some(0));
        assert_eq!(outcome.stdout_text(), "hello");
        assert_eq!(String::from_utf8_lossy(&outcome.stderr), "err");
        assert_eq!(outcome.combined_text(), "helloerr");
    }

    #[test]
    fn test_run_command_timeout() {
        let started = Instant::now();
        let result = run_command(
            "sh",
            &["-c".to_string(), "sleep 30".to_string()],
            Duration::from_millis(300),
        );
        assert!(
            matches!(result, Err(RunError::Timeout(_))),
            "应因超时失败: {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "不应等待完整 sleep"
        );
    }

    #[test]
    fn test_run_command_not_found() {
        assert!(run_command("/no/such/binary", &[], Duration::from_secs(1)).is_err());
    }

    #[test]
    fn test_io_context_message() {
        let error = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        assert!(io_context("读取文件失败", error).contains("读取文件失败"));
    }
}
