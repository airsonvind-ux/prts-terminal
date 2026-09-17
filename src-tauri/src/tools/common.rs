//! 查询工具共享类型与辅助函数
//!
//! 所有名称查询工具（query_enemy / query_operator / query_stage 等）
//! 共享的 Target 枚举和参数解析逻辑。避免逐文件重复。

use serde_json::Value;

use super::ToolError;

/// 查询目标：ID 精确匹配 或 名称模糊匹配。
#[derive(Debug)]
pub(crate) enum Target {
    Id(String),
    Name(String),
}

/// 从 params JSON 解析 ids/names 数组，校验互斥。
///
/// 返回 Target 列表。同时传入 ids 和 names 时报错，
/// 都不传时报错。
pub(crate) fn parse_targets(v: &Value) -> Result<Vec<Target>, ToolError> {
    let has_ids = v
        .get("ids")
        .map(|a| !a.as_array().map(|a| a.is_empty()).unwrap_or(true))
        .unwrap_or(false);
    let has_names = v
        .get("names")
        .map(|a| !a.as_array().map(|a| a.is_empty()).unwrap_or(true))
        .unwrap_or(false);

    if has_ids && has_names {
        return Err(ToolError::Parse(
            "ids 与 names 互斥，不能同时传入".into(),
        ));
    }
    if !has_ids && !has_names {
        return Err(ToolError::Parse(
            "需传入 ids 或 names（至少一个非空数组）".into(),
        ));
    }

    let targets: Vec<Target> = if has_ids {
        v["ids"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|s| s.as_str().map(|s| Target::Id(s.to_string())))
            .collect()
    } else {
        v["names"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|s| s.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .map(Target::Name)
            .collect()
    };

    if targets.is_empty() {
        return Err(ToolError::Parse("查询目标不能为空".into()));
    }

    Ok(targets)
}
