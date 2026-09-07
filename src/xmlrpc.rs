//! 最小可用的 XML-RPC 编解码(Supervisor 子集)。
//!
//! 请求体按 Supervisor 协议手工序列化;响应先用 `quick-xml` 建成轻量节点树,
//! 再递归映射为强类型的 [`Value`]。逐点对齐 Go `pkg/supervisor/types.go`。

use quick_xml::Reader;
use quick_xml::events::Event;
use std::collections::BTreeMap;

/// 一个 XML-RPC 值。与 Go 的 `interface{}` 相比,Rust 用枚举表达得更精确。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nil,
    Str(String),
    Int(i64),
    Bool(bool),
    Double(f64),
    Array(Vec<Value>),
    Struct(BTreeMap<String, Value>),
}

impl Value {
    /// 将值当作字符串读取(仅当值为 Str)。
    pub fn as_string(&self) -> Option<&str> {
        match self {
            Value::Str(text) => Some(text),
            _ => None,
        }
    }

    /// 将值当作有符号整数读取(整数或整数值浮点数)。
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(number) => Some(*number),
            Value::Double(number) => {
                if number.is_finite() && number.fract() == 0.0 {
                    Some(*number as i64)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// 将值当作浮点数读取。
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Double(number) => Some(*number),
            Value::Int(number) => Some(*number as f64),
            _ => None,
        }
    }

    /// 将 Struct 的某个键读作字符串。
    pub fn struct_string(&self, key: &str) -> Option<&str> {
        match self {
            Value::Struct(members) => members.get(key).and_then(Value::as_string),
            _ => None,
        }
    }

    /// 将 Struct 的某个键读作整数(供测试使用)。
    #[cfg(test)]
    pub fn struct_i64(&self, key: &str) -> Option<i64> {
        match self {
            Value::Struct(members) => members.get(key).and_then(Value::as_i64),
            _ => None,
        }
    }
}

/// XML 文本内容转义。
fn escape_xml(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn value_xml(value: &Value) -> String {
    match value {
        Value::Nil => "<nil/>".to_string(),
        Value::Str(text) => format!("<string>{}</string>", escape_xml(text)),
        Value::Int(number) => format!("<int>{number}</int>"),
        Value::Bool(flag) => {
            let digit = if *flag { "1" } else { "0" };
            format!("<boolean>{digit}</boolean>")
        }
        Value::Double(number) => format!("<double>{number}</double>"),
        Value::Array(items) => {
            let inner: String = items
                .iter()
                .map(|item| format!("<value>{}</value>", value_xml(item)))
                .collect();
            format!("<array><data>{inner}</data></array>")
        }
        Value::Struct(members) => {
            let inner: String = members
                .iter()
                .map(|(key, item)| {
                    format!(
                        "<member><name>{}</name><value>{}</value></member>",
                        escape_xml(key),
                        value_xml(item)
                    )
                })
                .collect();
            format!("<struct>{inner}</struct>")
        }
    }
}

/// 构造一个 `<methodCall>` 请求体。
pub fn method_call_xml(method: &str, params: &[Value]) -> String {
    let inner: String = params
        .iter()
        .map(|param| format!("<param><value>{}</value></param>", value_xml(param)))
        .collect();
    format!(
        "<?xml version=\"1.0\"?><methodCall><methodName>{}</methodName><params>{inner}</params></methodCall>",
        escape_xml(method)
    )
}

// ---- 响应解析 ---------------------------------------------------------------

/// 极简 XML 节点树,只关心元素结构。
struct Node {
    name: String,
    text: String,
    children: Vec<Node>,
}

impl Node {
    fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|child| child.name == name)
    }

    fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> {
        self.children.iter().filter(move |child| child.name == name)
    }
}

/// 将字节流解析成一棵节点树(忽略 XML 声明与注释)。
fn parse_tree(body: &str) -> Result<Node, String> {
    let mut reader = Reader::from_str(body);
    let mut stack: Vec<Node> = Vec::new();
    let mut root: Option<Node> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let name = event.name().into_inner().to_string();
                stack.push(Node {
                    name,
                    text: String::new(),
                    children: Vec::new(),
                });
            }
            Ok(Event::Empty(event)) => {
                let name = event.name().into_inner().to_string();
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(Node {
                        name,
                        text: String::new(),
                        children: Vec::new(),
                    });
                }
            }
            Ok(Event::Text(event)) => {
                if let Some(current) = stack.last_mut()
                    && let Ok(text) = quick_xml::escape::unescape(&event)
                {
                    current.text.push_str(&text);
                }
            }
            Ok(Event::End(_)) => {
                let finished = stack.pop().ok_or_else(|| "XML结束标签不匹配".to_string())?;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(finished);
                } else if root.is_none() {
                    root = Some(finished);
                } else {
                    return Err("XML包含多个根元素".to_string());
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("XML解析失败: {error}")),
            _ => {}
        }
    }
    root.ok_or_else(|| "XML缺少根元素".to_string())
}

/// 递归解析一个 `<value>` 节点(其直接子节点为某个类型标签)。
fn parse_value_node(value: &Node) -> Result<Value, String> {
    if value.child("nil").is_some() {
        return Ok(Value::Nil);
    }
    if let Some(child) = value.child("string") {
        return Ok(Value::Str(child.text.clone()));
    }
    for tag in ["int", "i4", "i8"] {
        if let Some(child) = value.child(tag) {
            let number = child
                .text
                .trim()
                .parse::<i64>()
                .map_err(|_| format!("XML-RPC整数无效: {}", child.text.trim()))?;
            return Ok(Value::Int(number));
        }
    }
    if let Some(child) = value.child("boolean") {
        let raw = child.text.trim().to_lowercase();
        return match raw.as_str() {
            "1" | "true" => Ok(Value::Bool(true)),
            "0" | "false" => Ok(Value::Bool(false)),
            _ => Err(format!("无效的XML-RPC布尔值: {raw:?}")),
        };
    }
    if let Some(child) = value.child("double") {
        let number = child
            .text
            .trim()
            .parse::<f64>()
            .map_err(|_| format!("XML-RPC浮点数无效: {}", child.text.trim()))?;
        return Ok(Value::Double(number));
    }
    if let Some(child) = value.child("array") {
        let mut items = Vec::new();
        for data in child.children_named("data") {
            for value_node in data.children_named("value") {
                items.push(parse_value_node(value_node)?);
            }
        }
        return Ok(Value::Array(items));
    }
    if let Some(child) = value.child("struct") {
        let mut members = BTreeMap::new();
        for member in child.children_named("member") {
            let key = member
                .child("name")
                .map(|n| n.text.clone())
                .unwrap_or_default();
            let value_node = member
                .child("value")
                .ok_or_else(|| "XML-RPC struct缺少value".to_string())?;
            members.insert(key, parse_value_node(value_node)?);
        }
        return Ok(Value::Struct(members));
    }
    Err("空或未知的XML-RPC值".to_string())
}

/// 把 fault 值转成可读的 XML-RPC 错误文案。
fn format_fault(value: &Value) -> String {
    if let Some(message) = value.struct_string("faultString")
        && !message.is_empty()
    {
        return format!("XML-RPC错误: {message}");
    }
    format!("XML-RPC错误: {value:?}")
}

/// 解析一个完整 `<methodResponse>`,返回成功值或 XML-RPC 错误文案。
pub fn parse_method_response(body: &str) -> Result<Value, String> {
    let root = parse_tree(body)?;
    if root.name != "methodResponse" {
        return Err("XML响应缺少methodResponse".to_string());
    }
    if let Some(fault) = root.child("fault") {
        let value_node = fault
            .child("value")
            .ok_or_else(|| "XML-RPC fault缺少value".to_string())?;
        return Err(format_fault(&parse_value_node(value_node)?));
    }
    let params = root
        .child("params")
        .ok_or_else(|| "XML响应缺少params".to_string())?;
    let param = params
        .child("param")
        .ok_or_else(|| "XML-RPC响应缺少参数".to_string())?;
    let value_node = param
        .child("value")
        .ok_or_else(|| "XML-RPC响应缺少value".to_string())?;
    parse_value_node(value_node)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_method_call_xml() {
        let body = method_call_xml(
            "supervisor.startProcess",
            &[Value::Str("web:api".to_string()), Value::Bool(true)],
        );
        assert!(body.contains("<methodName>supervisor.startProcess</methodName>"));
        assert!(body.contains("<string>web:api</string>"));
        assert!(body.contains("<boolean>1</boolean>"));
        assert!(!body.contains("<boolean>true</boolean>"));
    }

    #[test]
    fn test_parse_boolean_response() {
        let true_body = "<?xml version=\"1.0\"?><methodResponse><params><param><value><boolean>1</boolean></value></param></params></methodResponse>";
        assert_eq!(parse_method_response(true_body).unwrap(), Value::Bool(true));
        let false_body = true_body.replace("<boolean>1</boolean>", "<boolean>0</boolean>");
        assert_eq!(
            parse_method_response(&false_body).unwrap(),
            Value::Bool(false)
        );
    }

    #[test]
    fn test_parse_process_struct() {
        let body = "<?xml version=\"1.0\"?><methodResponse><params><param><value><array><data><value><struct>
<member><name>name</name><value><string>api</string></value></member>
<member><name>group</name><value><string>web</string></value></member>
<member><name>state</name><value><int>20</int></value></member>
<member><name>pid</name><value><int>1234</int></value></member>
<member><name>start</name><value><double>1000</double></value></member>
</struct></value></data></array></value></param></params></methodResponse>";
        let value = parse_method_response(body).unwrap();
        let Value::Array(items) = value else {
            panic!("期望数组")
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].struct_string("name"), Some("api"));
        assert_eq!(items[0].struct_string("group"), Some("web"));
        assert_eq!(items[0].struct_i64("state"), Some(20));
        assert_eq!(items[0].struct_i64("pid"), Some(1234));
    }

    #[test]
    fn test_parse_fault() {
        let body = "<?xml version=\"1.0\"?><methodResponse><fault><value><struct><member><name>faultString</name><value><string>NOT_RUNNING</string></value></member></struct></value></fault></methodResponse>";
        let error = parse_method_response(body).unwrap_err();
        assert_eq!(error, "XML-RPC错误: NOT_RUNNING");
    }

    #[test]
    fn test_parse_malformed() {
        assert!(parse_method_response("not xml at all").is_err());
    }
}
