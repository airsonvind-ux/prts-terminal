//! 插件系统：契约层、能力装配与 process 通道。
//!
//! 设计依据：`50-插件系统/_插件系统-设计.md`（D1–D41）与 `_插件系统-M2规格.md`
//! （草案 4）。分工：[`manifest`] 解析与十条校验、[`registry`] 内存台账、
//! [`builtin`] 内置插件的显式安装表、[`transport`] 分帧、[`rpc`] 请求应答配对、
//! [`process`] process 插件的生命周期，[`load_all`] 负责把两者装到一起。
//!
//! 诚实边界（D11）：builtin 插件的 capability 只是声明，运行时门禁无意义；
//! process 插件的能力由 `PluginCtx::call` 逐次校验（D25），工具调用走
//! `tool.call`，插件反向调用宿主走 `host.call`。

pub mod builtin;

mod manifest;
mod process;
mod registry;
mod rpc;
mod transport;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::host::{FsService, HostServices, MaaBridgeService, QueryService};
use crate::policy::Policy;
use crate::tools::{Origin, Tool, ToolRegistry};

use self::manifest::PluginKind;

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
    /// process 插件的启动、握手或调用失败（`id` 是被装配的插件）。
    Process { id: String, detail: String },
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
            PluginError::Process { id, detail } => write!(f, "插件 {id} 运行失败：{detail}"),
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

/// 插件系统的根目录（R2：一律构造时传入，不在调用期读 `config::*`）。
///
/// - `bundled`：随包发布的插件（开发构建指向源码树，发布构建指向 `resources/plugins`）；
/// - `user`：用户安装的插件（M2 只扫描、不安装，M4 才做安装流程）；
/// - `data`：数据根，`fs.plugin` / `fs.temp` / 桥的工作目录都从它推。
#[derive(Debug, Clone)]
pub struct PluginRoots {
    pub bundled: PathBuf,
    pub user: PathBuf,
    pub data: PathBuf,
}

impl PluginRoots {
    /// 生产装配用的一份（`lib.rs` 只调这里，测试自己造临时根）。
    pub fn from_config() -> Self {
        let data = crate::config::data_dir();
        Self {
            bundled: crate::config::bundled_plugins_dir(),
            user: data.join("plugins"),
            data,
        }
    }
}

/// 构造 M2a 登记的四条服务，返回注册表与 `query.call` 的绑定句柄。
///
/// `query.call` 需要工具注册表，而注册表要等 `load_all` 跑完才定型，所以这里
/// 把句柄一并交回去：`lib.rs` 在 `Arc::new(reg)` 之后调
/// [`crate::host::QueryService::bind_tools`]。
pub fn default_services(
    roots: &PluginRoots,
    db: Option<Arc<crate::db::Database>>,
) -> (Arc<HostServices>, Arc<QueryService>) {
    let query = Arc::new(QueryService::new(db));
    let query_owned: Arc<QueryService> = Arc::clone(&query);
    let query_service: Arc<dyn crate::host::HostService> = query_owned;
    let mut services = HostServices::new();
    services
        .register(Arc::new(FsService::new(roots.data.clone())))
        .expect("fs.plugin 只能登记一次");
    services
        .register(Arc::new(FsService::temp(roots.data.clone())))
        .expect("fs.temp 只能登记一次");
    services
        .register(query_service)
        .expect("query.call 只能登记一次");
    services
        .register(Arc::new(MaaBridgeService::new(roots.data.clone())))
        .expect("maa.bridge 只能登记一次");
    (Arc::new(services), query)
}

// ═══════════════════════════════════════════════════════
// 启用状态（data_dir()/plugins.json）
// ═══════════════════════════════════════════════════════

/// 启停状态文件名（D18：配置与启停状态分两处，这个文件只管启停）。
pub const ENABLED_FILE_NAME: &str = "plugins.json";

#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
struct EnabledFile {
    #[serde(default)]
    enabled: HashMap<String, bool>,
}

/// 读启用状态。缺失与读不懂都按「全启用」处理——插件不该因为一个小文件
/// 写坏就整体消失；损坏的文件先备份成 `plugins.json.bad` 再重建。
fn load_enabled(data_dir: &Path) -> HashMap<String, bool> {
    let path = data_dir.join(ENABLED_FILE_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(error) => {
            eprintln!(
                "[plugins] 启用状态读不出来：{}（{error}），按全启用处理",
                path.display()
            );
            return HashMap::new();
        }
    };

    match serde_json::from_str::<EnabledFile>(&text) {
        Ok(file) => file.enabled,
        Err(error) => {
            let backup = path.with_extension("json.bad");
            let note = match std::fs::rename(&path, &backup) {
                Ok(()) => format!("已备份到 {}", backup.display()),
                Err(rename_error) => format!("备份失败（{rename_error}），原文件未动"),
            };
            eprintln!(
                "[plugins] 启用状态损坏：{}（{error}），{note}；本次按全启用处理",
                path.display()
            );
            HashMap::new()
        }
    }
}

/// 写启用状态（M3 的扩展页调；M2b 只读，但落盘格式在这里定死）。
#[cfg_attr(not(test), allow(dead_code))]
pub fn set_enabled(data_dir: &Path, id: &str, enabled: bool) -> Result<(), String> {
    let path = data_dir.join(ENABLED_FILE_NAME);
    // 先读回原内容：未知 id 要原样保留（规格 §6「保留不清洗」）。
    let mut state = load_enabled(data_dir);
    state.insert(id.to_string(), enabled);
    let file = EnabledFile { enabled: state };
    let text = serde_json::to_string_pretty(&file).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(data_dir).map_err(|error| error.to_string())?;
    std::fs::write(&path, text).map_err(|error| format!("写 {} 失败：{error}", path.display()))
}

/// 扫描插件目录，逐个校验并安装，返回内存台账。
///
/// 装配顺序（规格 §7）：读 `plugin.json` → 反序列化 → 十条校验 → 能力裁剪 →
/// 按 id 分发到 [`builtin::install`] / [`process::install`]。**单个插件失败不影响
/// 宿主启动**：记一条 `Failed(原因)` 并 `eprintln!` 继续——M2 仍没有日志器
/// （D28），告警统一走 stderr。
///
/// 扫描顺序是先 `bundled` 后 `user`：同 id 时随包插件先到先得，用户目录里的
/// 同名者不覆盖、不记账（只报一条错）。
///
/// 目录不存在时返回空台账并告警：开发机构建里删掉 `content/plugins` 不该让应用
/// 起不来；发布构建由启动自检兜底（`lib.rs` 的 `precheck_install_surface`）。
///
/// 台账用 `Arc<Mutex<…>>` 返回：加载路径写 `Loaded`/`Failed`，调用路径写
/// `Faulted`（规格 §6），两边必须看到同一份记录。
///
/// `_policy` 是 M2b 的 process 通道要用的宿主策略，M2a 只保留签名位置。
pub fn load_all(
    reg: &mut ToolRegistry,
    roots: &PluginRoots,
    services: &Arc<HostServices>,
    _policy: &Arc<Policy>,
) -> Arc<Mutex<PluginRegistry>> {
    let ledger = Arc::new(Mutex::new(PluginRegistry::new()));
    // 启用状态与「已占用 id 的路径」都跨两个目录共享。
    let enabled = load_enabled(&roots.data);
    let mut scan = Scan::default();

    for dir in [&roots.bundled, &roots.user] {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                // 用户目录在多数机器上还不存在：那是正常状态，只在随包目录报错。
                if dir == &roots.bundled {
                    eprintln!("[plugins] 插件目录不可读：{}（{error}）", dir.display());
                }
                continue;
            }
        };

        let mut plugin_dirs: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        // 目录名升序：同 id 冲突时「先扫到的赢」是确定行为，不依赖文件系统顺序。
        plugin_dirs.sort();

        for plugin_dir in plugin_dirs {
            let Some(dir_name) = plugin_dir.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if dir_name.starts_with('.') {
                continue;
            }
            load_one(
                reg,
                services,
                &plugin_dir,
                dir_name,
                &mut scan,
                &enabled,
                &ledger,
            );
        }
    }

    ledger
}

/// 一次扫描的跨插件状态：已占用的 id（校验规则 2）与已成功加载插件的路径。
#[derive(Default)]
struct Scan {
    seen_ids: HashSet<String>,
    paths: HashMap<String, PathBuf>,
}

/// 装配单个插件目录；任何一步失败都只影响这个插件。
#[allow(clippy::too_many_arguments)]
fn load_one(
    reg: &mut ToolRegistry,
    services: &Arc<HostServices>,
    plugin_dir: &Path,
    dir_name: &str,
    scan: &mut Scan,
    enabled: &HashMap<String, bool>,
    ledger: &Arc<Mutex<PluginRegistry>>,
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
    if let Err(error) = manifest::validate(&manifest, dir_name, &mut scan.seen_ids) {
        match &error {
            // 同 id 的外部插件不写台账、不覆盖内置记录（N3），只报清两个路径。
            PluginError::DuplicateId(duplicated) => {
                let first = scan
                    .paths
                    .get(duplicated)
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "（已登记的插件）".to_string());
                eprintln!(
                    "[plugins] 跳过外部插件：{}（id={duplicated}）与已加载的 {first} 同名，内置优先",
                    plugin_dir.display()
                );
            }
            _ => {
                eprintln!("[plugins] {dir_name}（id={id}）校验失败：{error}");
                insert(ledger, PluginRecord {
                    manifest,
                    state: PluginState::Failed(error.to_string()),
                    tools: Vec::new(),
                    reported_capabilities: Vec::new(),
                });
            }
        }
        return;
    }
    scan.paths.insert(id.clone(), plugin_dir.to_path_buf());

    // 启用状态在加载期的最后一刻生效：校验失败与停用是两回事，前者报错、后者安静跳过。
    if !enabled.get(&id).copied().unwrap_or(true) {
        eprintln!("[plugins] {id} 已在 {ENABLED_FILE_NAME} 里停用，跳过加载");
        insert(ledger, PluginRecord {
            manifest,
            state: PluginState::Disabled,
            tools: Vec::new(),
            reported_capabilities: Vec::new(),
        });
        return;
    }

    // 能力裁剪：声明 ∩ 已登记；未登记、需审批、协议不够都在这里整体失败（D25）。
    let protocol = manifest
        .requires
        .as_ref()
        .and_then(|requires| requires.protocol)
        .unwrap_or(manifest::SUPPORTED_PROTOCOL);
    let granted = match crate::host::resolve_granted(services.as_ref(), &manifest.capabilities, protocol)
    {
        Ok(granted) => granted,
        Err(error) => {
            let reason = error.to_string();
            eprintln!("[plugins] {id} 能力裁剪失败：{reason}");
            insert(ledger, PluginRecord {
                manifest,
                state: PluginState::Failed(reason),
                tools: Vec::new(),
                reported_capabilities: Vec::new(),
            });
            return;
        }
    };

    let owned: Arc<HostServices> = Arc::clone(services);
    let api: Arc<dyn crate::host::HostApi> = owned;
    let ctx = crate::host::PluginCtx::new(id.clone(), api, granted);

    // process 插件的握手是同步的，但台账先落一条 Starting：装配期间若有别的
    // 读侧（M3 的扩展页），看到的是「正在启动」而不是「不存在」。
    if manifest.kind == PluginKind::Process {
        insert(ledger, PluginRecord {
            manifest: manifest.clone(),
            state: PluginState::Starting,
            tools: Vec::new(),
            reported_capabilities: Vec::new(),
        });
    }

    let outcome = match manifest.kind {
        PluginKind::Builtin => builtin::install(&id, ctx, reg).map(|tools| (tools, Vec::new())),
        PluginKind::Process => {
            // 校验已保证 process 一定有 entry。
            let entry = manifest
                .entry
                .clone()
                .expect("process 插件必须有 entry（校验规则 5）");
            process::install(&id, plugin_dir, &entry, ctx, Arc::clone(ledger), reg)
        }
    };

    match outcome {
        Ok((tools, reported_capabilities)) => {
            insert(ledger, PluginRecord {
                manifest,
                state: PluginState::Loaded,
                tools,
                reported_capabilities,
            });
        }
        Err(error) => {
            eprintln!("[plugins] {id} 安装失败：{error}");
            // 安装失败时 register_plugin_tools 与 process::install 都已把本次
            // 注册摘干净、把进程收掉。
            insert(ledger, PluginRecord {
                manifest,
                state: PluginState::Failed(error.to_string()),
                tools: Vec::new(),
                reported_capabilities: Vec::new(),
            });
        }
    }
}

/// 写台账。锁中毒说明别处已经 panic 过，这里沿用既有数据继续记。
fn insert(ledger: &Arc<Mutex<PluginRegistry>>, record: PluginRecord) {
    ledger
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(record);
}

/// 退出路径：收掉全部 process 插件，并清空 `plugin-temp`（规格 §4：退出时整体清理）。
///
/// 只由 `lib.rs` 的 `RunEvent::Exit` 调用。顺序不能反：先杀进程再删目录，
/// 否则还在跑的插件可能把文件写回来。
pub fn shutdown() {
    process::shutdown_all();
    let temp = crate::config::data_dir().join("plugin-temp");
    if let Err(error) = std::fs::remove_dir_all(&temp) {
        if error.kind() != std::io::ErrorKind::NotFound {
            eprintln!("[plugins] 清理临时目录失败：{}（{error}）", temp.display());
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

    // ── 4b：plugins.json 与外部目录 ──────────────────────────

    /// 一个只给本用例用的临时目录（测试并行，名字带计数器）。
    fn scratch(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "prts-plugins-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn missing_enabled_file_means_everything_is_enabled() {
        let dir = scratch("missing-enabled");
        assert!(load_enabled(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn enabled_file_round_trips_and_keeps_unknown_ids() {
        let dir = scratch("enabled-round-trip");
        // 先手写一条未知 id：M2 没有对应插件，但它必须被保留（规格 §6）。
        std::fs::write(
            dir.join(ENABLED_FILE_NAME),
            r#"{"enabled":{"ghost":true}}"#,
        )
        .expect("写初始文件");

        set_enabled(&dir, "maa", false).expect("写启用状态");
        let state = load_enabled(&dir);
        assert_eq!(state.get("maa"), Some(&false));
        assert_eq!(state.get("ghost"), Some(&true), "未知 id 不得被清洗");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_enabled_file_is_backed_up_and_ignored() {
        let dir = scratch("enabled-corrupt");
        std::fs::write(dir.join(ENABLED_FILE_NAME), "{ 不是 JSON").expect("写坏文件");

        assert!(load_enabled(&dir).is_empty(), "损坏时按全启用处理");
        assert!(
            dir.join(format!("{ENABLED_FILE_NAME}.bad")).is_file(),
            "损坏的文件要备份成 plugins.json.bad"
        );
        assert!(
            !dir.join(ENABLED_FILE_NAME).is_file(),
            "备份后原文件应当已经不在了"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_plugin_is_recorded_but_not_installed() {
        let data = scratch("disabled-data");
        set_enabled(&data, "maa", false).expect("停用 maa");
        let roots = PluginRoots {
            bundled: crate::config::bundled_plugins_dir(),
            user: data.join("plugins"),
            data: data.clone(),
        };
        let (services, _query) = default_services(&roots, None);
        let mut reg = ToolRegistry::new();
        let policy = Arc::new(Policy::new(std::env::temp_dir()));
        let ledger = load_all(&mut reg, &roots, &services, &policy);

        let ledger = ledger.lock().expect("台账锁不该中毒");
        let record = ledger.get("maa").expect("停用的插件也要留一条记录");
        assert_eq!(record.state, PluginState::Disabled);
        assert!(record.tools.is_empty());
        assert!(reg.get("run_maa_task").is_none(), "停用的插件不得注册工具");
        drop(ledger);

        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn external_plugin_with_a_bundled_id_does_not_override_it() {
        let data = scratch("duplicate-data");
        let user = data.join("plugins");
        let external = user.join("maa");
        std::fs::create_dir_all(&external).expect("建外部插件目录");
        std::fs::write(
            external.join("plugin.json"),
            r#"{"manifestVersion":1,"id":"maa","name":"外部的 maa","version":"0.1.1",
                "kind":"builtin","activation":[],"capabilities":[],"contributes":{}}"#,
        )
        .expect("写外部 manifest");

        let roots = PluginRoots {
            bundled: crate::config::bundled_plugins_dir(),
            user,
            data: data.clone(),
        };
        let (services, _query) = default_services(&roots, None);
        let mut reg = ToolRegistry::new();
        let policy = Arc::new(Policy::new(std::env::temp_dir()));
        let ledger = load_all(&mut reg, &roots, &services, &policy);

        let ledger = ledger.lock().expect("台账锁不该中毒");
        let record = ledger.get("maa").expect("随包的 maa 仍应在台账里");
        assert_eq!(record.state, PluginState::Loaded);
        assert_eq!(record.manifest.name, "MAA 自动化", "内置那份优先");
        assert_eq!(record.tools, vec!["run_maa_task".to_string()]);
        assert_eq!(reg.list_defs().len(), 1, "同名外部插件不得再注册一遍");
        drop(ledger);

        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn broken_manifest_does_not_break_startup() {
        let data = scratch("broken-data");
        let user = data.join("plugins");
        let broken = user.join("broken");
        std::fs::create_dir_all(&broken).expect("建坏插件目录");
        std::fs::write(broken.join("plugin.json"), "{ 这不是 JSON").expect("写坏 manifest");

        let roots = PluginRoots {
            bundled: crate::config::bundled_plugins_dir(),
            user,
            data: data.clone(),
        };
        let (services, _query) = default_services(&roots, None);
        let mut reg = ToolRegistry::new();
        let policy = Arc::new(Policy::new(std::env::temp_dir()));
        let ledger = load_all(&mut reg, &roots, &services, &policy);

        let ledger = ledger.lock().expect("台账锁不该中毒");
        assert!(ledger.get("broken").is_none(), "读不出 manifest 的目录不记账");
        assert_eq!(
            ledger.get("maa").map(|record| record.state.clone()),
            Some(PluginState::Loaded),
            "坏插件不该拖垮后面的插件"
        );
        drop(ledger);
        assert!(reg.get("run_maa_task").is_some());

        let _ = std::fs::remove_dir_all(&data);
    }
}
