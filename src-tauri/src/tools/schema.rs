//! 工具参数 JSON Schema 校验（架构升级 · 模块 3 P1）
//!
//! 两层校验，覆盖工具契约的声明与执行：
//! - **注册时**（`validate_tool_schema`）：工具 `parameters()` 必须是合法
//!   JSON Schema（顶层 `type: "object"` + `properties` 对象 + `required` 数组）
//! - **执行前**（`validate_arguments`）：LLM 传入的参数满足 schema——
//!   必填字段存在 + 顶层属性类型匹配；在 registry 执行管线的 guard 阶段
//!   调用（模块 3 P0 管线结构就位后的首个真实守卫）
//!
//! 只校验顶层（嵌套不深究），避免对现有工具的合法参数误判；
//! 深度 schema 校验（$defs/嵌套）随工具生态扩展按需加深。

use serde_json::Value;

/// 注册时校验：工具 parameters() 是否合法 JSON Schema（顶层契约）。
pub fn validate_tool_schema(schema: &Value) -> Result<(), String> {
    let obj = schema
        .as_object()
        .ok_or_else(|| "parameters 必须是 JSON Schema 对象".to_string())?;

    match obj.get("type").and_then(|t| t.as_str()) {
        Some("object") => {}
        other => {
            return Err(format!(
                "parameters.type 必须为 \"object\"（实际: {}）",
                other.unwrap_or("缺失")
            ))
        }
    }

    if let Some(props) = obj.get("properties") {
        if !props.is_object() {
            return Err("parameters.properties 必须是对象".to_string());
        }
    }

    if let Some(required) = obj.get("required") {
        if !required.is_array() {
            return Err("parameters.required 必须是数组".to_string());
        }
    }

    Ok(())
}

/// 执行前校验：参数满足 schema（顶层必填 + 类型）。
/// 违反返回 Err（由 guard 转为 `ToolError::Parse`）。
pub fn validate_arguments(schema: &Value, args: &Value) -> Result<(), String> {
    let obj = schema
        .as_object()
        .ok_or_else(|| "工具 schema 格式错误".to_string())?;

    // 必填字段存在性
    if let Some(required) = obj.get("required").and_then(|r| r.as_array()) {
        for name in required {
            let Some(name) = name.as_str() else { continue };
            if args.get(name).is_none() {
                return Err(format!("缺少必填参数: {name}"));
            }
        }
    }

    // 顶层属性类型匹配（参数是对象时）
    if let (Some(props), Some(args_obj)) = (
        obj.get("properties").and_then(|p| p.as_object()),
        args.as_object(),
    ) {
        for (name, val) in args_obj {
            // null 视为"未提供"（LLM 对可选参数常传 null；工具内部解析
            // 同样以 None 处理并走默认值），不参与类型校验
            if val.is_null() {
                continue;
            }
            let Some(spec) = props.get(name) else {
                continue;
            };
            let Some(expected) = spec.get("type").and_then(|t| t.as_str()) else {
                continue;
            };
            let actual = json_type_of(val);
            let ok = match expected {
                "number" => matches!(actual, "number" | "integer"), // number 兼容 integer
                _ => actual == expected,
            };
            if !ok {
                return Err(format!(
                    "参数 {name} 类型错误：期望 {expected}，实际 {actual}"
                ));
            }
            // minLength：string 参数的最小长度（空串防御，模块 C 缓存工具引入）
            if let Some(min) = spec.get("minLength").and_then(|m| m.as_u64()) {
                if let Some(s) = val.as_str() {
                    if (s.chars().count() as u64) < min {
                        return Err(format!("参数 {name} 长度不足：最少 {min} 个字符"));
                    }
                }
            }
        }
    }

    Ok(())
}

/// JSON 值的类型标签（对齐 JSON Schema type 字面量）。
fn json_type_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── 注册时校验 ──

    #[test]
    fn valid_schema_accepted() {
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        });
        assert!(validate_tool_schema(&schema).is_ok());
    }

    #[test]
    fn non_object_type_rejected() {
        let schema = json!({ "type": "string" });
        let err = validate_tool_schema(&schema).unwrap_err();
        assert!(err.contains("type"));
        // 非对象 schema 根
        assert!(validate_tool_schema(&json!("oops")).is_err());
        // 缺 type
        assert!(validate_tool_schema(&json!({ "properties": {} })).is_err());
    }

    #[test]
    fn malformed_properties_or_required_rejected() {
        assert!(validate_tool_schema(&json!({
            "type": "object",
            "properties": []
        }))
        .is_err());
        assert!(validate_tool_schema(&json!({
            "type": "object",
            "required": "name"
        }))
        .is_err());
    }

    // ── 执行前校验 ──

    #[test]
    fn missing_required_rejected() {
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } },
            "required": ["name"]
        });
        let err = validate_arguments(&schema, &json!({})).unwrap_err();
        assert!(err.contains("name"), "got: {err}");
        // 提供后通过
        assert!(validate_arguments(&schema, &json!({ "name": "源石虫" })).is_ok());
    }

    #[test]
    fn type_mismatch_rejected() {
        let schema = json!({
            "type": "object",
            "properties": {
                "fields": { "type": "array" },
                "level": { "type": "integer" },
                "ratio": { "type": "number" }
            }
        });
        assert!(validate_arguments(&schema, &json!({ "fields": "not-array" })).is_err());
        // integer 不接受浮点
        assert!(validate_arguments(&schema, &json!({ "fields": [], "level": 1.5 })).is_err());
        // number 兼容 integer
        assert!(validate_arguments(&schema, &json!({ "fields": [], "ratio": 3 })).is_ok());
        // 合法参数通过
        assert!(validate_arguments(
            &schema,
            &json!({ "fields": ["a"], "level": 1, "ratio": 0.5 })
        )
        .is_ok());
    }

    #[test]
    fn unknown_args_fields_ignored() {
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });
        assert!(validate_arguments(&schema, &json!({ "name": "x", "extra": 42 })).is_ok());
    }

    #[test]
    fn min_length_rejects_empty_string() {
        // 模块 C 缓存工具：key/content 空串防御（minLength 关键字）
        let schema = json!({
            "type": "object",
            "properties": { "key": { "type": "string", "minLength": 1 } }
        });
        let err = validate_arguments(&schema, &json!({ "key": "" })).unwrap_err();
        assert!(err.contains("长度不足"), "got: {err}");
        assert!(validate_arguments(&schema, &json!({ "key": "ok" })).is_ok());
    }

    #[test]
    fn null_treated_as_absent() {
        let schema = json!({
            "type": "object",
            "properties": { "base_url": { "type": "string" } }
        });
        // null 视为"未提供"（LLM 对可选参数常传 null），不参与类型校验
        assert!(validate_arguments(&schema, &json!({ "base_url": null })).is_ok());
    }
}
