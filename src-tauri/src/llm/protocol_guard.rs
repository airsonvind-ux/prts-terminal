//! protocol_guard —— 流式协议异常的清洗（对照既有实现的 stream-guard）。
//!
//! 现状缺口（2026-10-10）：供应商偶发抽出**没有名字的工具调用**（`name` 为空）。
//! 它一路走到工具注册表只会得到"工具不存在"，用户的观感是模型刚说的那段话不见了，
//! 还白烧一步。
//!
//! 处置沿用既有实现：匿名调用不让它流到上层，先把它累积的参数缓冲住，等这一段结束
//! 再尽力**回写成可见正文**：
//!   1. 参数是 JSON → 取 text / content / message / body / input 里的字符串；
//!   2. 参数看着像正文（不以 `{`、`[`、`"` 开头）→ 原样当正文；
//!   3. 都不行（协议碎片）→ 丢弃，不回写成可见文本。
//!
//! **为什么事件层与聚合层都要清**：事件层决定用户看到什么（流式卡片 + 日志 chunk），
//! 聚合层决定下一轮的助手消息与工具执行。少清一层就会出现"卡片没了，但下一轮
//! 还在调它"。

use super::{StreamEvent, StreamResult};

/// 一次流式调用期间的协议清洗状态。
#[derive(Default)]
pub struct ProtocolGuard {
    /// 缓冲中的匿名调用（按到达顺序）。
    pending: Vec<Pending>,
    /// 已回写出去的正文（聚合层据此补齐 `StreamResult::text`，两层保持一致）。
    recovered: String,
}

/// 一个被缓冲的匿名调用。
struct Pending {
    /// 归组键：有 id 用 id，没有就用序号（同一次流里多个匿名调用互不串味）。
    key: String,
    /// 累积的参数（同一调用多次到达时追加）。
    arguments: String,
}

impl ProtocolGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// 清洗一个事件：返回应当继续向上走的事件（可能为空，也可能被换成正文片段）。
    pub fn push(&mut self, event: StreamEvent) -> Vec<StreamEvent> {
        match event {
            StreamEvent::ToolCall {
                ref id,
                ref name,
                ref arguments,
            } if is_anonymous(name) => {
                let key = if id.trim().is_empty() {
                    format!("anon:{}", self.pending.len())
                } else {
                    id.clone()
                };
                match self.pending.iter_mut().find(|p| p.key == key) {
                    Some(pending) => pending.arguments.push_str(arguments),
                    None => self.pending.push(Pending {
                        key,
                        arguments: arguments.clone(),
                    }),
                }
                Vec::new()
            }
            other => vec![other],
        }
    }

    /// 流结束（或出错）时收尾：把缓冲住的匿名调用尽力回写成正文片段。
    ///
    /// 幂等：第二次调用不会再吐东西（`pending` 已排空）。
    pub fn finish(&mut self) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        for pending in std::mem::take(&mut self.pending) {
            if let Some(text) = recover_text(&pending.arguments) {
                self.recovered.push_str(&text);
                out.push(StreamEvent::TextDelta { content: text });
            }
        }
        out
    }

    /// 聚合层清洗：剔除匿名工具调用，并把回写出来的正文补进 `text`。
    pub fn sanitize_result(&mut self, result: &mut StreamResult) {
        result
            .tool_calls
            .retain(|call| !is_anonymous(&call.function.name));
        if !self.recovered.is_empty() {
            result.text.push_str(&self.recovered);
        }
    }
}

/// 无名调用（`name` 为空或全空白）。
fn is_anonymous(name: &str) -> bool {
    name.trim().is_empty()
}

/// 尽力把匿名调用的参数还原成正文。
fn recover_text(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(found) = from_json_like(trimmed) {
        return Some(found);
    }
    // 协议碎片（半截 JSON / 半个字符串）：是失败的工具调用，不是 prose，丢弃。
    if trimmed.starts_with('{') || trimmed.starts_with('[') || trimmed.starts_with('"') {
        return None;
    }
    Some(raw.to_string())
}

/// 参数像 JSON 时，从常见字段里取正文。
fn from_json_like(text: &str) -> Option<String> {
    if !(text.starts_with('{') || text.starts_with('[')) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    for key in ["text", "content", "message", "body", "input"] {
        if let Some(found) = value.get(key).and_then(|v| v.as_str()) {
            if !found.trim().is_empty() {
                return Some(found.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ToolCall, ToolCallFunction};

    fn anonymous(arguments: &str) -> StreamEvent {
        StreamEvent::ToolCall {
            id: String::new(),
            name: String::new(),
            arguments: arguments.to_string(),
        }
    }

    fn named(id: &str, name: &str, arguments: &str) -> StreamEvent {
        StreamEvent::ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        }
    }

    #[test]
    fn normal_tool_call_passes_through_untouched() {
        let mut guard = ProtocolGuard::new();
        assert_eq!(guard.push(named("t1", "echo", "{}")).len(), 1);
        assert!(guard.finish().is_empty());
    }

    #[test]
    fn normal_text_passes_through() {
        let mut guard = ProtocolGuard::new();
        assert_eq!(
            guard
                .push(StreamEvent::TextDelta {
                    content: "好".into()
                })
                .len(),
            1
        );
    }

    #[test]
    fn anonymous_prose_is_recovered_as_text() {
        let mut guard = ProtocolGuard::new();
        assert!(guard.push(anonymous("博士，这个界面就是主界面")).is_empty());
        let out = guard.finish();
        assert_eq!(out.len(), 1);
        match &out[0] {
            StreamEvent::TextDelta { content } => assert!(content.contains("主界面")),
            other => panic!("应为正文片段：{other:?}"),
        }
    }

    #[test]
    fn anonymous_json_field_is_recovered() {
        let mut guard = ProtocolGuard::new();
        assert!(guard
            .push(anonymous(r#"{"text":"落回正文","other":1}"#))
            .is_empty());
        match guard.finish().first() {
            Some(StreamEvent::TextDelta { content }) => assert_eq!(content, "落回正文"),
            other => panic!("应从字段里回写：{other:?}"),
        }
    }

    #[test]
    fn protocol_fragment_is_dropped() {
        let mut guard = ProtocolGuard::new();
        assert!(guard.push(anonymous(r#"{"nam"#)).is_empty());
        assert!(guard.finish().is_empty(), "半截 JSON 不回写成正文");
        assert!(guard.push(anonymous("\"未闭合")).is_empty());
        assert!(guard.finish().is_empty());
    }

    #[test]
    fn empty_arguments_recover_nothing() {
        let mut guard = ProtocolGuard::new();
        assert!(guard.push(anonymous("   ")).is_empty());
        assert!(guard.finish().is_empty());
    }

    #[test]
    fn fragmented_arguments_are_concatenated() {
        let mut guard = ProtocolGuard::new();
        // 同一调用分片到达（id 相同 → 归为一组）
        let _ = guard.push(StreamEvent::ToolCall {
            id: "anon-1".into(),
            name: String::new(),
            arguments: "博士".into(),
        });
        let _ = guard.push(StreamEvent::ToolCall {
            id: "anon-1".into(),
            name: String::new(),
            arguments: "，看这里".into(),
        });
        match guard.finish().first() {
            Some(StreamEvent::TextDelta { content }) => assert_eq!(content, "博士，看这里"),
            other => panic!("应拼成一整句：{other:?}"),
        }
    }

    #[test]
    fn finish_is_idempotent() {
        let mut guard = ProtocolGuard::new();
        let _ = guard.push(anonymous("一段正文"));
        assert_eq!(guard.finish().len(), 1);
        assert!(guard.finish().is_empty());
    }

    #[test]
    fn sanitize_result_drops_anonymous_and_keeps_recovered_text() {
        let mut guard = ProtocolGuard::new();
        let _ = guard.push(anonymous("被吞掉的正文"));
        let _ = guard.finish();

        let mut result = StreamResult {
            text: "已说：".to_string(),
            tool_calls: vec![
                ToolCall {
                    id: String::new(),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: String::new(),
                        arguments: "被吞掉的正文".into(),
                    },
                },
                ToolCall {
                    id: "t1".into(),
                    call_type: "function".into(),
                    function: ToolCallFunction {
                        name: "echo".into(),
                        arguments: "{}".into(),
                    },
                },
            ],
            reasoning: None,
            usage: None,
            finish_reason: None,
        };

        guard.sanitize_result(&mut result);
        assert_eq!(result.tool_calls.len(), 1, "匿名调用被剔除");
        assert_eq!(result.tool_calls[0].function.name, "echo");
        assert_eq!(result.text, "已说：被吞掉的正文");
    }
}
