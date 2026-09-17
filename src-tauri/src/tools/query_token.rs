//! query_token —— 干员召唤物查询工具
//!
//! 语义：输入名称或 ID → 返回召唤物的属性、技能、天赋。
//! tokens 表结构与 operators 高度相似（相同的 JSON 列结构），
//! 但没有模组、潜能数值、信赖机制。属性计算为纯精英化插值。
//!
//! ## 参数契约
//!
//! `names` / `ids`（数组，互斥，必填其一）+ `extra`（数组，可选）。
//! 不提供 `fields` 参数；不提供附加列、技能点单、逐级技能/天赋数据、天赋点单。
//!
//! ## 默认行为
//!
//! 默认返回：基本信息、属性、技能列表（全部技能最高级）、
//! 天赋（当前练度最优备选）。默认练度为所属干员最高精英化满级、0潜。
//!
//! | 维度   | 默认                    | extra 可切换              |
//! |--------|------------------------|--------------------------|
//! | 精英化 | 所属干员最高精英化满级    | `等级=精二40级`, `等级=精一满级` |
//! | 潜能   | 0潜                     | `潜能=满潜`, `潜能=五潜`, `潜能=0潜` |
//! | 技能   | 全部技能最高级           | `技能=3级`, `技能=专2`（全部技能统一调整） |
//! | 天赋   | 当前练度最优备选          | 随练度重算               |
//!
//! ## extra 语法
//!
//! 含 `=` 为**状态指定**：键限 等级/潜能/技能 三种，指定即替换默认练度，
//! 属性与天赋随之重算。不含 `=` 的项报错并指路键=值形式；
//! 旧裸写语法（`精一20级`、`满潜`、`3级一技能`、`逐级技能数据`）不再解析。

use crate::db::{Database, Filter, FilterOp};
use crate::tools::common::{self, Target};
use crate::tools::common_fmt::{
    collect_attr_names, find_bounding_keyframes, format_skill_expanded,
    format_stats, format_talents, parse_chinese_or_digit, parse_json,
    pick_talent_alt, split_digits, ComputedStats, ElitePhase, Skill, Talent,
};
use crate::tools::display::{array_summary, targets_summary};
use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

use std::collections::HashMap;

pub struct QueryToken;

impl Tool for QueryToken {
    fn name(&self) -> &str {
        "query_token"
    }

    fn description(&self) -> &str {
        "按名称或 ID 查询召唤物（token），支持批量（ids 主键精确 / names 精确优先、未中模糊取最佳，二选一）。默认返回：基本信息、属性、技能列表（全部最高级）、天赋（当前练度最优备选）。默认练度 = 所属干员最高精英化满级、0潜。extra 状态指定（等级/潜能/技能）见参数说明。仅 tokens 表"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "召唤物 ID 列表，如 [“token_10014_bstalk_crab”]。与 names 二选一"
                },
                "names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "召唤物名称列表，如 [“触手”]。与 ids 二选一"
                },
                "extra": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "状态指定，可选，每项独立生效，必须含 =（键限 等级/潜能/技能）：等级=精二40级、精一满级、精二（省略即满级），值语法同干员等级；潜能=满潜、五潜、0潜；技能=3级（1~7级）、专2（专1~专3），默认最高级，调低用。指定即替换对应默认值，属性与天赋随之重算"
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

        // ── 1. 解析查询目标 ──
        let targets = common::parse_targets(&v)?;

        // ── 2. 解析 extra（状态指定）──
        let extra = parse_token_extra(&v["extra"]);

        let db = ctx
            .db
            .as_ref()
            .ok_or_else(|| ToolError::Execution("数据库未连接".into()))?;

        // ── 3. 固定查询完整列（不裁剪）──
        let all_fields: Vec<String> = TOKEN_ALWAYS_COLS.iter().map(|s| s.to_string()).collect();

        // ── 4. 逐个查询并格式化 ──
        let mut output = String::new();
        let mut all_rows: Vec<HashMap<String, String>> = Vec::new();

        for (i, target) in targets.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }

            let row = match target {
                Target::Id(id) => query_token_by_id(db, id, &all_fields)?,
                Target::Name(name) => query_token_by_name(db, name, &all_fields)?,
            };

            match row {
                Some(row) => {
                    all_rows.push(row.clone());
                    format_one_token(&row, &extra, &mut output);
                }
                None => {
                    let label = match target {
                        Target::Id(id) => format!("ID: {id}"),
                        Target::Name(name) => format!("名称: {name}"),
                    };
                    output.push_str(&format!("✗ {label}：未找到\n"));
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
// 数据库查询时始终加载的列
// ═══════════════════════════════════════════════════════

const TOKEN_ALWAYS_COLS: &[&str] = &[
    "名称", "代号", "特性", "持有者",
    "部署位置", "部署费用", "阻挡数", "嘲讽等级",
    "再部署时间", "攻击速度", "攻击间隔", "备注",
    "精英化阶段", "技能列表", "天赋列表",
];

// ═══════════════════════════════════════════════════════
// 单召唤物格式化
// ═══════════════════════════════════════════════════════

fn format_one_token(
    row: &HashMap<String, String>,
    extra: &TokenExtraConfig,
    out: &mut String,
) {
    let elite_phases: Vec<ElitePhase> =
        parse_json(row, "精英化阶段").unwrap_or_default();
    let skills: Vec<Skill> =
        parse_json(row, "技能列表").unwrap_or_default();
    let talents: Vec<Talent> =
        parse_json(row, "天赋列表").unwrap_or_default();

    let token_name = row.get("名称").map(|s| s.as_str()).unwrap_or("未知");

    // 默认最高精英化满级；等级= / 潜能= 替换对应默认值
    let phase_idx = extra
        .elite_phase
        .unwrap_or(elite_phases.len().saturating_sub(1))
        .min(elite_phases.len().saturating_sub(1));
    let phase = if let Some(p) = elite_phases.get(phase_idx) { p } else { return; };
    let level = extra.elite_level.unwrap_or(phase.max_level).min(phase.max_level);
    let pot_level = extra.pot_level.unwrap_or(0);

    out.push_str(&format!("━━━ {token_name} ━━━\n"));

    // ── 基本信息 ──
    format_token_basic_info(row, out);

    // ── 属性（默认或状态指定后的练度）──
    let phase_label = if phase_idx == 0 { "精0".to_string() } else if phase_idx == 1 { "精1".to_string() } else { format!("精{phase_idx}") };
    let level_disp = if level >= phase.max_level {
        "满级".to_string()
    } else {
        format!("{level}级")
    };
    out.push_str(&format!("\n── 属性（{phase_label} {level_disp} / {pot_level}潜）──\n"));
    let stats = compute_token_stats(phase, level);
    format_stats(&stats, out);
    let range_str = phase.range_id.as_deref().unwrap_or("无");
    out.push_str(&format!("攻击范围: {range_str}\n"));

    // ── 技能（全部技能最高级，或 技能= 统一调整）──
    if !skills.is_empty() {
        out.push_str("\n── 技能列表 ──\n");
        for skill in &skills {
            out.push('\n');
            format_skill_expanded(skill, extra.skill_level, false, out);
        }
    }

    // ── 天赋（当前练度最优备选，不展开备选列表）──
    if !talents.is_empty() {
        let talent_elite = extra.elite_phase.unwrap_or(elite_phases.len().saturating_sub(1));
        // 先检查是否有匹配当前练度的备选，避免输出空标题
        if talents.iter().any(|t| {
            pick_talent_alt(&t.alternatives, talent_elite as u32, pot_level).is_some()
        }) {
            out.push_str("\n── 天赋 ──\n");
            format_talents(
                &talents,
                extra.elite_phase,
                extra.pot_level,
                talent_elite,
                pot_level,
                false,
                None,
                out,
            );
        }
    }

    // ── extra 解析期错误（逐条标注，不阻塞其它有效结果）──
    for err in &extra.errors {
        out.push_str(&format!("✗ {err}\n"));
    }
}

// ═══════════════════════════════════════════════════════
// 基本信息格式化
// ═══════════════════════════════════════════════════════

fn format_token_basic_info(row: &HashMap<String, String>, out: &mut String) {
    out.push_str("\n── 基本信息 ──\n");

    // 按逻辑分组展示
    let labels = [
        ("名称", ""),
        ("代号", ""),
        ("持有者", ""),
        ("特性", ""),
        ("部署位置", ""),
        ("部署费用", ""),
        ("阻挡数", ""),
        ("嘲讽等级", ""),
        ("再部署时间", "s"),
        ("攻击速度", ""),
        ("攻击间隔", "s"),
    ];

    for (col, unit) in labels {
        if let Some(val) = row.get(col) {
            if val.is_empty() || val == "null" {
                continue;
            }
            out.push_str(&format!("{col}: {val}{unit}\n"));
        }
    }

    // 备注
    if let Some(raw) = row.get("备注") {
        let note = raw.trim();
        if !note.is_empty() && note != "null" {
            out.push_str(&format!("备注: {note}\n"));
        }
    }
}
// ═══════════════════════════════════════════════════════

// ═══════════════════════════════════════════════════════
// TokenExtraConfig —— tokens 支持的 extra 维度（仅状态指定）
// ═══════════════════════════════════════════════════════

#[derive(Debug, Default)]
struct TokenExtraConfig {
    elite_phase: Option<usize>,
    elite_level: Option<u32>,
    pot_level: Option<u32>,
    /// 技能= ：全部技能统一调整到的等级（1~7 为 LvN，8~10 为 专1~专3）
    skill_level: Option<u32>,
    /// 解析期无法识别的项（正文，输出时加 ✗ 前缀）
    errors: Vec<String>,
}

fn parse_token_extra(extra_val: &serde_json::Value) -> TokenExtraConfig {
    let arr = match extra_val.as_array() {
        Some(a) => a,
        None => return TokenExtraConfig::default(),
    };

    let mut cfg = TokenExtraConfig::default();
    for item in arr {
        // 非字符串项忽略（与旧行为一致）
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
                "等级" => parse_token_elite_status(value, &mut cfg),
                "潜能" => parse_token_potential_status(value, &mut cfg),
                "技能" => parse_token_skill_status(value, &mut cfg),
                _ => cfg.errors.push(format!(
                    "状态键 '{key}' 不支持：仅限 等级/潜能/技能，请用 键=值 形式"
                )),
            }
            continue;
        }

        // ── 不含 =：旧裸写语法与任意列名均不再支持 ──
        cfg.errors.push(format!(
            "extra 项 '{s}' 无法识别：状态请用 键=值 形式，合法键：等级/潜能/技能"
        ));
    }

    cfg
}

/// 状态指定 · 等级：等级=精二40级 / 等级=精一满级 / 等级=精二（省略等级=该阶段满级）。
fn parse_token_elite_status(value: &str, cfg: &mut TokenExtraConfig) {
    let rest = match value.strip_prefix("精") {
        Some(r) => r,
        None => {
            cfg.errors.push(format!(
                "等级 值 '{value}' 无法解析：请用 精N、精N满级 或 精N+等级，如 等级=精二40级、等级=精一满级、等级=精二"
            ));
            return;
        }
    };
    let (phase_str, level_str) = split_digits(rest);
    let mut matched = false;
    // "精0"/"精1"/"精2" — 无中文标记时 0/1/2 视为阶段数字
    if phase_str.is_empty() && matches!(level_str, "0" | "1" | "2") {
        cfg.elite_phase = Some(level_str.parse::<u32>().unwrap() as usize);
        matched = true;
    }
    if let Some(idx) = parse_chinese_or_digit(phase_str) {
        cfg.elite_phase = Some(idx as usize);
        matched = true;
    }
    if level_str == "满级" || level_str == "满" {
        cfg.elite_level = None;
        matched = true;
    } else if !level_str.is_empty() {
        if let Ok(n) = level_str.trim_end_matches('级').parse::<u32>() {
            cfg.elite_level = Some(n);
            matched = true;
        }
    }
    if !matched {
        cfg.errors.push(format!(
            "等级 值 '{value}' 无法解析：请用 等级=精二40级 / 等级=精一满级 / 等级=精二"
        ));
    }
}

/// 状态指定 · 潜能：潜能=满潜 / 潜能=五潜 / 潜能=0潜。
fn parse_token_potential_status(value: &str, cfg: &mut TokenExtraConfig) {
    if value == "满潜" {
        cfg.pot_level = Some(5);
        return;
    }
    if let Some(num_part) = value.strip_suffix("潜") {
        if num_part == "零" || num_part == "0" {
            cfg.pot_level = Some(0);
            return;
        }
        if let Some(n) = parse_chinese_or_digit(num_part) {
            cfg.pot_level = Some(n);
            return;
        }
    }
    cfg.errors.push(format!(
        "潜能 值 '{value}' 无法解析：请用 潜能=满潜 / 潜能=五潜 / 潜能=0潜"
    ));
}

/// 状态指定 · 技能：技能=3级（1~7级）/ 技能=专2（专1~专3），对全部技能统一调整。
fn parse_token_skill_status(value: &str, cfg: &mut TokenExtraConfig) {
    if let Some(r) = value.strip_prefix("专") {
        let digit_count = r.chars().take_while(|c| c.is_ascii_digit()).count();
        let num_str: String = r.chars().take(digit_count).collect();
        if let Ok(n) = num_str.parse::<u32>() {
            if (1..=3).contains(&n) {
                cfg.skill_level = Some(7 + n);
                return;
            }
        }
    } else if value.ends_with('级') {
        // "N级"（N=1~7）：级 必须位于末尾，前部为纯数字
        let num_str = &value[..value.len() - "级".len()];
        if let Ok(n) = num_str.parse::<u32>() {
            if (1..=7).contains(&n) {
                cfg.skill_level = Some(n);
                return;
            }
        }
    }
    cfg.errors.push(format!(
        "技能 值 '{value}' 无法解析：请用 技能=3级（1~7级） / 技能=专2（专1~专3），对全部技能统一调整"
    ));
}
// ═══════════════════════════════════════════════════════

// ═══════════════════════════════════════════════════════
// 数据库查询
// ═══════════════════════════════════════════════════════

fn query_token_by_name(
    db: &Database,
    name: &str,
    fields: &[String],
) -> Result<Option<HashMap<String, String>>, ToolError> {
    let result = db
        .query_filtered("tokens", fields, &[Filter::new("名称", FilterOp::Eq, name)], 1)
        .map_err(|e| ToolError::Execution(e))?;

    if !result.rows.is_empty() {
        return Ok(result.rows.into_iter().next());
    }

    let candidates = db
        .query_filtered(
            "tokens",
            fields,
            &[Filter::new("名称", FilterOp::Like, name)],
            0,
        )
        .map_err(|e| ToolError::Execution(e))?;

    if candidates.rows.is_empty() {
        return Ok(None);
    }

    let best = candidates
        .rows
        .into_iter()
        .min_by_key(|row| row.get("名称").map(|s| s.len()).unwrap_or(usize::MAX))
        .expect("candidates 非空");

    Ok(Some(best))
}

fn query_token_by_id(
    db: &Database,
    id: &str,
    fields: &[String],
) -> Result<Option<HashMap<String, String>>, ToolError> {
    let result = db
        .query_filtered("tokens", fields, &[Filter::new("tokenID", FilterOp::Eq, id)], 1)
        .map_err(|e| ToolError::Execution(e))?;
    Ok(result.rows.into_iter().next())
}
// ═══════════════════════════════════════════════════════

/// tokens 专属：纯精英化插值，无信赖/潜能/模组。
/// 只计算 keyframe 中实际存在的属性，而非预设的全套八项。
fn compute_token_stats(phase: &ElitePhase, level: u32) -> ComputedStats {
    let mut stats = ComputedStats::default();

    let (lo, hi) = find_bounding_keyframes(&phase.keyframes, level);
    let frac = if hi.level == lo.level {
        0.0
    } else {
        (level - lo.level) as f64 / (hi.level - lo.level) as f64
    };

    for attr in collect_attr_names(&phase.keyframes) {
        let lo_val = lo.attrs.get(&attr).copied().unwrap_or(0.0);
        let hi_val = hi.attrs.get(&attr).copied().unwrap_or(lo_val);
        let interp = if (hi_val - lo_val).abs() < 0.001 {
            lo_val
        } else {
            lo_val + (hi_val - lo_val) * frac
        };
        stats.attrs.insert(attr, (interp, 0.0, 0.0, 0.0));
    }

    stats.base_atk_interval = stats.attrs.get("攻击间隔").map(|v| v.0).unwrap_or(1.0);

    stats
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

    #[test]
    fn tool_metadata() {
        let tool = QueryToken;
        assert_eq!(tool.name(), "query_token");
        assert!(tool.description().contains("召唤物"));
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert!(params["properties"].get("fields").is_none(), "parameters 不应再有 fields");
        assert!(params["properties"].get("extra").is_some());
    }

    #[test]
    fn query_default_all() {
        // 无 extra：默认输出基本信息 + 属性；触手无技能和天赋，不应有对应区块
        let ctx = make_ctx();
        let tool = QueryToken;
        let result = tool.execute(&ctx, r#"{"names":["触手"]}"#).unwrap().summary;
        assert!(result.contains("触手"));
        assert!(result.contains("基本信息"));
        assert!(result.contains("属性"));
        assert!(!result.contains("技能列表"));
        assert!(!result.contains("── 天赋 ──"));
    }

    #[test]
    fn default_skills_top_level() {
        // 默认：全部技能最高级（不逐级展开）
        let ctx = make_ctx();
        let tool = QueryToken;
        let result = tool.execute(&ctx, r#"{"names":["机械水獭"]}"#).unwrap().summary;
        assert!(result.contains("迷惑装置"));
        assert!(result.contains("爆破回收"));
        assert!(result.contains("专3"), "默认最高级应为专3\n结果: {result}");
        assert!(!result.contains("Lv1"), "默认不应逐级展开\n结果: {result}");
    }

    #[test]
    fn status_spec_keeps_default_blocks() {
        // extra 状态指定 → 默认区块（基本信息/属性/技能）照常输出
        let ctx = make_ctx();
        let tool = QueryToken;
        let result = tool.execute(&ctx, r#"{"names":["机械水獭"],"extra":["潜能=五潜"]}"#).unwrap().summary;
        assert!(result.contains("── 基本信息"), "应包含基本信息\n结果: {result}");
        assert!(result.contains("── 属性"), "应包含属性\n结果: {result}");
        assert!(result.contains("── 技能列表"), "应包含技能列表\n结果: {result}");
        assert!(result.contains("[迷惑装置]"), "应包含技能\n结果: {result}");
    }

    // ── 状态指定（含 =）──

    #[test]
    fn extra_status_level_syntax() {
        let ctx = make_ctx();
        let tool = QueryToken;
        let r1 = tool
            .execute(&ctx, r#"{"names":["机械水獭"],"extra":["等级=精二40级"]}"#)
            .unwrap().summary;
        assert!(r1.contains("精2 40级"), "应显示精2 40级\n结果: {r1}");
        let r2 = tool
            .execute(&ctx, r#"{"names":["机械水獭"],"extra":["等级=精一满级"]}"#)
            .unwrap().summary;
        assert!(r2.contains("精1 满级"), "应显示精1 满级\n结果: {r2}");
        assert!(!r2.contains("精2"), "不应显示精2\n结果: {r2}");
        let r3 = tool
            .execute(&ctx, r#"{"names":["机械水獭"],"extra":["等级=精二"]}"#)
            .unwrap().summary;
        assert!(r3.contains("精2 满级"), "省略等级=该阶段满级\n结果: {r3}");
    }

    #[test]
    fn extra_status_potential_syntax() {
        let ctx = make_ctx();
        let tool = QueryToken;
        let r1 = tool
            .execute(&ctx, r#"{"names":["机械水獭"],"extra":["潜能=满潜"]}"#)
            .unwrap().summary;
        assert!(r1.contains("/ 5潜"), "满潜=5潜\n结果: {r1}");
        let r2 = tool
            .execute(&ctx, r#"{"names":["机械水獭"],"extra":["潜能=五潜"]}"#)
            .unwrap().summary;
        assert!(r2.contains("/ 5潜"), "五潜=5潜\n结果: {r2}");
        let r3 = tool
            .execute(&ctx, r#"{"names":["机械水獭"],"extra":["潜能=0潜"]}"#)
            .unwrap().summary;
        assert!(r3.contains("/ 0潜"), "0潜\n结果: {r3}");
        // 属性头完整形态（对齐 operator 风格）：阶段 + 等级 + 潜能
        let r4 = tool
            .execute(&ctx, r#"{"names":["机械水獭"],"extra":["等级=精二","潜能=五潜"]}"#)
            .unwrap().summary;
        assert!(r4.contains("精2 满级 / 5潜"), "属性头应含阶段/等级/潜能\n结果: {r4}");
    }

    #[test]
    fn extra_status_skill_syntax() {
        let ctx = make_ctx();
        let tool = QueryToken;
        // 技能=3级 → 全部技能 Lv3
        let r1 = tool
            .execute(&ctx, r#"{"names":["机械水獭"],"extra":["技能=3级"]}"#)
            .unwrap().summary;
        assert!(r1.contains("  Lv3:"), "应显示 Lv3\n结果: {r1}");
        assert!(!r1.contains("  Lv1:"), "不应显示 Lv1\n结果: {r1}");
        assert!(!r1.contains("  专"), "不应显示专精等级\n结果: {r1}");
        // 技能=专2 → 全部技能 专2
        let r2 = tool
            .execute(&ctx, r#"{"names":["机械水獭"],"extra":["技能=专2"]}"#)
            .unwrap().summary;
        assert!(r2.contains("  专2:"), "应显示专2\n结果: {r2}");
        assert!(!r2.contains("  专3:"), "不应显示专3\n结果: {r2}");
        assert!(!r2.contains("  Lv7:"), "不应显示 Lv7\n结果: {r2}");
    }

    #[test]
    fn extra_status_skill_applies_all_skills() {
        // 技能= 对全部技能统一生效：机械水獭两个技能均输出 Lv3
        let ctx = make_ctx();
        let tool = QueryToken;
        let result = tool
            .execute(&ctx, r#"{"names":["机械水獭"],"extra":["技能=3级"]}"#)
            .unwrap().summary;
        assert!(result.contains("[迷惑装置]"));
        assert!(result.contains("[爆破回收]"));
        assert_eq!(result.matches("  Lv3:").count(), 2, "两个技能均应 Lv3\n结果: {result}");
    }

    #[test]
    fn extra_status_skill_full_level_rejected() {
        // "技能=满级"已移除（歧义），技能=8级 / 专4 越界 → 报错指路
        let ctx = make_ctx();
        let tool = QueryToken;
        for bad in ["技能=满级", "技能=8级", "技能=专4"] {
            let result = tool
                .execute(&ctx, &format!(r#"{{"names":["机械水獭"],"extra":["{bad}"]}}"#))
                .unwrap().summary;
            assert!(result.contains("✗"), "'{bad}' 应报错\n结果: {result}");
            assert!(result.contains("技能 值"), "'{bad}' 应指路技能值语法\n结果: {result}");
        }
    }

    #[test]
    fn extra_status_invalid_key_rejected() {
        let ctx = make_ctx();
        let tool = QueryToken;
        let result = tool
            .execute(&ctx, r#"{"names":["机械水獭"],"extra":["职业=近卫"]}"#)
            .unwrap().summary;
        assert!(result.contains("✗"), "非法状态键应报错\n结果: {result}");
        assert!(result.contains("仅限 等级/潜能/技能"), "应指路合法键\n结果: {result}");
    }

    #[test]
    fn extra_status_invalid_value_rejected() {
        let ctx = make_ctx();
        let tool = QueryToken;
        let result = tool
            .execute(&ctx, r#"{"names":["机械水獭"],"extra":["等级=三级跳"]}"#)
            .unwrap().summary;
        assert!(result.contains("✗"), "非法状态值应报错\n结果: {result}");
        assert!(result.contains("无法解析"), "应指路\n结果: {result}");
    }

    // ── 旧语法废除 ──

    #[test]
    fn extra_legacy_syntax_rejected() {
        // 旧裸写语法不再解析 → 报错指路键=值
        let ctx = make_ctx();
        let tool = QueryToken;
        for legacy in ["精一20级", "满潜", "3级一技能", "逐级技能数据"] {
            let result = tool
                .execute(&ctx, &format!(r#"{{"names":["机械水獭"],"extra":["{legacy}"]}}"#))
                .unwrap().summary;
            assert!(result.contains("✗"), "旧语法 '{legacy}' 应报错\n结果: {result}");
            assert!(result.contains("键=值"), "旧语法 '{legacy}' 应指路键=值\n结果: {result}");
            assert!(result.contains("等级/潜能/技能"), "旧语法 '{legacy}' 应列出合法键\n结果: {result}");
        }
    }

    // ── 练度/数值相关（语法迁移后的旧用例）──

    #[test]
    fn extra_elite_override() {
        // extra 状态指定 等级=精一满级 → 属性块用精1
        let ctx = make_ctx();
        let tool = QueryToken;
        let result = tool.execute(&ctx, r#"{"names":["触手"],"extra":["等级=精一满级"]}"#).unwrap().summary;
        assert!(result.contains("精1 满级"), "应显示精1 满级\n结果: {result}");
        assert!(!result.contains("精2"));
    }

    #[test]
    fn extra_elite_recomputes_talent() {
        // 状态指定 等级=精一 → 属性与天赋按精1 重算，不出现精2 条件
        let ctx = make_ctx();
        let tool = QueryToken;
        let result = tool.execute(&ctx, r#"{"names":["镜中虚影"],"extra":["等级=精一满级"]}"#).unwrap().summary;
        assert!(result.contains("── 属性"), "应输出属性区块\n结果: {result}");
        assert!(result.contains("精1"), "属性应为精1\n结果: {result}");
        assert!(!result.contains("精2"), "精1 练度不应出现精2\n结果: {result}");
    }

    // ── 边界 ──

    #[test]
    fn query_by_id() {
        let ctx = make_ctx();
        let tool = QueryToken;
        let result = tool.execute(&ctx, r#"{"ids":["token_10007_phatom_twin"]}"#).unwrap().summary;
        assert!(result.contains("镜中虚影"));
    }

    #[test]
    fn not_found() {
        let ctx = make_ctx();
        let tool = QueryToken;
        let result = tool.execute(&ctx, r#"{"names":["绝对不存在的召唤物xyz"]}"#).unwrap().summary;
        assert!(result.contains("未找到"));
    }

    #[test]
    fn no_db_returns_error() {
        let ctx = ToolContext::new();
        let tool = QueryToken;
        let result = tool.execute(&ctx, r#"{"names":["触手"]}"#);
        assert!(result.is_err());
    }

    #[test]
    fn missing_names_and_ids() {
        let ctx = make_ctx();
        let tool = QueryToken;
        let result = tool.execute(&ctx, r#"{}"#);
        assert!(result.is_err());
    }

    #[test]
    fn both_ids_and_names_error() {
        let ctx = make_ctx();
        let tool = QueryToken;
        let result = tool.execute(&ctx, r#"{"ids":["a"],"names":["b"]}"#);
        assert!(result.is_err());
    }

    #[test]
    fn token_with_talent() {
        // 有天赋的召唤物：镜中虚影
        let ctx = make_ctx();
        let tool = QueryToken;
        let result = tool.execute(&ctx, r#"{"names":["镜中虚影"]}"#).unwrap().summary;
        assert!(result.contains("── 天赋 ──"), "镜中虚影应有天赋\n结果: {result}");
    }

    // ── 手动测试入口 ──

    #[test]
    fn show_output() {
        fn call(ctx: &ToolContext, params: &str) -> String {
            let tool = QueryToken;
            match tool.execute(ctx, params) {
                Ok(s) => s.summary,
                Err(e) => format!("ERROR: {e}"),
            }
        }

        let ctx = make_ctx();

        // 通过环境变量驱动：
        //   PRTS_TOKEN_NAME="触手"
        //   PRTS_TOKEN_ID="token_10003_cqbw_p_1"
        //   PRTS_TOKEN_EXTRA="等级=精一满级,技能=3级"
        if let Ok(name) = std::env::var("PRTS_TOKEN_NAME") {
            let mut params = serde_json::json!({ "names": [name] });
            if let Ok(estr) = std::env::var("PRTS_TOKEN_EXTRA") {
                let ev: Vec<String> = if estr.is_empty() { vec![] } else { estr.split(',').map(|s| s.trim().to_string()).collect() };
                params["extra"] = serde_json::json!(ev);
            }
            println!("{}", call(&ctx, &params.to_string()));
            return;
        }

        if let Ok(id) = std::env::var("PRTS_TOKEN_ID") {
            let mut params = serde_json::json!({ "ids": [id] });
            if let Ok(estr) = std::env::var("PRTS_TOKEN_EXTRA") {
                let ev: Vec<String> = if estr.is_empty() { vec![] } else { estr.split(',').map(|s| s.trim().to_string()).collect() };
                params["extra"] = serde_json::json!(ev);
            }
            println!("{}", call(&ctx, &params.to_string()));
            return;
        }

        // 无环境变量时跑几个典型用例
        let cases: &[(&str, &str, Option<&[&str]>)] = &[
            ("1. 触手 · 默认", "触手", None),
            ("2. 机械水獭 · 默认（含技能）", "机械水獭", None),
            ("3. 机械水獭 · 技能=3级", "机械水獭", Some(&["技能=3级"])),
            ("4. 机械水獭 · 等级=精一满级", "机械水獭", Some(&["等级=精一满级"])),
            ("5. 镜中虚影 · 默认（含天赋）", "镜中虚影", None),
            ("6. 机械水獭 · 旧语法报错示例", "机械水獭", Some(&["3级一技能"])),
        ];
        for (label, name, extra) in cases {
            let mut p = serde_json::json!({ "names": [name] });
            if let Some(e) = extra { p["extra"] = serde_json::json!(e); }
            println!("========== {label} ==========");
            println!("{}", call(&ctx, &p.to_string()));
            println!();
        }
    }
}
