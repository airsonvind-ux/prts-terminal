//! query_rogue —— 集成战略条目查询工具
//!
//! 语义：输入肉鸽主题 + 名称或 ID → 返回该条目的全部有值字段。
//! 跨六表独立查询，由 `theme` 参数指定目标表。
//! 与 search_entities 互为逆操作：
//!   query_rogue：身份 → 属性（名称查询）
//!   search_entities：条件 → 身份列表（集合筛选）
//!
//! ## 与已建查询工具的差异
//!
//! 1. **必填 theme 参数**：用户几乎不跨主题查询，先选主题再查
//! 2. **无 fields/extra**：输出全部有值字段，不做条件控制
//! 3. **buff JSON 格式化**：buff 列做 pretty-print，提升可读性
//! 4. **输出策略**：按字段逐行输出，空值列跳过
//!
//! ## 主题映射
//!
//! 接受中文全称/简称/编号/英文名：
//!   傀影/傀影与猩红孤钻/rogue1/1   → rogue_1_entries
//!   水月/水月与深蓝之树/rogue2/2   → rogue_2_entries
//!   萨米/探索者的银凇止境/rogue3/3  → rogue_3_entries
//!   萨卡兹/萨卡兹的无终奇语/rogue4/4 → rogue_4_entries
//!   界园/rogue5/5                 → rogue_5_entries
//!   黑流树海/rogue6/6             → rogue_6_entries

use crate::db::{Database, Filter, FilterOp};
use crate::tools::common::{self, Target};
use crate::tools::display::targets_summary;
use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

use serde_json::Value;
use std::collections::HashMap;

pub struct QueryRogue;

// ═══════════════════════════════════════════════════════
// 主题 → 表名映射
// ═══════════════════════════════════════════════════════

/// 主题标识符 → 表名 + 主题显示名。
///
/// `aliases` 是这一个主题的全部叫法（全称/简称/英文/序号）。
/// 精确匹配与模糊包含匹配都只读这一份，新增主题只需在 THEMES 里加一条。
#[derive(Debug)]
struct ThemeInfo {
    table: &'static str,
    display: &'static str,
    aliases: &'static [&'static str],
}

/// 所有可用主题。display 用于输出中的主题标注。
const THEMES: &[ThemeInfo] = &[
    ThemeInfo {
        table: "rogue_1_entries",
        display: "傀影与猩红孤钻",
        aliases: &["1", "rogue1", "rogue_1", "傀影", "傀影与猩红孤钻"],
    },
    ThemeInfo {
        table: "rogue_2_entries",
        display: "水月与深蓝之树",
        aliases: &["2", "rogue2", "rogue_2", "水月", "水月与深蓝之树"],
    },
    ThemeInfo {
        table: "rogue_3_entries",
        display: "探索者的银凇止境",
        aliases: &[
            "3",
            "rogue3",
            "rogue_3",
            "萨米",
            "银凇",
            "凇止境",
            "探索者的银凇止境",
        ],
    },
    ThemeInfo {
        table: "rogue_4_entries",
        display: "萨卡兹的无终奇语",
        aliases: &["4", "rogue4", "rogue_4", "萨卡兹", "萨卡兹的无终奇语"],
    },
    ThemeInfo {
        table: "rogue_5_entries",
        display: "岁的界园志异",
        aliases: &["5", "rogue5", "rogue_5", "界园", "岁的界园志异"],
    },
    ThemeInfo {
        table: "rogue_6_entries",
        display: "沉沦者的黑流树海",
        aliases: &[
            "6",
            "rogue6",
            "rogue_6",
            "黑流",
            "黑流树海",
            "沉沦者的黑流树海",
        ],
    },
];

/// 解析用户输入的 theme 参数，返回对应的 ThemeInfo。
///
/// 接受：中文全称、简称（傀影/水月/萨米/萨卡兹/界园/黑流树海）、
/// 英文标识符（rogue1/rogue_1/1 等）。
fn resolve_theme(input: &str) -> Result<&'static ThemeInfo, String> {
    let s = input.trim().to_lowercase();

    // 精确匹配：主题的全部叫法都在 ThemeInfo.aliases 里
    for t in THEMES {
        if t.aliases.iter().any(|a| s == *a) {
            return Ok(t);
        }
    }

    // 模糊匹配（处理用户加了空格/带了后缀等情况）—— 同一份别名做包含匹配，
    // 单字别名（序号）不参与，需唯一命中，多命中报错。
    let mut fuzzy_hits: Vec<&ThemeInfo> = Vec::new();
    for t in THEMES {
        if t.aliases
            .iter()
            .any(|a| a.chars().count() >= 2 && s.contains(*a))
        {
            fuzzy_hits.push(t);
        }
    }

    match fuzzy_hits.len() {
        1 => return Ok(fuzzy_hits[0]),
        n if n > 1 => {
            let names: Vec<&str> = fuzzy_hits.iter().map(|t| t.display).collect();
            return Err(format!(
                "主题不明确 \"{}\"，同时匹配到多个主题：{}。请精确指定",
                input,
                names.join("、")
            ));
        }
        _ => {}
    }

    let available: Vec<String> = THEMES
        .iter()
        .map(|t| format!("{}（{}）", t.display, t.table))
        .collect();
    Err(format!(
        "无法识别肉鸽主题 \"{}\"。可用主题：{}",
        input,
        available.join("、")
    ))
}

// ═══════════════════════════════════════════════════════
// Tool trait 实现
// ═══════════════════════════════════════════════════════

impl Tool for QueryRogue {
    fn name(&self) -> &str {
        "query_rogue"
    }

    fn description(&self) -> &str {
        "按名称或 ID 查询集成战略（肉鸽）条目，需指定主题。ids 主键精确 / names 精确优先、未中 LIKE 模糊取最佳（二选一）。精确命中多条同名记录全部返回；命中基础版本自动附带 -α/-β/-γ 变体。返回全部有值字段"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "theme": {
                    "type": "string",
                    "description": "肉鸽主题：简称（傀影/水月/萨米/萨卡兹/界园/黑流树海/黑流）、序号 1~6、rogue1~6 或完整主题名"
                },
                "ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "条目 ID 列表，如 [“rogue_1_relic_1”]。与 names 二选一"
                },
                "names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "条目名称列表，如 [“热水壶”]。与 ids 二选一"
                }
            },
            "required": ["theme"]
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(params) else {
            return ToolDisplay::default();
        };
        let raw_theme = v.get("theme").and_then(|x| x.as_str()).unwrap_or("");
        // 主题枚举 → 中文全称（如 “5”/“rogue5”/“界园” → “岁的界园志异”）
        let theme = if raw_theme.trim().is_empty() {
            ""
        } else {
            resolve_theme(raw_theme)
                .map(|t| t.display)
                .unwrap_or(raw_theme)
        };
        let name = targets_summary(&v);
        match (theme.is_empty(), name) {
            (true, None) => ToolDisplay::default(),
            (false, None) => ToolDisplay::info(theme),
            (true, Some(n)) => ToolDisplay::info(n),
            (false, Some(n)) => ToolDisplay::info(format!("{theme} · {n}")),
        }
    }

    fn execute(&self, ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;

        // ── 1. 解析 theme ──
        let theme_str = v["theme"]
            .as_str()
            .ok_or_else(|| ToolError::Parse("\"theme\" 参数必填，请指定肉鸽主题".into()))?;
        let theme = resolve_theme(theme_str).map_err(ToolError::Execution)?;

        // ── 2. 解析查询目标 ──
        let targets = common::parse_targets(&v)?;

        // ── 3. 获取数据库 ──
        let db = ctx
            .db
            .as_ref()
            .ok_or_else(|| ToolError::Execution("数据库未连接".into()))?;

        // ── 4. 逐目标查询 ──
        let mut output = String::new();
        let mut all_dropped: Vec<String> = Vec::new();
        // canonical 收集：原始行数据（presentation 之外的结构化值）
        let mut all_rows: Vec<HashMap<String, String>> = Vec::new();

        for (i, target) in targets.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }

            match target {
                Target::Id(id) => {
                    let (row, dropped) = query_by_id(db, theme.table, id)?;
                    collect_dropped(&mut all_dropped, &dropped);
                    match row {
                        Some(row) => {
                            all_rows.push(row.clone());
                            format_one_entry(&row, theme.display, &mut output);
                        }
                        None => {
                            output.push_str(&format!(
                                "✗ ID: {id}：未在 {theme_name} 中找到\n",
                                theme_name = theme.display
                            ));
                        }
                    }
                }
                Target::Name(name) => {
                    let (rows, dropped) = query_by_name_with_variants(db, theme.table, name)?;
                    collect_dropped(&mut all_dropped, &dropped);
                    if rows.is_empty() {
                        output.push_str(&format!(
                            "✗ 名称: {name}：未在 {theme_name} 中找到\n",
                            theme_name = theme.display
                        ));
                    } else {
                        all_rows.extend(rows.iter().cloned());
                        for (j, row) in rows.iter().enumerate() {
                            if j > 0 {
                                output.push('\n');
                            }
                            format_one_entry(row, theme.display, &mut output);
                        }
                    }
                }
            }
        }

        if !all_dropped.is_empty() {
            // collect_dropped 已保证不重复，此处仅排序保证输出顺序稳定
            all_dropped.sort();
            output.push_str(&format!(
                "\n⚠ 以下请求字段不存在，已忽略：{}",
                all_dropped.join("、")
            ));
        }

        Ok(ToolOutput::json(
            serde_json::json!({ "rows": all_rows, "dropped_fields": all_dropped }),
            output.trim_end(),
        ))
    }
}

// ═══════════════════════════════════════════════════════
// 数据库查询
// ═══════════════════════════════════════════════════════

/// 所有需要查询的列（六表通用 schema）。
const ALL_COLS: &[&str] = &[
    "id",
    "名称",
    "效果",
    "类型",
    "稀有度",
    "buff",
    "备注",
    "特性",
];

/// 收集 dropped_fields，去重。
fn collect_dropped(acc: &mut Vec<String>, incoming: &[String]) {
    for f in incoming {
        if !acc.contains(f) {
            acc.push(f.clone());
        }
    }
}

/// 按 ID 精确查询，返回 (行, dropped_fields)。
#[allow(clippy::type_complexity)] // 单条/多条查询共用行映射与丢弃字段的双返回结构
fn query_by_id(
    db: &Database,
    table: &str,
    id: &str,
) -> Result<(Option<HashMap<String, String>>, Vec<String>), ToolError> {
    let fields: Vec<String> = ALL_COLS.iter().map(|c| c.to_string()).collect();
    let result = db
        .query_filtered(table, &fields, &[Filter::new("id", FilterOp::Eq, id)], 1)
        .map_err(ToolError::Execution)?;
    let row = result.rows.into_iter().next();
    Ok((row, result.dropped_fields))
}

/// 按名称查询，精确优先 → LIKE 降级。返回 (结果行列表, dropped_fields)。
///
/// 精确命中多条同名记录时全部返回（年代追忆分时期、岁时分形态等，
/// 类型名不同者以此区分；类型名相同者仅靠效果/buff 文本区分）。
/// 若命中基础版本（名称不以 -α/-β/-γ 结尾），自动拉取同一主题下的
/// 多元奇物变体（-α、-β、-γ），按 基础→α→β→γ 顺序返回。
#[allow(clippy::type_complexity)] // 单条/多条查询共用行映射与丢弃字段的双返回结构
fn query_by_name_with_variants(
    db: &Database,
    table: &str,
    name: &str,
) -> Result<(Vec<HashMap<String, String>>, Vec<String>), ToolError> {
    let fields: Vec<String> = ALL_COLS.iter().map(|c| c.to_string()).collect();

    let mut all_dropped: Vec<String> = Vec::new();

    // 精确匹配（同名最多 4 条 + 变体 3 条，20 留有充足余量）
    let exact_result = db
        .query_filtered(
            table,
            &fields,
            &[Filter::new("名称", FilterOp::Eq, name)],
            20,
        )
        .map_err(ToolError::Execution)?;
    for d in &exact_result.dropped_fields {
        if !all_dropped.contains(d) {
            all_dropped.push(d.clone());
        }
    }

    if !exact_result.rows.is_empty() {
        // 同名多条记录全部返回，逐条展开多元奇物变体
        let mut results: Vec<HashMap<String, String>> = Vec::new();
        for row in exact_result.rows {
            append_with_variants(db, table, &fields, row, &mut results, &mut all_dropped)?;
        }
        return Ok((results, all_dropped));
    }

    // LIKE 降级：取第一条。无 ORDER BY，按 rowid（数据管线插入顺序）返回，
    // 依赖"基础版先于变体插入"，query_same_name_* 与 like_fallback_* 测试守护该假设
    let like_result = db
        .query_filtered(
            table,
            &fields,
            &[Filter::new("名称", FilterOp::Like, name)],
            5,
        )
        .map_err(ToolError::Execution)?;
    for d in &like_result.dropped_fields {
        if !all_dropped.contains(d) {
            all_dropped.push(d.clone());
        }
    }

    let Some(primary) = like_result.rows.into_iter().next() else {
        return Ok((vec![], all_dropped));
    };

    let mut results = Vec::new();
    append_with_variants(db, table, &fields, primary, &mut results, &mut all_dropped)?;
    Ok((results, all_dropped))
}

/// 追加一行及其多元奇物变体（-α/-β/-γ）。命中本身是变体时不展开。
fn append_with_variants(
    db: &Database,
    table: &str,
    fields: &[String],
    row: HashMap<String, String>,
    out: &mut Vec<HashMap<String, String>>,
    all_dropped: &mut Vec<String>,
) -> Result<(), ToolError> {
    let base_name = row.get("名称").cloned().unwrap_or_default();
    out.push(row);

    if base_name.ends_with("-α") || base_name.ends_with("-β") || base_name.ends_with("-γ") {
        return Ok(());
    }

    for suffix in &["-α", "-β", "-γ"] {
        let variant_name = format!("{base_name}{suffix}");
        let variant_result = db
            .query_filtered(
                table,
                fields,
                &[Filter::new("名称", FilterOp::Eq, &variant_name)],
                1,
            )
            .map_err(ToolError::Execution)?;
        for d in &variant_result.dropped_fields {
            if !all_dropped.contains(d) {
                all_dropped.push(d.clone());
            }
        }
        if let Some(vrow) = variant_result.rows.into_iter().next() {
            out.push(vrow);
        }
    }

    Ok(())
}

// ═══════════════════════════════════════════════════════
// 条目格式化
// ═══════════════════════════════════════════════════════

/// 格式化单条肉鸽条目：逐列输出所有有值字段。
fn format_one_entry(row: &HashMap<String, String>, theme_display: &str, out: &mut String) {
    let name = row.get("名称").map(|s| s.as_str()).unwrap_or("未知");
    let entry_type = row.get("类型").map(|s| s.as_str()).unwrap_or("");

    // ── 标题行 ──
    out.push_str(&format!("━━━ {name}（{entry_type}）━━━\n"));
    out.push_str(&format!("主题：{theme_display}\n"));

    // ── 稀有度（仅藏品类有值）──
    if let Some(rarity) = row.get("稀有度") {
        if !rarity.is_empty() {
            out.push_str(&format!("稀有度：{rarity}\n"));
        }
    }

    // ── 效果 ──
    if let Some(effect) = row.get("效果") {
        if !effect.is_empty() {
            out.push_str(&format!("效果：{effect}\n"));
        }
    }

    // ── buff JSON 格式化 ──
    if let Some(buff_raw) = row.get("buff") {
        if !buff_raw.is_empty() && buff_raw != "null" && buff_raw != "[]" {
            if let Ok(buff_json) = serde_json::from_str::<Value>(buff_raw) {
                if let Some(arr) = buff_json.as_array() {
                    if !arr.is_empty() {
                        out.push_str("buff：\n");
                        format_buff(arr, out);
                    }
                }
            }
        }
    }

    // ── 备注 ──
    if let Some(note) = row.get("备注") {
        let note = note.trim();
        if !note.is_empty() {
            out.push_str(&format!("备注：{note}\n"));
        }
    }

    // ── 特性 ──
    if let Some(trait_val) = row.get("特性") {
        let trait_val = trait_val.trim();
        if !trait_val.is_empty() {
            // 尝试 prettify JSON（藏品类为数组、通宝类为对象）
            if let Ok(v) = serde_json::from_str::<Value>(trait_val) {
                match v {
                    Value::Array(items) if !items.is_empty() => {
                        let texts: Vec<&str> = items.iter().filter_map(|i| i.as_str()).collect();
                        if !texts.is_empty() {
                            out.push_str(&format!("特性：{}\n", texts.join("、")));
                        }
                    }
                    Value::Object(map) if !map.is_empty() => {
                        // 键顺序为 serde_json 的 BTreeMap 字典序，非数据管线写入顺序
                        for (k, val) in map {
                            match val {
                                Value::Array(items) => {
                                    let texts: Vec<&str> =
                                        items.iter().filter_map(|i| i.as_str()).collect();
                                    if !texts.is_empty() {
                                        out.push_str(&format!("特性·{k}：{}\n", texts.join("、")));
                                    }
                                }
                                Value::String(s) => out.push_str(&format!("特性·{k}：{s}\n")),
                                Value::Number(n) => out.push_str(&format!("特性·{k}：{n}\n")),
                                // Bool/Null/嵌套对象等形态当前数据不出现，静默跳过
                                _ => {}
                            }
                        }
                    }
                    Value::String(s) => out.push_str(&format!("特性：{s}\n")),
                    _ => {}
                }
            } else {
                // 非 JSON（如思绪的多行文本）→ 原文输出
                out.push_str(&format!("特性：{trait_val}\n"));
            }
        }
    }
}

// ═══════════════════════════════════════════════════════
// buff JSON 格式化
// ═══════════════════════════════════════════════════════

/// 格式化 buff JSON 数组为可读的缩进文本。
///
/// 输入格式（藏品）：
/// ```json
/// [{"key": "immediate_reward", "blackboard": [
///   {"key": "id", "value": 0.0, "valueStr": "rogue_1_hp"},
///   {"key": "count", "value": 2.0, "valueStr": null}
/// ]}]
/// ```
///
/// 输出：
/// ```text
///   immediate_reward
///     id: rogue_1_hp
///     count: 2
/// ```
fn format_buff(buffs: &[Value], out: &mut String) {
    for buff in buffs {
        let key = buff["key"].as_str().unwrap_or("?");
        let bb = &buff["blackboard"];

        if bb.is_array() {
            let params = bb.as_array().unwrap();
            if params.is_empty() {
                out.push_str(&format!("  {key}\n"));
            } else {
                out.push_str(&format!("  {key}\n"));
                for param in params {
                    let pkey = param["key"].as_str().unwrap_or("?");
                    // valueStr 非空优先；为 null 时回退 value（空串视为无值）
                    let pval = if let Some(vs) = param["valueStr"].as_str() {
                        if vs.is_empty() {
                            None
                        } else {
                            Some(vs.to_string())
                        }
                    } else if let Some(v) = param["value"].as_f64() {
                        if v.fract() == 0.0 {
                            Some(format!("{:.0}", v))
                        } else {
                            Some(v.to_string())
                        }
                    } else {
                        None
                    };
                    match pval {
                        Some(val) => out.push_str(&format!("    {pkey}: {val}\n")),
                        None => out.push_str(&format!("    {pkey}\n")),
                    }
                }
            }
        } else {
            out.push_str(&format!("  {key}\n"));
        }
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn db_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/arknights.db")
    }

    fn make_ctx() -> ToolContext {
        let db = Database::open(db_path().to_str().unwrap()).unwrap();
        ToolContext::with_db(Arc::new(db))
    }

    // ── 主题解析 ──

    #[test]
    fn resolve_theme_by_number() {
        assert!(resolve_theme("1").unwrap().table == "rogue_1_entries");
        assert!(resolve_theme("5").unwrap().table == "rogue_5_entries");
    }

    #[test]
    fn resolve_theme_by_rogue_id() {
        assert!(resolve_theme("rogue1").unwrap().table == "rogue_1_entries");
        assert!(resolve_theme("rogue_3").unwrap().table == "rogue_3_entries");
    }

    #[test]
    fn resolve_theme_by_short_name() {
        assert!(resolve_theme("傀影").unwrap().table == "rogue_1_entries");
        assert!(resolve_theme("水月").unwrap().table == "rogue_2_entries");
        assert!(resolve_theme("萨米").unwrap().table == "rogue_3_entries");
        assert!(resolve_theme("萨卡兹").unwrap().table == "rogue_4_entries");
        assert!(resolve_theme("界园").unwrap().table == "rogue_5_entries");
    }

    #[test]
    fn resolve_theme_by_full_name() {
        assert!(resolve_theme("傀影与猩红孤钻").unwrap().table == "rogue_1_entries");
        assert!(resolve_theme("水月与深蓝之树").unwrap().table == "rogue_2_entries");
        assert!(resolve_theme("探索者的银凇止境").unwrap().table == "rogue_3_entries");
    }

    #[test]
    fn resolve_theme_fuzzy() {
        // 部分匹配
        assert!(resolve_theme("傀影主题").unwrap().table == "rogue_1_entries");
        assert!(resolve_theme("银凇").unwrap().table == "rogue_3_entries");
    }

    #[test]
    fn resolve_theme_ambiguous() {
        // 同时命中多个主题 → 报错
        let err = resolve_theme("傀影与萨米").unwrap_err();
        assert!(err.contains("主题不明确"));
        assert!(err.contains("傀影"));
        assert!(err.contains("萨米"));
    }

    #[test]
    fn resolve_theme_invalid() {
        let err = resolve_theme("明日方舟").unwrap_err();
        assert!(err.contains("无法识别"));
        assert!(err.contains("可用主题"));
    }

    // ── 基础查询 ──

    #[test]
    fn tool_metadata() {
        let tool = QueryRogue;
        assert_eq!(tool.name(), "query_rogue");
        assert!(tool.description().contains("集成战略"));
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert!(params["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("theme")));
    }

    #[test]
    fn query_relic_by_name() {
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"傀影","names":["热水壶"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("热水壶"));
        assert!(result.contains("藏品"));
        assert!(result.contains("傀影与猩红孤钻"));
        assert!(result.contains("效果"));
    }

    #[test]
    fn query_relic_by_id() {
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"1","ids":["rogue_1_relic_r01"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("热水壶"));
    }

    #[test]
    fn query_sa_rogue3() {
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"萨米","names":["地形图"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("地形图"));
        assert!(result.contains("探索者的银凇止境"));
    }

    #[test]
    fn query_sa_rogue4_thought() {
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"萨卡兹","names":["死仇时代的恨意"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("死仇时代的恨意"));
        assert!(result.contains("萨卡兹的无终奇语"));
    }

    #[test]
    fn query_sa_rogue5_coin() {
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"界园","names":["大炎通宝"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("大炎通宝"));
        assert!(result.contains("通宝"));
        assert!(result.contains("界园"));
    }

    #[test]
    fn query_nobuff_item_no_panic() {
        // 剧目类型 buff 为 null — 验证不崩溃
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"傀影","names":["《凯旋颂》"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("《凯旋颂》"));
        assert!(result.contains("剧目"));
        // buff 为空，不应出现 "buff：" 行
        assert!(!result.contains("buff："));
    }

    #[test]
    fn query_mutation_by_name() {
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"水月","names":["造血障碍"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("造血障碍"));
        assert!(result.contains("排异反应"));
        assert!(result.contains("buff"));
    }

    #[test]
    fn query_collapse_paradigm() {
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"萨米","names":["去量化"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("去量化"));
        assert!(result.contains("坍缩范式"));
    }

    // ── 多元奇物变体 ──

    #[test]
    fn multi_relic_sa_rogue4() {
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"萨卡兹","names":["精选兽肉罐头"]}"#)
            .unwrap()
            .summary;
        // 基础版本 + 三种变体全部出现
        assert!(result.contains("精选兽肉罐头"));
        assert!(result.contains("精选兽肉罐头-α"));
        assert!(result.contains("精选兽肉罐头-β"));
        assert!(result.contains("精选兽肉罐头-γ"));
        // 各变体效果不同
        assert!(result.contains("回复3点"));
        assert!(result.contains("回复5点"));
        assert!(result.contains("回复8点"));
        assert!(result.contains("回复10点"));
    }

    #[test]
    fn multi_relic_sa_rogue3() {
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"萨米","names":["地形图"]}"#)
            .unwrap()
            .summary;
        // 萨米的地形图也有多元奇物变体
        assert!(result.contains("地形图"));
        assert!(result.contains("地形图-α"));
    }

    #[test]
    fn multi_relic_variant_direct_search_no_expand() {
        // 直接搜索变体名称时不应再展开
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"萨卡兹","names":["精选兽肉罐头-α"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("精选兽肉罐头-α"));
        // 不应出现其他变体
        assert!(!result.contains("精选兽肉罐头-β"));
        assert!(!result.contains("精选兽肉罐头-γ"));
    }

    // ── 同名多条记录 ──

    #[test]
    fn query_same_name_era_all_three_returned() {
        // 萨卡兹年代追忆：同名三条（成型期/扩张期/鼎盛期），全部返回
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"萨卡兹","names":["魔王年代"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("魔王年代"));
        assert!(result.contains("年代追忆（成型期）"));
        assert!(result.contains("年代追忆（扩张期）"));
        assert!(result.contains("年代追忆（鼎盛期）"));
    }

    #[test]
    fn query_same_name_shishi_all_four_returned() {
        // 界园岁时：同名四条（朦胧/真切/入髓/醒觉），全部返回
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"界园","names":["酉疗"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("岁时（朦胧）"));
        assert!(result.contains("岁时（真切）"));
        assert!(result.contains("岁时（入髓）"));
        assert!(result.contains("岁时（醒觉）"));
    }

    #[test]
    fn query_same_name_mutation_both_returned() {
        // 水月排异反应：同名两条（mutation_1 / mutation_9），全部返回；
        // 效果文本相同但 buff 不同（常规/组合排异），以 buff key 区分
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"水月","names":["造血障碍"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("char_ability_new"));
        assert!(result.contains("char_shared_data"));
    }

    // ── 特性 JSON 对象（通宝标识）──

    #[test]
    fn query_coin_identifier_object_formatted() {
        // 界园通宝：特性为 JSON 对象 {"标识": [...]}，应格式化输出
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"界园","names":["大炎通宝"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("特性·标识"));
        assert!(result.contains("不在诡意行商和易与中出售"));
    }

    // ── LIKE 降级路径 ──

    #[test]
    fn like_fallback_with_variants() {
        // 名称不完全匹配时走 LIKE：命中基础版并展开 -α/-β/-γ 变体
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"萨卡兹","names":["精选兽肉罐"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("精选兽肉罐头"));
        assert!(result.contains("精选兽肉罐头-α"));
        assert!(result.contains("精选兽肉罐头-β"));
        assert!(result.contains("精选兽肉罐头-γ"));
    }

    #[test]
    fn like_fallback_no_match_returns_not_found() {
        // LIKE 也找不到 → 报未找到
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(
                &ctx,
                r#"{"theme":"萨卡兹","names":["完全不存在的东西xyz"]}"#,
            )
            .unwrap()
            .summary;
        assert!(result.contains("✗"));
        assert!(result.contains("未"));
    }

    // ── 界园完整名解析 ──

    #[test]
    fn resolve_theme_rogue5_full_name() {
        assert!(resolve_theme("岁的界园志异").unwrap().table == "rogue_5_entries");
    }

    #[test]
    fn resolve_theme_rogue6() {
        assert!(resolve_theme("6").unwrap().table == "rogue_6_entries");
        assert!(resolve_theme("rogue6").unwrap().table == "rogue_6_entries");
        assert!(resolve_theme("rogue_6").unwrap().table == "rogue_6_entries");
        assert!(resolve_theme("黑流树海").unwrap().table == "rogue_6_entries");
        assert!(resolve_theme("沉沦者的黑流树海").unwrap().table == "rogue_6_entries");
        // 简称与模糊：用户常把黑流树海叫成"黑流"
        assert!(resolve_theme("黑流").unwrap().table == "rogue_6_entries");
        assert!(resolve_theme("黑流树海主题").unwrap().table == "rogue_6_entries");
    }

    #[test]
    fn not_found() {
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool
            .execute(&ctx, r#"{"theme":"傀影","names":["绝对不存在的条目xyz"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("✗"));
        assert!(result.contains("未"));
    }

    #[test]
    fn missing_theme() {
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool.execute(&ctx, r#"{"names":["热水壶"]}"#);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("theme"));
    }

    #[test]
    fn invalid_theme() {
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool.execute(&ctx, r#"{"theme":"不存在的主题","names":["热水壶"]}"#);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("无法识别"));
    }

    #[test]
    fn no_db_returns_error() {
        let ctx = ToolContext::new();
        let tool = QueryRogue;
        let result = tool.execute(&ctx, r#"{"theme":"傀影","names":["热水壶"]}"#);
        assert!(result.is_err());
    }

    #[test]
    fn missing_both_ids_and_names() {
        let ctx = make_ctx();
        let tool = QueryRogue;
        let result = tool.execute(&ctx, r#"{"theme":"傀影"}"#);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("ids") || err.contains("names"));
    }

    // ── 手动测试入口 ──

    #[test]
    fn show_output() {
        fn call(ctx: &ToolContext, theme: &str, params_json: &str) -> String {
            let tool = QueryRogue;
            // 将 theme 注入 params
            let mut v: Value = serde_json::from_str(params_json).unwrap_or(serde_json::json!({}));
            v["theme"] = serde_json::json!(theme);
            match tool.execute(ctx, &v.to_string()) {
                Ok(s) => s.summary,
                Err(e) => format!("ERROR: {e}"),
            }
        }

        let ctx = make_ctx();

        // 从环境变量读取测试参数
        if let Ok(theme) = std::env::var("PRTS_ROGUE_THEME") {
            if let Ok(name) = std::env::var("PRTS_ROGUE_NAME") {
                println!(
                    "{}",
                    call(
                        &ctx,
                        &theme,
                        &serde_json::json!({"names": [name]}).to_string()
                    )
                );
                return;
            }
            if let Ok(id) = std::env::var("PRTS_ROGUE_ID") {
                println!(
                    "{}",
                    call(&ctx, &theme, &serde_json::json!({"ids": [id]}).to_string())
                );
                return;
            }
        }

        // 默认示例
        let cases: &[(&str, &str, &str)] = &[
            ("傀影", "热水壶", "names"),
            ("水月", "造血障碍", "names"),
            ("萨米", "地形图", "names"),
            ("萨卡兹", "精选兽肉罐头", "names"),
            ("界园", "大炎通宝", "names"),
        ];
        for (theme, name, mode) in cases {
            let params = if *mode == "names" {
                serde_json::json!({"names": [name]})
            } else {
                serde_json::json!({"ids": [name]})
            };
            println!("========== {theme} · {name} ==========");
            println!("{}", call(&ctx, theme, &params.to_string()));
            println!();
        }
    }
}
