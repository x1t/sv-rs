//! RPC struct → ProcessInfo 字段映射(镜像 Go `parseProcessList`/`parseProcessInfoFromMap`)。
//!
//! 把 `supervisor.getAllProcessInfo` 返回的 XML-RPC 值解析为进程列表,与传输层
//! (client.rs)解耦:传输只管拿值,这里只管把值翻译成进程模型,便于独立测试。

use std::collections::BTreeMap;

use crate::parse::{self, ProcessInfo};
use crate::xmlrpc::Value;

/// 把 getAllProcessInfo 返回的数组解析为进程列表。
pub(crate) fn parse_process_list(value: &Value) -> Result<Vec<ProcessInfo>, String> {
    let items = match value {
        Value::Array(items) => items,
        _ => return Err(format!("RPC返回的进程列表类型错误: {value:?}")),
    };
    let mut processes = Vec::with_capacity(items.len());
    for process_data in items {
        let members = match process_data {
            Value::Struct(members) => members,
            _ => return Err(format!("RPC进程信息类型错误: {process_data:?}")),
        };
        processes.push(parse_process_info_from_map(members, processes.len() + 1)?);
    }
    Ok(processes)
}

/// 从 RPC struct 字段映射为 [`ProcessInfo`](镜像 Go `parseProcessInfoFromMap`)。
fn parse_process_info_from_map(
    members: &BTreeMap<String, Value>,
    index: usize,
) -> Result<ProcessInfo, String> {
    let name = match member_string(members, "name") {
        Some(name) if !name.trim().is_empty() => name.to_string(),
        _ => return Err("RPC进程缺少有效名称".to_string()),
    };
    let group = member_string(members, "group")
        .unwrap_or_default()
        .to_string();
    let start = member_f64(members, "start").unwrap_or(0.0);
    let stop = member_f64(members, "stop").unwrap_or(0.0);
    let now = member_f64(members, "now").unwrap_or(0.0);
    let state = member_i64(members, "state").unwrap_or(0) as i32;
    let state_name = member_string(members, "statename")
        .unwrap_or_default()
        .to_string();
    let spawn_err = member_string(members, "spawnerr")
        .unwrap_or_default()
        .to_string();
    let pid = member_i64(members, "pid").unwrap_or(0) as i32;
    let logfile = member_string(members, "logfile")
        .unwrap_or_default()
        .to_string();
    let stdout_logfile = member_string(members, "stdout_logfile")
        .unwrap_or_default()
        .to_string();
    let stderr_logfile = member_string(members, "stderr_logfile")
        .unwrap_or_default()
        .to_string();
    let exit_status = member_i64(members, "exitstatus").unwrap_or(0) as i32;
    let description = member_string(members, "description")
        .unwrap_or_default()
        .to_string();

    let full_name = if !group.is_empty() && group != name && !name.is_empty() && !name.contains(':')
    {
        format!("{group}:{name}")
    } else {
        name.clone()
    };

    let uptime = if state == parse::state::STOPPED || pid == 0 {
        "已停止".to_string()
    } else if start > 0.0 && now >= start {
        parse::format_uptime((now - start) as i64)
    } else {
        String::new()
    };

    Ok(ProcessInfo {
        index,
        name: full_name,
        group,
        start,
        stop,
        now,
        state,
        state_name,
        spawn_err,
        pid,
        logfile,
        stdout_logfile,
        stderr_logfile,
        uptime,
        description,
        exit_status,
    })
}

fn member_string<'a>(members: &'a BTreeMap<String, Value>, key: &str) -> Option<&'a str> {
    members.get(key).and_then(Value::as_string)
}

fn member_i64(members: &BTreeMap<String, Value>, key: &str) -> Option<i64> {
    members.get(key).and_then(Value::as_i64)
}

fn member_f64(members: &BTreeMap<String, Value>, key: &str) -> Option<f64> {
    members.get(key).and_then(Value::as_f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xmlrpc::parse_method_response;

    #[test]
    fn test_parse_process_list_maps_fields() {
        let body = "<?xml version=\"1.0\"?><methodResponse><params><param><value><array><data><value><struct><member><name>name</name><value><string>api</string></value></member><member><name>group</name><value><string>web</string></value></member><member><name>state</name><value><int>20</int></value></member><member><name>statename</name><value><string>RUNNING</string></value></member><member><name>pid</name><value><int>1234</int></value></member><member><name>start</name><value><double>1000</double></value></member><member><name>now</name><value><double>18406</double></value></member></struct></value><value><struct><member><name>name</name><value><string>worker</string></value></member><member><name>group</name><value><string>web</string></value></member><member><name>state</name><value><int>0</int></value></member><member><name>statename</name><value><string>STOPPED</string></value></member><member><name>pid</name><value><int>0</int></value></member></struct></value></data></array></value></param></params></methodResponse>";
        let value = parse_method_response(body).unwrap();
        let processes = parse_process_list(&value).unwrap();
        assert_eq!(processes.len(), 2);
        assert_eq!(processes[0].index, 1);
        assert_eq!(processes[0].name, "web:api");
        assert_eq!(processes[0].state, 20);
        assert_eq!(processes[0].pid, 1234);
        assert_eq!(processes[0].uptime, "4小时50分06秒");
        assert_eq!(processes[1].name, "web:worker");
        assert_eq!(processes[1].uptime, "已停止");
    }

    #[test]
    fn test_parse_process_list_rejects_non_array() {
        let error = parse_process_list(&Value::Str("x".to_string())).unwrap_err();
        assert!(error.contains("RPC返回的进程列表类型错误"));
    }
}
