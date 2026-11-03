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

pub mod install;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::host::{FsService, HostServices, MaaBridgeService, QueryService};
use crate::policy::Policy;
use crate::tools::{Origin, Tool, ToolRegistry};

use self::manifest::PluginManifest;

pub use manifest::PluginKind;
#[cfg(test)]
pub(crate) use manifest::parse_manifest;
pub use registry::{PluginRecord, PluginRegistry, PluginState};

/// 台账句柄：技能索引靠它把「已加载的插件」翻译成技能根。
/// 装配时登记一次（`lib.rs`）；未登记时 `skill_roots()` 只返回宿主根。
static SKILL_LEDGER: OnceLock<Arc<Mutex<PluginRegistry>>> = OnceLock::new();

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
    reg: &ToolRegistry,
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
fn rollback_registered(reg: &ToolRegistry, registered: &[String]) {
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

/// 技能索引的根列表：宿主根在前，插件根按 id 排序在其后。
///
/// 索引是「先注册者赢」，宿主根排第一即宿主优先。插件根只含**已启用且加载
/// 成功**的插件（停用插件的技能不该出现在目录里）；技能目录固定为插件目录下
/// 的 `skills/`，`contributes.skills` 只作声明、目录为准。
pub fn skill_roots() -> Vec<crate::skills_index::SkillRoot> {
    let mut roots = vec![crate::skills_index::SkillRoot::host(
        crate::skills_index::skills_dir(),
    )];
    let Some(ledger) = SKILL_LEDGER.get() else {
        // 未登记（单元测试 / 装配早期）：只扫宿主根。
        return roots;
    };
    let guard = match ledger.lock() {
        Ok(guard) => guard,
        Err(poison) => poison.into_inner(),
    };
    let mut plugin_dirs: Vec<(String, String, PathBuf)> = guard
        .list()
        .into_iter()
        .filter(|record| record.state == PluginState::Loaded)
        .map(|record| {
            (
                record.manifest.id.clone(),
                record.manifest.name.clone(),
                record.dir.join("skills"),
            )
        })
        .filter(|(_, _, dir)| dir.is_dir())
        .collect();
    plugin_dirs.sort_by(|a, b| a.0.cmp(&b.0));
    for (id, display_name, dir) in plugin_dirs {
        roots.push(crate::skills_index::SkillRoot::plugin(
            dir,
            id,
            display_name,
        ));
    }
    roots
}

/// 登记台账句柄，让技能索引能按 id 找到插件技能根（`lib.rs` 装配时调一次）。
pub fn attach_skill_ledger(ledger: Arc<Mutex<PluginRegistry>>) {
    let _ = SKILL_LEDGER.set(ledger);
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
struct PluginsFile {
    #[serde(default)]
    enabled: HashMap<String, bool>,
}

/// 启停配置的唯一读写入口（M3 §八）。
///
/// 启停、安装、卸载都会改这份配置，各自「读文件 → 改 → 写回」会互相覆盖；
/// 所以内存里只留一份，所有写路径经它串行化并统一落盘。
pub struct PluginsConfig {
    path: PathBuf,
    file: Mutex<PluginsFile>,
}

impl PluginsConfig {
    /// 从 `data_dir` 读一次启停状态。缺失与读不懂都按「全启用」处理——插件
    /// 不该因为一个小文件写坏就整体消失；损坏的文件先备份成
    /// `plugins.json.bad` 再重建。
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(ENABLED_FILE_NAME);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Self {
                    path,
                    file: Mutex::new(PluginsFile::default()),
                };
            }
            Err(error) => {
                eprintln!(
                    "[plugins] 启用状态读不出来：{}（{error}），按全启用处理",
                    path.display()
                );
                return Self {
                    path,
                    file: Mutex::new(PluginsFile::default()),
                };
            }
        };

        let file = match serde_json::from_str::<PluginsFile>(&text) {
            Ok(file) => file,
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
                PluginsFile::default()
            }
        };

        Self {
            path,
            file: Mutex::new(file),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PluginsFile> {
        self.file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 没写过的插件默认启用（规格 §6：文件缺失 = 全启用）。
    pub fn is_enabled(&self, id: &str) -> bool {
        self.lock().enabled.get(id).copied().unwrap_or(true)
    }

    /// 当前全量状态（诊断 / 测试用）。
    #[allow(dead_code)] // 提交 3 的启停路径落笔后删除
    pub fn snapshot(&self) -> HashMap<String, bool> {
        self.lock().enabled.clone()
    }

    /// 写一条启停状态并落盘。未知 id 原样保留，不清洗（规格 §6）。
    #[allow(dead_code)] // 提交 3 的启停路径落笔后删除
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        let mut file = self.lock();
        file.enabled.insert(id.to_string(), enabled);
        self.persist(&file)
    }

    /// 删除一条状态（卸载时用：插件没了，旧开关不该继续留着）。
    #[allow(dead_code)] // 提交 3 的卸载路径落笔后删除
    pub fn forget(&self, id: &str) -> Result<(), String> {
        let mut file = self.lock();
        file.enabled.remove(id);
        self.persist(&file)
    }

    fn persist(&self, file: &PluginsFile) -> Result<(), String> {
        let text = serde_json::to_string_pretty(file).map_err(|error| error.to_string())?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        std::fs::write(&self.path, text)
            .map_err(|error| format!("写 {} 失败：{error}", self.path.display()))
    }
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
    reg: &ToolRegistry,
    roots: &PluginRoots,
    config: &PluginsConfig,
    services: &Arc<HostServices>,
    _policy: &Arc<Policy>,
) -> Arc<Mutex<PluginRegistry>> {
    let ledger = Arc::new(Mutex::new(PluginRegistry::new()));
    // 「已占用 id 的路径」跨两个目录共享；启停状态由 config 统一持有。
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
                config,
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
    reg: &ToolRegistry,
    services: &Arc<HostServices>,
    plugin_dir: &Path,
    dir_name: &str,
    scan: &mut Scan,
    config: &PluginsConfig,
    ledger: &Arc<Mutex<PluginRegistry>>,
) {
    // manifest 读不出来就没有可记账的 id：记进「未登记目录」让扩展页能看见，
    // 否则用户装错位置的插件会静默消失（S7）。
    let manifest = match manifest::read_manifest(plugin_dir) {
        Ok(manifest) => manifest,
        Err(error) => {
            eprintln!("[plugins] 跳过 {dir_name}：{error}");
            ledger
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .insert_unregistered(dir_name, &error.to_string());
            return;
        }
    };

    let id = manifest.id.clone();
    if let Err(error) = manifest::validate_with(
        &manifest,
        dir_name,
        &mut scan.seen_ids,
        &services.keys(),
    ) {
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
                    dir: plugin_dir.to_path_buf(),
                });
            }
        }
        return;
    }
    scan.paths.insert(id.clone(), plugin_dir.to_path_buf());

    // 启用状态在加载期的最后一刻生效：校验失败与停用是两回事，前者报错、后者安静跳过。
    if !config.is_enabled(&id) {
        eprintln!("[plugins] {id} 已在 {ENABLED_FILE_NAME} 里停用，跳过加载");
        insert(ledger, PluginRecord {
            manifest,
            state: PluginState::Disabled,
            tools: Vec::new(),
            reported_capabilities: Vec::new(),
            dir: plugin_dir.to_path_buf(),
        });
        return;
    }

    // process 插件的握手是同步的，但台账先落一条 Starting：装配期间若有别的
    // 读侧（M3 的扩展页），看到的是「正在启动」而不是「不存在」。
    if manifest.kind == PluginKind::Process {
        insert(ledger, PluginRecord {
            manifest: manifest.clone(),
            state: PluginState::Starting,
            tools: Vec::new(),
            reported_capabilities: Vec::new(),
            dir: plugin_dir.to_path_buf(),
        });
    }

    // 能力裁剪（声明 ∩ 已登记）与按 kind 分发都在 install_record 里：
    // 启用、重试走的是同一条路径，差别只在调用方怎么记状态。
    let outcome = install_record(&manifest, plugin_dir, reg, services, ledger);

    match outcome {
        Ok((tools, reported_capabilities)) => {
            warn_about_declared_skills(&manifest, plugin_dir);
            insert(ledger, PluginRecord {
                manifest,
                state: PluginState::Loaded,
                tools,
                reported_capabilities,
                dir: plugin_dir.to_path_buf(),
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
                dir: plugin_dir.to_path_buf(),
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

/// 声明与目录对不上时只告警：`contributes.skills` 是说明，目录才是事实。
fn warn_about_declared_skills(manifest: &PluginManifest, plugin_dir: &Path) {
    let skills_dir = plugin_dir.join("skills");
    for declared in &manifest.contributes.skills {
        if !skills_dir.join(declared).is_dir() {
            eprintln!(
                "[plugins] {} 声明的技能不存在：{declared}（以 skills/ 目录为准，已忽略）",
                manifest.id
            );
        }
    }
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

// ═══════════════════════════════════════════════════════
// 启停 / 重试 / 卸载（M3 §四、§六）
// ═══════════════════════════════════════════════════════

/// 取一份台账记录；不在台账里的 id 一律报错（命令层先列出来再操作）。
#[allow(dead_code)] // 提交 5 的 command 层接线后删除
fn record_of(ledger: &Arc<Mutex<PluginRegistry>>, id: &str) -> Result<PluginRecord, String> {
    ledger
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(id)
        .cloned()
        .ok_or_else(|| format!("插件 {id} 不在台账里"))
}

/// 按 manifest 装一次（启用与重试共用同一条路径）。
///
/// 与加载期唯一的区别是不写台账：调用方拿到结果后自己决定写 `Loaded` 还是
/// `Failed`，因为它的前置状态不同（停用 vs 进程退出）。
pub(super) fn install_record(
    manifest: &PluginManifest,
    plugin_dir: &Path,
    reg: &ToolRegistry,
    services: &Arc<HostServices>,
    ledger: &Arc<Mutex<PluginRegistry>>,
) -> Result<(Vec<String>, Vec<String>), PluginError> {
    let protocol = manifest
        .requires
        .as_ref()
        .and_then(|requires| requires.protocol)
        .unwrap_or(manifest::SUPPORTED_PROTOCOL);
    let granted =
        crate::host::resolve_granted(services.as_ref(), &manifest.capabilities, protocol).map_err(
            |error| PluginError::Invalid {
                field: "capabilities",
                value: error.to_string(),
            },
        )?;
    let owned: Arc<HostServices> = Arc::clone(services);
    let api: Arc<dyn crate::host::HostApi> = owned;
    let ctx = crate::host::PluginCtx::new(manifest.id.clone(), api, granted);

    match manifest.kind {
        PluginKind::Builtin => {
            builtin::install(&manifest.id, ctx, reg).map(|tools| (tools, Vec::new()))
        }
        PluginKind::Process => {
            let entry = manifest
                .entry
                .clone()
                .expect("process 插件必须有 entry（校验规则 5）");
            process::install(&manifest.id, plugin_dir, &entry, ctx, Arc::clone(ledger), reg)
        }
    }
}

/// 停用一个插件：摘工具、留墓碑、收进程，最后才落配置。
///
/// 顺序是有意的：配置写成功但工具还挂着，会出现「界面显示已停用、模型还能调」
/// 的分裂状态；反过来最坏只是重复停用一次。
#[allow(dead_code)] // 提交 5 的 command 层接线后删除
pub fn disable_plugin(
    id: &str,
    reg: &ToolRegistry,
    config: &PluginsConfig,
    services: &Arc<HostServices>,
    ledger: &Arc<Mutex<PluginRegistry>>,
) -> Result<(), String> {
    let record = record_of(ledger, id)?;
    match record.state {
        PluginState::Disabled => {
            config.set_enabled(id, false)?;
            return Ok(());
        }
        PluginState::Loaded => {}
        other => return Err(format!("插件 {id} 当前是 {other:?}，不能停用")),
    }

    for name in &record.tools {
        reg.remove(name);
    }
    // 冻结快照的对话里工具面还是旧的：墓碑让那些调用能得到一句人话，而不是 NotFound。
    reg.bury(&record.tools, &record.manifest.name, "已停用");

    if record.manifest.kind == PluginKind::Process {
        process::shutdown_plugin(id);
    }
    // 内置 MAA 的桥是宿主托管的独立进程：插件停了，它也该收场（§四）。
    if record
        .manifest
        .capabilities
        .iter()
        .any(|key| key == "maa.bridge")
    {
        let _ = crate::host::HostApi::call(
            services.as_ref(),
            "plugins",
            "maa.bridge",
            serde_json::json!({ "op": "shutdown" }),
        );
    }

    config.set_enabled(id, false)?;
    ledger
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .mark_disabled(id);
    Ok(())
}

/// 重新启用一个插件：重走安装路径，成功后才清墓碑、写配置。
#[allow(dead_code)] // 提交 5 的 command 层接线后删除
pub fn enable_plugin(
    id: &str,
    reg: &ToolRegistry,
    config: &PluginsConfig,
    services: &Arc<HostServices>,
    ledger: &Arc<Mutex<PluginRegistry>>,
) -> Result<(), String> {
    let record = record_of(ledger, id)?;
    if record.state == PluginState::Loaded {
        config.set_enabled(id, true)?;
        return Ok(());
    }

    // 先清掉旧墓碑：接下来要么装成功（不需要墓碑），要么装失败（该报 Failed 原因，
    // 而不是继续声称「已停用」）。
    reg.unbury(&record.tools);

    let (tools, reported) = install_record(&record.manifest, &record.dir, reg, services, ledger)
        .map_err(|error| {
            eprintln!("[plugins] {id} 重新启用失败：{error}");
            let reason = error.to_string();
            if let Ok(mut ledger) = ledger.lock() {
                ledger.mark_failed(id, reason.clone());
            }
            reason
        })?;

    config.set_enabled(id, true)?;
    ledger
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .mark_enabled(id, tools, reported);
    Ok(())
}

/// 重试一个「进程退出」的插件（只对 `Faulted` 有效，§三）。
#[allow(dead_code)] // 提交 5 的 command 层接线后删除
pub fn retry_plugin(id: &str, ledger: &Arc<Mutex<PluginRegistry>>) -> Result<(), String> {
    let record = record_of(ledger, id)?;
    if !matches!(record.state, PluginState::Faulted(_)) {
        return Err(format!("插件 {id} 当前不是进程退出状态，不能重试"));
    }
    process::retry(id).map_err(|error| format!("重试 {id} 失败：{error}"))?;
    ledger
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .mark_loaded(id);
    Ok(())
}

/// 卸载一个外部插件：摘工具、杀进程、删目录、清配置（§六）。
#[allow(dead_code)] // 提交 5 的 command 层接线后删除
pub fn uninstall_plugin(
    id: &str,
    delete_data: bool,
    roots: &PluginRoots,
    config: &PluginsConfig,
    reg: &ToolRegistry,
    ledger: &Arc<Mutex<PluginRegistry>>,
) -> Result<(), String> {
    let record = record_of(ledger, id)?;
    if record.manifest.kind == PluginKind::Builtin {
        return Err(format!("内置插件 {id} 不能卸载"));
    }

    for name in &record.tools {
        reg.remove(name);
    }
    reg.bury(&record.tools, &record.manifest.name, "已卸载");
    process::shutdown_plugin(id);

    remove_dir_all(&record.dir)?;
    remove_dir_all(&roots.data.join("plugin-temp").join(id))?;
    if delete_data {
        remove_dir_all(&roots.data.join("plugin-data").join(id))?;
    }

    config.forget(id)?;
    ledger
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .remove(id);
    Ok(())
}

/// 删一个目录；不存在不算错（卸载要可重复调用）。
#[allow(dead_code)] // 提交 5 的 command 层接线后删除
fn remove_dir_all(path: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("删除 {} 失败：{error}", path.display())),
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
        let reg = ToolRegistry::new();
        let names = register_plugin_tools(
            &reg,
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
        let reg = ToolRegistry::new();
        reg.register(StubTool::ok("taken")).unwrap();

        let err = register_plugin_tools(
            &reg,
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
        let reg = ToolRegistry::new();
        let bad = Box::new(StubTool {
            name: "bad_schema",
            schema: json!({ "type": "string" }),
        });
        let err = register_plugin_tools(&reg, "probe", vec![StubTool::ok("fresh_a"), bad])
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
        let reg = ToolRegistry::new();
        let names = register_plugin_tools(&reg, "probe", vec![]).unwrap();
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
        let config = PluginsConfig::load(&dir);
        assert!(config.snapshot().is_empty());
        assert!(config.is_enabled("maa"), "没写过就是启用");
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

        let config = PluginsConfig::load(&dir);
        config.set_enabled("maa", false).expect("写启用状态");

        // 换一份实例从磁盘读回：证明落盘的是完整状态，不是内存里那份。
        let state = PluginsConfig::load(&dir).snapshot();
        assert_eq!(state.get("maa"), Some(&false));
        assert_eq!(state.get("ghost"), Some(&true), "未知 id 不得被清洗");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_enabled_file_is_backed_up_and_ignored() {
        let dir = scratch("enabled-corrupt");
        std::fs::write(dir.join(ENABLED_FILE_NAME), "{ 不是 JSON").expect("写坏文件");

        let config = PluginsConfig::load(&dir);
        assert!(config.snapshot().is_empty(), "损坏时按全启用处理");
        assert!(config.is_enabled("maa"));
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

    /// 启停会从设置页、安装、卸载三处改同一份文件：内存副本 + 串行落盘，
    /// 交替二十次后磁盘状态必须与内存一致（旧实现每次读-改-写会丢更新）。
    #[test]
    fn repeated_toggling_keeps_memory_and_disk_in_sync() {
        let dir = scratch("enabled-toggle");
        let config = PluginsConfig::load(&dir);
        for round in 0..20 {
            let enabled = round % 2 == 0;
            config.set_enabled("maa", enabled).expect("写启用状态");
            assert_eq!(config.is_enabled("maa"), enabled);
        }
        assert_eq!(PluginsConfig::load(&dir).snapshot().get("maa"), Some(&false));
        assert!(!config.is_enabled("maa"), "最后一次是 false");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_plugin_is_recorded_but_not_installed() {
        let data = scratch("disabled-data");
        let config = PluginsConfig::load(&data);
        config.set_enabled("maa", false).expect("停用 maa");
        let roots = PluginRoots {
            bundled: crate::config::bundled_plugins_dir(),
            user: data.join("plugins"),
            data: data.clone(),
        };
        let (services, _query) = default_services(&roots, None);
        let reg = ToolRegistry::new();
        let policy = Arc::new(Policy::new(std::env::temp_dir()));
        let ledger = load_all(&reg, &roots, &config, &services, &policy);

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
        let reg = ToolRegistry::new();
        let policy = Arc::new(Policy::new(std::env::temp_dir()));
        let config = PluginsConfig::load(&roots.data);
        let ledger = load_all(&reg, &roots, &config, &services, &policy);

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
        let reg = ToolRegistry::new();
        let policy = Arc::new(Policy::new(std::env::temp_dir()));
        let config = PluginsConfig::load(&roots.data);
        let ledger = load_all(&reg, &roots, &config, &services, &policy);

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

    // ── 第 3 步：启停 / 重试 / 卸载 ──────────────────────────

    /// 一个装载了随包 maa 的测试装配。
    fn loaded(tag: &str) -> (PluginRoots, Arc<HostServices>, ToolRegistry, PluginsConfig, Arc<Mutex<PluginRegistry>>) {
        let data = scratch(tag);
        let roots = PluginRoots {
            bundled: crate::config::bundled_plugins_dir(),
            user: data.join("plugins"),
            data: data.clone(),
        };
        let (services, _query) = default_services(&roots, None);
        let reg = ToolRegistry::new();
        let policy = Arc::new(Policy::new(std::env::temp_dir()));
        let config = PluginsConfig::load(&roots.data);
        let ledger = load_all(&reg, &roots, &config, &services, &policy);
        (roots, services, reg, config, ledger)
    }

    #[test]
    fn disabling_removes_tools_and_writes_the_flag() {
        let (roots, services, reg, config, ledger) = loaded("disable-maa");
        assert!(reg.get("run_maa_task").is_some());

        disable_plugin("maa", &reg, &config, &services, &ledger).expect("停用 maa");

        assert!(reg.get("run_maa_task").is_none(), "停用后工具必须摘掉");
        assert!(!config.is_enabled("maa"), "plugins.json 要写 false");
        assert_eq!(
            ledger.lock().expect("台账锁不该中毒").get("maa").map(|r| r.state.clone()),
            Some(PluginState::Disabled)
        );

        // 旧对话的快照里还留着这个工具：调用要得到一句人话，而不是 NotFound。
        let ctx = crate::tools::ToolContext::new();
        match reg.execute("run_maa_task", &ctx, "{}") {
            Err(crate::tools::ToolError::Disabled(message)) => {
                assert!(message.contains("MAA 自动化"), "got: {message}");
                assert!(message.contains("已停用"), "got: {message}");
            }
            other => panic!("停用后的调用应报 Disabled，got: {other:?}"),
        }

        // 再启用：工具回来、墓碑清掉、配置写回 true。
        enable_plugin("maa", &reg, &config, &services, &ledger).expect("重新启用 maa");
        assert!(reg.get("run_maa_task").is_some(), "启用后工具应重新注册");
        assert!(config.is_enabled("maa"));
        assert_eq!(
            ledger.lock().expect("台账锁不该中毒").get("maa").map(|r| r.state.clone()),
            Some(PluginState::Loaded)
        );
        assert!(
            !matches!(
                reg.execute("run_maa_task", &ctx, "{}"),
                Err(crate::tools::ToolError::Disabled(_))
            ),
            "启用后不该再是 Disabled"
        );

        let _ = std::fs::remove_dir_all(&roots.data);
    }

    #[test]
    fn builtin_plugins_cannot_be_uninstalled() {
        let (roots, _services, reg, config, ledger) = loaded("uninstall-builtin");
        let err = uninstall_plugin("maa", false, &roots, &config, &reg, &ledger).unwrap_err();
        assert!(err.contains("不能卸载"), "got: {err}");
        assert!(reg.get("run_maa_task").is_some(), "拒绝之后不该动工具面");
        let _ = std::fs::remove_dir_all(&roots.data);
    }

    #[test]
    fn uninstalling_an_external_plugin_removes_its_directories() {
        let data = scratch("uninstall-external");
        let user = data.join("plugins");
        let dir = user.join("demo");
        std::fs::create_dir_all(&dir).expect("建外部插件目录");
        // 没有可执行文件 → 加载期记 Failed，但记录与目录都在，正是卸载要清的现场。
        std::fs::write(
            dir.join("plugin.json"),
            r#"{"manifestVersion":1,"id":"demo","name":"演示","version":"0.1.1",
                "kind":"process","entry":{"exe":"nope.exe"},"activation":[],
                "capabilities":[],"contributes":{}}"#,
        )
        .expect("写 manifest");
        let plugin_data = data.join("plugin-data").join("demo");
        std::fs::create_dir_all(&plugin_data).expect("建插件数据目录");
        std::fs::write(plugin_data.join("memo.txt"), "留着").expect("写数据");

        let roots = PluginRoots {
            bundled: crate::config::bundled_plugins_dir(),
            user,
            data: data.clone(),
        };
        let (services, _query) = default_services(&roots, None);
        let reg = ToolRegistry::new();
        let policy = Arc::new(Policy::new(std::env::temp_dir()));
        let config = PluginsConfig::load(&roots.data);
        config.set_enabled("demo", true).expect("写启用状态");
        let ledger = load_all(&reg, &roots, &config, &services, &policy);
        assert!(ledger.lock().expect("台账锁不该中毒").get("demo").is_some());

        uninstall_plugin("demo", true, &roots, &config, &reg, &ledger).expect("卸载 demo");

        assert!(!dir.exists(), "插件目录要删掉");
        assert!(!plugin_data.exists(), "勾了删数据才删数据");
        assert!(ledger.lock().expect("台账锁不该中毒").get("demo").is_none());
        assert!(config.snapshot().is_empty(), "卸载要清掉开关记录");

        let _ = std::fs::remove_dir_all(&data);
    }
}
