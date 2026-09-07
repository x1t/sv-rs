//! 进程信息数据模型与纯文本解析逻辑(逐条对齐 Go 版 `pkg/utils/common.go`)。

/// Supervisor 进程状态码,与 supervisor 的状态机一致。
pub mod state {
    pub const STOPPED: i32 = 0;
    pub const STARTING: i32 = 10;
    pub const RUNNING: i32 = 20;
    pub const STOPPING: i32 = 30;
    pub const FATAL: i32 = 100;
    pub const BACKOFF: i32 = 200;
}

/// 一个 Supervisor 进程的完整信息。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcessInfo {
    pub index: usize,
    pub name: String,
    pub group: String,
    pub start: f64,
    pub stop: f64,
    pub now: f64,
    pub state: i32,
    pub state_name: String,
    pub spawn_err: String,
    pub pid: i32,
    pub logfile: String,
    pub stdout_logfile: String,
    pub stderr_logfile: String,
    pub uptime: String,
    pub description: String,
    pub exit_status: i32,
}

/// 根据状态名称返回状态码;无法识别时返回 STOPPED。
pub fn state_value(state_name: &str) -> i32 {
    match state_name.trim().to_uppercase().as_str() {
        "RUNNING" => state::RUNNING,
        "STARTING" => state::STARTING,
        "STOPPING" => state::STOPPING,
        "STOPPED" => state::STOPPED,
        "FATAL" => state::FATAL,
        "BACKOFF" => state::BACKOFF,
        _ => state::STOPPED,
    }
}

/// 返回状态对应的 ANSI 前景色,颜色由调用方负责追加重置码。
pub fn color_by_state(state: i32) -> &'static str {
    match state {
        state::RUNNING => "\u{1b}[32m",
        state::STARTING | state::STOPPING | state::BACKOFF => "\u{1b}[33m",
        state::FATAL => "\u{1b}[31m",
        _ => "\u{1b}[37m",
    }
}

/// 返回状态对应的人类可读图标文案。
pub fn state_icon(state: i32) -> &'static str {
    match state {
        state::RUNNING => "✅ 运行中",
        state::STARTING => "🚀 启动中",
        state::STOPPING => "⏹️ 停止中",
        state::STOPPED => "⏸️ 已停止",
        state::FATAL => "❌ 致命错误",
        state::BACKOFF => "⚠️ 重试中",
        _ => "❓ 未知",
    }
}

/// 返回操作动作对应的图标文案。
pub fn action_icon(action: &str) -> &'static str {
    match action.trim().to_lowercase().as_str() {
        "start" => "🚀 启动",
        "stop" => "⏹️ 停止",
        "restart" => "🔄 重启",
        _ => "⚙️ 操作",
    }
}

/// 将 supervisorctl / RPC 返回的秒数格式化为中文可读时长。
pub fn format_uptime(seconds: i64) -> String {
    if seconds < 0 {
        return "无效时长".to_string();
    }
    if seconds == 0 {
        return "已停止".to_string();
    }
    let days = seconds / 86400;
    let mut rest = seconds % 86400;
    let hours = rest / 3600;
    rest %= 3600;
    let minutes = rest / 60;
    let seconds = rest % 60;
    format_duration_parts(days, hours, minutes, seconds)
}

/// 将 supervisorctl 返回的 "X days, H:M:S" / "H:M:S" / "M:S" 转成中文可读形式。
/// 无法识别的输入原样返回。
pub fn process_uptime_string(uptime: &str) -> String {
    let original = uptime.trim();
    if original.is_empty() {
        return String::new();
    }

    let mut day_count = 0i64;
    let mut time_part = original;
    if let Some((day_text, rest)) = original.split_once(',') {
        let day_words: Vec<&str> = day_text.split_whitespace().collect();
        if day_words.len() == 2 && matches!(day_words[1], "day" | "days") {
            match day_words[0].parse::<i64>() {
                Ok(parsed) if parsed >= 0 => {
                    day_count = parsed;
                    time_part = rest.trim();
                }
                _ => return original.to_string(),
            }
        }
    }

    let parts: Vec<&str> = time_part.split(':').collect();
    if parts.len() != 2 && parts.len() != 3 {
        return original.to_string();
    }
    let mut values = Vec::with_capacity(parts.len());
    for part in &parts {
        match part.trim().parse::<i64>() {
            Ok(value) if value >= 0 => values.push(value),
            _ => return original.to_string(),
        }
    }

    let (hours, minutes, seconds) = if values.len() == 3 {
        (values[0], values[1], values[2])
    } else {
        (0, values[0], values[1])
    };
    if minutes >= 60 || seconds >= 60 {
        return original.to_string();
    }
    format_duration_parts(day_count, hours, minutes, seconds)
}

fn format_duration_parts(days: i64, hours: i64, minutes: i64, seconds: i64) -> String {
    if days > 0 {
        format!("{days}天{hours}小时{minutes:02}分{seconds:02}秒")
    } else if hours > 0 {
        format!("{hours}小时{minutes:02}分{seconds:02}秒")
    } else if minutes > 0 {
        format!("{minutes}分{seconds:02}秒")
    } else {
        format!("{seconds}秒")
    }
}

/// 判断一个进程行是否有效(名称非空且状态可识别)。
#[cfg(test)]
pub fn is_valid_process_line(name: &str, rest: &str) -> bool {
    if name.trim().is_empty() {
        return false;
    }
    let fields: Vec<&str> = rest.split_whitespace().collect();
    if fields.is_empty() {
        return false;
    }
    state_value(fields[0]) != state::STOPPED || fields[0].eq_ignore_ascii_case("STOPPED")
}

/// 解析 `supervisorctl status` 的真实文本输出。
pub fn parse_supervisorctl_output(output: &str) -> Vec<ProcessInfo> {
    let mut result: Vec<ProcessInfo> = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }
        let value = state_value(fields[1]);
        if value == state::STOPPED && !fields[1].eq_ignore_ascii_case("STOPPED") {
            continue;
        }

        let mut process = ProcessInfo {
            index: result.len() + 1,
            name: fields[0].to_string(),
            state_name: fields[1].to_uppercase(),
            state: value,
            pid: 0,
            uptime: "已停止".to_string(),
            ..ProcessInfo::default()
        };

        let rest = &fields[2..];
        let mut index = 0;
        while index < rest.len() {
            match rest[index].to_uppercase().as_str() {
                "PID" => {
                    if index + 1 < rest.len() {
                        process.pid = rest[index + 1].trim_end_matches(',').parse().unwrap_or(0);
                    }
                }
                "UPTIME" if index + 1 < rest.len() => {
                    let joined = rest[index + 1..].join(" ");
                    let uptime = process_uptime_string(joined.trim().trim_end_matches(','));
                    process.uptime = uptime;
                }
                _ => {}
            }
            index += 1;
        }
        if process.state == state::STOPPED {
            process.uptime = "已停止".to_string();
        }
        process.description = state_icon(process.state).to_string();
        result.push(process);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_uptime_string() {
        let cases: Vec<(&str, &str)> = vec![
            ("30 days, 16:17:38", "30天16小时17分38秒"),
            ("1 day, 00:00:03", "1天0小时00分03秒"),
            ("1:59:48", "1小时59分48秒"),
            ("12:34", "12分34秒"),
            ("00:05", "5秒"),
            ("Not started", "Not started"),
        ];
        for (input, want) in cases {
            assert_eq!(process_uptime_string(input), want, "input: {input}");
        }
    }

    #[test]
    fn test_format_uptime() {
        assert_eq!(format_uptime(0), "已停止");
        assert_eq!(format_uptime(65), "1分05秒");
        assert_eq!(format_uptime(3661), "1小时01分01秒");
        assert_eq!(format_uptime(-1), "无效时长");
    }

    #[test]
    fn test_parse_supervisorctl_output() {
        let output = "app RUNNING pid 1234, uptime 0:01:05\n\
                      worker\tSTOPPED\tNot started\n\
                      broken UNKNOWN no useful state\n";
        let processes = parse_supervisorctl_output(output);
        assert_eq!(processes.len(), 2);
        assert_eq!(processes[0].index, 1);
        assert_eq!(processes[0].name, "app");
        assert_eq!(processes[0].state, state::RUNNING);
        assert_eq!(processes[0].pid, 1234);
        assert_eq!(processes[0].uptime, "1分05秒");
        assert_eq!(processes[1].index, 2);
        assert_eq!(processes[1].state, state::STOPPED);
        assert_eq!(processes[1].uptime, "已停止");
    }

    #[test]
    fn test_state_value_mapping() {
        assert_eq!(state_value("running"), state::RUNNING);
        assert_eq!(state_value("STOPPED"), state::STOPPED);
        assert_eq!(state_value("garbage"), state::STOPPED);
    }

    #[test]
    fn test_is_valid_process_line() {
        assert!(is_valid_process_line("app", "RUNNING pid 1"));
        assert!(is_valid_process_line("app", "STOPPED Not started"));
        assert!(!is_valid_process_line("", "RUNNING"));
        assert!(!is_valid_process_line("app", ""));
    }
}
