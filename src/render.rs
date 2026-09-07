//! 表格渲染:逐字节对齐 Go 版 `olekukonko/tablewriter`(StyleLight)的输出几何。
//!
//! 复刻规则:
//! - 列宽 = 该列所有单元格「可见显示宽」最大值 + 2(两侧各 1 空格边距);
//! - 表头在 列宽−2 的区域内居中(余量左侧下取整、右侧上取整);
//! - 正文在 列宽−2 的区域内左对齐;
//! - 边框采用 `┌┬┐ ├┼┤ └┴┘ ─ │`,与 Go 的 StyleLight 一致。
//!
//! 显示宽按 `unicode-width` 计算(CJK 宽字符记 2 列),ANSI 转义序列记 0 列,
//! 因此带颜色的状态格不影响表格列宽。

use unicode_width::UnicodeWidthChar;

/// 去除 ANSI 转义序列后的可见字符序列。
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// 计算可见显示宽(ANSI 为 0,CJK 宽字符为 2)。
fn display_width(text: &str) -> usize {
    strip_ansi(text)
        .chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

/// 单元格对齐方式。
#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Center,
}

/// 生成一个单元格:两侧各留 1 空格边距,内容按对齐方式在 列宽−2 区域内摆放。
fn pad_cell(content: &str, width: usize, align: Align) -> String {
    let visible = display_width(content);
    let zone = width.saturating_sub(2);
    let left = match align {
        Align::Left => 0,
        Align::Center => (zone - visible.min(zone)) / 2,
    };
    let right = zone.saturating_sub(visible).saturating_sub(left);
    let mut cell = String::with_capacity(width);
    cell.push(' ');
    for _ in 0..left {
        cell.push(' ');
    }
    cell.push_str(content);
    for _ in 0..right {
        cell.push(' ');
    }
    cell.push(' ');
    cell
}

/// 计算各列宽度 = 该列最大可见宽 + 2。
fn compute_widths(headers: &[String], rows: &[Vec<String>]) -> Vec<usize> {
    let mut widths: Vec<usize> = headers.iter().map(|h| display_width(h)).collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            if index < widths.len() {
                widths[index] = widths[index].max(display_width(cell));
            }
        }
    }
    widths.iter().map(|w| w + 2).collect()
}

/// 生成一条横线(顶/中/底共用)。
fn border(left: char, mid: char, right: char, widths: &[usize]) -> String {
    let mut line = String::with_capacity(widths.iter().sum::<usize>() + widths.len() + 1);
    line.push(left);
    for (index, width) in widths.iter().enumerate() {
        for _ in 0..*width {
            line.push('─');
        }
        if index + 1 < widths.len() {
            line.push(mid);
        }
    }
    line.push(right);
    line
}

/// 渲染一张表格,返回不含结尾换行的文本块。
pub fn render_table(headers: &[String], rows: &[Vec<String>]) -> String {
    if headers.is_empty() {
        return String::new();
    }
    let widths = compute_widths(headers, rows);

    let mut lines = Vec::with_capacity(rows.len() + 4);
    lines.push(border('┌', '┬', '┐', &widths));

    let header_cells: Vec<String> = headers
        .iter()
        .enumerate()
        .map(|(index, header)| pad_cell(header, widths[index], Align::Center))
        .collect();
    lines.push(join_row(&header_cells));
    lines.push(border('├', '┼', '┤', &widths));

    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(index, cell)| pad_cell(cell, widths[index], Align::Left))
            .collect();
        lines.push(join_row(&cells));
    }
    lines.push(border('└', '┴', '┘', &widths));
    lines.join("\n")
}

fn join_row(cells: &[String]) -> String {
    format!("│{}│", cells.join("│"))
}

/// 空进程提示文案(与 Go `RenderStatus` 一致)。
pub const NO_PROCESSES_TEXT: &str = "没有找到任何进程";

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    /// 来自真实 Go 输出的两进程表格(golden/status_two.txt 的第 3~8 行)。
    const EXPECTED_TWO: &str = "┌──────┬────────────┬─────────┬──────┬───────────────┐\n\
│ 序号 │    名称    │  状态   │ PID  │   运行时间    │\n\
├──────┼────────────┼─────────┼──────┼───────────────┤\n\
│ 1    │ web:api    │ RUNNING │ 1234 │ 4小时50分06秒 │\n\
│ 2    │ web:worker │ STOPPED │ -    │ 已停止        │\n\
└──────┴────────────┴─────────┴──────┴───────────────┘";

    fn two_rows() -> Vec<Vec<String>> {
        vec![
            strings(&["1", "web:api", "RUNNING", "1234", "4小时50分06秒"]),
            strings(&["2", "web:worker", "STOPPED", "-", "已停止"]),
        ]
    }

    #[test]
    fn test_two_process_table_matches_golden() {
        let headers = strings(&["序号", "名称", "状态", "PID", "运行时间"]);
        let table = render_table(&headers, &two_rows());
        assert_eq!(table, EXPECTED_TWO);
    }

    #[test]
    fn test_single_process_column_width_shrinks() {
        let headers = strings(&["序号", "名称", "状态", "PID", "运行时间"]);
        let rows = vec![strings(&["1", "app", "RUNNING", "99", "3秒"])];
        let table = render_table(&headers, &rows);
        assert_eq!(
            table,
            "┌──────┬──────┬─────────┬─────┬──────────┐\n\
│ 序号 │ 名称 │  状态   │ PID │ 运行时间 │\n\
├──────┼──────┼─────────┼─────┼──────────┤\n\
│ 1    │ app  │ RUNNING │ 99  │ 3秒      │\n\
└──────┴──────┴─────────┴─────┴──────────┘"
        );
    }

    #[test]
    fn test_display_width_counts_cjk() {
        assert_eq!(display_width("序号"), 4);
        assert_eq!(display_width("web:api"), 7);
        assert_eq!(display_width("\x1b[32mRUNNING\x1b[0m"), 7);
        assert_eq!(display_width("4小时50分06秒"), 13);
    }

    #[test]
    fn test_pad_cell_center_and_left() {
        assert_eq!(pad_cell("名称", 12, Align::Center), "    名称    ");
        assert_eq!(pad_cell("1", 6, Align::Left), " 1    ");
        assert_eq!(pad_cell("RUNNING", 9, Align::Left), " RUNNING ");
        assert_eq!(
            pad_cell("\x1b[32mRUNNING\x1b[0m", 9, Align::Left),
            " \x1b[32mRUNNING\x1b[0m "
        );
    }

    #[test]
    fn test_empty_headers_yields_empty() {
        assert_eq!(render_table(&[], &[]), "");
    }
}
