//! memory::schema —— 记忆盘上结构（成长页 / 经历条目 / 水位状态）
//!
//! 三份结构对应盘上三类文件（`data_dir()/memory/`）：
//! - [`GrowthPage`]      → `profile.md` 与 `topics/*.md`（同构 Markdown + frontmatter）
//! - [`ExperienceEntry`] → `experience.jsonl` 每行（append-only；经历层，本模块只读）
//! - [`MemoryState`]     → `state.json`（水位 / 压缩游标 / 冷却）
//!
//! frontmatter 解析复用 `knowledge` 的手写 YAML 子集解析器（同仓库既有契约，
//! 支持标量 + 行内数组 + 块列表；LF/CRLF 兼容），不引入 yaml crate。
//!
//! 语义要点（对齐方案 §3）：
//! - 「关于博士」＝ `name` 为 [`PROFILE_NAME`] 的成长页，与专题页同构；
//! - 经历条目 `detail` 已移除（无正文层）：条目 = 标题 + 状态句(summary) + 标签；
//!   `revisionCount` 记重写次数（≥ [`REVISION_SEALED`] 即封存，不可再重写）；
//!   `absorbedIn` 记 longterm 版本号；
//!   时间戳为 unix 毫秒（前端 `Date.now()` 语义）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 记忆 schema 版本（state.json 顶层；升版走迁移，见方案 §3）。
pub const MEMORY_SCHEMA_VERSION: u32 = 1;

/// 「关于博士」页名（固定键；与专题页同构读写）。
pub const PROFILE_NAME: &str = "关于博士";

/// 封存线：`revisionCount` 达到此值即封存（不可再重写，只能另起新条目）。
pub const REVISION_SEALED: u32 = 2;

/// 成长页写入来源（集中定义，防拼写漂移）：
/// - [`SOURCE_TOOL`]：`memory_growth` 工具写入；
/// - [`SOURCE_USER`]：设置页命令（博士）写入；
/// - [`SOURCE_SYSTEM`]：初始化（如 [`GrowthPage::empty_profile`]）。
pub const SOURCE_TOOL: &str = "工具";
pub const SOURCE_USER: &str = "博士";
pub const SOURCE_SYSTEM: &str = "系统";

// ═══════════════════════════════════════════════════════
// 成长页
// ═══════════════════════════════════════════════════════

/// 成长页（`profile.md` 与 `topics/*.md` 同构）。
///
/// `name` 为唯一键（frontmatter `name`）；`brief` 进主题目录；`body` 为正文
/// （「关于博士」页正文进提示词，专题页正文按需工具展开）。
#[derive(Debug, Clone, PartialEq)]
pub struct GrowthPage {
    pub name: String,
    pub brief: String,
    pub tags: Vec<String>,
    pub source: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub body: String,
}

impl GrowthPage {
    /// 新建页（时间戳由调用方注入——装配层统一取 `now_ms()`）。
    pub fn new(
        name: impl Into<String>,
        brief: impl Into<String>,
        body: impl Into<String>,
        tags: Vec<String>,
        source: impl Into<String>,
        now: u64,
    ) -> Self {
        Self {
            name: name.into(),
            brief: brief.into(),
            tags,
            source: source.into(),
            created_at: now,
            updated_at: now,
            body: body.into(),
        }
    }

    /// 空的「关于博士」页（首次启动 / 删除后复位用）——来源＝「系统」（初始化）。
    pub fn empty_profile(now: u64) -> Self {
        Self::new(PROFILE_NAME, "", "", Vec::new(), SOURCE_SYSTEM, now)
    }

    /// 渲染为 Markdown 文本（frontmatter + 正文）。
    ///
    /// tags 用**块列表**（`tags:\n  - x`）而非行内数组：行内数组需转义
    /// 逗号/`]`（本项目手写 YAML 子集不处理转义），块列表天然无此问题。
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("---\n");
        out.push_str(&format!("name: {}\n", one_line(&self.name)));
        out.push_str(&format!("brief: {}\n", one_line(&self.brief)));
        if self.tags.is_empty() {
            out.push_str("tags: []\n");
        } else {
            out.push_str("tags:\n");
            for t in &self.tags {
                out.push_str(&format!("  - {}\n", one_line(t)));
            }
        }
        out.push_str(&format!("source: {}\n", one_line(&self.source)));
        out.push_str(&format!("createdAt: {}\n", self.created_at));
        out.push_str(&format!("updatedAt: {}\n", self.updated_at));
        out.push_str("---\n");
        out.push_str(&self.body);
        out.push('\n');
        out
    }

    /// 从 Markdown 文本解析；无 frontmatter / 无 `name` → None。
    pub fn parse(text: &str) -> Option<Self> {
        let fields = crate::knowledge::parse_frontmatter_fields(text)?;
        let mut name = String::new();
        let mut brief = String::new();
        let mut tags: Vec<String> = Vec::new();
        let mut source = String::new();
        let mut created_at: u64 = 0;
        let mut updated_at: u64 = 0;
        for (key, values) in fields {
            match key.as_str() {
                "name" => name = values.into_iter().next().unwrap_or_default(),
                "brief" => brief = values.into_iter().next().unwrap_or_default(),
                "tags" => {
                    tags = values
                        .into_iter()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                }
                "source" => source = values.into_iter().next().unwrap_or_default(),
                "createdAt" => created_at = values.first().and_then(|s| s.parse().ok()).unwrap_or(0),
                "updatedAt" => updated_at = values.first().and_then(|s| s.parse().ok()).unwrap_or(0),
                _ => {} // 未知字段忽略（向前兼容）
            }
        }
        if name.trim().is_empty() {
            return None;
        }
        // 尾随换行是 render 的格式产物，归一化以保证往返稳定
        let body = crate::knowledge::body_after_frontmatter(text)
            .trim_end_matches(['\n', '\r'])
            .to_string();
        Some(Self {
            name: name.trim().to_string(),
            brief,
            tags,
            source,
            created_at,
            updated_at,
            body,
        })
    }
}

/// frontmatter 单行标量净化：剥换行 + 首尾空白（防破坏行结构）。
fn one_line(s: &str) -> String {
    s.replace(['\r', '\n'], " ").trim().to_string()
}

// ═══════════════════════════════════════════════════════
// 经历条目
// ═══════════════════════════════════════════════════════

/// 经历条目（`experience.jsonl` 每行）。无正文层：`title` / `summary` / `tags`
/// 即全部内容（`summary` 为「状态句」）；`revisionCount` 记重写次数（重写＝覆盖，
/// 非追加）；`absorbedIn` 为 longterm 版本号（Some = 已蒸进长期）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExperienceEntry {
    pub id: String,
    pub ts: u64,
    pub conversation_id: String,
    #[serde(default)]
    pub line_from: String,
    #[serde(default)]
    pub turn_from: u64,
    #[serde(default)]
    pub turn_to: u64,
    /// "short" | "long"
    pub level: String,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// 重写次数（重写 +1；≥ [`REVISION_SEALED`] 即封存）
    #[serde(default)]
    pub revision_count: u32,
    #[serde(default)]
    pub absorbed_in: Option<u64>,
}

impl ExperienceEntry {
    /// 是否已被压缩蒸进长期（absorbed）。
    pub fn is_absorbed(&self) -> bool {
        self.absorbed_in.is_some()
    }

    /// 是否未吸收的短期条目（"近期"候选）。
    pub fn is_recent_candidate(&self) -> bool {
        !self.is_absorbed() && self.level == "short"
    }

    /// 重写资格：未吸收且未封存（`revisionCount` < [`REVISION_SEALED`]）。
    pub fn is_rewritable(&self) -> bool {
        !self.is_absorbed() && self.revision_count < REVISION_SEALED
    }
}

// ═══════════════════════════════════════════════════════
// 水位 / 压缩状态
// ═══════════════════════════════════════════════════════

/// 单对话水位（断点续跑锚点）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationWatermark {
    /// 已提炼到的轮数（对话 turns 下标语义；水位之后为增量）
    #[serde(default)]
    pub processed_turns: u64,
    #[serde(default)]
    pub last_run_at: u64,
    #[serde(default)]
    pub last_error: Option<String>,
    /// 冷却截止（unix 毫秒；整轮报废后的退避，None = 无冷却）
    #[serde(default)]
    pub cooldown_until: Option<u64>,
}

/// 压缩游标。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompressionState {
    /// 长期记忆版本号（每压缩一次 +1；经历条目 absorbedIn 引用之）
    #[serde(default)]
    pub longterm_version: u64,
    #[serde(default)]
    pub compressed_through_ts: u64,
    #[serde(default)]
    pub last_run_at: u64,
}

/// 计数口径（对齐方案 §6：近期计数＝工作线条数）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    /// 未吸收短期条目数（到 10 压 5 的计数）
    #[serde(default)]
    pub recent_lines: u64,
    #[serde(default)]
    pub long_chars: u64,
}

/// 记忆水位 / 压缩状态（`state.json`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryState {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub conversations: HashMap<String, ConversationWatermark>,
    #[serde(default)]
    pub compression: CompressionState,
    #[serde(default)]
    pub totals: Totals,
}

fn default_schema_version() -> u32 {
    MEMORY_SCHEMA_VERSION
}

impl Default for MemoryState {
    fn default() -> Self {
        Self {
            schema_version: MEMORY_SCHEMA_VERSION,
            conversations: HashMap::new(),
            compression: CompressionState::default(),
            totals: Totals::default(),
        }
    }
}

/// 当前 unix 毫秒（对齐前端 `Date.now()` 语义）。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growth_page_render_parse_roundtrip() {
        let page = GrowthPage::new(
            "伤害计算",
            "暴击与减伤的关系",
            "## 正文\n内容……",
            vec!["数值".into(), "机制".into()],
            "提炼",
            1000,
        );
        let text = page.render();
        let back = GrowthPage::parse(&text).unwrap();
        assert_eq!(back.name, "伤害计算");
        assert_eq!(back.brief, "暴击与减伤的关系");
        assert_eq!(back.tags, vec!["数值".to_string(), "机制".to_string()]);
        assert_eq!(back.source, "提炼");
        assert_eq!(back.created_at, 1000);
        assert_eq!(back.updated_at, 1000);
        assert!(back.body.contains("## 正文"));
        assert_eq!(back, page);
    }

    #[test]
    fn growth_page_parse_requires_name() {
        assert!(GrowthPage::parse("无 frontmatter 的正文").is_none());
        assert!(GrowthPage::parse("---\nbrief: x\n---\nbody").is_none());
    }

    #[test]
    fn growth_page_render_sanitizes_newlines_in_scalars() {
        let page = GrowthPage::new("名\n字", "简\n述", "正文", Vec::new(), "源", 1);
        let text = page.render();
        // 标量行不含裸换行（否则破坏 frontmatter）
        assert!(text.contains("name: 名 字"));
        assert!(text.contains("brief: 简 述"));
        let back = GrowthPage::parse(&text).unwrap();
        assert_eq!(back.name, "名 字");
    }

    #[test]
    fn growth_page_empty_tags_render_parse() {
        let page = GrowthPage::new("标题", "简述", "正文", Vec::new(), "手动", 5);
        let text = page.render();
        assert!(text.contains("tags: []"), "空 tags 用行内空数组");
        let back = GrowthPage::parse(&text).unwrap();
        assert!(back.tags.is_empty());
    }

    #[test]
    fn growth_page_tags_with_delimiters_roundtrip() {
        // 含逗号 / 方括号 / 冒号的标签：块列表不应破坏往返（旧行内数组会炸）
        let tags = vec!["a,b".to_string(), "c]d".to_string(), "e:f".to_string()];
        let page = GrowthPage::new("标签页", "简述", "正文", tags.clone(), "手动", 5);
        let text = page.render();
        assert!(text.contains("tags:\n  - a,b"), "块列表形态");
        let back = GrowthPage::parse(&text).unwrap();
        assert_eq!(back.tags, tags);
    }

    #[test]
    fn experience_entry_jsonl_camel_case_roundtrip() {
        let e = ExperienceEntry {
            id: "exp_1".into(),
            ts: 1726000000000,
            conversation_id: "c1".into(),
            line_from: "伤害计算".into(),
            turn_from: 3,
            turn_to: 8,
            level: "short".into(),
            title: "伤害公式讨论".into(),
            summary: "确认了暴击与减伤的相乘关系".into(),
            tags: vec!["数值".into()],
            revision_count: 1,
            absorbed_in: None,
        };
        let line = serde_json::to_string(&e).unwrap();
        assert!(line.contains("\"conversationId\":\"c1\""));
        assert!(line.contains("\"turnFrom\":3"));
        assert!(line.contains("\"revisionCount\":1"));
        assert!(!line.contains("detail"), "无正文层：不落 detail 字段");
        let back: ExperienceEntry = serde_json::from_str(&line).unwrap();
        assert_eq!(back, e);
        assert!(back.is_recent_candidate());
        assert!(back.is_rewritable(), "revisionCount=1 仍可重写");
    }

    #[test]
    fn experience_entry_tolerates_legacy_detail_and_missing_revision() {
        // 旧行带 detail（忽略）、缺 revisionCount（默认 0）
        let json = r#"{"id":"e","ts":1,"conversationId":"c","level":"long","title":"t","summary":"s","detail":"旧正文"}"#;
        let e: ExperienceEntry = serde_json::from_str(json).unwrap();
        assert!(e.absorbed_in.is_none());
        assert_eq!(e.revision_count, 0);
        assert!(!e.is_recent_candidate(), "long 不是近期候选");
    }

    #[test]
    fn rewrite_eligibility_seals_at_limit() {
        let mut e = ExperienceEntry {
            id: "e".into(),
            ts: 1,
            conversation_id: "c".into(),
            line_from: String::new(),
            turn_from: 0,
            turn_to: 1,
            level: "short".into(),
            title: "t".into(),
            summary: "s".into(),
            tags: vec![],
            revision_count: REVISION_SEALED,
            absorbed_in: None,
        };
        assert!(!e.is_rewritable(), "达封存线不可重写");
        e.revision_count = 0;
        assert!(e.is_rewritable());
        e.absorbed_in = Some(1);
        assert!(!e.is_rewritable(), "已吸收不可重写");
    }

    #[test]
    fn memory_state_roundtrip_and_defaults() {
        let mut st = MemoryState::default();
        st.conversations.insert(
            "c1".into(),
            ConversationWatermark {
                processed_turns: 8,
                last_run_at: 100,
                last_error: Some("坏件".into()),
                cooldown_until: Some(200),
            },
        );
        st.totals.recent_lines = 12;
        let text = serde_json::to_string(&st).unwrap();
        assert!(text.contains("\"processedTurns\":8"));
        assert!(text.contains("\"recentLines\":12"));
        let back: MemoryState = serde_json::from_str(&text).unwrap();
        assert_eq!(back, st);

        // 空文件 → 版本兜底
        let empty: MemoryState = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.schema_version, MEMORY_SCHEMA_VERSION);
    }
}
