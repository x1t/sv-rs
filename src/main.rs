//! sv-rs:Supervisor 进程管理工具的 Rust 实现(迁移自 Go 版 `github.com/x1t/sv`)。

mod cli;
mod client;
mod client_map;
mod config;
mod parse;
mod render;
mod service;
mod service_files;
mod spec;
mod util;
mod xmlrpc;

fn main() {
    use std::io::{IsTerminal, Write};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let color = std::io::stdout().is_terminal()
        && std::env::var("NO_COLOR")
            .map(|value| value.is_empty())
            .unwrap_or(true);
    let mut stdout = std::io::stdout();
    let result = cli::run(&args, &mut stdout, color);
    let _ = stdout.flush();

    if let Err(error) = result {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "❌ {error}");
        let _ = stderr.flush();
        std::process::exit(1);
    }
}
