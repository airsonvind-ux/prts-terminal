//! `query.call`：插件同步调用宿主的**只读**工具。
//!
//! 为什么要有这条：插件（尤其 MAA）需要查关卡、查干员、算理智，这些已经是
//! 宿主工具，重写一遍既重复又会和数据库版本脱节。能力层把「工具」也当成一种
//! 原语暴露出去，插件就能把查询与自己的编排拼在一起（D30 的三层契约）。
//!
//! 两道闸：
//!
//! 1. **allowlist**（13 键，M2 首批）：纳入标准是「只读，且不构成越权通道」。
//!    `read_file` / `list_dir` / `grep` 虽然只读却被排除——它们能读任意路径，
//!    等于绕开 `fs.plugin` 的边界；`run_command`、`write_*`、`memory_*`、
//!    `capture_game_screen`（设备侧）、`web_*`（网络）自然也不在里面。
//! 2. **来源拒绝**：目标是插件贡献的工具时一律拒（`Origin::Plugin`）。否则
//!    A 插件可以借 B 的工具再回调 A 的能力，形成 A→B→A 的递归死锁。
//!
//! 注册表在 `load_all` 之后才定型，所以这里用 [`OnceLock`] 延迟绑定：服务先
//! 造出来交给 `load_all`，`lib.rs` 在 `Arc::new(reg)` 之后调用
//! [`QueryService::bind_tools`]。二次绑定返回错误——装配错误应当当场暴露。

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use crate::db::Database;
use crate::host::{HostError, HostService, HostServiceInfo};
use crate::tools::{Origin, ToolContext, ToolRegistry};

const KEY: &str = "query.call";

/// M2 首批允许经 `query.call` 调用的宿主工具（顺序仅作文档，判定用 `contains`）。
///
/// 改动这张表要同时改规格 §4 与扩展页的能力说明：它是**对插件公开的 API 面**。
pub const ALLOWED_TOOLS: [&str; 13] = [
    "search_entities",
    "sim_path",
    "query_enemy",
    "query_level",
    "query_operator",
    "query_stage",
    "query_token",
    "query_trap",
    "query_table",
    "query_rogue",
    "time",
    "read_knowledge",
    "list_knowledge",
];

static INFO: HostServiceInfo = HostServiceInfo {
    key: KEY,
    summary: "同步调用宿主的只读工具（13 个允许项，见 query.call allowlist）",
    params_doc: "{ tool: 工具名, args: 该工具的参数对象 }",
    returns_doc: "{ result: 工具的文本结果 }",
    stateful: false,
    requires_user_grant: false,
    timeout: Some(Duration::from_secs(30)),
    concurrency_safe: true,
    since: 1,
};

/// `query.call` 的服务实现。
pub struct QueryService {
    /// 数据库句柄：查询类工具几乎都要它；None 时（测试/未初始化）工具会自行报错。
    db: Option<Arc<Database>>,
    /// 延迟绑定的工具注册表（`load_all` 之后由 `lib.rs` 注入）。
    registry: OnceLock<Arc<ToolRegistry>>,
}

impl QueryService {
    /// 构造；`db` 与宿主工具注册表用同一份数据库连接。
    pub fn new(db: Option<Arc<Database>>) -> Self {
        Self {
            db,
            registry: OnceLock::new(),
        }
    }

    /// 绑定工具注册表；重复绑定返回错误（装配期只该绑一次）。
    pub fn bind_tools(&self, registry: Arc<ToolRegistry>) -> Result<(), String> {
        self.registry
            .set(registry)
            .map_err(|_| "query.call 的工具注册表被重复绑定".to_string())
    }
}

impl HostService for QueryService {
    fn info(&self) -> &'static HostServiceInfo {
        &INFO
    }

    fn call(&self, _caller: &str, args: Value) -> Result<Value, HostError> {
        let tool = args
            .get("tool")
            .and_then(Value::as_str)
            .ok_or_else(|| HostError::invalid_args(KEY, "缺少 tool 字段"))?;

        if !ALLOWED_TOOLS.contains(&tool) {
            return Err(HostError::invalid_args(
                KEY,
                format!(
                    "工具 {tool} 不在 query.call 允许列表内（可用：{}）",
                    ALLOWED_TOOLS.join("、")
                ),
            ));
        }

        let registry = self
            .registry
            .get()
            .ok_or_else(|| HostError::failed(KEY, "工具注册表尚未就绪（装配顺序错误）"))?;

        if let Some(Origin::Plugin(_)) = registry.origin_of(tool) {
            return Err(HostError::invalid_args(
                KEY,
                format!("{tool} 是插件工具，不能经 query.call 调用（防止插件间递归）"),
            ));
        }

        let params = args.get("args").cloned().unwrap_or_else(|| json!({}));
        let ctx = match &self.db {
            Some(db) => ToolContext::with_db(Arc::clone(db)),
            None => ToolContext::new(),
        };

        registry
            .execute(tool, &ctx, &params.to_string())
            .map(|output| output.canonical)
            .map_err(|err| HostError::failed(KEY, format!("{tool} 执行失败：{err}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Tool, ToolError, ToolOutput};

    struct StubTool {
        name: &'static str,
    }

    impl Tool for StubTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "测试桩"
        }

        fn parameters(&self) -> Value {
            json!({"type": "object", "properties": {}})
        }

        fn execute(&self, _ctx: &ToolContext, _params: &str) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::json(json!({"ok": true}), "done"))
        }
    }

    fn service_with(registry: Arc<ToolRegistry>) -> QueryService {
        let service = QueryService::new(None);
        service.bind_tools(registry).unwrap();
        service
    }

    #[test]
    fn calls_an_allowlisted_host_tool() {
        let registry = ToolRegistry::new();
        registry
            .register(Box::new(StubTool {
                name: "query_stage",
            }))
            .unwrap();
        let service = service_with(Arc::new(registry));

        let value = service
            .call(
                "maa",
                json!({"tool": "query_stage", "args": {"stage": "1-7"}}),
            )
            .unwrap();
        assert_eq!(value, json!({"ok": true}));
    }

    #[test]
    fn rejects_tools_outside_the_allowlist() {
        let registry = Arc::new(ToolRegistry::new());
        let service = service_with(registry);

        for tool in [
            "read_file",
            "run_command",
            "capture_game_screen",
            "no_such_tool",
        ] {
            let err = service.call("maa", json!({"tool": tool})).unwrap_err();
            assert!(
                matches!(err, HostError::InvalidArgs { .. }),
                "{tool} 应该被拒：{err:?}"
            );
        }
    }

    #[test]
    fn rejects_plugin_contributed_tools() {
        let registry = ToolRegistry::new();
        registry
            .register_with_origin(
                Box::new(StubTool {
                    name: "query_stage",
                }),
                Origin::Plugin("other".to_string()),
            )
            .unwrap();
        let service = service_with(Arc::new(registry));

        let err = service
            .call("maa", json!({"tool": "query_stage"}))
            .unwrap_err();
        assert!(
            matches!(&err, HostError::InvalidArgs { detail, .. } if detail.contains("插件工具")),
            "{err:?}"
        );
    }

    #[test]
    fn reports_a_missing_binding_instead_of_panicking() {
        let service = QueryService::new(None);
        let err = service
            .call("maa", json!({"tool": "query_stage"}))
            .unwrap_err();
        assert!(matches!(err, HostError::Failed { .. }), "{err:?}");
    }

    #[test]
    fn binding_twice_is_an_error() {
        let service = service_with(Arc::new(ToolRegistry::new()));
        assert!(service.bind_tools(Arc::new(ToolRegistry::new())).is_err());
    }
}
