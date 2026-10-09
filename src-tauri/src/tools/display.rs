//! tools::display —— 工具调用的展示切片（声明式）
//!
//! 目的：让工具卡片在**调用进行中即**展示「在做什么、对什么做」，涉及文件时
//! 给出可点击链接。工具（`Tool::display`）声明"本次调用要展示给用户的切片"，
//! 由中枢（orchestrator）在发出 `tool/call` 事件前调用解析，解析结果随事件
//! 透出前端——路径解析（相对→绝对、逻辑名→实际文件）收口在后端，前端不拼路径。
//!
//! 与 `ToolOutput` 的区别：`ToolOutput` 是**执行后**的结果（canonical + 渲染
//! 文本），本模块是**执行前**（仅有参数）即可导出的展示切片。二者互不影响：
//! 展示切片挂在 tool/call 侧，结果失败时链接/信息仍保留（设计裁定）。
//!
//! 失败容忍：`display` 是纯展示路径，解析失败（参数缺失/文件不存在）一律降级
//! 为空切片或尽力而为的路径拼接，绝不因展示而阻断工具调用本身。

use serde::Serialize;

use crate::path_guard;
use crate::policy::Policy;

/// 展示文本的粗上限（字符）：超长截断，避免事件载荷膨胀。
/// 前端 EventBadge 另有更短的视觉截断（纯长度约束），此处仅兜底。
pub(crate) const MAX_DISPLAY_TEXT_CHARS: usize = 200;

// ═══════════════════════════════════════════════════════
// 展示切片
// ═══════════════════════════════════════════════════════

/// 一次工具调用的展示切片：文本信息 +（可选）链接 +（可选）完整原文。
///
/// 序列化时字段为空即省略（`skip_serializing_if`），旧归档无此字段 →
/// 前端按缺省处理（不附加任何信息）。
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDisplay {
    /// 文本信息（对象名、筛选摘要、命令等）；空 = 不展示
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// 链接（文件可点击打开 / URL 浏览器打开）；None = 无链接
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<DisplayLink>,
    /// 完整原文（展示被截断时给出，供前端悬停显示）。截断只是视觉约束，用户始终
    /// 要有看到全文的渠道；仅在确实被截断时才附带（见 info_with_full）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full: Option<String>,
}

impl ToolDisplay {
    /// 空切片判定（serde `skip_serializing_if` 消费）。
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.link.is_none() && self.full.is_none()
    }

    /// 仅文本信息（超长截断）。
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: truncate_chars(&text.into(), MAX_DISPLAY_TEXT_CHARS),
            link: None,
            full: None,
        }
    }

    /// 文本信息（展示仍截断）+ 完整原文（仅当展示被截断时附带，供悬停显示全文）。
    pub fn info_with_full(text: impl Into<String>, full: impl Into<String>) -> Self {
        let full: String = full.into();
        let truncated = truncate_chars(&text.into(), MAX_DISPLAY_TEXT_CHARS);
        let full = if full.chars().count() > MAX_DISPLAY_TEXT_CHARS {
            Some(full)
        } else {
            None
        };
        Self {
            text: truncated,
            link: None,
            full,
        }
    }

    /// 仅链接。
    pub fn link(link: DisplayLink) -> Self {
        Self {
            text: String::new(),
            link: Some(link),
            full: None,
        }
    }
}

/// 展示链接：文件引用（open_path 打开）或外部 URL（系统浏览器打开）。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayLink {
    /// 链接类型：file / url
    pub kind: LinkKind,
    /// file = 绝对路径；url = 完整 URL
    pub target: String,
    /// 显示标签（文件名 / URL）
    pub label: String,
}

impl DisplayLink {
    /// 文件链接（target 为绝对路径）。
    pub fn file(target: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            kind: LinkKind::File,
            target: target.into(),
            label: label.into(),
        }
    }

    /// URL 链接（label 取 URL 自身）。
    pub fn url(url: impl Into<String>) -> Self {
        let url = url.into();
        Self {
            kind: LinkKind::Url,
            label: url.clone(),
            target: url,
        }
    }
}

/// 链接类型判别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    File,
    Url,
}

// ═══════════════════════════════════════════════════════
// 共享辅助
// ═══════════════════════════════════════════════════════

/// 按字符数截断（保留前 max 个字符，超出追加省略号）。
pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}…")
}

/// 从 params JSON 取字符串数组（键缺失/非数组/元素非字符串 → 空 Vec）。
pub(crate) fn string_array(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// 查询目标摘要：names 优先，其次 ids（用「、」连接）；都空 → None。
pub(crate) fn targets_summary(v: &serde_json::Value) -> Option<String> {
    let names = string_array(v, "names");
    let ids = string_array(v, "ids");
    let list = if !names.is_empty() { names } else { ids };
    if list.is_empty() {
        None
    } else {
        Some(list.join("、"))
    }
}

/// `extra` / `sections` 等字符串数组的摘要（用「、」连接）；空 → None。
pub(crate) fn array_summary(v: &serde_json::Value, key: &str) -> Option<String> {
    let list = string_array(v, key);
    if list.is_empty() {
        None
    } else {
        Some(list.join("、"))
    }
}

/// 路径末段（文件名）作为链接标签；无法取末段时原样返回。
pub(crate) fn file_name_label(raw: &str) -> String {
    std::path::Path::new(raw)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| raw.to_string())
}

/// 文本型文件引用的展示链接解析（read_file / write_file / edit 共用）。
///
/// 相对路径基于工作区拼接 + canonicalize（沿用 path_guard 既有入口）；
/// 目标可能尚不存在（write_file 新建）时退 `resolve_write_create`；
/// 仍失败——纯粹为展示——退"相对路径 join 工作区"的尽力而为路径（不
/// canonicalize），保证链接存在、点击行为交给 open_path 校验。
pub(crate) fn file_link(policy: &Policy, raw: &str) -> DisplayLink {
    let resolved = path_guard::resolve_read(policy, raw)
        .or_else(|_| path_guard::resolve_write_create(policy, raw))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            let p = std::path::Path::new(raw);
            if p.is_absolute() {
                raw.to_string()
            } else {
                policy.workspace_root.join(raw).to_string_lossy().into_owned()
            }
        });
    DisplayLink::file(resolved, file_name_label(raw))
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_display_serializes_to_nothing() {
        // 空切片：字段全省略（旧数据兼容口径）
        let json = serde_json::to_value(ToolDisplay::default()).unwrap();
        assert_eq!(json, serde_json::json!({}));
        assert!(ToolDisplay::default().is_empty());
    }

    #[test]
    fn info_only_serializes_text() {
        let d = ToolDisplay::info("源石虫");
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json, serde_json::json!({ "text": "源石虫" }));
        assert!(!d.is_empty());
    }

    #[test]
    fn info_truncates_over_limit() {
        let long = "甲".repeat(MAX_DISPLAY_TEXT_CHARS + 50);
        let d = ToolDisplay::info(long);
        // 前 MAX 字符 + 省略号
        assert_eq!(d.text.chars().count(), MAX_DISPLAY_TEXT_CHARS + 1);
        assert!(d.text.ends_with('…'));
    }

    #[test]
    fn link_serializes_kind_lowercase() {
        let d = ToolDisplay::link(DisplayLink::file("C:/tmp/a.md", "a.md"));
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["link"]["kind"], "file");
        assert_eq!(json["link"]["target"], "C:/tmp/a.md");
        assert_eq!(json["link"]["label"], "a.md");
        assert!(json.get("text").is_none());
    }

    #[test]
    fn url_link_label_is_url() {
        let d = DisplayLink::url("https://example.com/x");
        assert_eq!(d.kind, LinkKind::Url);
        assert_eq!(d.label, "https://example.com/x");
        assert_eq!(d.target, "https://example.com/x");
    }

    #[test]
    fn truncate_chars_is_char_safe() {
        assert_eq!(truncate_chars("abcdef", 3), "abc…");
        assert_eq!(truncate_chars("abc", 3), "abc");
        // 多字节字符不 panic、不切碎
        assert_eq!(truncate_chars("汉字测试", 2), "汉字…");
    }

    #[test]
    fn targets_and_array_summary() {
        let v = serde_json::json!({ "names": ["源石虫", "碎骨"], "ids": ["x"] });
        assert_eq!(targets_summary(&v).as_deref(), Some("源石虫、碎骨"));
        let v2 = serde_json::json!({ "ids": ["a", "b"], "extra": ["级别=1", "天赋文本"] });
        assert_eq!(targets_summary(&v2).as_deref(), Some("a、b"));
        assert_eq!(
            array_summary(&v2, "extra").as_deref(),
            Some("级别=1、天赋文本")
        );
        assert_eq!(targets_summary(&serde_json::json!({})), None);
    }

    #[test]
    fn file_name_label_extracts_last_segment() {
        assert_eq!(file_name_label("notes/a.md"), "a.md");
        assert_eq!(file_name_label("a.md"), "a.md");
    }

    #[test]
    fn info_with_full_only_when_truncated() {
        let short = ToolDisplay::info_with_full("echo hi", "echo hi");
        assert_eq!(short.text, "echo hi");
        assert!(short.full.is_none(), "未截断不必附带原文");

        let long = "甲".repeat(MAX_DISPLAY_TEXT_CHARS + 10);
        let d = ToolDisplay::info_with_full(long.clone(), long.clone());
        assert!(d.text.ends_with('…'));
        assert_eq!(d.full.as_deref(), Some(long.as_str()), "截断了就要给看全文的渠道");
    }

    #[test]
    fn file_link_relative_joins_workspace_when_missing() {
        // 不存在的相对路径：尽力而为 join 工作区（不 panic、链接仍在）
        let ws = std::env::temp_dir().join("prts-display-test-ws");
        let policy = Policy::new(ws.clone());
        let link = file_link(&policy, "sub/missing.md");
        assert_eq!(link.kind, LinkKind::File);
        assert_eq!(link.label, "missing.md");
        assert!(link.target.contains("missing.md"));
    }
}
