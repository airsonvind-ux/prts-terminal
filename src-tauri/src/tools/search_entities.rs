//! search_entities —— 跨表条件筛选工具
//!
//! 语义：输入表名 + 条件 → 返回满足条件的实体名称列表或计数。
//! 与名称查询工具（query_enemy 等）互为逆操作：
//!   名称查询：身份 → 属性
//!   集合筛选：条件 → 身份列表

use crate::db::{Filter, FilterOp};
// Database 仅在测试中使用，lint 警告为误报
#[allow(unused_imports)]
use crate::db::Database;
use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

/// 表名 → 名称列的映射。
///
/// 各表的"主名称"列名不同（enemies/operators/tokens/traps/rogue 叫"名称"，
/// stages 叫"关卡名"，levels 叫"levelId"）。
fn name_column_for(table: &str) -> Result<&'static str, ToolError> {
    match table {
        "enemies" | "operators" | "tokens" | "traps" => Ok("名称"),
        "stages" => Ok("关卡名"),
        "levels" => Ok("levelId"),
        t if t.starts_with("rogue_") && t.ends_with("_entries") => Ok("名称"),
        _ => Err(ToolError::Execution(format!(
            "不支持的表: {table}。可用: enemies / operators / stages / levels / tokens / traps / rogue_1_entries ~ rogue_5_entries"
        ))),
    }
}

/// 表名 → ID 列的映射。
///
/// list 模式始终返回 ID，供后续 query_* 工具做精确查询。
fn id_column_for(table: &str) -> Result<&'static str, ToolError> {
    match table {
        "enemies" => Ok("敌人ID"),
        "operators" => Ok("干员ID"),
        "stages" => Ok("关卡ID"),
        "levels" => Ok("levelId"),
        "tokens" => Ok("tokenID"),
        "traps" => Ok("装置ID"),
        t if t.starts_with("rogue_") && t.ends_with("_entries") => Ok("id"),
        _ => Err(ToolError::Execution(format!("不支持的表: {table}"))),
    }
}

pub struct SearchEntities;

impl Tool for SearchEntities {
    fn name(&self) -> &str {
        "search_entities"
    }

    fn description(&self) -> &str {
        "按条件筛选数据库实体。list 模式返回名称+ID 列表，用于先找出“有哪些”及“有没有”，再按 ID 走 query_* 查属性；count 模式仅返回数量。覆盖 11 张表（enemies/operators/stages/levels/tokens/traps/rogue_1~5），多条件 AND 连接，支持比较与 LIKE 模糊匹配，不支持跨表/OR/排序"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "table": {
                    "type": "string",
                    "description": "目标表：enemies=敌人、operators=干员、stages=关卡、levels=地图、tokens=召唤物、traps=陷阱、rogue_1_entries~rogue_5_entries=对应肉鸽主题（傀影/水月/萨米/萨卡兹/界园）",
                    "enum": ["enemies", "operators", "stages", "levels", "tokens", "traps", "rogue_1_entries", "rogue_2_entries", "rogue_3_entries", "rogue_4_entries", "rogue_5_entries"]
                },
                "filters": {
                    "type": "array",
                    "description": "组 AND 过滤条件，可为空 []。每项含 field、op、value，如 [{“field”:“法术抗性”,“op”:“>”,“value”:“90”}]。常用列：技能、法术抗性、地位、级别、种类（enemies）；职业、职业分支、稀有度（operators）；关卡类型（stages）；类型、效果（rogue）。完整列名见知识库对应《表结构》条目",
                    "items": {
                        "type": "object",
                        "properties": {
                            "field": { "type": "string", "description": "筛选目标列名。不在此表或填错→报错" },
                            "op": {
                                "type": "string",
                                "enum": ["=", ">", "<", ">=", "<=", "!=", "LIKE"],
                                "description": "比较操作符"
                            },
                            "value": { "type": "string", "description": "比较值。LIKE 时无需加 %，自动模糊匹配" }
                        },
                        "required": ["field", "op", "value"]
                    }
                },
                "mode": {
                    "type": "string",
                    "enum": ["list", "count"],
                    "description": "默认 list",
                    "default": "list"
                },
                "limit": {
                    "type": "integer",
                    "description": "list 模式最大返回条数，默认 20，上限 200。count 模式忽略",
                    "default": 20,
                    "minimum": 1,
                    "maximum": 200
                }
            },
            "required": ["table", "filters"]
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(params) else {
            return ToolDisplay::default();
        };
        let table = v.get("table").and_then(|t| t.as_str()).unwrap_or("");
        let head = table_display_name(table);
        let filters = v
            .get("filters")
            .and_then(|f| f.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|it| {
                        let field = it.get("field").and_then(|x| x.as_str())?;
                        let op = it.get("op").and_then(|x| x.as_str()).unwrap_or("");
                        let value = it.get("value").and_then(|x| x.as_str()).unwrap_or("");
                        Some(format!("{field}{op}{value}"))
                    })
                    .collect::<Vec<_>>()
                    .join("、")
            })
            .unwrap_or_default();
        let text = if filters.is_empty() {
            head.to_string()
        } else {
            format!("{head} · {filters}")
        };
        ToolDisplay::info(text)
    }

    fn execute(&self, ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        // ── 1. 解析参数 ──
        let v: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;

        let table = v["table"]
            .as_str()
            .ok_or_else(|| ToolError::Parse("缺少必填参数: table".into()))?;

        let mode = v["mode"].as_str().unwrap_or("list");
        let limit = v["limit"].as_u64().unwrap_or(20).min(200) as u32;

        // ── 2. 解析 filters ──
        let filters = parse_filters(&v["filters"])?;

        // ── 3. 获取数据库 ──
        let db = ctx
            .db
            .as_ref()
            .ok_or_else(|| ToolError::Execution("数据库未连接".into()))?;

        // ── 4. 获取名称列和 ID 列 ──
        let name_col = name_column_for(table)?;
        let id_col = id_column_for(table)?;

        // ── 5. count 模式：只返回数量 ──
        if mode == "count" {
            let count = db
                .query_count(table, &filters)
                .map_err(ToolError::Execution)?;
            return Ok(ToolOutput::json(
                serde_json::json!({ "count": count }),
                count.to_string(),
            ));
        }

        // ── 6. list 模式：名称+ID 列表 + 总数 ──
        let total = db
            .query_count(table, &filters)
            .map_err(ToolError::Execution)?;

        let result = db
            .query_filtered(
                table,
                &[name_col.to_string(), id_col.to_string()],
                &filters,
                limit,
            )
            .map_err(ToolError::Execution)?;

        let rows = result.rows;
        let dropped_fields = result.dropped_fields;

        let table_display = table_display_name(table);

        if rows.is_empty() {
            return Ok(ToolOutput::json(
                serde_json::json!({
                    "rows": rows,
                    "total": total,
                    "dropped_fields": dropped_fields,
                }),
                format!("在{table_display}中未找到符合条件的实体（共 0 条）"),
            ));
        }

        let shown = rows.len();
        let mut output = if (total as usize) > shown {
            format!(
                "在{table_display}中找到 {} 条结果（共 {} 条，当前展示 {} 条）：\n",
                shown, total, shown
            )
        } else {
            format!("在{table_display}中找到 {} 条结果：\n", total)
        };

        for row in &rows {
            let name = row.get(name_col).map(|s| s.as_str()).unwrap_or("?");
            let id = row.get(id_col).map(|s| s.as_str()).unwrap_or("?");
            output.push_str(&format!("  - {name}（{id}）\n"));
        }

        if !dropped_fields.is_empty() {
            output.push_str(&format!(
                "\n⚠ 以下请求字段不存在，已忽略：{}",
                dropped_fields.join("、")
            ));
        }

        Ok(ToolOutput::json(
            serde_json::json!({
                "rows": rows,
                "total": total,
                "dropped_fields": dropped_fields,
            }),
            output.trim(),
        ))
    }
}

// ═══════════════════════════════════════════════════════
// 辅助函数
// ═══════════════════════════════════════════════════════

/// 从 JSON 数组解析 Filter 列表。
fn parse_filters(arr: &serde_json::Value) -> Result<Vec<Filter>, ToolError> {
    let items = arr
        .as_array()
        .ok_or_else(|| ToolError::Parse("filters 必须是数组".into()))?;

    items
        .iter()
        .map(|item| {
            let field = item["field"]
                .as_str()
                .ok_or_else(|| ToolError::Parse("filter 缺少 field".into()))?;
            let op_str = item["op"]
                .as_str()
                .ok_or_else(|| ToolError::Parse("filter 缺少 op".into()))?;
            let value = item["value"]
                .as_str()
                .ok_or_else(|| ToolError::Parse("filter 缺少 value".into()))?;

            let op: FilterOp = op_str.parse().map_err(|e: String| ToolError::Parse(e))?;

            Ok(Filter::new(field, op, value))
        })
        .collect()
}

/// 表名 → 中文显示名。
fn table_display_name(table: &str) -> &str {
    match table {
        "enemies" => "敌人",
        "operators" => "干员",
        "stages" => "关卡",
        "levels" => "地图",
        "tokens" => "召唤物",
        "traps" => "陷阱",
        "rogue_1_entries" => "傀影肉鸽藏品",
        "rogue_2_entries" => "水月肉鸽藏品",
        "rogue_3_entries" => "萨米肉鸽藏品",
        "rogue_4_entries" => "萨卡兹肉鸽藏品",
        "rogue_5_entries" => "界园肉鸽藏品",
        _ => table,
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

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn list_mode_returns_names_with_ids() {
        let ctx = make_ctx();
        let tool = SearchEntities;

        let result = tool
            .execute(
                &ctx,
                r#"{"table":"enemies","filters":[{"field":"地位","op":"=","value":"领袖"}],"mode":"list"}"#,
            )
            .unwrap().summary;

        assert!(result.contains("敌人"));
        assert!(result.contains("条结果"));
        // 验证包含某个领袖级敌人名和 ID
        assert!(
            result.contains("大鲍勃") || result.contains("特雷西斯") || result.contains("碎骨")
        );
        // 输出格式包含 ID
        assert!(
            result.contains("（") && result.contains("）"),
            "应包含ID: {result}"
        );
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn count_mode_returns_number() {
        let ctx = make_ctx();
        let tool = SearchEntities;

        let result = tool
            .execute(
                &ctx,
                r#"{"table":"enemies","filters":[{"field":"地位","op":"=","value":"领袖"}],"mode":"count"}"#,
            )
            .unwrap().summary;

        let count: u32 = result.parse().unwrap();
        assert!(count > 0, "领袖敌人数量应 > 0");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn list_mode_default_when_omitted() {
        let ctx = make_ctx();
        let tool = SearchEntities;

        let result = tool
            .execute(
                &ctx,
                r#"{"table":"enemies","filters":[{"field":"法术抗性","op":">","value":"90"}]}"#,
            )
            .unwrap()
            .summary;

        assert!(result.contains("条结果"));
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn like_search_works() {
        let ctx = make_ctx();
        let tool = SearchEntities;

        let result = tool
            .execute(
                &ctx,
                r#"{"table":"enemies","filters":[{"field":"名称","op":"LIKE","value":"源石虫"}],"mode":"list"}"#,
            )
            .unwrap().summary;

        assert!(result.contains("源石虫"));
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn multiple_filters() {
        let ctx = make_ctx();
        let tool = SearchEntities;

        let result = tool
            .execute(
                &ctx,
                r#"{"table":"enemies","filters":[{"field":"地位","op":"=","value":"精英"},{"field":"级别","op":"=","value":"0"}],"mode":"count"}"#,
            )
            .unwrap().summary;

        let count: u32 = result.parse().unwrap();
        assert!(count > 0);
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn cross_table_tokens() {
        let ctx = make_ctx();
        let tool = SearchEntities;

        let result = tool
            .execute(&ctx, r#"{"table":"tokens","filters":[],"mode":"list"}"#)
            .unwrap()
            .summary;

        assert!(result.contains("召唤物"));
    }

    #[test]
    fn no_db_returns_error() {
        let ctx = ToolContext::new(); // 无 db
        let tool = SearchEntities;
        let result = tool.execute(&ctx, r#"{"table":"enemies","filters":[]}"#);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("数据库未连接"));
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn invalid_table_returns_error() {
        let ctx = make_ctx();
        let tool = SearchEntities;
        let result = tool.execute(&ctx, r#"{"table":"nonexistent","filters":[]}"#);
        assert!(result.is_err());
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn empty_result_shows_zero() {
        let ctx = make_ctx();
        let tool = SearchEntities;

        let result = tool
            .execute(
                &ctx,
                r#"{"table":"enemies","filters":[{"field":"名称","op":"LIKE","value":"这个敌人绝对不存在xyz"}],"mode":"list"}"#,
            )
            .unwrap().summary;

        assert!(result.contains("共 0 条"));
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn custom_limit() {
        let ctx = make_ctx();
        let tool = SearchEntities;

        let result = tool
            .execute(
                &ctx,
                r#"{"table":"enemies","filters":[{"field":"地位","op":"=","value":"普通"}],"mode":"list","limit":3}"#,
            )
            .unwrap().summary;

        // 应显示 3 条而非默认 20 条
        assert!(result.contains("找到 3 条结果") || result.contains("当前展示 3 条"));
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn rogue_table_query() {
        let ctx = make_ctx();
        let tool = SearchEntities;

        let result = tool
            .execute(
                &ctx,
                r#"{"table":"rogue_5_entries","filters":[{"field":"类型","op":"=","value":"藏品"}],"mode":"count"}"#,
            )
            .unwrap().summary;

        let count: u32 = result.parse().unwrap();
        assert!(count > 0, "界园肉鸽应有藏品");
    }

    #[cfg_attr(
        not(feature = "data-contract"),
        ignore = "需要本地 arknights.db；用 --features data-contract 运行"
    )]
    #[test]
    fn rogue_list_with_ids() {
        let ctx = make_ctx();
        let tool = SearchEntities;

        let result = tool
            .execute(
                &ctx,
                r#"{"table":"rogue_5_entries","filters":[{"field":"类型","op":"=","value":"藏品"}],"mode":"list","limit":5}"#,
            )
            .unwrap().summary;

        // 输出格式应包含 ID
        assert!(result.contains("（"), "应包含 ID: {result}");
        assert!(result.contains("界园肉鸽藏品"));
    }
}
