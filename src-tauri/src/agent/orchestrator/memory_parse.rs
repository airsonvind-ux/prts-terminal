//! 记忆提炼输出的预处理、解析与校验。
//!
//! 只处理文本到操作清单的转换，不触碰存储与管线状态。

use super::memory::{MAX_SUMMARY_CHARS, MAX_TITLE_CHARS};

/// 已校验的原始操作（target 为 1-based 编号或 None）。无正文层。
pub(super) struct RawOp {
    pub(super) target: Option<i64>,
    pub(super) title: String,
    pub(super) summary: String,
    pub(super) tags: Vec<String>,
}

pub(super) struct ExtractOutcome {
    pub(super) ops: Vec<RawOp>,
    /// 输入合法且为空数组（非坏件）
    pub(super) empty_input_valid: bool,
    /// 本次提炼（含修复一轮）的 LLM 用量
    pub(super) usage: Option<crate::llm::TokenUsage>,
}

/// 解析并校验模型输出为操作清单。
pub(super) fn parse_ops(raw: &str, recent_count: usize) -> Result<ExtractOutcome, String> {
    let text = preprocess(raw);
    if text.is_empty() {
        return Err("输出为空".into());
    }
    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&text).map_err(|e| format!("不是合法 JSON 数组: {e}"))?;
    let mut ops = Vec::new();
    let mut hard_errors: Vec<String> = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        match validate_item(item, recent_count) {
            Ok(op) => ops.push(op),
            Err(ItemIssue::InvalidTarget) => {
                // 无效编号：降级为新建（保留字段）。字段本身非法则记坏件。
                match validate_fields(item) {
                    Some((title, summary, tags)) => ops.push(RawOp {
                        target: None,
                        title,
                        summary,
                        tags,
                    }),
                    None => hard_errors.push(format!("第 {} 条字段非法", i + 1)),
                }
            }
            Err(ItemIssue::BadFields(msg)) => hard_errors.push(format!("第 {} 条：{msg}", i + 1)),
        }
    }
    // 结构/必填严重错误 → 交给修复轮（整轮报废路径）
    if !hard_errors.is_empty() {
        return Err(hard_errors.join("；"));
    }
    Ok(ExtractOutcome {
        empty_input_valid: arr.is_empty(),
        ops,
        usage: None,
    })
}

enum ItemIssue {
    InvalidTarget,
    BadFields(String),
}

fn validate_item(v: &serde_json::Value, recent_count: usize) -> Result<RawOp, ItemIssue> {
    let target = match v.get("target") {
        None | Some(serde_json::Value::Null) => None,
        Some(t) => {
            let n = t.as_i64().ok_or(ItemIssue::InvalidTarget)?;
            if n < 1 || (n as usize) > recent_count {
                return Err(ItemIssue::InvalidTarget);
            }
            Some(n)
        }
    };
    let (title, summary, tags) = validate_fields(v)
        .ok_or_else(|| ItemIssue::BadFields("title/summary 缺失或超长".into()))?;
    Ok(RawOp {
        target,
        title,
        summary,
        tags,
    })
}

fn validate_fields(v: &serde_json::Value) -> Option<(String, String, Vec<String>)> {
    let title = v.get("title")?.as_str()?.trim().to_string();
    let summary = v.get("summary")?.as_str()?.trim().to_string();
    if title.is_empty() || summary.is_empty() {
        return None;
    }
    if title.chars().count() > MAX_TITLE_CHARS || summary.chars().count() > MAX_SUMMARY_CHARS {
        return None;
    }
    let tags = v
        .get("tags")
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some((title, summary, tags))
}

/// 预处理：剥代码围栏 / 思考块，抽取 JSON 数组主体。
pub(super) fn preprocess(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if let Some(start) = s.find("```") {
        let after = &s[start + 3..];
        let after = after
            .strip_prefix("json")
            .or_else(|| after.strip_prefix("JSON"))
            .unwrap_or(after);
        if let Some(end) = after.find("```") {
            s = after[..end].to_string();
        } else {
            s = after.to_string();
        }
    }
    // 去思考块（若模型误带）
    for (open, close) in [("<thinking>", "</thinking>")] {
        while let (Some(a), Some(b)) = (s.find(open), s.find(close)) {
            if b > a {
                s.replace_range(a..b + close.len(), "");
            } else {
                break;
            }
        }
    }
    if let (Some(a), Some(b)) = (s.find('['), s.rfind(']')) {
        if b > a {
            s = s[a..=b].to_string();
        }
    }
    s.trim().to_string()
}

/// 剥代码围栏（压缩输出用；保留正文）。
pub(super) fn strip_fences(raw: &str) -> String {
    let s = raw.trim();
    if let Some(start) = s.find("```") {
        let after = &s[start + 3..];
        let after = after
            .strip_prefix("markdown")
            .or_else(|| after.strip_prefix("md"))
            .unwrap_or(after);
        if let Some(end) = after.find("```") {
            return after[..end].trim().to_string();
        }
        return after.trim().to_string();
    }
    s.to_string()
}

/// 剥 `[prts_reminder]` 块（提炼材料里不携带环境噪声）。
fn strip_reminder(content: &str) -> String {
    const START: &str = "[prts_reminder]";
    const END: &str = "[/prts_reminder]";
    let Some(a) = content.find(START) else {
        return content.to_string();
    };
    let Some(b) = content[a..].find(END) else {
        return content[..a].to_string();
    };
    let rest = &content[a + b + END.len()..];
    format!("{}{}", &content[..a], rest.trim_start_matches(['\r', '\n']))
}

/// 把 jsonl 轮行渲染为可读增量文本。
///
/// 除 user/assistant 可见文本外，工具调用与结果也作为事实材料进入记忆提炼：
/// 只取工具名、参数和结果摘要，长内容截断，不搬运原始输出。
pub(super) fn render_delta(lines: &[String]) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in lines {
        let Ok(events) = serde_json::from_str::<Vec<serde_json::Value>>(line) else {
            continue;
        };
        for ev in &events {
            let ty = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match ty {
                "user/message" => {
                    let content = ev.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    let text = strip_reminder(content);
                    let text = text.trim();
                    if !text.is_empty() {
                        out.push(format!("用户：{text}"));
                    }
                }
                "assistant/message" => {
                    let content = ev.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    let text = content.trim();
                    if !text.is_empty() {
                        out.push(format!("助手：{text}"));
                    }
                }
                "tool/call" => {
                    let name = ev
                        .get("toolName")
                        .and_then(|v| v.as_str())
                        .unwrap_or("工具");
                    let params = ev.get("params").and_then(|v| v.as_str()).unwrap_or("");
                    out.push(format!("工具调用：{name} {}", compact_inline(params, 200)));
                }
                "tool/result" => {
                    let summary = ev.get("summary").and_then(|v| v.as_str()).unwrap_or("");
                    if summary.trim().is_empty() {
                        continue;
                    }
                    let name = ev
                        .get("toolName")
                        .and_then(|v| v.as_str())
                        .filter(|n| !n.is_empty())
                        .unwrap_or("工具");
                    out.push(format!("{name}结果：{}", compact_inline(summary, 240)));
                }
                _ => {}
            }
        }
    }
    out.join("\n\n")
}

/// 把多行文本压成单行，并截断到 max_chars（中文安全）。
fn compact_inline(text: &str, max_chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return "（空）".into();
    }
    if flat.chars().count() > max_chars {
        let s: String = flat.chars().take(max_chars).collect();
        format!("{s}…")
    } else {
        flat
    }
}
