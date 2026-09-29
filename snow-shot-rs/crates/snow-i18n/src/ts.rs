//! Qt `.ts` 翻译文件解析器（XML 层交给 quick-xml，这里只做 `.ts` 语义）。

use quick_xml::XmlVersion;
use quick_xml::escape::unescape;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::fmt;

/// 解析错误，携带字节偏移与原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsError {
    /// 出错位置（字节偏移）。
    pub offset: usize,
    /// 错误原因。
    pub reason: String,
}

impl fmt::Display for TsError {
    /// 输出“原因 (偏移)”。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (偏移 {})", self.reason, self.offset)
    }
}

impl std::error::Error for TsError {}

/// 条目翻译状态（对应 `translation@type`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsStatus {
    /// 已完成（无 type 属性）。
    Finished,
    /// 未完成（`unfinished`）。
    Unfinished,
    /// 已过时（`obsolete`）。
    Obsolete,
    /// 源码中已消失（`vanished`）。
    Vanished,
}

/// 单条翻译消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsMessage {
    /// 源文（保留首尾空白）。
    pub source: String,
    /// 消歧注释（`<comment>`）。
    pub comment: Option<String>,
    /// 译文；复数条目按 `<numerusform>` 顺序，否则只有一项。
    pub translations: Vec<String>,
    /// 是否为复数条目。
    pub numerus: bool,
    /// 翻译状态。
    pub status: TsStatus,
    /// `<location>` 个数。
    pub location_count: usize,
}

/// 翻译上下文（一个 `<context>`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsContext {
    /// 上下文名（通常是类名）。
    pub name: String,
    /// 该上下文下的消息。
    pub messages: Vec<TsMessage>,
}

/// 一份 `.ts` 目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsCatalog {
    /// 目标语言，如 `zh_CN`。
    pub language: String,
    /// 源语言，如 `en_US`。
    pub source_language: Option<String>,
    /// 全部上下文。
    pub contexts: Vec<TsContext>,
}

/// XML 节点。
enum Node {
    Elem(Element),
    Text(String),
}

/// XML 元素。
struct Element {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
}

impl Element {
    /// 取属性值。
    fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// 按名字取第一个子元素。
    fn child<'a>(&'a self, name: &'a str) -> Option<&'a Element> {
        self.elems(name).next()
    }

    /// 迭代指定名字的子元素。
    fn elems<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter_map(move |n| match n {
            Node::Elem(e) if e.name == name => Some(e),
            _ => None,
        })
    }

    /// 元素文本（含 Qt 的 `<byte value="x9"/>` 控制字符转义）。
    fn text(&self) -> String {
        let mut out = String::new();
        for n in &self.children {
            match n {
                Node::Text(t) => out.push_str(t),
                Node::Elem(e) if e.name == "byte" => {
                    if let Some(c) = e.attr("value").and_then(decode_byte_value) {
                        out.push(c);
                    }
                }
                Node::Elem(_) => {}
            }
        }
        out
    }
}

/// 解码 `<byte value>`：`x` 前缀为十六进制，否则十进制。
fn decode_byte_value(v: &str) -> Option<char> {
    let code = match v.strip_prefix('x') {
        Some(h) => u32::from_str_radix(h, 16).ok()?,
        None => v.parse().ok()?,
    };
    char::from_u32(code)
}

/// 构造带偏移的错误。
fn err(offset: usize, reason: impl ToString) -> TsError {
    TsError {
        offset,
        reason: reason.to_string(),
    }
}

/// 把 quick-xml 的开始标签转成元素（属性值已解码）。
fn element_from_start(e: &BytesStart<'_>, pos: usize) -> Result<Element, TsError> {
    let name = e.name().as_ref().to_string();
    let mut attrs = Vec::new();
    for a in e.attributes() {
        let a = a.map_err(|x| err(pos, x))?;
        let key = a.key.as_ref().to_string();
        let val = a
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|x| err(pos, x))?;
        attrs.push((key, val.into_owned()));
    }
    Ok(Element {
        name,
        attrs,
        children: Vec::new(),
    })
}

/// 把 XML 文本解析为根元素。
fn parse_xml(src: &str) -> Result<Element, TsError> {
    let mut reader = Reader::from_str(src);
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    loop {
        let pos = reader.buffer_position() as usize;
        let event = reader.read_event().map_err(|x| err(pos, x))?;
        match event {
            Event::Start(e) => stack.push(element_from_start(&e, pos)?),
            Event::Empty(e) => attach(&mut stack, &mut root, element_from_start(&e, pos)?, pos)?,
            Event::End(_) => {
                let el = stack.pop().ok_or_else(|| err(pos, "多余的结束标签"))?;
                attach(&mut stack, &mut root, el, pos)?;
            }
            Event::Text(t) => push_text(&mut stack, &t),
            Event::CData(t) => push_text(&mut stack, &t),
            Event::GeneralRef(r) => {
                let ent = format!("&{};", &*r);
                let ch = unescape(&ent).map_err(|x| err(pos, x))?;
                push_text(&mut stack, &ch);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(err(src.len(), "存在未闭合的元素"));
    }
    root.ok_or_else(|| err(0, "缺少根元素"))
}

/// 把文本追加到当前元素（相邻文本合并；根之外的文本忽略）。
fn push_text(stack: &mut [Element], text: &str) {
    let Some(top) = stack.last_mut() else { return };
    if let Some(Node::Text(t)) = top.children.last_mut() {
        t.push_str(text);
    } else {
        top.children.push(Node::Text(text.to_string()));
    }
}

/// 把完成的元素挂到父节点或作为根。
fn attach(
    stack: &mut [Element],
    root: &mut Option<Element>,
    el: Element,
    pos: usize,
) -> Result<(), TsError> {
    match stack.last_mut() {
        Some(parent) => parent.children.push(Node::Elem(el)),
        None if root.is_none() => *root = Some(el),
        None => return Err(err(pos, "存在多个根元素")),
    }
    Ok(())
}

/// 解析一份 `.ts` 文本。
///
/// # 参数
/// - `xml`：`.ts` 文件全文。
///
/// # 返回
/// 解析后的目录；XML 非法或根不是 `<TS>` 时返回 [`TsError`]。
///
/// # 示例
/// ```
/// let xml = r#"<TS language="zh_CN"><context><name>A</name><message>
/// <source>Hi</source><translation>你好</translation></message></context></TS>"#;
/// let cat = snow_i18n::ts::parse_ts(xml).unwrap();
/// assert_eq!(cat.contexts[0].messages[0].translations[0], "你好");
/// ```
pub fn parse_ts(xml: &str) -> Result<TsCatalog, TsError> {
    let root = parse_xml(xml)?;
    if root.name != "TS" {
        return Err(err(0, "根元素不是 TS"));
    }
    let language = root.attr("language").unwrap_or_default().to_string();
    let source_language = root.attr("sourcelanguage").map(str::to_string);
    let mut contexts = Vec::new();
    for c in root.elems("context") {
        let name = c.child("name").map(Element::text).unwrap_or_default();
        let mut messages = Vec::new();
        for m in c.elems("message") {
            messages.push(parse_message(m)?);
        }
        contexts.push(TsContext { name, messages });
    }
    Ok(TsCatalog {
        language,
        source_language,
        contexts,
    })
}

/// 解析单个 `<message>`。
fn parse_message(m: &Element) -> Result<TsMessage, TsError> {
    let source = m.child("source").map(Element::text).unwrap_or_default();
    let tr = m.child("translation");
    let status = match tr.and_then(|t| t.attr("type")) {
        None => TsStatus::Finished,
        Some("unfinished") => TsStatus::Unfinished,
        Some("obsolete") => TsStatus::Obsolete,
        Some("vanished") => TsStatus::Vanished,
        Some(_) => return Err(err(0, "未知的 translation type")),
    };
    let numerus = m.attr("numerus") == Some("yes");
    let translations = if numerus {
        tr.map(|t| t.elems("numerusform").map(Element::text).collect())
            .unwrap_or_default()
    } else {
        vec![tr.map(Element::text).unwrap_or_default()]
    };
    Ok(TsMessage {
        source,
        comment: m.child("comment").map(Element::text),
        translations,
        numerus,
        status,
        location_count: m.elems("location").count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 实体、byte 转义、复数与状态应被正确解析。
    #[test]
    fn parses_entities_numerus_status() {
        let xml = r#"<?xml version="1.0"?><!DOCTYPE TS><TS language="en_US" sourcelanguage="en_US">
<context><name>C</name>
<message numerus="yes"><source>%n a</source><translation><numerusform>%n a</numerusform><numerusform>%n as</numerusform></translation></message>
<message><source> A &amp; B &lt;x&gt; &#65;</source><translation type="unfinished">t<byte value="x9"/>u</translation></message>
</context></TS>"#;
        let c = parse_ts(xml).unwrap();
        let ms = &c.contexts[0].messages;
        assert!(ms[0].numerus);
        assert_eq!(ms[0].translations.len(), 2);
        assert_eq!(ms[1].source, " A & B <x> A");
        assert_eq!(ms[1].translations[0], "t\tu");
        assert_eq!(ms[1].status, TsStatus::Unfinished);
    }

    /// 标签不匹配应报错。
    #[test]
    fn rejects_mismatched_tags() {
        assert!(parse_ts("<TS><context></TS>").is_err());
    }
}
