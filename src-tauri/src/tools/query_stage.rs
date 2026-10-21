//! query_stage —— 关卡查询工具
//!
//! 语义：输入名称或编号 → 返回关卡属性。
//! 与 search_entities 互为逆操作：
//!   query_stage：身份 → 属性（名称查询）
//!   search_entities：条件 → 身份列表（集合筛选）
//!
//! ## 匹配策略
//!
//! 三步级联 fallback：
//! 1. 关卡名精确匹配 → 返回全部命中行（含 #f# 变体，LIMIT 5）
//! 2. 关卡编号精确匹配 → 返回全部命中行（LIMIT 5）
//! 3. 关卡名 LIKE → 取名称最短的单条最佳匹配
//!
//! 返回多行的原因：同一关卡的普通版与磨难险地版（#f#）是
//! 不同规则配置（关卡描述、敌人列表可能不同），非简单的级别递增。
//!
//! ## 默认 7 列
//!
//! 默认返回 关卡ID、关卡名、关卡编号、关卡描述、关卡类型、情报、levelId。
//! 关卡名仅出现在区块标题（header），正文不重复打印。
//! levelId 是连接 stages ↔ levels 的桥梁，可继续用于 query_level。
//!
//! ## extra 附加列
//!
//! extra 仅接受列名（无状态键）。命中全部表列中的非默认列时附加输出；
//! 默认列重复指定忽略；含 `=` 的项与未识别列名报错指路（✗ 行，不阻塞）。
//!
//! ## 特殊列加工
//!
//! | 列名     | 原始格式                              | 输出格式                    |
//! |---------|--------------------------------------|---------------------------|
//! | 敌人列表 | JSON `[{"名称":"x","敌人ID":"enemy_x","级别":0,...}]` | 紧凑列表（含敌人ID与覆写属性） |
//! | 情报    | PRTS wiki 文本                        | 经 wiki::clean_wiki 清洗    |
//! | 关卡描述 | 含 `\n` 的文本                        | 自然换行保留                |
//! | 解锁条件 | JSON `[{stageId,completeState}]`     | 编号列表 + 中文状态         |

use crate::db::{Database, Filter, FilterOp};
use crate::tools::common::{self, Target};
use crate::tools::display::{array_summary, targets_summary};
use crate::tools::wiki;
use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

use serde::Deserialize;
use std::collections::HashMap;

/// stages 表完整列名（附加列识别与查询字段共用）。
const ALL_COLUMNS: &[&str] = &[
    "关卡ID",
    "关卡名",
    "关卡描述",
    "关卡编号",
    "关卡类型",
    "解锁条件",
    "levelId",
    "推荐等级",
    "理智消耗",
    "部署上限",
    "初始费用",
    "费用上限",
    "目标点耐久",
    "敌人列表",
    "情报",
    "待处理目标数量",
];

/// 默认 7 列：附加列中重复指定时忽略（不重复输出、不报错）。
const DEFAULT_COLUMNS: &[&str] = &[
    "关卡ID",
    "关卡名",
    "关卡编号",
    "关卡描述",
    "关卡类型",
    "情报",
    "levelId",
];

pub struct QueryStage;

impl Tool for QueryStage {
    fn name(&self) -> &str {
        "query_stage"
    }

    fn description(&self) -> &str {
        "按名称、编号或 ID 查询关卡信息，支持批量（ids 主键精确；names 三步级联：关卡名精确→编号精确→关卡名 LIKE 取最佳）。同名或编号可能返回多条（普通版/磨难险地版等难度），全部保留。默认返回 7 列：关卡ID、关卡名、关卡编号、关卡描述、关卡类型、情报、levelId；levelId 可续查 query_level。extra 仅附加列，无状态键。仅 stages 表"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "关卡 ID 列表，如 [“main_01-07”]。与 names 二选一"
                },
                "names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "关卡名称或编号列表，如 [“暴君”, “1-7”]。与 ids 二选一"
                },
                "extra": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "可选，仅附加列（关卡无状态键）。常用列：敌人列表、部署上限、初始费用、费用上限、目标点耐久、待处理目标数量；其余表列命中即返回全部信息。默认 7 列重复指定忽略。完整列名见知识库《关卡表结构》条目"
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

        // ── 2. 解析 extra（仅附加列，无状态键）──
        let extra = parse_extra(&v["extra"]);

        let db = ctx
            .db
            .as_ref()
            .ok_or_else(|| ToolError::Execution("数据库未连接".into()))?;

        // ── 3. 固定查询完整列（不裁剪）──
        let query_fields: Vec<String> = ALL_COLUMNS.iter().map(|s| s.to_string()).collect();

        // ── 4. 逐个查询并格式化 ──
        let mut output = String::new();
        // canonical 收集：原始行数据（presentation 之外的结构化值）
        let mut all_rows: Vec<HashMap<String, String>> = Vec::new();

        for (i, target) in targets.iter().enumerate() {
            if i > 0 {
                output.push('\n');
            }

            let rows = match target {
                Target::Id(id) => query_stage_by_id(db, id, &query_fields)?,
                Target::Name(name) => query_stage_by_name(db, name, &query_fields)?,
            };

            if rows.is_empty() {
                let label = match target {
                    Target::Id(id) => format!("ID: {id}"),
                    Target::Name(name) => format!("名称/编号: {name}"),
                };
                output.push_str(&format!("✗ {label}：未找到\n"));
                continue;
            }

            all_rows.extend(rows.iter().cloned());
            let total = rows.len();
            for (idx, row) in rows.iter().enumerate() {
                if idx > 0 {
                    output.push('\n');
                }
                format_one_stage(row, &extra, idx, total, &mut output);
            }
        }

        Ok(ToolOutput::json(
            serde_json::json!({ "rows": all_rows, "dropped_fields": [] }),
            output.trim_end(),
        ))
    }
}

// ═══════════════════════════════════════════════════════
// 查询函数
// ═══════════════════════════════════════════════════════

/// 按 ID 精确查询关卡（可能返回多条变体）。
fn query_stage_by_id(
    db: &Database,
    id: &str,
    query_fields: &[String],
) -> Result<Vec<HashMap<String, String>>, ToolError> {
    let result = db
        .query_filtered(
            "stages",
            query_fields,
            &[Filter::new("关卡ID", FilterOp::Eq, id)],
            5,
        )
        .map_err(ToolError::Execution)?;
    Ok(result.rows)
}

/// 按名称/编号三步级联查询。
fn query_stage_by_name(
    db: &Database,
    name: &str,
    query_fields: &[String],
) -> Result<Vec<HashMap<String, String>>, ToolError> {
    // 1. 关卡名精确
    let r = db
        .query_filtered(
            "stages",
            query_fields,
            &[Filter::new("关卡名", FilterOp::Eq, name)],
            5,
        )
        .map_err(ToolError::Execution)?;
    if !r.rows.is_empty() {
        return Ok(r.rows);
    }

    // 2. 关卡编号精确
    let r = db
        .query_filtered(
            "stages",
            query_fields,
            &[Filter::new("关卡编号", FilterOp::Eq, name)],
            5,
        )
        .map_err(ToolError::Execution)?;
    if !r.rows.is_empty() {
        return Ok(r.rows);
    }

    // 3. 关卡名 LIKE 取最短
    let r = db
        .query_filtered(
            "stages",
            query_fields,
            &[Filter::new("关卡名", FilterOp::Like, name)],
            0,
        )
        .map_err(ToolError::Execution)?;
    let best = r
        .rows
        .into_iter()
        .min_by_key(|row| row.get("关卡名").map(|s| s.len()).unwrap_or(usize::MAX));
    Ok(best.into_iter().collect())
}

// ═══════════════════════════════════════════════════════
// extra 解析
// ═══════════════════════════════════════════════════════

#[derive(Debug, Default)]
struct ExtraConfig {
    /// 附加列（不含 =，命中 ALL_COLUMNS 且非默认列）
    extra_columns: Vec<String>,
    /// 解析期无法识别的项（正文，输出时加 ✗ 前缀）
    errors: Vec<String>,
}

/// 解析 extra：逐项处理，非字符串/空串忽略；含 = 的项入 errors（关卡无状态键）；
/// 不含 = 的项命中 ALL_COLUMNS → 非默认列入 extra_columns、默认列忽略；未命中入 errors。
fn parse_extra(extra_val: &serde_json::Value) -> ExtraConfig {
    let arr = match extra_val.as_array() {
        Some(a) => a,
        None => return ExtraConfig::default(),
    };

    let mut cfg = ExtraConfig::default();
    for item in arr {
        // 非字符串项忽略（与旧行为一致）
        let s = match item.as_str() {
            Some(s) => s.trim(),
            None => continue,
        };
        if s.is_empty() {
            continue;
        }

        // 含 = 的状态指定：关卡无状态键，报错指路
        if s.contains('=') {
            cfg.errors.push(format!(
                "状态指定 '{s}' 不支持：关卡无状态键，extra 仅接受列名（如 敌人列表、部署上限），完整列名见知识库《关卡表结构》条目"
            ));
            continue;
        }

        // 不含 = 的附加列
        if ALL_COLUMNS.contains(&s) {
            if !DEFAULT_COLUMNS.contains(&s) {
                cfg.extra_columns.push(s.to_string());
            }
            // 默认列重复指定：忽略
        } else {
            cfg.errors.push(format!(
                "无法识别的 extra 项 '{s}'：关卡无状态指定，extra 仅接受列名，完整列名见知识库《关卡表结构》条目"
            ));
        }
    }

    cfg
}

// ═══════════════════════════════════════════════════════
// 格式化
// ═══════════════════════════════════════════════════════

fn format_one_stage(
    row: &HashMap<String, String>,
    extra: &ExtraConfig,
    idx: usize,
    total: usize,
    out: &mut String,
) {
    let stage_name = row.get("关卡名").map(|s| s.as_str()).unwrap_or("未知");
    let stage_id = row.get("关卡ID").map(|s| s.as_str()).unwrap_or("?");
    let stage_num = row.get("关卡编号").map(|s| s.as_str()).unwrap_or("?");
    let stage_type = row.get("关卡类型").map(|s| s.as_str()).unwrap_or("?");
    let level_id = row.get("levelId").map(|s| s.as_str()).unwrap_or("?");

    if total > 1 {
        out.push_str(&format!(
            "━━━ {stage_name}（{}/{} | {stage_id}）━━━\n",
            idx + 1,
            total
        ));
    } else {
        out.push_str(&format!("━━━ {stage_name}（{stage_id}）━━━\n"));
    }

    // 默认 7 列：关卡ID、关卡名（header 已含）、关卡编号、关卡描述、关卡类型、情报、levelId
    out.push_str(&format!("关卡ID: {stage_id}\n"));
    out.push_str(&format!("关卡编号: {stage_num}\n"));
    out.push_str(&format!("关卡类型: {stage_type}\n"));
    out.push_str(&format!("levelId: {level_id}\n"));

    match row.get("关卡描述") {
        Some(raw) if !raw.is_empty() && raw != "null" => {
            out.push_str(&format!("关卡描述:\n{raw}\n"));
        }
        _ => out.push_str("关卡描述: 无\n"),
    }

    match row.get("情报") {
        Some(raw) if !raw.is_empty() && raw != "null" => {
            let cleaned = wiki::clean_wiki(raw);
            out.push_str(&format!("情报:\n{cleaned}\n"));
        }
        _ => out.push_str("情报: 无\n"),
    }

    // 附加列（按 extra_columns 顺序）
    for field in &extra.extra_columns {
        out.push('\n');
        match field.as_str() {
            "敌人列表" => format_enemy_list(row, out),
            "解锁条件" => format_unlock(row, out),
            _ => match row.get(field) {
                Some(raw) if !raw.is_empty() && raw != "null" => {
                    out.push_str(&format!("{field}: {raw}\n"));
                }
                _ => out.push_str(&format!("{field}: 无\n")),
            },
        }
    }

    // extra 解析期错误（逐条标注，不阻塞其它有效结果）
    for err in &extra.errors {
        out.push_str(&format!("✗ {err}\n"));
    }
}

// ═══════════════════════════════════════════════════════
// 特殊列格式化
// ═══════════════════════════════════════════════════════

/// 敌人列表 JSON → 紧凑编号列表。
///
/// `[{"名称":"士兵","敌人ID":"enemy_1002_nsabr","级别":0,"覆写属性":{"防御力":30}}]` →
/// ```ignore
/// 敌人列表:
///   1. 士兵[enemy_1002_nsabr] Lv0（覆写：防御力 30）
///   2. 源石虫[enemy_1007_slime] Lv0
/// ```
/// 敌人ID 放在方括号里，用于同名敌人时精确查询（同名有 66 组，按名字查会撞）。
fn format_enemy_list(row: &HashMap<String, String>, out: &mut String) {
    let raw = match row.get("敌人列表") {
        Some(v) if !v.is_empty() && v != "null" => v,
        _ => {
            out.push_str("敌人列表: 无\n");
            return;
        }
    };

    #[derive(Deserialize)]
    struct EnemyEntry {
        #[serde(rename = "名称")]
        name: String,
        #[serde(rename = "敌人ID")]
        id: Option<String>,
        #[serde(rename = "级别")]
        level: Option<u32>,
        #[serde(rename = "覆写属性")]
        overrides: Option<HashMap<String, serde_json::Value>>,
    }

    let entries: Vec<EnemyEntry> = match serde_json::from_str(raw) {
        Ok(arr) => arr,
        Err(_) => {
            out.push_str(&format!("敌人列表: {raw}\n"));
            return;
        }
    };

    if entries.is_empty() {
        out.push_str("敌人列表: 无\n");
        return;
    }

    out.push_str("敌人列表:\n");
    for (i, entry) in entries.iter().enumerate() {
        let lv = entry.level.unwrap_or(0);
        let mut line = match entry.id {
            Some(ref id) if !id.is_empty() => format!("  {}. {}[{id}] Lv{lv}", i + 1, entry.name),
            _ => format!("  {}. {} Lv{lv}", i + 1, entry.name),
        };

        // 覆写属性
        if let Some(ref overrides) = entry.overrides {
            let mut parts: Vec<String> = Vec::new();
            for (attr, val) in overrides {
                let val_str = match val {
                    serde_json::Value::Number(n) => {
                        if let Some(i) = n.as_i64() {
                            i.to_string()
                        } else if let Some(f) = n.as_f64() {
                            if f.fract() == 0.0 {
                                format!("{:.0}", f)
                            } else {
                                f.to_string()
                            }
                        } else {
                            n.to_string()
                        }
                    }
                    _ => val.to_string(),
                };
                parts.push(format!("{attr} {val_str}"));
            }
            if !parts.is_empty() {
                line.push_str(&format!("（覆写：{}）", parts.join("，")));
            }
        }

        out.push_str(&format!("{line}\n"));
    }
}

/// 解锁条件 JSON → 编号列表 + 中文状态。
///
/// `[{"stageId":"main_02-08","completeState":"PASS"}]` →
/// ```ignore
/// 解锁条件:
///   1. 通关 main_02-08
///   2. 三星通关 main_00-01
/// ```
fn format_unlock(row: &HashMap<String, String>, out: &mut String) {
    let raw = match row.get("解锁条件") {
        Some(v) if !v.is_empty() && v != "null" && v != "[]" => v,
        _ => {
            out.push_str("解锁条件: 无\n");
            return;
        }
    };

    #[derive(Deserialize)]
    struct UnlockEntry {
        #[serde(rename = "stageId")]
        stage_id: String,
        #[serde(rename = "completeState")]
        complete_state: String,
    }

    let entries: Vec<UnlockEntry> = match serde_json::from_str(raw) {
        Ok(arr) => arr,
        Err(_) => {
            out.push_str(&format!("解锁条件: {raw}\n"));
            return;
        }
    };

    if entries.is_empty() {
        out.push_str("解锁条件: 无\n");
        return;
    }

    out.push_str("解锁条件:\n");
    for (i, entry) in entries.iter().enumerate() {
        let state_label = match entry.complete_state.as_str() {
            "PASS" => "通关",
            "COMPLETE" => "三星通关",
            other => other,
        };
        out.push_str(&format!("  {}. {state_label} {}\n", i + 1, entry.stage_id));
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

    // ── 工具元数据 ──

    #[test]
    fn tool_metadata() {
        let tool = QueryStage;
        assert_eq!(tool.name(), "query_stage");
        assert!(tool.description().contains("关卡"));
        let params = tool.parameters();
        assert_eq!(params["type"], "object");
        // extra 可选：不出现 required（names/ids 互斥必填其一，由解析层校验）
        assert!(params.get("required").is_none());
    }

    // ── 名称精确匹配 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn query_by_name_exact() {
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool
            .execute(&ctx, r#"{"names":["暴君"],"extra":["理智消耗"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("暴君"), "应包含关卡名\n结果: {result}");
        assert!(result.contains("1-7"), "应包含关卡编号\n结果: {result}");
        assert!(result.contains("主线"), "应包含关卡类型\n结果: {result}");
        assert!(result.contains("levelId"), "应包含 levelId\n结果: {result}");
        assert!(result.contains("关卡ID"), "应包含关卡ID\n结果: {result}");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn query_by_name_exact_returns_multiple_variants() {
        // 暴君同时有 main_01-07 和 main_01-07#f#
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool
            .execute(&ctx, r#"{"names":["暴君"],"extra":["理智消耗"]}"#)
            .unwrap()
            .summary;

        assert!(
            result.contains("main_01-07"),
            "应包含普通版\n结果: {result}"
        );
        assert!(
            result.contains("main_01-07#f#"),
            "应包含磨难版\n结果: {result}"
        );
        assert!(result.contains("1/2"), "多行应有计数");
        assert!(result.contains("2/2"), "多行应有计数");
    }

    // ── 编号精确匹配 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn query_by_number_fallback() {
        // 用户输入 "1-7"，关卡名是 "暴君"
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool.execute(&ctx, r#"{"names":["1-7"]}"#).unwrap().summary;

        assert!(result.contains("暴君"), "应找到关卡名\n结果: {result}");
        assert!(result.contains("1-7"), "应包含编号\n结果: {result}");
    }

    // ── LIKE fallback ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn query_like_fallback() {
        let ctx = make_ctx();
        let tool = QueryStage;
        // "暴君" 是关卡名=暴君，编号=1-7。用 "暴" 模糊匹配应命中它
        let result = tool.execute(&ctx, r#"{"names":["暴"]}"#).unwrap().summary;

        assert!(result.contains("暴君"), "应模糊匹配到暴君\n结果: {result}");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn query_not_found() {
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool
            .execute(&ctx, r#"{"names":["绝对不存在的关卡xyz"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("未找到"), "空结果应有提示\n结果: {result}");
    }

    // ── 特殊字段 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn enemy_list_formatted() {
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool
            .execute(&ctx, r#"{"names":["1-7"],"extra":["敌人列表"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("敌人列表"), "应有列名\n结果: {result}");
        assert!(result.contains("暴徒"), "应包含敌人名\n结果: {result}");
        assert!(result.contains("Lv0"), "应包含级别\n结果: {result}");
        assert!(!result.contains("[\""), "不应含 JSON 括号\n结果: {result}");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn enemy_list_with_overrides() {
        // TR-1 有覆写属性的敌人
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool
            .execute(&ctx, r#"{"names":["TR-1"],"extra":["敌人列表"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("覆写"), "应显示覆写属性\n结果: {result}");
        assert!(
            result.contains("防御力") || result.contains("最大生命值"),
            "应包含覆写的属性名\n结果: {result}"
        );
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn description_with_newlines() {
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool.execute(&ctx, r#"{"names":["0-1"]}"#).unwrap().summary;

        assert!(result.contains("关卡描述"), "应有列名\n结果: {result}");
        assert!(
            result.contains("附加条件"),
            "应保留描述内容\n结果: {result}"
        );
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn intel_wiki_cleaned() {
        // SS-8 情报含 {{ruby}} 和 {{简易折叠}}
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool.execute(&ctx, r#"{"names":["SS-8"]}"#).unwrap().summary;

        assert!(!result.contains("{{ruby}}"), "ruby 应已清洗");
        assert!(!result.contains("{{简易折叠}}"), "简易折叠应已展开");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn unlock_condition_formatted() {
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool
            .execute(&ctx, r#"{"names":["0-1"],"extra":["解锁条件"]}"#)
            .unwrap()
            .summary;

        // 0-1 有解锁条件（至少一行有）
        let has_unlock = result.contains("通关") || result.contains("三星通关");
        assert!(
            has_unlock || result.contains("解锁条件"),
            "应有解锁条件或列名\n结果: {result}"
        );
    }

    // ── 多字段查询 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn multiple_extra_columns() {
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool
            .execute(
                &ctx,
                r#"{"names":["1-7"],"extra":["理智消耗","推荐等级","部署上限"]}"#,
            )
            .unwrap()
            .summary;

        assert!(result.contains("理智消耗"), "结果: {result}");
        assert!(result.contains("推荐等级"), "结果: {result}");
        assert!(result.contains("部署上限"), "结果: {result}");
    }

    // ── 默认输出与附加列 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn default_returns_seven_columns() {
        // 默认 7 列：关卡ID、关卡名（header）、关卡编号、关卡描述、关卡类型、情报、levelId
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool.execute(&ctx, r#"{"names":["1-7"]}"#).unwrap().summary;

        assert!(result.contains("暴君"), "header 应含关卡名\n结果: {result}");
        assert!(result.contains("关卡ID"), "结果: {result}");
        assert!(result.contains("关卡编号"), "结果: {result}");
        assert!(result.contains("关卡描述"), "结果: {result}");
        assert!(result.contains("关卡类型"), "结果: {result}");
        assert!(result.contains("情报"), "结果: {result}");
        assert!(result.contains("levelId"), "结果: {result}");
        // 非默认列不出现
        assert!(!result.contains("推荐等级"), "结果: {result}");
        assert!(!result.contains("部署上限"), "结果: {result}");
        assert!(!result.contains("理智消耗"), "结果: {result}");
        assert!(!result.contains("敌人列表"), "结果: {result}");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_common_columns() {
        // 6 个常用列全命中：敌人列表、部署上限、初始费用、费用上限、目标点耐久、待处理目标数量
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool
            .execute(&ctx, r#"{"names":["1-7"],"extra":["敌人列表","部署上限","初始费用","费用上限","目标点耐久","待处理目标数量"]}"#)
            .unwrap().summary;

        assert!(result.contains("敌人列表"), "结果: {result}");
        assert!(result.contains("部署上限"), "结果: {result}");
        assert!(result.contains("初始费用"), "结果: {result}");
        assert!(result.contains("费用上限"), "结果: {result}");
        assert!(result.contains("目标点耐久"), "结果: {result}");
        assert!(result.contains("待处理目标数量"), "结果: {result}");
        assert!(!result.contains("✗"), "常用列不应报错\n结果: {result}");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_default_column_ignored() {
        // 默认列重复指定：不报错、不重复输出
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool
            .execute(&ctx, r#"{"names":["H7-4"],"extra":["关卡描述"]}"#)
            .unwrap()
            .summary;

        assert!(
            !result.contains("✗"),
            "默认列重复指定不应报错\n结果: {result}"
        );
        assert_eq!(
            result.matches("关卡描述:").count(),
            1,
            "默认列不应重复输出\n结果: {result}"
        );
    }

    #[test]
    fn enemy_list_with_id() {
        // 新格式（stages 更新脚本产出的形态）：每项带 敌人ID，输出应显示 [id]。
        // 测试夹具库是随包的旧版，敌人列表里还没有 ID，所以这里直接喂 JSON 验格式。
        let mut row = HashMap::new();
        row.insert(
            "敌人列表".to_string(),
            r#"[{"名称":"士兵","敌人ID":"enemy_1002_nsabr","级别":0},{"名称":"士兵","敌人ID":"enemy_5056_yrnorm","级别":0}]"#.to_string(),
        );
        let mut out = String::new();
        format_enemy_list(&row, &mut out);

        assert!(out.contains("士兵[enemy_1002_nsabr] Lv0"), "结果: {out}");
        assert!(
            out.contains("士兵[enemy_5056_yrnorm] Lv0"),
            "两个同名敌人应能靠 ID 区分\n结果: {out}"
        );
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn empty_enemy_list() {
        // 有些关卡的敌人列表是空数组
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool
            .execute(&ctx, r#"{"names":["TR-1"],"extra":["敌人列表"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("敌人列表"), "空列表也应显示列名");
    }

    // ── 错误处理 ──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn missing_name_returns_error() {
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool.execute(&ctx, r#"{"extra":["关卡名"]}"#);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("name"));
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_state_key_error() {
        // 关卡无状态键：含 = 的项报错指路，不阻塞有效结果
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool
            .execute(&ctx, r#"{"names":["暴君"],"extra":["等级=1"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("✗"), "应有报错\n结果: {result}");
        assert!(
            result.contains("状态指定 '等级=1' 不支持"),
            "应含状态指定文案\n结果: {result}"
        );
        assert!(
            result.contains("《关卡表结构》"),
            "应指路知识库\n结果: {result}"
        );
        assert!(
            result.contains("暴君"),
            "正文仍应有有效结果\n结果: {result}"
        );
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn extra_unknown_column_error() {
        // 未识别列名报错指路，不阻塞有效结果
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool
            .execute(&ctx, r#"{"names":["暴君"],"extra":["不存在的列"]}"#)
            .unwrap()
            .summary;

        assert!(result.contains("✗"), "应有报错\n结果: {result}");
        assert!(
            result.contains("无法识别的 extra 项 '不存在的列'"),
            "应含未识别文案\n结果: {result}"
        );
        assert!(
            result.contains("《关卡表结构》"),
            "应指路知识库\n结果: {result}"
        );
    }

    #[test]
    fn no_db_returns_error() {
        let ctx = ToolContext::new();
        let tool = QueryStage;
        let result = tool.execute(&ctx, r#"{"names":["1-7"]}"#);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("数据库未连接"));
    }

    // ── 单行显示（无变体）──

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn single_row_no_variants() {
        // H7-4 只有一条记录，无 #f# 变体
        let ctx = make_ctx();
        let tool = QueryStage;
        let result = tool.execute(&ctx, r#"{"names":["H7-4"]}"#).unwrap().summary;

        assert!(result.contains("星火行动"), "结果: {result}");
        assert!(!result.contains("1/"), "单行不应有计数");
    }

    // ── 交互式输出（PRTS_NAME / PRTS_EXTRA）──

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
            let tool = QueryStage;
            match tool.execute(ctx, &params.to_string()) {
                Ok(s) => s.summary,
                Err(e) => format!("ERROR: {e}"),
            }
        }

        let ctx = make_ctx();

        // 支持环境变量交互：PRTS_NAME / PRTS_EXTRA（逗号分隔，空串 → 空数组）
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
            ("1. 暴君 · 默认（7 列，多难度多条）", "暴君", None),
            ("2. 1-7 · 关卡编号检索", "1-7", None),
            (
                "3. 暴君 · 敌人列表 + 部署上限",
                "暴君",
                Some(&["敌人列表", "部署上限"]),
            ),
            ("4. 暴君 · 等级=1 报错指路", "暴君", Some(&["等级=1"])),
            (
                "5. 暴君 · 不存在的列 报错指路",
                "暴君",
                Some(&["不存在的列"]),
            ),
        ];
        for (label, name, extra) in cases {
            println!("========== {label} ==========");
            println!("{}", call(&ctx, name, *extra));
            println!();
        }
    }
}
