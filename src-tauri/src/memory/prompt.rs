//! memory::prompt —— 记忆区文本组装（冻结快照的唯一来源）
//!
//! 四节来源（装配**不产生新内容**，纯读 + 格式，对齐方案 §5）：
//! - `### 关于博士` ← 成长页「关于博士」正文；
//! - `### 近期`     ← 经历未吸收短期条目（最近 [`RECENT_IN_PROMPT`] 条，
//!   **行原文＝日期＋标题＋摘要**，装配不二次改写）；
//! - `### 先前`     ← `longterm.md` 摘要原文；
//! - `### 主题`     ← 成长专题页 `name：brief` 索引（正文按需工具展开）。
//!
//! 空节省略；全空 → 空串（调用方不注入，保持无记忆态与旧行为逐字节一致）。
//! 输出必须确定性可复现（同状态同一字符串）：专题页已按名排序、经历按时间排序。

use crate::memory::schema::PROFILE_NAME;
use crate::memory::store::MemoryStore;

/// 「近期」入提示词的最大条数（K 默认 5）。
pub const RECENT_IN_PROMPT: usize = 5;

/// 组装记忆区文本。全空返回空串。
pub fn assemble_memory_block(store: &MemoryStore) -> String {
    let mut sections: Vec<String> = Vec::new();

    // ── 关于博士 ──
    let profile_body = store.profile().body.trim();
    if !profile_body.is_empty() {
        sections.push(format!("### {PROFILE_NAME}\n{profile_body}"));
    }

    // ── 近期 ──
    let recent = recent_items(store);
    if !recent.is_empty() {
        let lines: Vec<String> = recent
            .iter()
            .map(|(date, title, summary)| format!("- {date} {title}：{summary}"))
            .collect();
        sections.push(format!("### 近期\n{}", lines.join("\n")));
    }

    // ── 先前 ──
    let long = store.longterm().trim();
    if !long.is_empty() {
        sections.push(format!("### 先前\n{long}"));
    }

    // ── 主题 ──
    if !store.topics().is_empty() {
        let lines: Vec<String> = store
            .topics()
            .iter()
            .map(|p| format!("- {}：{}", p.name, p.brief.trim()))
            .collect();
        sections.push(format!("### 主题\n{}", lines.join("\n")));
    }

    if sections.is_empty() {
        return String::new();
    }
    format!("## 记忆\n\n{}", sections.join("\n\n"))
}

/// 「近期」条目（最近 [`RECENT_IN_PROMPT`] 条未吸收短期）：(本地日期, 标题, 摘要)。
/// 提示词组装与设置页「当前注入」共用同一来源（所见即所注入）。
pub(crate) fn recent_items(store: &MemoryStore) -> Vec<(String, String, String)> {
    store
        .recent_unabsorbed(RECENT_IN_PROMPT)
        .iter()
        .map(|e| {
            (
                ymd_local(e.ts),
                e.title.trim().to_string(),
                e.summary.trim().to_string(),
            )
        })
        .collect()
}

/// unix 毫秒 → **本地时区**日期 `YYYY-MM-DD`（提示词行 / 工具输出 / 提醒共用）。
/// 复用仓库既有 chrono 依赖（`tools/time.rs` 同源），不自写日历算法。
pub(crate) fn ymd_local(ts_ms: u64) -> String {
    fmt_local(ts_ms, "%Y-%m-%d")
}

/// unix 毫秒 → **本地时区**日期＋时间 `YYYY-MM-DD HH:MM:SS`。
/// 成长页 read 元数据行用（秒级粒度，同分钟内 create/update 的变化可辨）。
pub(crate) fn ymd_hms_local(ts_ms: u64) -> String {
    fmt_local(ts_ms, "%Y-%m-%d %H:%M:%S")
}

/// 本地时区格式化公共实现（按 `fmt` 输出；越界时间戳 → 空串）。
fn fmt_local(ts_ms: u64, fmt: &str) -> String {
    use chrono::{DateTime, Local, Utc};
    match DateTime::<Utc>::from_timestamp_millis(ts_ms as i64) {
        Some(dt) => dt.with_timezone(&Local).format(fmt).to_string(),
        None => String::new(),
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::schema::ExperienceEntry;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-mem-prompt-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empty_store_yields_empty_block() {
        let dir = temp_dir("empty");
        let s = MemoryStore::load(&dir).unwrap();
        assert_eq!(assemble_memory_block(&s), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn four_sections_present_and_deterministic() {
        let dir = temp_dir("four");
        let mut s = MemoryStore::load(&dir).unwrap();
        s.growth_update(PROFILE_NAME, Some("博士偏好简洁回答"), None, None)
            .unwrap();
        s.growth_create("伤害计算", "暴击与减伤", "正文", vec![], "手动").unwrap();
        s.growth_create("索敌", "仇恨规则", "正文", vec![], "手动").unwrap();
        // 近期条目：直接注入经历（批 1 只读路径，测试经内部构造）
        s.inject_experience_for_test(ExperienceEntry {
            id: "e1".into(),
            ts: 1_700_000_000_000,
            conversation_id: "c1".into(),
            line_from: String::new(),
            turn_from: 0,
            turn_to: 1,
            level: "short".into(),
            title: "伤害公式".into(),
            summary: "确认暴击与减伤相乘".into(),
            tags: vec![],
            revision_count: 0,
            absorbed_in: None,
        });

        let block = assemble_memory_block(&s);
        assert!(block.starts_with("## 记忆"));
        assert!(block.contains("### 关于博士\n博士偏好简洁回答"));
        // 日期为本地时区（时区敏感：由 ymd_local 计算期望，不写死日历值）
        assert!(block.contains(&format!(
            "### 近期\n- {} 伤害公式：确认暴击与减伤相乘",
            ymd_local(1_700_000_000_000)
        )));
        assert!(block.contains("### 主题\n- 伤害计算：暴击与减伤\n- 索敌：仇恨规则"));
        // 确定性：同状态两次组装逐字节相等
        assert_eq!(block, assemble_memory_block(&s));
        // 各节顺序：关于博士 → 近期 → 主题（无先前）
        let i1 = block.find("关于博士").unwrap();
        let i2 = block.find("近期").unwrap();
        let i3 = block.find("主题").unwrap();
        assert!(i1 < i2 && i2 < i3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ymd_local_and_hms_are_local_and_well_formed() {
        // 时区敏感：断言与 chrono Local 一致 + 形状，不写死日历值
        use chrono::{DateTime, Local, Utc};
        let ts = 1_700_000_000_000u64;
        let expect_date = DateTime::<Utc>::from_timestamp_millis(ts as i64)
            .unwrap()
            .with_timezone(&Local)
            .format("%Y-%m-%d")
            .to_string();
        assert_eq!(ymd_local(ts), expect_date);
        let hms = ymd_hms_local(ts);
        assert_eq!(hms.len(), "YYYY-MM-DD HH:MM:SS".len());
        assert!(hms.starts_with(&expect_date), "hms={hms}");
        // 位置：10=空格, 13=:, 16=:
        assert_eq!(hms.as_bytes()[10], b' ');
        assert_eq!(hms.as_bytes()[13], b':');
        assert_eq!(hms.as_bytes()[16], b':');
        assert_eq!(ymd_local(0).len(), 10, "纯日期长度稳定");
    }
}
