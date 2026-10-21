//! query_enemy —— 敌人名称查询工具
//!
//! 语义：输入名称 → 返回敌人属性（优先精确匹配，fallback 模糊取最佳）。
//! 与 search_entities 互为逆操作：
//!   query_enemy：身份 → 属性（名称查询）
//!   search_entities：条件 → 身份列表（集合筛选）
//!
//! ## 参数契约
//!
//! `names` / `ids`（数组，互斥，必填其一）+ `extra`（数组，可选）。
//! 不提供 `fields` 参数。
//!
//! ## 默认行为
//!
//! 默认返回：名称、级别、描述、地位、种类、能力列表、攻击方式、伤害类型、
//! 行动方式、最大生命值、攻击力、防御力、法术抗性、移动速度、攻击间隔、
//! 重量、异常抗性、备注。默认级别 0，输出标注该敌人的级别变体总数
//! （如"共 2 级变体"），说明级别可切换范围。
//!
//! ## 名称口径
//!
//! 展示用**图鉴名**（玩家在游戏内看到、PRTS 直接查询的名字）。
//! `enemies` 表记录的是游戏数据名，少数敌人（`enemy_name_map` 里的那些）两者不同，
//! 此时在该条下方注明表里记录的名字。按名称查也认图鉴名。
//!
//! | 维度 | 默认 | extra 可切换 |
//! |------|------|-------------|
//! | 级别 | 0    | `级别=1`、`级别=2` |
//!
//! ## extra 语法
//!
//! 含 `=` 为**状态指定**：键限 `级别`，指定即切换级别变体。
//! 旧裸写语法（`级别1`）不再作为状态指定解析，落入附加列后无法命中时报错并指路
//! （键=值 形式）。
//!
//! 不含 `=` 为**附加列**：常用列（天赋文本/技能文本/基础嘲讽等级/目标价值/
//! 损伤抗性/元素抗性）或任意完整表列，JSON 列按现有格式化方式输出。
//! 无法识别的项报错并指路（完整列名见知识库《敌人表结构》条目）。
//!
//! ## 特殊列加工
//!
//! enemies 表有 7 个需要格式化的列，格式规则内聚在本文件：
//!
//! | 列名     | 原始格式                    | 输出格式              |
//! |---------|---------------------------|---------------------|
//! | 能力列表  | JSON `["a","b"]`          | 编号列表              |
//! | 技能文本  | JSON `[{名称,冷却,...}]`   | 逐条展开              |
//! | 天赋文本  | 文本（含 wiki 标记）        | 清洗后原样             |
//! | 异常抗性  | JSON `["a","b"]`          | "、"拼接              |
//! | 种类     | JSON `["a","b"]`          | "、"拼接              |
//! | 伤害类型  | JSON `["a"]` 或 `[]`      | "、"拼接 / "无"       |
//! | 关联敌人  | JSON `["a"]` 或 null      | "、"拼接 / "无"       |

use crate::db::{Database, Filter, FilterOp};
use crate::tools::common::{self, Target};
use crate::tools::display::{array_summary, targets_summary};
use crate::tools::wiki;
use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

use std::collections::{HashMap, HashSet};

/// enemies 表完整列名（附加列识别与查询字段共用）。
const ALL_COLUMNS: &[&str] = &[
    "敌人ID",
    "名称",
    "级别",
    "描述",
    "地位",
    "能力列表",
    "种类",
    "关联敌人",
    "攻击方式",
    "伤害类型",
    "行动方式",
    "攻击半径",
    "目标价值",
    "不计入敌人总数",
    "numOfExtraDrops",
    "viewRadius",
    "最大生命值",
    "攻击力",
    "防御力",
    "法术抗性",
    "移动速度",
    "攻击速度",
    "攻击间隔",
    "生命自回速度",
    "重量",
    "基础嘲讽等级",
    "损伤抗性",
    "元素抗性",
    "异常抗性",
    "天赋文本",
    "技能文本",
    "备注",
];

/// 默认结果已包含的列：附加列中重复指定时忽略（不重复输出、不报错）。
const DEFAULT_COLUMNS: &[&str] = &[
    "名称",
    "级别",
    "描述",
    "地位",
    "种类",
    "能力列表",
    "攻击方式",
    "伤害类型",
    "行动方式",
    "最大生命值",
    "攻击力",
    "防御力",
    "法术抗性",
    "移动速度",
    "攻击间隔",
    "重量",
    "异常抗性",
    "备注",
];

pub struct QueryEnemy;

impl Tool for QueryEnemy {
    fn name(&self) -> &str {
        "query_enemy"
    }

    fn description(&self) -> &str {
        "按名称或 ID 查询敌人属性，支持批量（ids 主键精确 / names 精确优先、未中模糊取最佳，二选一）。默认返回：名称、级别、描述、地位、种类、能力列表、攻击方式、伤害类型、行动方式、最大生命值、攻击力、防御力、法术抗性、移动速度、攻击间隔、重量、异常抗性、备注。默认级别 0，输出标注级别变体总数（如“共 2 级变体”），变体用 extra 切换。仅 enemies 表"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "敌人 ID 列表，如 [“enemy_1007_slime”]。与 names 二选一"
                },
                "names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "敌人名称列表，如 [“源石虫”]。与 ids 二选一"
                },
                "extra": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "增量声明，可选，每项独立生效。含 = 为状态指定，不含 = 为附加列。状态指定（键限 级别）：级别=N（整数级别，默认 0）。附加列：常用列（天赋文本、技能文本、基础嘲讽等级、目标价值、损伤抗性、元素抗性）或任意完整表列；完整列名见知识库《敌人表结构》条目"
                }
            }
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(params) else {
            return ToolDisplay::default();
        };
        let mut parts = Vec::new();
        if let Some(t) = targets_summary(&v) {
            parts.push(t);
        }
        if let Some(e) = array_summary(&v, "extra") {
            parts.push(e);
        }
        if parts.is_empty() {
            return ToolDisplay::default();
        }
        ToolDisplay::info(parts.join(" · "))
    }

    fn execute(&self, ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;

        // ── 1. 解析查询目标（id/ids 与 name/names 互斥）──
        let targets = common::parse_targets(&v)?;

        // ── 2. 解析 extra（状态指定 + 附加列）──
        let extra = parse_extra(&v["extra"]);
        let level = extra.level.unwrap_or(0);

        // ── 3. 获取数据库 ──
        let db = ctx
            .db
            .as_ref()
            .ok_or_else(|| ToolError::Execution("数据库未连接".into()))?;

        // ── 4. 固定查询完整列（不裁剪）──
        let all_fields: Vec<String> = ALL_COLUMNS.iter().map(|s| s.to_string()).collect();

        // ── 5. 逐个查询并格式化 ──
        let mut output = String::new();
        let mut all_rows: Vec<HashMap<String, String>> = Vec::new();

        for (i, target) in targets.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }

            let row = match target {
                Target::Id(id) => query_by_id(db, id, &all_fields, level)?,
                Target::Name(name) => query_by_name(db, name, &all_fields, level)?,
            };

            match row {
                Some(row) => {
                    all_rows.push(row.clone());
                    // 变体数：该敌人ID 在表中的总行数（含级别 0）；ID 缺失或统计失败时降级为不标注
                    let variant_count = row
                        .get("敌人ID")
                        .and_then(|id| count_variants(db, id).ok())
                        .unwrap_or(0);
                    format_one_enemy(db, &row, &extra, variant_count, &mut output);
                }
                None => {
                    let label = match target {
                        Target::Id(id) => format!("ID: {id}"),
                        Target::Name(name) => format!("名称: {name}"),
                    };
                    output.push_str(&format!("✗ {label}（级别 {level}）：未找到\n"));
                }
            }
        }

        Ok(ToolOutput::json(
            serde_json::json!({ "rows": all_rows, "dropped_fields": [] }),
            output.trim_end(),
        ))
    }
}

// ═══════════════════════════════════════════════════════
// extra 解析
// ═══════════════════════════════════════════════════════

#[derive(Debug, Default)]
struct ExtraConfig {
    /// 状态指定 · 级别：级别=N（默认 None = 级别 0）
    level: Option<u32>,
    /// 附加列（不含 =）：默认列之外的完整表列
    extra_columns: Vec<String>,
    /// 解析期无法识别的项（正文，输出时加 ✗ 前缀）
    errors: Vec<String>,
}

/// 解析 extra 数组。
///
/// 判定顺序：含 `=` → 状态指定（键限 `级别`）→ 不含 `=` → 表列名（默认列忽略）→ 未命中报错指路。
/// 非字符串项与空串忽略（与 query_operator 一致）。
fn parse_extra(extra_val: &serde_json::Value) -> ExtraConfig {
    let arr = match extra_val.as_array() {
        Some(a) => a,
        None => return ExtraConfig::default(),
    };

    let mut cfg = ExtraConfig::default();
    for item in arr {
        let s = match item.as_str() {
            Some(s) => s.trim(),
            None => continue,
        };
        if s.is_empty() {
            continue;
        }

        // ── 状态指定（含 =）──
        if let Some(eq) = s.find('=') {
            let key = &s[..eq];
            let value = &s[eq + 1..];
            match key {
                "级别" => parse_level_status(value, &mut cfg),
                _ => cfg
                    .errors
                    .push(format!("状态键 '{key}' 不支持：仅限 级别，请用 键=值 形式")),
            }
            continue;
        }

        // ── 附加列（不含 =）──
        if ALL_COLUMNS.contains(&s) {
            // 默认已返回的列：重复指定忽略
            if !DEFAULT_COLUMNS.contains(&s) {
                cfg.extra_columns.push(s.to_string());
            }
            continue;
        }

        cfg.errors.push(format!(
            "无法识别的 extra 项 '{s}'：状态请用 键=值 形式（级别）；完整列名见知识库《敌人表结构》条目"
        ));
    }

    cfg
}

/// 状态指定 · 级别：级别=N（N 为整数级别编号）。
fn parse_level_status(value: &str, cfg: &mut ExtraConfig) {
    match value.parse::<u32>() {
        Ok(n) => cfg.level = Some(n),
        Err(_) => cfg.errors.push(format!(
            "级别 值 '{value}' 无法解析：请用 级别=N（N 为整数级别编号）"
        )),
    }
}

// ═══════════════════════════════════════════════════════
// 单敌人格式化
// ═══════════════════════════════════════════════════════

fn format_one_enemy(
    db: &Database,
    row: &HashMap<String, String>,
    extra: &ExtraConfig,
    variant_count: usize,
    out: &mut String,
) {
    let table_name = row.get("名称").map(|s| s.as_str()).unwrap_or("未知");
    // 展示用图鉴名：PRTS 直接查的就是这个名字
    let alias = row
        .get("敌人ID")
        .and_then(|id| display_name_by_id(db, id, table_name));
    let enemy_name = alias.as_deref().unwrap_or(table_name);
    let enemy_level = row.get("级别").map(|s| s.as_str()).unwrap_or("?");

    if variant_count > 0 {
        out.push_str(&format!(
            "━━━ {enemy_name}（级别 {enemy_level}，共 {variant_count} 级变体）━━━\n"
        ));
    } else {
        out.push_str(&format!("━━━ {enemy_name}（级别 {enemy_level}）━━━\n"));
    }

    // ── 默认返回列（固定顺序）──
    for col in DEFAULT_COLUMNS {
        if *col == "名称" {
            out.push_str(&format!("名称: {enemy_name}\n"));
            if alias.is_some() {
                out.push_str(&format!(
                    "  ※ 上面是图鉴名，enemies 表记录的是“{table_name}”\n"
                ));
            }
            continue;
        }
        format_field(col, row, out);
    }

    // ── 附加列（常用列 + 其余完整表列）──
    for field in &extra.extra_columns {
        format_field(field, row, out);
    }

    // ── extra 解析期错误（逐条标注，不阻塞其它有效结果）──
    for err in &extra.errors {
        out.push_str(&format!("✗ {err}\n"));
    }
}

// ═══════════════════════════════════════════════════════
// 特殊列格式化
// ═══════════════════════════════════════════════════════

/// 特殊列的列名集合（懒初始化）。
fn special_columns() -> &'static HashSet<&'static str> {
    static SET: std::sync::LazyLock<HashSet<&'static str>> = std::sync::LazyLock::new(|| {
        let mut s = HashSet::new();
        s.insert("能力列表");
        s.insert("技能文本");
        s.insert("天赋文本");
        s.insert("异常抗性");
        s.insert("种类");
        s.insert("伤害类型");
        s.insert("关联敌人");
        s
    });
    &SET
}

/// 格式化特殊列。返回 None 表示非特殊列，走默认键值对输出。
fn format_special_column(field: &str, raw: &str) -> Option<String> {
    if !special_columns().contains(field) {
        return None;
    }

    match field {
        "能力列表" => Some(format_ability_list(raw)),
        "技能文本" => Some(format_skill_text(raw)),
        "天赋文本" => Some(format_talent_text(raw)),
        "异常抗性" | "种类" | "伤害类型" | "关联敌人" => {
            Some(format_json_string_array(raw))
        }
        _ => None,
    }
}

/// 是否是需要块级输出的特殊列（字段名独占一行，内容缩进）。
fn is_block_special(field: &str) -> bool {
    matches!(field, "能力列表" | "技能文本" | "天赋文本")
}

// ── 能力列表 ──

/// `["描述1", "描述2"]` → 编号列表。
/// 空数组 / 解析失败 → "无"
fn format_ability_list(raw: &str) -> String {
    let items: Vec<String> = match serde_json::from_str(raw) {
        Ok(arr) => arr,
        Err(_) => return "无".to_string(),
    };

    if items.is_empty() {
        return "无".to_string();
    }

    items
        .iter()
        .enumerate()
        .map(|(i, text)| format!("  {}. {}", i + 1, text))
        .collect::<Vec<_>>()
        .join("\n")
}

// ── 技能文本 ──

/// `[{"名称":"咆哮","初始冷却":"5",...}]` → 逐条展开。
/// null / 空数组 / 解析失败 → "无"
fn format_skill_text(raw: &str) -> String {
    #[derive(serde::Deserialize)]
    struct SkillEntry {
        #[serde(rename = "名称")]
        name: Option<String>,
        #[serde(rename = "初始冷却")]
        init_cooldown: Option<String>,
        #[serde(rename = "周期冷却")]
        cycle_cooldown: Option<String>,
        #[serde(rename = "技力消耗")]
        sp_cost: Option<String>,
        #[serde(rename = "效果")]
        effect: Option<String>,
    }

    let entries: Vec<SkillEntry> = match serde_json::from_str(raw) {
        Ok(arr) => arr,
        Err(_) => return "无".to_string(),
    };

    if entries.is_empty() {
        return "无".to_string();
    }

    let mut lines = Vec::new();
    for (i, skill) in entries.iter().enumerate() {
        let name = skill.name.as_deref().unwrap_or("未知技能");
        // 名称可能含换行符（如"咆哮\n[陆行姿态]"），替换为空格
        let name = name.replace('\n', " ").replace("\r", "");

        // 冷却信息行
        let mut meta_parts: Vec<String> = Vec::new();
        if let Some(ref cd) = skill.init_cooldown {
            let cd = cd.trim();
            if !cd.is_empty() && cd != "/" {
                meta_parts.push(format!("初始冷却 {}s", cd));
            }
        }
        if let Some(ref cd) = skill.cycle_cooldown {
            let cd = cd.trim();
            if !cd.is_empty() && cd != "/" && cd != "9999" {
                meta_parts.push(format!("周期冷却 {}s", cd));
            }
        }
        if let Some(ref sp) = skill.sp_cost {
            let sp = sp.trim();
            if !sp.is_empty() && sp != "/" {
                meta_parts.push(format!("技力消耗 {}", sp));
            }
        }

        lines.push(format!(
            "  [{name}]{}",
            if meta_parts.is_empty() {
                String::new()
            } else {
                format!(" {}", meta_parts.join(" / "))
            }
        ));

        // 效果（清洗 wiki 标记）
        if let Some(ref effect) = skill.effect {
            let eff = wiki::clean_wiki(effect);
            let eff = eff.trim();
            if !eff.is_empty() {
                for eff_line in eff.lines() {
                    lines.push(format!("  {}", eff_line.trim()));
                }
            }
        }

        if i + 1 < entries.len() {
            lines.push(String::new()); // 技能间空行
        }
    }

    lines.join("\n")
}

// ── 天赋文本 ──

/// 清洗 wiki 标记（委托至 tools::wiki 模块）。
fn format_talent_text(raw: &str) -> String {
    wiki::clean_wiki(raw)
}

// ── JSON 字符串数组 → "、"拼接 ──

/// `["a", "b"]` → `a、b`。空数组 → `无`。null / 解析失败 → 原样返回。
fn format_json_string_array(raw: &str) -> String {
    let items: Vec<String> = match serde_json::from_str(raw) {
        Ok(arr) => arr,
        Err(_) => return raw.to_string(),
    };

    if items.is_empty() {
        return "无".to_string();
    }

    items.join("、")
}

// ═══════════════════════════════════════════════════════
// 辅助函数
// ═══════════════════════════════════════════════════════

/// 格式化单个字段并追加到 output。
fn format_field(field: &str, row: &HashMap<String, String>, output: &mut String) {
    let raw = match row.get(field) {
        Some(v) => v.as_str(),
        None => return,
    };

    if raw.is_empty() || raw == "null" {
        output.push_str(&format!("{field}: 无\n"));
    } else if let Some(formatted) = format_special_column(field, raw) {
        if is_block_special(field) {
            output.push_str(&format!("{field}:\n{formatted}\n"));
        } else {
            output.push_str(&format!("{field}: {formatted}\n"));
        }
    } else {
        output.push_str(&format!("{field}: {raw}\n"));
    }
}

// ═══════════════════════════════════════════════════════
// 名称映射（enemy_name_map）
// ═══════════════════════════════════════════════════════

/// 图鉴名 → 敌人ID。表不存在（旧库）或未命中都返回 None。
fn display_name_to_id(db: &Database, name: &str) -> Option<String> {
    match db.query_filtered(
        "enemy_name_map",
        &["敌人ID".to_string()],
        &[Filter::new("图鉴名", FilterOp::Eq, name)],
        1,
    ) {
        Ok(result) => result
            .rows
            .into_iter()
            .next()
            .and_then(|row| row.get("敌人ID").filter(|s| !s.is_empty()).cloned()),
        Err(e) => {
            // 旧库没有这张表时会走到这儿；不静默 —— 出事时日志里要看得见
            log::warn!("[query_enemy] 图鉴名映射不可用：{e}");
            None
        }
    }
}

/// 该敌人在图鉴里的名字；映射未命中、或与表内名称相同时返回 None。
fn display_name_by_id(db: &Database, enemy_id: &str, table_name: &str) -> Option<String> {
    let name = match db.query_filtered(
        "enemy_name_map",
        &["图鉴名".to_string()],
        &[Filter::new("敌人ID", FilterOp::Eq, enemy_id)],
        1,
    ) {
        Ok(result) => result
            .rows
            .into_iter()
            .next()
            .and_then(|row| row.get("图鉴名").cloned())?,
        Err(e) => {
            log::warn!("[query_enemy] 图鉴名映射不可用：{e}");
            return None;
        }
    };
    if name.is_empty() || name == table_name {
        None
    } else {
        Some(name)
    }
}

// ═══════════════════════════════════════════════════════
// 查询函数
// ═══════════════════════════════════════════════════════

/// 按 ID 精确查询（主键匹配 + 级别过滤）。
fn query_by_id(
    db: &Database,
    id: &str,
    query_fields: &[String],
    level: u32,
) -> Result<Option<HashMap<String, String>>, ToolError> {
    let result = db
        .query_filtered(
            "enemies",
            query_fields,
            &[
                Filter::new("敌人ID", FilterOp::Eq, id),
                Filter::new("级别", FilterOp::Eq, level.to_string()),
            ],
            1,
        )
        .map_err(ToolError::Execution)?;

    Ok(result.rows.into_iter().next())
}

/// 按名称查询（精确→模糊取最短）。
fn query_by_name(
    db: &Database,
    name: &str,
    query_fields: &[String],
    level: u32,
) -> Result<Option<HashMap<String, String>>, ToolError> {
    // 阶段 1：精确匹配
    let exact = db
        .query_filtered(
            "enemies",
            query_fields,
            &[
                Filter::new("名称", FilterOp::Eq, name),
                Filter::new("级别", FilterOp::Eq, level.to_string()),
            ],
            1,
        )
        .map_err(ToolError::Execution)?;

    if !exact.rows.is_empty() {
        return Ok(exact.rows.into_iter().next());
    }

    // 阶段 2：图鉴名精确（`enemy_name_map`）。PRTS 用图鉴名查，而 enemies 表存的是数据名，
    // 少数敌人两个名字不同（如图鉴名“浆果磐蟹” / 数据名“带果壳的磐蟹”），靠映射接上。
    if let Some(id) = display_name_to_id(db, name) {
        if let Some(row) = query_by_id(db, &id, query_fields, level)? {
            return Ok(Some(row));
        }
    }

    // 阶段 3：LIKE 模糊匹配，取名称最短者
    let candidates = db
        .query_filtered(
            "enemies",
            query_fields,
            &[
                Filter::new("名称", FilterOp::Like, name),
                Filter::new("级别", FilterOp::Eq, level.to_string()),
            ],
            0,
        )
        .map_err(ToolError::Execution)?;

    let best = candidates
        .rows
        .into_iter()
        .min_by_key(|row| row.get("名称").map(|s| s.len()).unwrap_or(usize::MAX));

    Ok(best)
}

/// 统计该敌人（敌人ID）的级别变体总数。
///
/// 主键为 (敌人ID, 级别)，该 ID 的全部行数即变体数（含级别 0）。
fn count_variants(db: &Database, enemy_id: &str) -> Result<usize, ToolError> {
    let result = db
        .query_filtered(
            "enemies",
            &["敌人ID".to_string()],
            &[Filter::new("敌人ID", FilterOp::Eq, enemy_id)],
            0,
        )
        .map_err(ToolError::Execution)?;

    Ok(result.rows.len())
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

    // ── 工具元数据 ──

    #[test]
    fn tool_metadata() {
        let tool = QueryEnemy;
        assert_eq!(tool.name(), "query_enemy");
        assert!(
            tool.description().contains("默认级别"),
            "描述应说明默认级别"
        );
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert!(
            params["properties"].get("fields").is_none(),
            "parameters 不应再有 fields"
        );
        assert!(params["properties"].get("extra").is_some());
        // 增量声明契约由 extra 参数描述单点承载（desc 不再重复声明，2026-08-31）
        assert!(
            params["properties"]["extra"]["description"]
                .as_str()
                .map(|s| s.contains("增量声明"))
                .unwrap_or(false),
            "extra 参数应声明增量声明契约"
        );
    }

    // ── 默认返回 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn default_output_columns() {
        // 无 extra：默认 18 列 + 变体数标注
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["源石虫"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("源石虫"), "应包含敌人名称\n结果: {result}");
        for col in [
            "名称",
            "级别",
            "描述",
            "地位",
            "种类",
            "能力列表",
            "攻击方式",
            "伤害类型",
            "行动方式",
            "最大生命值",
            "攻击力",
            "防御力",
            "法术抗性",
            "移动速度",
            "攻击间隔",
            "重量",
            "异常抗性",
            "备注",
        ] {
            assert!(
                result.contains(col),
                "默认输出应包含列 {col}\n结果: {result}"
            );
        }
        // 源石虫（enemy_1007_slime）有 0/1 两级变体
        assert!(
            result.contains("共 2 级变体"),
            "应标注级别变体数\n结果: {result}"
        );
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn query_exact_match_single_result() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["源石虫"]}"#)
            .unwrap()
            .summary;

        // 精确匹配 → 单条结果
        assert!(result.contains("源石虫"), "应包含敌人名称\n结果: {result}");
        assert!(!result.contains("源石虫·α"), "不应包含变种\n结果: {result}");
        assert!(
            result.contains("攻击力"),
            "默认输出应含攻击力\n结果: {result}"
        );
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn display_name_from_name_map() {
        // 图鉴名与表内数据名不同时：按图鉴名能查到，并注明表里记的是另一个名字。
        // 不设“表不在就跳过”的开关 —— 跳过的测试等于没测（这个坑真实踩过）：
        // 表不在直接报错，表的就位由 open_database_succeeds 守。
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["浆果磐蟹"]}"#)
            .unwrap()
            .summary;

        assert!(
            result.contains("浆果磐蟹"),
            "应按图鉴名查到\n结果: {result}"
        );
        assert!(
            result.contains("图鉴名"),
            "应注明图鉴名与表内名称不同\n结果: {result}"
        );
        assert!(
            result.contains("带果壳的磐蟹"),
            "应给出表内记录的数据名\n结果: {result}"
        );
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn query_like_fallback_picks_best() {
        // 非精确名称，LIKE fallback 取最短匹配
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["酸液源石"]}"#)
            .unwrap()
            .summary;

        // "酸液源石虫"比"酸液源石虫·α"更短，应命中前者
        assert!(
            result.contains("酸液源石虫"),
            "应命中最短匹配\n结果: {result}"
        );
        assert!(!result.contains("·α"), "不应包含更长变种\n结果: {result}");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn query_empty_result() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["绝对不存在的敌人xyz"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("未找到"), "空结果应有明确提示");
    }

    // ── 特殊列格式化 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn format_ability_list_output() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        // 能力列表在默认列中
        let result = tool.execute(&ctx, r#"{"names":["碎骨"]}"#).unwrap().summary;

        assert!(result.contains("能力列表"), "应包含列名");
        // 碎骨有能力列表（至少"未被阻挡时会发射榴弹"）
        assert!(
            result.contains("未被阻挡") || result.contains("榴弹"),
            "应包含能力描述\n结果: {result}"
        );
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn format_skill_text_output() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["火龙"],"extra":["技能文本"]}"#)
            .unwrap()
            .summary;

        // 火龙有技能文本
        assert!(result.contains("技能文本"), "应包含列名");
        assert!(result.contains("咆哮"), "应包含技能名称\n结果: {result}");
        assert!(
            result.contains("初始冷却") || result.contains("效果"),
            "应包含技能详情\n结果: {result}"
        );
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn format_talent_text_output() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["碎骨"],"extra":["天赋文本"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("天赋文本"), "应包含列名");
        // 碎骨有天赋：不会攻击飞行单位
        assert!(
            result.contains("飞行单位") || result.contains("攻击力"),
            "应包含天赋内容\n结果: {result}"
        );
    }

    #[test]
    fn format_talent_text_no_wiki_markers() {
        // 验证 {{popup|...}} 和 ''' 被清洗
        let raw = "占用'''3'''个阻挡数{{popup|内容=包含以下敌人：\n浅睡的臼齿、沉睡的臼齿}}";
        let cleaned = format_talent_text(raw);

        assert!(!cleaned.contains("{{popup"), "应去除 {{popup}} 标记");
        assert!(!cleaned.contains("'''"), "应去除粗体标记");
        assert!(cleaned.contains("（补充："), "popup 应转为补充说明");
        assert!(cleaned.contains("浅睡的臼齿"), "popup 内容应保留");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn format_abnormal_resistance() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        // 异常抗性在默认列中
        let result = tool
            .execute(&ctx, r#"{"names":["浆果虫"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("晕眩"), "应包含异常类型\n结果: {result}");
        assert!(result.contains("沉默"), "应包含异常类型\n结果: {result}");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn format_json_array_simple() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        // 岁相的种类是 ["化物"]
        let result = tool.execute(&ctx, r#"{"names":["岁相"]}"#).unwrap().summary;

        assert!(result.contains("化物"), "应包含种类\n结果: {result}");
        // 应该用顿号而非 JSON 格式
        assert!(
            !result.contains("[\""),
            "不应包含 JSON 方括号\n结果: {result}"
        );
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn null_column_shows_none() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        // 源石虫的关联敌人通常是 null
        let result = tool
            .execute(&ctx, r#"{"names":["源石虫"],"extra":["关联敌人"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("关联敌人"), "应包含列名");
        assert!(!result.contains("[\""), "不应是 JSON 格式");
    }

    // ── 级别（extra 状态指定）──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn default_level_zero() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["源石虫"]}"#)
            .unwrap()
            .summary;

        // 默认级别 0，不应出现级别 1
        assert!(result.contains("级别 0"), "默认应查询级别0\n结果: {result}");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_level_override() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["源石虫"],"extra":["级别=1"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("级别 1"), "应查询级别1\n结果: {result}");
        assert!(!result.contains("级别 0"), "不应出现级别0\n结果: {result}");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_level_switch_variant() {
        // 碎骨有 0/1/2 三级变体：级别=2 查询第二级变体，变体数标注保持一致
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["碎骨"],"extra":["级别=2"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("级别 2"), "应查询级别2\n结果: {result}");
        assert!(
            result.contains("共 3 级变体"),
            "级别切换后变体数标注不变\n结果: {result}"
        );
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_duplicate_level_last_wins() {
        // 多条级别指定：后写覆盖（与 query_operator 状态覆盖语义一致）
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["源石虫"],"extra":["级别=1","级别=0"]}"#)
            .unwrap()
            .summary;

        assert!(
            result.contains("级别 0"),
            "后写级别=0 应生效\n结果: {result}"
        );
        assert!(
            !result.contains("级别 1"),
            "前写级别=1 应被覆盖\n结果: {result}"
        );
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_level_invalid_value_rejected() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["源石虫"],"extra":["级别=abc"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("✗"), "非法级别值应报错\n结果: {result}");
        assert!(result.contains("无法解析"), "应指路\n结果: {result}");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_status_invalid_key_rejected() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["源石虫"],"extra":["职业=近卫"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("✗"), "非法状态键应报错\n结果: {result}");
        assert!(result.contains("仅限 级别"), "应指路合法键\n结果: {result}");
    }

    // ── 旧语法废除 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_legacy_level_syntax_rejected() {
        // 旧裸写 "级别1" 不再作为状态指定解析 → 附加列无法命中 → 报错指路键=值
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["源石虫"],"extra":["级别1"]}"#)
            .unwrap()
            .summary;

        assert!(
            result.contains("✗"),
            "旧语法 '级别1' 应报错\n结果: {result}"
        );
        assert!(
            result.contains("键=值"),
            "旧语法 '级别1' 应指路键=值\n结果: {result}"
        );
    }

    // ── 附加列（不含 =）──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_common_columns() {
        // 常用列：天赋文本、技能文本、基础嘲讽等级、目标价值、损伤抗性、元素抗性
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(
                &ctx,
                r#"{"names":["碎骨"],"extra":["天赋文本","技能文本","基础嘲讽等级","目标价值","损伤抗性","元素抗性"]}"#,
            )
            .unwrap()
            .summary;

        for col in [
            "天赋文本",
            "技能文本",
            "基础嘲讽等级",
            "目标价值",
            "损伤抗性",
            "元素抗性",
        ] {
            assert!(result.contains(col), "应输出附加列 {col}\n结果: {result}");
        }
        // 碎骨目标价值为 2
        assert!(
            result.contains("目标价值: 2"),
            "目标价值应有值\n结果: {result}"
        );
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_table_column_json() {
        // 非默认 JSON 列按现有格式化：染污战士关联敌人 ["浆果虫","硕果虫"] → "、"
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["染污战士"],"extra":["关联敌人"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("浆果虫"), "应包含关联敌人\n结果: {result}");
        assert!(result.contains("硕果虫"), "应包含关联敌人\n结果: {result}");
        assert!(!result.contains("[\""), "不应是 JSON 格式\n结果: {result}");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_table_column_id() {
        // 其余表列命中即返回：敌人ID（与 query_operator 的 干员ID 先例一致）
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["源石虫"],"extra":["敌人ID"]}"#)
            .unwrap()
            .summary;

        assert!(
            result.contains("敌人ID: enemy_1007_slime"),
            "应输出敌人ID\n结果: {result}"
        );
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_unknown_extra_error() {
        // 完全未知的字符串 → 报错并指路（键=值 / 知识库列名）
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["源石虫"],"extra":["不存在的列xyz"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("✗"), "未命中应报错\n结果: {result}");
        assert!(result.contains("键=值"), "应指路键=值\n结果: {result}");
        assert!(
            result.contains("知识库"),
            "应指路知识库列名\n结果: {result}"
        );
    }

    // ── 错误处理 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn missing_name_returns_error() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool.execute(&ctx, r#"{}"#);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("name"));
    }

    #[test]
    fn no_db_returns_error() {
        let ctx = ToolContext::new();
        let tool = QueryEnemy;
        let result = tool.execute(&ctx, r#"{"names":["源石虫"]}"#);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("数据库未连接"));
    }

    // ── 单元解析 ──

    #[test]
    fn parse_extra_level_status() {
        let cfg = parse_extra(&serde_json::json!(["级别=1"]));
        assert_eq!(cfg.level, Some(1));
        assert!(cfg.extra_columns.is_empty());
        assert!(cfg.errors.is_empty());
    }

    #[test]
    fn parse_extra_columns() {
        let cfg = parse_extra(&serde_json::json!(["天赋文本", "技能文本", "敌人ID"]));
        assert!(cfg.level.is_none());
        assert_eq!(cfg.extra_columns, vec!["天赋文本", "技能文本", "敌人ID"]);
        assert!(cfg.errors.is_empty());
    }

    #[test]
    fn parse_extra_default_column_ignored() {
        // 默认列重复指定：忽略不报错
        let cfg = parse_extra(&serde_json::json!(["名称", "备注", "级别"]));
        assert!(cfg.extra_columns.is_empty());
        assert!(cfg.errors.is_empty());
    }

    #[test]
    fn parse_extra_legacy_level_rejected() {
        // 旧裸写 "级别1"：非状态指定、非表列 → 报错指路
        let cfg = parse_extra(&serde_json::json!(["级别1"]));
        assert!(cfg.level.is_none());
        assert!(cfg.errors.iter().any(|e| e.contains("键=值")));
    }

    #[test]
    fn parse_extra_unknown_key() {
        let cfg = parse_extra(&serde_json::json!(["职业=近卫"]));
        assert!(cfg.level.is_none());
        assert!(cfg.errors.iter().any(|e| e.contains("仅限 级别")));
    }

    #[test]
    fn parse_extra_invalid_level_value() {
        let cfg = parse_extra(&serde_json::json!(["级别=abc"]));
        assert!(cfg.level.is_none());
        assert!(cfg.errors.iter().any(|e| e.contains("无法解析")));
    }

    #[test]
    fn parse_extra_unknown_string() {
        let cfg = parse_extra(&serde_json::json!(["不存在的列xyz"]));
        assert!(cfg.errors.iter().any(|e| e.contains("知识库")));
    }

    #[test]
    fn parse_extra_empty_and_non_string() {
        let cfg = parse_extra(&serde_json::json!([]));
        assert!(cfg.level.is_none() && cfg.extra_columns.is_empty() && cfg.errors.is_empty());
        let cfg = parse_extra(&serde_json::Value::Null);
        assert!(cfg.level.is_none() && cfg.extra_columns.is_empty() && cfg.errors.is_empty());
    }

    // ── 格式化单元 ──

    #[test]
    fn format_ability_list_empty() {
        assert_eq!(format_ability_list("[]"), "无");
    }

    #[test]
    fn format_ability_list_invalid_json() {
        assert_eq!(format_ability_list("not json"), "无");
    }

    #[test]
    fn format_skill_text_empty() {
        assert_eq!(format_skill_text("null"), "无");
        assert_eq!(format_skill_text("[]"), "无");
    }

    #[test]
    fn format_json_string_array_empty() {
        assert_eq!(format_json_string_array("[]"), "无");
    }

    #[test]
    fn format_json_string_array_non_json() {
        // 非 JSON 原样返回
        assert_eq!(format_json_string_array("hello"), "hello");
    }

    // ── ID 查询 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn query_by_id_exact() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"ids":["enemy_1007_slime"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("源石虫"));
        assert!(!result.contains("源石虫·α"));
        assert!(
            result.contains("共 2 级变体"),
            "ID 查询也应标注变体数\n结果: {result}"
        );
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn query_by_id_not_found() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"ids":["nonexistent_id_xyz"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("未找到"));
    }

    // ── 批量查询 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn batch_by_names() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["源石虫","碎骨"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("源石虫"));
        assert!(result.contains("碎骨"));
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn batch_partial_miss() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["源石虫","绝对不存在的敌人xyz"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("源石虫"));
        assert!(result.contains("未找到"));
    }

    // ── 互斥校验 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn ids_and_names_mutex_error() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool.execute(&ctx, r#"{"ids":["enemy_1007_slime"],"names":["源石虫"]}"#);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("互斥"));
    }

    // ── 默认备注 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn note_always_returned() {
        let ctx = make_ctx();
        let tool = QueryEnemy;
        let result = tool
            .execute(&ctx, r#"{"names":["源石虫"]}"#)
            .unwrap()
            .summary;
        // 备注在默认列中，即使为 null 也会输出 "备注: 无"
        assert!(result.contains("备注"));
    }

    // ── 演示输出（供 scripts/manual/test_enemy.py 交互调用）──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn show_output() {
        fn call(ctx: &ToolContext, name: &str, extra: Option<&[&str]>) -> String {
            let params = match extra {
                Some(ext) => serde_json::json!({
                    "names": [name], "extra": ext
                }),
                None => serde_json::json!({"names": [name]}),
            };
            let tool = QueryEnemy;
            match tool.execute(ctx, &params.to_string()) {
                Ok(s) => s.summary,
                Err(e) => format!("ERROR: {e}"),
            }
        }

        let ctx = make_ctx();

        // 支持环境变量交互：PRTS_NAME / PRTS_EXTRA（逗号分隔）
        if let Ok(name) = std::env::var("PRTS_NAME") {
            let evec: Option<Vec<String>> = std::env::var("PRTS_EXTRA").ok().map(|s| {
                if s.is_empty() {
                    vec![]
                } else {
                    s.split(',').map(|x| x.trim().to_string()).collect()
                }
            });
            let extra_ref: Option<Vec<&str>> = evec
                .as_ref()
                .map(|v| v.iter().map(|s| s.as_str()).collect());
            println!("{}  extra={:?}", name, extra_ref);
            println!("{}", call(&ctx, &name, extra_ref.as_deref()));
            return;
        }

        let cases: &[(&str, &str, Option<&[&str]>)] = &[
            ("1. 源石虫 · 默认（级别 0）", "源石虫", None),
            ("2. 碎骨 · 级别=1 切换变体", "碎骨", Some(&["级别=1"])),
            (
                "3. 火龙 · 附加列 技能文本+天赋文本",
                "火龙",
                Some(&["技能文本", "天赋文本"]),
            ),
            ("4. 源石虫 · 级别=abc 非法值", "源石虫", Some(&["级别=abc"])),
            (
                "5. 源石虫 · 旧裸写 级别1 报错示例",
                "源石虫",
                Some(&["级别1"]),
            ),
        ];
        for (label, name, extra) in cases {
            println!("========== {label} ==========");
            println!("{}", call(&ctx, name, *extra));
            println!();
        }
    }
}
