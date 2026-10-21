//! query_trap —— 关卡装置查询工具
//!
//! 语义：输入名称或 ID → 返回装置的属性、技能、天赋。
//! traps 表结构与 operators/tokens 高度相似（相同的 JSON 列结构），
//! 但没有模组、潜能、信赖机制。装置全部只有 1 个精英化阶段（精0）。
//!
//! ## 参数契约
//!
//! `names` / `ids`（数组，互斥，必填其一）+ `extra`（数组，可选）。
//! 不提供 `fields` 参数；不提供潜能、技能相关指定、附加列。
//!
//! ## 默认行为
//!
//! 默认返回：基本信息、属性、技能列表、天赋（现有效果保持）。
//! 属性默认按阶段最高等级计算。
//!
//! | 维度   | 默认        | extra 可切换          |
//! |--------|------------|----------------------|
//! | 等级   | 最高等级     | `等级=3`（超上限 clamp 到最高等级） |
//! | 技能   | 全部技能最高级 | （不提供指定）         |
//! | 天赋   | 精0/0潜 最优备选 | （不提供指定）         |
//!
//! ## extra 语法
//!
//! 含 `=` 为**状态指定**：键限 等级 一种（纯数字，`级` 后缀可选）。
//! 不含 `=` 的项报错并指路键=值形式；旧裸写语法（`精N`、`满潜`、`一技能` 等）不再解析。

use crate::tools::common::{self, Target};
use crate::tools::common_fmt::{
    collect_attr_names, find_bounding_keyframes, format_skill_expanded, format_stats,
    format_talents, parse_json, pick_talent_alt, ComputedStats, ElitePhase, Skill, Talent,
};
use crate::tools::display::{array_summary, targets_summary};
use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

use std::collections::HashMap;

pub struct QueryTrap;

impl Tool for QueryTrap {
    fn name(&self) -> &str {
        "query_trap"
    }

    fn description(&self) -> &str {
        "按名称或 ID 查询关卡装置（trap），支持批量（ids 主键精确 / names 精确优先、未中模糊取最佳，二选一）。默认返回：基本信息、属性、技能列表、天赋。装置无精英化，默认按最高等级计算属性。extra 状态指定（等级）见参数说明。仅 traps 表"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "装置 ID 列表，如 [“trap_001_crate”]。与 names 二选一"
                },
                "names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "装置名称列表，如 [“障碍物”]。与 ids 二选一"
                },
                "extra": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "状态指定，可选，必须含 =（键限 等级）：等级=3 或 3级（纯数字，级 后缀可选）；装置无精英化，等级直接参与属性计算，超过阶段上限按最高等级计算。指定即替换默认等级，属性随之重算"
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
        let extra = parse_trap_extra(&v["extra"]);

        let db = ctx
            .db
            .as_ref()
            .ok_or_else(|| ToolError::Execution("数据库未连接".into()))?;

        // ── 3. 固定查询完整列（不裁剪）──
        let all_fields: Vec<String> = TRAP_ALWAYS_COLS.iter().map(|s| s.to_string()).collect();

        // ── 4. 逐个查询并格式化 ──
        let mut output = String::new();
        let mut all_rows: Vec<HashMap<String, String>> = Vec::new();

        for (i, target) in targets.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }

            let row = match target {
                Target::Id(id) => common::query_by_id(db, "traps", "装置ID", id, &all_fields)?,
                Target::Name(name) => common::query_by_name(db, "traps", name, &all_fields)?,
            };

            match row {
                Some(row) => {
                    all_rows.push(row.clone());
                    format_one_trap(&row, &extra, &mut output);
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

const TRAP_ALWAYS_COLS: &[&str] = &[
    "名称",
    "代号",
    "阻挡半径",
    "再部署时间",
    "部署费用",
    "嘲讽等级",
    "阻挡数",
    "技力回复速度",
    "攻击间隔",
    "元素抗性",
    "损伤抗性",
    "装置机制",
    "精英化阶段",
    "技能列表",
    "天赋列表",
];

// ═══════════════════════════════════════════════════════
// 单装置格式化
// ═══════════════════════════════════════════════════════

fn format_one_trap(row: &HashMap<String, String>, extra: &TrapExtraConfig, out: &mut String) {
    let elite_phases: Vec<ElitePhase> = parse_json(row, "精英化阶段").unwrap_or_default();
    let skills: Vec<Skill> = parse_json(row, "技能列表").unwrap_or_default();
    let talents: Vec<Talent> = parse_json(row, "天赋列表").unwrap_or_default();

    let trap_name = row.get("名称").map(|s| s.as_str()).unwrap_or("未知");

    // 装置无精英化概念：恒精0；等级= 直接作用，超上限 clamp 到最高等级
    let phase_idx = elite_phases.len().saturating_sub(1);
    let phase = if let Some(p) = elite_phases.get(phase_idx) {
        p
    } else {
        return;
    };
    let level = extra.level.unwrap_or(phase.max_level).min(phase.max_level);

    out.push_str(&format!("━━━ {trap_name} ━━━\n"));

    // ── 基本信息 ──
    format_trap_basic_info(row, out);

    // ── 属性（等级= 指定或最高等级）──
    out.push_str(&format!("\n── 属性（精0 {level}级）──\n"));
    let stats = compute_trap_stats(phase, level);
    format_stats(&stats, out);
    let range_str = phase.range_id.as_deref().unwrap_or("无");
    out.push_str(&format!("攻击范围: {range_str}\n"));

    // ── 技能（全部技能最高级）──
    if !skills.is_empty() {
        out.push_str("\n── 技能列表 ──\n");
        for skill in &skills {
            out.push('\n');
            format_skill_expanded(skill, None, false, out);
        }
    }

    // ── 天赋（精0/0潜 最优备选，不展开备选列表）──
    if !talents.is_empty() {
        let pot_default = 0;
        // 先检查是否有匹配当前练度的备选，避免输出空标题
        if talents
            .iter()
            .any(|t| pick_talent_alt(&t.alternatives, phase_idx as u32, pot_default).is_some())
        {
            out.push_str("\n── 天赋 ──\n");
            format_talents(
                &talents,
                None,
                None,
                phase_idx,
                pot_default,
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

fn format_trap_basic_info(row: &HashMap<String, String>, out: &mut String) {
    out.push_str("\n── 基本信息 ──\n");

    let labels = [
        ("名称", ""),
        ("代号", ""),
        ("阻挡半径", ""),
        ("再部署时间", "s"),
        ("部署费用", ""),
        ("嘲讽等级", ""),
        ("阻挡数", ""),
        ("技力回复速度", "/s"),
        ("攻击间隔", "s"),
        ("元素抗性", ""),
        ("损伤抗性", ""),
    ];

    for (col, unit) in labels {
        if let Some(val) = row.get(col) {
            if val.is_empty() || val == "null" {
                continue;
            }
            out.push_str(&format!("{col}: {val}{unit}\n"));
        }
    }

    // 装置机制
    if let Some(raw) = row.get("装置机制") {
        let note = raw.trim();
        if !note.is_empty() && note != "null" {
            out.push_str(&format!("装置机制: {note}\n"));
        }
    }
}
// ═══════════════════════════════════════════════════════

// ═══════════════════════════════════════════════════════
// TrapExtraConfig —— traps 支持的 extra 维度（仅状态指定）
// ═══════════════════════════════════════════════════════

#[derive(Debug, Default)]
struct TrapExtraConfig {
    /// 等级= ：装置等级（纯数字，级 后缀可选）
    level: Option<u32>,
    /// 解析期无法识别的项（正文，输出时加 ✗ 前缀）
    errors: Vec<String>,
}

fn parse_trap_extra(extra_val: &serde_json::Value) -> TrapExtraConfig {
    let arr = match extra_val.as_array() {
        Some(a) => a,
        None => return TrapExtraConfig::default(),
    };

    let mut cfg = TrapExtraConfig::default();
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
                "等级" => parse_trap_level_status(value, &mut cfg),
                _ => cfg
                    .errors
                    .push(format!("状态键 '{key}' 不支持：仅限 等级，请用 键=值 形式")),
            }
            continue;
        }

        // ── 不含 =：旧裸写语法与任意列名均不再支持 ──
        cfg.errors.push(format!(
            "extra 项 '{s}' 无法识别：状态请用 键=值 形式，合法键：等级"
        ));
    }

    cfg
}

/// 状态指定 · 等级：等级=3 / 等级=3级（纯数字，级 后缀可选）。
fn parse_trap_level_status(value: &str, cfg: &mut TrapExtraConfig) {
    let num = value.trim_end_matches('级');
    match num.parse::<u32>() {
        Ok(n) => cfg.level = Some(n),
        Err(_) => cfg.errors.push(format!(
            "等级 值 '{value}' 无法解析：请用 等级=数字（可带 级 后缀），如 等级=3"
        )),
    }
}
// ═══════════════════════════════════════════════════════

// ═══════════════════════════════════════════════════════
// 数据库查询
// ═══════════════════════════════════════════════════════

// ═══════════════════════════════════════════════════════

/// traps 专属：纯精英化插值（装置仅精0 一层）。
/// 只计算 keyframe 中实际存在的属性，而非预设的全套八项。
fn compute_trap_stats(phase: &ElitePhase, level: u32) -> ComputedStats {
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
    use crate::db::Database;
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
        let tool = QueryTrap;
        assert_eq!(tool.name(), "query_trap");
        assert!(tool.description().contains("装置"));
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        assert!(
            params["properties"].get("fields").is_none(),
            "parameters 不应再有 fields"
        );
        assert!(params["properties"].get("extra").is_some());
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn query_default_all() {
        let ctx = make_ctx();
        let tool = QueryTrap;
        let result = tool
            .execute(&ctx, r#"{"names":["障碍物"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("障碍物"));
        assert!(result.contains("基本信息"));
        assert!(result.contains("属性"));
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn status_spec_keeps_default_blocks() {
        // extra 状态指定 → 默认区块（基本信息/属性/技能）照常输出
        let ctx = make_ctx();
        let tool = QueryTrap;
        let result = tool
            .execute(&ctx, r#"{"names":["障碍物"],"extra":["等级=3"]}"#)
            .unwrap()
            .summary;
        assert!(
            result.contains("── 基本信息"),
            "应包含基本信息\n结果: {result}"
        );
        assert!(result.contains("── 属性"), "应包含属性\n结果: {result}");
        assert!(
            result.contains("── 技能列表"),
            "应包含技能列表\n结果: {result}"
        );
        assert!(result.contains("[阻挡路线]"), "应包含技能\n结果: {result}");
    }

    // ── 状态指定（含 =）──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_status_level_syntax() {
        let ctx = make_ctx();
        let tool = QueryTrap;
        let r1 = tool
            .execute(&ctx, r#"{"names":["障碍物"],"extra":["等级=3"]}"#)
            .unwrap()
            .summary;
        assert!(r1.contains("精0 3级"), "应显示精0 3级\n结果: {r1}");
        // 级 后缀等价（宽容处理）
        let r2 = tool
            .execute(&ctx, r#"{"names":["障碍物"],"extra":["等级=3级"]}"#)
            .unwrap()
            .summary;
        assert!(r2.contains("精0 3级"), "级 后缀应可解析\n结果: {r2}");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_status_level_clamp() {
        // 等级=99 超上限 → clamp 到最高等级（障碍物最高 30 级）
        let ctx = make_ctx();
        let tool = QueryTrap;
        let result = tool
            .execute(&ctx, r#"{"names":["障碍物"],"extra":["等级=99"]}"#)
            .unwrap()
            .summary;
        assert!(
            result.contains("精0 30级"),
            "应 clamp 到最高等级\n结果: {result}"
        );
        assert!(!result.contains("99级"), "不应显示 99 级\n结果: {result}");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_status_invalid_key_rejected() {
        let ctx = make_ctx();
        let tool = QueryTrap;
        let result = tool
            .execute(&ctx, r#"{"names":["障碍物"],"extra":["潜能=满潜"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("✗"), "非法状态键应报错\n结果: {result}");
        assert!(result.contains("仅限 等级"), "应指路合法键\n结果: {result}");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_status_invalid_value_rejected() {
        let ctx = make_ctx();
        let tool = QueryTrap;
        let result = tool
            .execute(&ctx, r#"{"names":["障碍物"],"extra":["等级=abc"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("✗"), "非法状态值应报错\n结果: {result}");
        assert!(result.contains("无法解析"), "应指路\n结果: {result}");
    }

    // ── 旧语法废除 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_legacy_syntax_rejected() {
        // 旧裸写语法不再解析 → 报错指路键=值
        let ctx = make_ctx();
        let tool = QueryTrap;
        for legacy in ["精一", "精一20级", "满潜", "一技能", "专2一技能"] {
            let result = tool
                .execute(
                    &ctx,
                    &format!(r#"{{"names":["障碍物"],"extra":["{legacy}"]}}"#),
                )
                .unwrap()
                .summary;
            assert!(
                result.contains("✗"),
                "旧语法 '{legacy}' 应报错\n结果: {result}"
            );
            assert!(
                result.contains("键=值"),
                "旧语法 '{legacy}' 应指路键=值\n结果: {result}"
            );
            assert!(
                result.contains("合法键：等级"),
                "旧语法 '{legacy}' 应列出合法键\n结果: {result}"
            );
        }
    }

    // ── 边界 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn query_by_id() {
        let ctx = make_ctx();
        let tool = QueryTrap;
        let result = tool
            .execute(&ctx, r#"{"ids":["trap_001_crate"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("障碍物"));
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn not_found() {
        let ctx = make_ctx();
        let tool = QueryTrap;
        let result = tool
            .execute(&ctx, r#"{"names":["绝对不存在的装置xyz"]}"#)
            .unwrap()
            .summary;
        assert!(result.contains("未找到"));
    }

    #[test]
    fn no_db_returns_error() {
        let ctx = ToolContext::new();
        let tool = QueryTrap;
        let result = tool.execute(&ctx, r#"{"names":["障碍物"]}"#);
        assert!(result.is_err());
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn missing_names_and_ids() {
        let ctx = make_ctx();
        let tool = QueryTrap;
        let result = tool.execute(&ctx, r#"{}"#);
        assert!(result.is_err());
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn both_ids_and_names_error() {
        let ctx = make_ctx();
        let tool = QueryTrap;
        let result = tool.execute(&ctx, r#"{"ids":["a"],"names":["b"]}"#);
        assert!(result.is_err());
    }

    // ── 手动测试入口 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn show_output() {
        fn call(ctx: &ToolContext, params: &str) -> String {
            let tool = QueryTrap;
            match tool.execute(ctx, params) {
                Ok(s) => s.summary,
                Err(e) => format!("ERROR: {e}"),
            }
        }

        let ctx = make_ctx();

        // 通过环境变量驱动：
        //   PRTS_TRAP_NAME="障碍物"
        //   PRTS_TRAP_ID="trap_001_crate"
        //   PRTS_TRAP_EXTRA="等级=3"
        if let Ok(name) = std::env::var("PRTS_TRAP_NAME") {
            let mut params = serde_json::json!({ "names": [name] });
            if let Ok(estr) = std::env::var("PRTS_TRAP_EXTRA") {
                let ev: Vec<String> = if estr.is_empty() {
                    vec![]
                } else {
                    estr.split(',').map(|s| s.trim().to_string()).collect()
                };
                params["extra"] = serde_json::json!(ev);
            }
            println!("{}", call(&ctx, &params.to_string()));
            return;
        }

        if let Ok(id) = std::env::var("PRTS_TRAP_ID") {
            let mut params = serde_json::json!({ "ids": [id] });
            if let Ok(estr) = std::env::var("PRTS_TRAP_EXTRA") {
                let ev: Vec<String> = if estr.is_empty() {
                    vec![]
                } else {
                    estr.split(',').map(|s| s.trim().to_string()).collect()
                };
                params["extra"] = serde_json::json!(ev);
            }
            println!("{}", call(&ctx, &params.to_string()));
            return;
        }

        // 无环境变量时跑几个典型用例
        let cases: &[(&str, &str, Option<&[&str]>)] = &[
            ("1. 障碍物 · 默认", "障碍物", None),
            ("2. 障碍物 · 等级=3", "障碍物", Some(&["等级=3"])),
            ("3. 障碍物 · 等级=99（clamp）", "障碍物", Some(&["等级=99"])),
            ("4. 指挥终端 · 默认", "指挥终端", None),
            ("5. 道路障碍物 · 默认", "道路障碍物", None),
            ("6. 障碍物 · 旧语法报错示例", "障碍物", Some(&["精一"])),
        ];
        for (label, name, extra) in cases {
            let mut p = serde_json::json!({ "names": [name] });
            if let Some(e) = extra {
                p["extra"] = serde_json::json!(e);
            }
            println!("========== {label} ==========");
            println!("{}", call(&ctx, &p.to_string()));
            println!();
        }
    }
}
