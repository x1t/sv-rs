//! 进程操作参数解析与校验(对齐 Go `pkg/utils/common.go` 与 `pkg/supervisor/process_control.go`)。

use crate::parse::ProcessInfo;

/// 校验动作是否为 start / stop / restart。
pub fn validate_action(action: &str) -> Result<(), String> {
    match action.trim().to_lowercase().as_str() {
        "start" | "stop" | "restart" => Ok(()),
        other => Err(format!("不支持的操作: {other}")),
    }
}

/// 校验进程名称:非空、无首尾空白、不以连字符开头、不含控制字符。
pub fn validate_process_name(process_name: &str) -> Result<(), String> {
    if process_name.trim().is_empty() {
        return Err("进程名称不能为空".to_string());
    }
    if process_name.trim() != process_name {
        return Err("进程名称不能包含首尾空白".to_string());
    }
    if process_name.starts_with('-') {
        return Err("进程名称不能以连字符开头".to_string());
    }
    for ch in process_name.chars() {
        if ch.is_control() || ch == '\0' {
            return Err("进程名称包含控制字符".to_string());
        }
    }
    Ok(())
}

/// 将序号、名称、序号范围解析为去重后的唯一进程名称列表。
/// 支持:单个序号、进程名、多个参数、"A-B" 范围、名称后缀以及混合输入。
pub fn parse_process_indices(
    args: &[String],
    processes: &[ProcessInfo],
) -> Result<Vec<String>, String> {
    if args.is_empty() {
        return Err("未提供进程参数".to_string());
    }

    let mut by_index: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    let mut by_name: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (index, process) in processes.iter().enumerate() {
        let actual_index = if process.index > 0 {
            process.index
        } else {
            index + 1
        };
        by_index.insert(actual_index, process.name.clone());
        by_name.insert(process.name.clone(), process.name.clone());
    }

    let mut result: Vec<String> = Vec::with_capacity(args.len());
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let append_name =
        |name: &str, result: &mut Vec<String>, seen: &mut std::collections::HashSet<String>| {
            if seen.contains(name) {
                return;
            }
            seen.insert(name.to_string());
            result.push(name.to_string());
        };

    for raw in args {
        let argument = raw.trim();
        if argument.is_empty() {
            return Err("进程参数不能为空".to_string());
        }

        if let Some((start_text, end_text)) = argument.split_once('-') {
            let start_ok = start_text.trim().parse::<usize>();
            let end_ok = end_text.trim().parse::<usize>();
            if let (Ok(start), Ok(end)) = (start_ok, end_ok) {
                if start == 0 || end == 0 || start > end {
                    return Err(format!("无效的进程范围 {argument:?}"));
                }
                for index in start..=end {
                    match by_index.get(&index) {
                        Some(name) => append_name(name, &mut result, &mut seen),
                        None => return Err(format!("未找到序号为{index}的进程")),
                    }
                }
                continue;
            }
        }

        if let Ok(index) = argument.parse::<usize>() {
            match by_index.get(&index) {
                Some(name) => {
                    append_name(name, &mut result, &mut seen);
                    continue;
                }
                None => return Err(format!("未找到序号为{index}的进程")),
            }
        }

        if let Some(name) = by_name.get(argument) {
            append_name(name, &mut result, &mut seen);
            continue;
        }

        let suffix = format!(":{argument}");
        let matches: Vec<&String> = by_name
            .keys()
            .filter(|name| name.ends_with(&suffix))
            .collect();
        if matches.len() == 1 {
            append_name(matches[0], &mut result, &mut seen);
            continue;
        }
        if matches.len() > 1 {
            return Err(format!("进程名称 {argument:?} 不唯一，请使用完整名称"));
        }
        return Err(format!("未找到进程 {argument:?}"));
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<ProcessInfo> {
        vec![
            ProcessInfo {
                index: 1,
                name: "web:api".to_string(),
                ..ProcessInfo::default()
            },
            ProcessInfo {
                index: 2,
                name: "web:worker".to_string(),
                ..ProcessInfo::default()
            },
            ProcessInfo {
                index: 3,
                name: "batch".to_string(),
                ..ProcessInfo::default()
            },
            ProcessInfo {
                index: 4,
                name: "my-app".to_string(),
                ..ProcessInfo::default()
            },
        ]
    }

    #[test]
    fn test_parse_indices_forms() {
        let got = parse_process_indices(
            &[
                "1-2".to_string(),
                "batch".to_string(),
                "api".to_string(),
                "my-app".to_string(),
                "1".to_string(),
            ],
            &sample(),
        )
        .unwrap();
        assert_eq!(got, vec!["web:api", "web:worker", "batch", "my-app"]);
    }

    #[test]
    fn test_parse_indices_errors() {
        let err = parse_process_indices(&["5".to_string()], &sample()).unwrap_err();
        assert!(err.contains("未找到序号为5的进程"));
        let err = parse_process_indices(
            &["api".to_string()],
            &[
                ProcessInfo {
                    index: 1,
                    name: "web:api".to_string(),
                    ..ProcessInfo::default()
                },
                ProcessInfo {
                    index: 2,
                    name: "batch:api".to_string(),
                    ..ProcessInfo::default()
                },
            ],
        )
        .unwrap_err();
        assert!(err.contains("不唯一"));
        let err = parse_process_indices(&["3-1".to_string()], &sample()).unwrap_err();
        assert!(err.contains("无效的进程范围"));
        let got = parse_process_indices(&["web:api".to_string()], &sample()).unwrap();
        assert_eq!(got, vec!["web:api"]);
        let err = parse_process_indices(&["ghost".to_string()], &sample()).unwrap_err();
        assert!(err.contains("未找到进程"));
    }

    #[test]
    fn test_validate_action_and_name() {
        assert!(validate_action("restart").is_ok());
        assert!(validate_action("bogus").is_err());
        assert!(validate_process_name("nginx").is_ok());
        assert!(validate_process_name("-x").is_err());
        assert!(validate_process_name(" a").is_err());
        assert!(validate_process_name("").is_err());
    }
}
