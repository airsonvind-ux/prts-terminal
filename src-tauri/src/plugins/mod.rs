//! 插件系统（M1）：契约层与装配。
//!
//! 设计依据：`50-插件系统/_插件系统-设计.md`（D1–D28）与 `_插件系统-M1规格.md`
//! （草案 2）。M1 只做三件事：manifest 解析与十条校验（[`manifest`]）、内存插件
//! 台账（[`registry`]）、内置插件的显式安装表与装配（[`builtin`] / [`load_all`]）。
//!
//! 诚实边界（D11）：M1 的 capability 只是声明，不做运行时门禁——builtin 插件
//! 的代码本来就在宿主进程里，门禁无意义；真正的门禁在 M2 的 process 通道生效。

pub mod builtin;

mod manifest;
mod registry;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::policy::Policy;
use crate::tools::{Origin, Tool, ToolRegistry};

pub use registry::{PluginRecord, PluginRegistry, PluginState};

/// 插件层的统一错误。
///
/// `Display` 文案就是写进 `PluginState::Failed(..)` 与 `eprintln!` 的文本，
/// 因此每条都带字段名与实际值（规格 §5 的硬要求）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginError {
    /// 某个 manifest 字段不合法（`field` 为字段名，`value` 为实际值）。
    Invalid { field: &'static str, value: String },
    /// 同一父目录下出现第二个同 id 插件（后者不注册任何工具）。
    DuplicateId(String),
    /// 协议版本缺失或不受支持（`None` = 未声明；仅 process 必填）。
    UnsupportedProtocol(Option<u32>),
    /// 插件要求的宿主版本高于当前版本。
    RequiresApp { required: String, current: String },
    /// capability 不在白名单内（D25：指出是哪一条）。
    UnknownCapability(String),
    /// `plugin.json` 不存在或不可读。
    ManifestRead { path: String, error: String },
    /// `plugin.json` 不是合法 JSON 或字段类型不对。
    ManifestParse { path: String, error: String },
    /// 宿主没有该 id 的内置安装实现。
    UnknownBuiltin(String),
    /// 插件工具与已注册工具重名（插件一律失败，不覆盖）。
    ToolConflict(String),
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginError::Invalid { field, value } => {
                write!(f, "manifest 字段 {field} 非法：{value}")
            }
            PluginError::DuplicateId(id) => write!(f, "duplicate plugin id: {id}"),
            PluginError::UnsupportedProtocol(found) => match found {
                Some(v) => write!(
                    f,
                    "不支持的插件协议版本：{v}（本宿主支持 {}）",
                    manifest::SUPPORTED_PROTOCOL
                ),
                None => write!(
                    f,
                    "缺少 requires.protocol（本宿主支持 {}）",
                    manifest::SUPPORTED_PROTOCOL
                ),
            },
            PluginError::RequiresApp { required, current } => {
                write!(f, "插件要求宿主版本 {required}，当前 {current}")
            }
            PluginError::UnknownCapability(name) => write!(f, "未知 capability：{name}"),
            PluginError::ManifestRead { path, error } => {
                write!(f, "读取 manifest 失败：{path}（{error}）")
            }
            PluginError::ManifestParse { path, error } => {
                write!(f, "manifest 解析失败：{path}（{error}）")
            }
            PluginError::UnknownBuiltin(id) => write!(f, "宿主没有内置插件 {id} 的安装实现"),
            PluginError::ToolConflict(name) => write!(f, "插件工具与已注册工具重名：{name}"),
        }
    }
}

/// 把一个插件贡献的工具注册进注册表；任一失败则整体回滚（D24）。
///
/// 顺序注册，成功返回工具名列表（写进 `PluginRecord.tools`）。同名策略与宿主
/// 相反：插件不得覆盖任何已存在的工具——跨插件覆盖是最难排查的一类故障，
/// 因此先查重，失败即把本次已注册的名字逆序摘除，注册表回到调用前的状态。
pub fn register_plugin_tools(
    reg: &mut ToolRegistry,
    plugin_id: &str,
    tools: Vec<Box<dyn Tool>>,
) -> Result<Vec<String>, PluginError> {
    let origin = Origin::Plugin(plugin_id.to_string());
    let mut registered: Vec<String> = Vec::new();

    for tool in tools {
        let name = tool.name().to_string();
        if reg.get(&name).is_some() {
            rollback_registered(reg, &registered);
            return Err(PluginError::ToolConflict(name));
        }
        if let Err(error) = reg.register_with_origin(tool, origin.clone()) {
            rollback_registered(reg, &registered);
            return Err(PluginError::Invalid {
                field: "parameters",
                value: format!("{name}: {error}"),
            });
        }
        registered.push(name);
    }

    Ok(registered)
}

/// 逆序摘除本次已注册的工具（后注册的先摘，与注册顺序对称）。
fn rollback_registered(reg: &mut ToolRegistry, registered: &[String]) {
    for name in registered.iter().rev() {
        reg.remove(name);
    }
}

/// 扫描随包插件目录，逐个校验并安装，返回内存台账。
///
/// 装配顺序（规格 §7）：读 `plugin.json` → 反序列化 → 十条校验 → 按 id 分发到
/// [`builtin::install`]。**单个插件失败不影响宿主启动**：记一条 `Failed(原因)`
/// 并 `eprintln!` 继续——M1 没有日志器（D28），告警统一走 stderr。
///
/// 目录不存在时返回空台账并告警：开发机构建里删掉 `content/plugins` 不该让应用
/// 起不来；发布构建由启动自检兜底（`lib.rs` 的 `precheck_install_surface`）。
///
/// `_policy` 是 M2 的 process 通道要用的宿主策略，M1 只保留签名位置。
pub fn load_all(reg: &mut ToolRegistry, _policy: &Arc<Policy>) -> PluginRegistry {
    let dir = crate::config::bundled_plugins_dir();
    let mut registry = PluginRegistry::new();

    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("[plugins] 插件目录不可读：{}（{error}）", dir.display());
            return registry;
        }
    };

    let mut plugin_dirs: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    // 目录名升序：同 id 冲突时「先扫到的赢」是确定行为，不依赖文件系统顺序。
    plugin_dirs.sort();

    let mut seen_ids: HashSet<String> = HashSet::new();
    for plugin_dir in plugin_dirs {
        let Some(dir_name) = plugin_dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if dir_name.starts_with('.') {
            continue;
        }
        load_one(reg, &plugin_dir, dir_name, &mut seen_ids, &mut registry);
    }

    registry
}

/// 装配单个插件目录；任何一步失败都只影响这个插件。
fn load_one(
    reg: &mut ToolRegistry,
    plugin_dir: &Path,
    dir_name: &str,
    seen_ids: &mut HashSet<String>,
    registry: &mut PluginRegistry,
) {
    // manifest 读不出来就没有可记账的 id：报错并跳过，不建记录。
    let manifest = match manifest::read_manifest(plugin_dir) {
        Ok(manifest) => manifest,
        Err(error) => {
            eprintln!("[plugins] 跳过 {dir_name}：{error}");
            return;
        }
    };

    let id = manifest.id.clone();
    if let Err(error) = manifest::validate(&manifest, dir_name, seen_ids) {
        eprintln!("[plugins] {id} 校验失败：{error}");
        let reason = error.to_string();
        // 同 id 的后来者不得覆盖先到者的记录（先到先得，后者整体作废）。
        if !matches!(error, PluginError::DuplicateId(_)) {
            registry.insert(PluginRecord {
                manifest,
                state: PluginState::Failed(reason),
                tools: Vec::new(),
            });
        }
        return;
    }

    match builtin::install(&id, reg) {
        Ok(tools) => {
            registry.insert(PluginRecord {
                manifest,
                state: PluginState::Loaded,
                tools,
            });
        }
        Err(error) => {
            eprintln!("[plugins] {id} 安装失败：{error}");
            // 安装失败时 register_plugin_tools 已把本次注册摘干净。
            registry.insert(PluginRecord {
                manifest,
                state: PluginState::Failed(error.to_string()),
                tools: Vec::new(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Tool, ToolContext, ToolError, ToolOutput};
    use serde_json::{json, Value};

    /// 插件工具测试桩：只关心名字与 schema 合法性。
    struct StubTool {
        name: &'static str,
        schema: Value,
    }

    impl StubTool {
        fn ok(name: &'static str) -> Box<dyn Tool> {
            Box::new(StubTool {
                name,
                schema: json!({ "type": "object", "properties": {} }),
            })
        }
    }

    impl Tool for StubTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "插件测试桩"
        }

        fn parameters(&self) -> Value {
            self.schema.clone()
        }

        fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text(params))
        }
    }

    #[test]
    fn register_plugin_tools_registers_in_order() {
        let mut reg = ToolRegistry::new();
        let names = register_plugin_tools(
            &mut reg,
            "maa",
            vec![StubTool::ok("plugin_a"), StubTool::ok("plugin_b")],
        )
        .unwrap();
        assert_eq!(names, vec!["plugin_a", "plugin_b"]);
        assert!(reg.get("plugin_a").is_some());
        assert!(reg.get("plugin_b").is_some());
        assert_eq!(reg.list_defs().len(), 2);
    }

    #[test]
    fn duplicate_tool_name_fails_and_rolls_back() {
        let mut reg = ToolRegistry::new();
        reg.register(StubTool::ok("taken")).unwrap();

        let err = register_plugin_tools(
            &mut reg,
            "probe",
            vec![
                StubTool::ok("fresh_a"),
                StubTool::ok("taken"),
                StubTool::ok("fresh_b"),
            ],
        )
        .unwrap_err();

        assert_eq!(err, PluginError::ToolConflict("taken".to_string()));
        assert!(reg.get("fresh_a").is_none(), "回滚必须摘掉本次已注册的工具");
        assert!(reg.get("fresh_b").is_none());
        assert!(reg.get("taken").is_some(), "宿主工具不能被插件覆盖或摘除");
        assert_eq!(reg.list_defs().len(), 1, "注册表必须回到调用前的规模");
    }

    #[test]
    fn invalid_plugin_tool_schema_fails_and_rolls_back() {
        let mut reg = ToolRegistry::new();
        let bad = Box::new(StubTool {
            name: "bad_schema",
            schema: json!({ "type": "string" }),
        });
        let err = register_plugin_tools(&mut reg, "probe", vec![StubTool::ok("fresh_a"), bad])
            .unwrap_err();
        match err {
            PluginError::Invalid {
                field: "parameters",
                ..
            } => {}
            other => panic!("schema 非法应报 Invalid{{field: parameters}}，got: {other:?}"),
        }
        assert!(reg.get("fresh_a").is_none());
        assert_eq!(reg.list_defs().len(), 0);
    }

    #[test]
    fn empty_tool_list_is_ok() {
        let mut reg = ToolRegistry::new();
        let names = register_plugin_tools(&mut reg, "probe", vec![]).unwrap();
        assert!(names.is_empty());
        assert_eq!(reg.list_defs().len(), 0);
    }
}
