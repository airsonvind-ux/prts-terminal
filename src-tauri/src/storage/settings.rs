//! settings —— 用户设置持久化层（data_dir/settings.json）
//!
//! 单一 settings.json 承载分段：llm（现 LlmConfig 全字段 + thinking_level）/
//! exec（ExecutionSettings）/ workspace / window。生命周期与可靠性与
//! session_store 的 index.json 同款策略：
//!
//! - **tmp + rename 原子替换 + 写后重读校验**（校验失败 Err 上抛——调用方
//!   set_settings 返回 Err → 前端可见；内存已同步，下次变更兜底重写）
//! - **schemaVersion 过高 → 只读降级**：get 返回默认、set 拒绝（绝不覆盖写
//!   高版本数据，session_store 同原则）
//! - **文件缺失 → 默认**（不落盘；首次 set_settings 成功保存时才建文件）
//! - **parse 失败 → 备份 settings.json.bak 后回默认**（损坏不阻断启动）
//! - 读端 normalize：未知字段/缺字段由 serde default 兜底（未写 = 默认）
//!
//! 装配语义（设置页扩展定稿）：**文件存在用文件；文件缺失用默认值；env 完全不
//! 参与配置装配**（PRTS_LLM_* / PRTS_WORKSPACE 已从 dev.ps1 清除）。
//!
//! 内存职责划分：各段运行 State（llm_config / exec_settings）仍是运行期权威，
//! 本模块 = 持久化层 + get_settings 读取源；set_settings 单点同步两侧
//! （先 patch 本层，再同步 State，最后 save；锁序见 commands::set_settings 注释）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::exec_settings::ExecutionSettings;
use crate::llm::{ApiType, LlmConfig};
use crate::maa::settings::MaaSettings;

/// settings.json 文件名（data_dir 下）。
pub const SETTINGS_FILE: &str = "settings.json";
const SETTINGS_TMP: &str = "settings.json.tmp";
pub const SETTINGS_BAK: &str = "settings.json.bak";

/// 当前 schema 版本。**v2 = llm 段供应商分区**（v1 单份结构在 load 嗅探迁移）；
/// 新增普通字段走 Option/serde default 不升版；结构性破坏才升版 → 旧程序读
/// 新文件进入只读降级（绝不覆盖写高版本数据）。
pub const SCHEMA_VERSION: u32 = 2;

/// api_key 掩码占位（get_settings 下发；set_settings 三态解析见 [`LlmPatch::api_key`]）。
pub const MASKED_SECRET: &str = "********";

// ═══════════════════════════════════════════════════════
// 盘上结构
// ═══════════════════════════════════════════════════════

/// 设置全量（settings.json 顶层）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// 写入时置当前版本；load 时校验（过高 → 只读降级）。
    #[serde(default)]
    pub schema_version: u32,
    /// v2 分区结构（active + providers 各配；thinking_level 全局顶层）
    #[serde(default)]
    pub llm: LlmSection,
    #[serde(default)]
    pub exec: ExecutionSettings,
    #[serde(default)]
    pub maa: MaaSettings,
    #[serde(default)]
    pub workspace: WorkspaceSettings,
    #[serde(default)]
    pub window: WindowSettings,
    /// 界面态（非配置）：引导「已看过」键集等。与配置同文件不同段——它的写入
    /// 不受 set_settings 的 busy 闸约束（那道闸管的是配置），走独立命令。
    #[serde(default)]
    pub ui: UiSection,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            llm: LlmSection::default(),
            exec: ExecutionSettings::default(),
            maa: MaaSettings::default(),
            workspace: WorkspaceSettings::default(),
            window: WindowSettings::default(),
            ui: UiSection::default(),
        }
    }
}

// ═══════════════════════════════════════════════════════
// ui 段（界面态）
// ═══════════════════════════════════════════════════════

/// 界面态：与「配置」分开——它记录的是「界面见过什么」，不是用户的选择。
///
/// 为什么不并进配置：`set_settings` 有 busy 闸（对话处理中拒绝一切配置写入），
/// 而「看过没」必须在任何时候都能落盘（设置页通知可能正好赶上处理中）。
/// 故单独两个命令 `get_guide_seen` / `mark_guide_seen` 直接读写本段。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UiSection {
    /// 已看过的引导项 key（欢迎页 3 + 设置页 9 + 首次出现型 2，见台账引导文案）。
    #[serde(default)]
    pub guide_seen: Vec<String>,
}

// ═══════════════════════════════════════════════════════
// llm 段（v2 供应商分区）
// ═══════════════════════════════════════════════════════

/// llm 段：active 供应商 + 各供应商独立配置 + 全局思考偏好。
///
/// providers 键 = 供应商名（预设 id 或自定义名）；条目懒建（patch 首次配某家
/// 时创建，缺省字段走 ProviderConfig default）。多供应商各存各家 key/模型/
/// base_url——切换供应商零丢失（B 方案核心，修复单槽退化）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LlmSection {
    #[serde(default)]
    pub active_provider: String,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    /// 全局思考偏好（跨供应商；请求时按 active 模型白名单 clamp——thinking.rs
    /// 消费 resolve 后的 LlmConfig.thinking_level）。空 = 跟随模型默认档。
    #[serde(default)]
    pub thinking_level: Option<String>,
}

/// 单家供应商配置（原单份 llm 字段去 provider）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default)]
    pub api_type: ApiType,
    #[serde(default)]
    pub model: String,
    /// API Key。**持久层不留值**：settings.json 上一律写空，钥值由
    /// crate::credentials 加密落盘（DPAPI，绑当前用户与本机），运行时经
    /// LlmSection::resolve_active_with 取；本字段仅作内存载体（历史明文迁移读取用）。
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub headers: Option<String>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
}

fn default_max_tokens() -> u32 {
    4096
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            api_type: ApiType::OpenAiCompletions,
            model: String::new(),
            api_key: String::new(),
            base_url: None,
            headers: None,
            max_tokens: 4096,
        }
    }
}

impl LlmSection {
    /// 由 active 供应商 resolve 出请求用 LlmConfig（下游零改动：适配器/
    /// orchestrator/thinking 照旧消费）。active 无条目/空 → LlmConfig::default()
    /// （未配置态）。thinking_level 取段顶层全局值。
    /// 同 [`LlmSection::resolve_active`]，但密钥由外部查（凭据层）提供。
    ///
    /// 持久层已经不留明文（见 crate::credentials），运行时值必须从凭据层取。
    /// 这层薄包装让"取钥"只有一个出口，适配器与 orchestrator 零改动。
    pub fn resolve_active_with<F>(&self, lookup: F) -> LlmConfig
    where
        F: Fn(&str) -> Option<String>,
    {
        let mut cfg = self.resolve_active();
        if !cfg.provider.is_empty() {
            cfg.api_key = lookup(&cfg.provider).unwrap_or_default();
        }
        cfg
    }

    pub fn resolve_active(&self) -> LlmConfig {
        let Some(pc) = self.providers.get(&self.active_provider) else {
            return LlmConfig::default();
        };
        LlmConfig {
            provider: self.active_provider.clone(),
            api_type: pc.api_type,
            model: pc.model.clone(),
            api_key: pc.api_key.clone(),
            base_url: pc.base_url.clone(),
            headers: pc.headers.clone(),
            max_tokens: pc.max_tokens,
            thinking_level: self.thinking_level.clone(),
        }
    }

    /// 按会话保存的 provider/model/thinking 解析请求配置；凭据仍从全局
    /// 连接配置取（对话只决定“用哪家、哪个模型、什么档”）。
    ///
    /// provider 未配置 → None（调用方应明确报错，不做静默回退）。
    pub fn resolve_for<F>(
        &self,
        provider: &str,
        model: &str,
        thinking_level: Option<&str>,
        lookup: F,
    ) -> Option<LlmConfig>
    where
        F: Fn(&str) -> Option<String>,
    {
        let pc = self.providers.get(provider)?;
        Some(LlmConfig {
            provider: provider.to_string(),
            api_type: pc.api_type,
            model: model.to_string(),
            api_key: lookup(provider).unwrap_or_default(),
            base_url: pc.base_url.clone(),
            headers: pc.headers.clone(),
            max_tokens: pc.max_tokens,
            thinking_level: thinking_level
                .filter(|v| !v.trim().is_empty())
                .map(str::to_string),
        })
    }
}

/// workspace 段。workspace_root 空串 = 用默认工作区
/// （`<主目录>/PRTS-WorkSpace`，见 `crate::config::default_workspace_path`；
/// 空值不是「无工作目录」，故不存在空态）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WorkspaceSettings {
    #[serde(default)]
    pub workspace_root: String,
}

/// window 段。always_on_top = 运行期置顶（启动时 setup 应用初值；
/// 运行时经 set_always_on_top 命令即时切换 + 写回本字段）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSettings {
    #[serde(default = "default_always_on_top")]
    pub always_on_top: bool,
}

fn default_always_on_top() -> bool {
    true // 与 tauri.conf.json 静态初值一致
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            always_on_top: default_always_on_top(),
        }
    }
}

// ═══════════════════════════════════════════════════════
// 局部 patch（per-field 语义：字段缺席 = 不更新）
// ═══════════════════════════════════════════════════════
//
// Option<T> 字段：Some(v) = 更新为 v（None 缺席 = 不更新）。
// Option<Option<T>> 字段（serde double-option）：null = 显式清除回默认，
//   缺席 = 不更新。沿用压缩模块 MetaPatch 的「显式清空标记」先例。

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SettingsPatch {
    pub llm: Option<LlmPatch>,
    pub exec: Option<ExecPatch>,
    pub maa: Option<MaaPatch>,
    pub workspace: Option<WorkspacePatch>,
    pub window: Option<WindowPatch>,
}

impl SettingsPatch {
    /// 本次 patch 是否携带 API Key 写入/清除（掩码占位不算）。
    /// 命令层用它把凭据禁写挡在 `apply_patch` 之前，避免设置内存半应用。
    pub fn touches_api_key(&self) -> bool {
        self.llm
            .as_ref()
            .and_then(|l| l.providers.as_ref())
            .map(|providers| {
                providers.values().any(|p| match p.api_key.as_deref() {
                    Some(v) => v != MASKED_SECRET,
                    None => false,
                })
            })
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LlmPatch {
    /// 切换 active 供应商（只动指针；目标家配置在 providers 内保留）。
    pub active_provider: Option<String>,
    /// 供应商级字段 patch（深层合并：键缺席 = 不更新该家；新键 = 懒建条目）。
    pub providers: Option<HashMap<String, ProviderPatch>>,
    /// 全局思考偏好（canonical 档）；Some("") = 清除回"跟随模型默认"。
    pub thinking_level: Option<String>,
}

/// 单家供应商字段 patch（per-field：字段缺席 = 不更新；空串清除沿用）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProviderPatch {
    pub api_type: Option<ApiType>,
    /// Some(v) 更新（含空串 = 清除模型选择）；缺席 = 不更新。
    pub model: Option<String>,
    /// **三态解析**（按家）：None = 不更新（keyEdited=false 前端不发）；
    /// Some("********") = 掩码占位 → 保留该家现值；Some("") = 清除；
    /// 其他 = 明文新 key 写入。
    pub api_key: Option<String>,
    /// Some(v) 更新；空串 = 清除回 None；缺席 = 不更新。
    pub base_url: Option<String>,
    /// Some(v) 更新；空串 = 清除回 None；缺席 = 不更新。
    pub headers: Option<String>,
    pub max_tokens: Option<u32>,
}

impl LlmPatch {
    /// 本 patch 是否触及 llm 段（决定是否同步 llm_config State）。
    pub fn is_empty(&self) -> bool {
        self.active_provider.is_none() && self.providers.is_none() && self.thinking_level.is_none()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExecPatch {
    pub mode: Option<crate::exec_settings::ExecutionMode>,
}

/// MAA 段的字段 patch（per-field：缺席 = 不更新；字符串空串 = 回自动）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MaaPatch {
    pub server: Option<crate::maa::settings::Server>,
    pub auto_detect: Option<bool>,
    pub screenshot_extras: Option<bool>,
    pub touch_extras: Option<bool>,
    pub emulator: Option<String>,
    pub emulator_path: Option<String>,
    pub adb_path: Option<String>,
    pub address: Option<String>,
}

impl ExecPatch {
    /// 本 patch 是否触及 exec 段（保留备用；当前 apply_patch 以 mode 字段判定）。
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.mode.is_none()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WorkspacePatch {
    /// Some(v) 直接写入（**含空串 = 回默认工作区**）；缺席 = 不更新。
    /// 修改**即时生效**：命令层写后即更新工作根共享句柄（见 `crate::workspace`）。
    pub workspace_root: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WindowPatch {
    pub always_on_top: Option<bool>,
}

// ═══════════════════════════════════════════════════════
// patch 应用结果（命令层消费）
// ═══════════════════════════════════════════════════════

/// patch 携带的密钥操作（命令层落凭据层用）。
///
/// **明文一律不进 settings 持久层**：这里只承接"要写什么"，钥值本身由
/// crate::credentials 加密落盘（DPAPI）。空串 = 清除该家密钥。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyOp {
    /// 供应商名（providers 表的键）。
    pub provider: String,
    /// 明文；空串 = 清除。
    pub plaintext: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PatchChanges {
    /// llm.provider / llm.model 任一变化（notices Model 维度触发点）。
    pub model_changed: bool,
    /// llm 段有实际变化（需同步 llm_config State）。
    pub llm_changed: bool,
    /// exec 段有实际变化。
    pub exec_changed: bool,
    /// maa 段有实际变化。
    pub maa_changed: bool,
    /// workspace 段有实际变化（命令层据此更新工作根共享句柄——热生效）。
    pub workspace_changed: bool,
    /// llm 段携带的密钥变更（命令层据此写凭据层；settings 侧不留值）。
    pub key_ops: Vec<KeyOp>,
}

// ═══════════════════════════════════════════════════════
// SettingsStore
// ═══════════════════════════════════════════════════════

/// 加载来源（决定 set 是否放行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// 文件缺失（首启）：默认值；首次 set 保存时创建文件。
    Default,
    /// 正常从文件加载。
    File,
    /// 文件 parse 失败（已尝试备份 .bak）：回默认；首次 set 覆盖写。
    Corrupted,
    /// 文件 schema_version 高于当前：**只读降级**（get 回默认、set 拒绝，
    /// 绝不覆盖写高版本数据）。
    FutureVersion(u32),
}

/// 设置存储：内存权威副本 + data_dir 定位 + 来源标记。
pub struct SettingsStore {
    pub settings: Settings,
    source: Source,
    /// data_dir（settings.json 所在目录；load 注入）
    data_dir: PathBuf,
    path: PathBuf,
}

impl SettingsStore {
    /// 从 data_dir 加载（确保目录存在）。损坏 → 备份 + 回默认（不 panic）。
    pub fn load(data_dir: &Path) -> Self {
        std::fs::create_dir_all(data_dir).ok();
        let path = data_dir.join(SETTINGS_FILE);
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Self {
                    settings: Settings::default(),
                    source: Source::Default,
                    data_dir: data_dir.to_path_buf(),
                    path,
                };
            }
            Err(e) => {
                // 权限等读取失败：按损坏处理（尝试备份后回默认）
                eprintln!("[settings] 读取失败（{}）：{e}", path.display());
                return Self::corrupted_fallback(data_dir, &path);
            }
        };

        // 版本前置检查 + v1 结构嗅探：过高 → 只读降级；v1 单份（llm 含 provider
        // 键、无 providers——serde default 会静默吞掉旧键，须先于此迁移）
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(n) = v.get("schema_version").and_then(|x| x.as_u64()) {
                if n > SCHEMA_VERSION as u64 {
                    eprintln!("[settings] schemaVersion {n} 高于当前 {SCHEMA_VERSION}：只读降级");
                    return Self {
                        settings: Settings::default(),
                        source: Source::FutureVersion(n as u32),
                        data_dir: data_dir.to_path_buf(),
                        path,
                    };
                }
            }
            if let Some(llm_v) = v.get("llm") {
                if llm_v.is_object()
                    && llm_v.get("providers").is_none()
                    && llm_v.get("provider").is_some()
                {
                    // v1 单份结构 → 内存迁移（幂等；首次保存落盘 v2）
                    if let Some(migrated) = Self::migrate_v1_llm_section(&raw) {
                        eprintln!(
                            "[settings] 检测到 v1 单份 llm 结构，已内存迁移为供应商分区（下次保存落盘 v2）"
                        );
                        return Self {
                            settings: migrated,
                            source: Source::File,
                            data_dir: data_dir.to_path_buf(),
                            path,
                        };
                    }
                }
            }
        }

        match serde_json::from_str::<Settings>(&raw) {
            Ok(mut s) => {
                if s.schema_version != SCHEMA_VERSION {
                    // 低版本（无 schema_version 或缺字段）→ 读端 normalize 已兜底，现版本归一
                    s.schema_version = SCHEMA_VERSION;
                }
                Self {
                    settings: s,
                    source: Source::File,
                    data_dir: data_dir.to_path_buf(),
                    path,
                }
            }
            Err(e) => {
                // 解析失败：先尝试 v1 单份 llm 结构嗅探迁移（幂等、内存级，
                // 首次 set 保存时自然升 v2——load 不产生写盘副作用）
                if let Some(migrated) = Self::migrate_v1_llm_section(&raw) {
                    eprintln!(
                        "[settings] 检测到 v1 单份 llm 结构，已内存迁移为供应商分区（下次保存落盘 v2）"
                    );
                    Self {
                        settings: migrated,
                        source: Source::File,
                        data_dir: data_dir.to_path_buf(),
                        path,
                    }
                } else {
                    eprintln!("[settings] 解析失败（备份后回默认）：{e}");
                    Self::corrupted_fallback(data_dir, &path)
                }
            }
        }
    }

    /// v1 单份 llm 结构（`llm.provider/…/thinking_level`，无 `providers`）嗅探迁移。
    /// 仅在 llm 段含 `provider` 键且无 `providers` 键时执行；其余段原样保留。
    fn migrate_v1_llm_section(raw: &str) -> Option<Settings> {
        let v: serde_json::Value = serde_json::from_str(raw).ok()?;
        let l = v.get("llm")?;
        if !l.is_object() || l.get("providers").is_some() || l.get("provider").is_none() {
            return None; // 非 v1 形态
        }
        #[derive(serde::Deserialize)]
        struct LegacyLlm {
            #[serde(default)]
            provider: String,
            #[serde(default)]
            api_type: ApiType,
            #[serde(default)]
            model: String,
            #[serde(default)]
            api_key: String,
            #[serde(default)]
            base_url: Option<String>,
            #[serde(default)]
            headers: Option<String>,
            #[serde(default)]
            max_tokens: u32,
            #[serde(default)]
            thinking_level: Option<String>,
        }
        let old: LegacyLlm = serde_json::from_value(l.clone()).ok()?;

        let mut providers = HashMap::new();
        let active = old.provider.trim().to_string();
        if !active.is_empty() {
            providers.insert(
                active.clone(),
                ProviderConfig {
                    api_type: old.api_type,
                    model: old.model,
                    api_key: old.api_key,
                    base_url: old.base_url,
                    headers: old.headers,
                    max_tokens: if old.max_tokens == 0 {
                        4096
                    } else {
                        old.max_tokens
                    },
                },
            );
        }
        // 移除 llm 段后按现 Settings 解析（exec/workspace/window 原样保留）
        let mut obj = v.as_object().cloned()?;
        obj.remove("llm");
        let mut s: Settings = serde_json::from_value(serde_json::Value::Object(obj)).ok()?;
        s.llm = LlmSection {
            active_provider: active,
            providers,
            thinking_level: old.thinking_level,
        };
        s.schema_version = SCHEMA_VERSION;
        Some(s)
    }

    fn corrupted_fallback(data_dir: &Path, path: &Path) -> Self {
        let bak = path.with_file_name(SETTINGS_BAK);
        if let Err(e) = std::fs::copy(path, &bak) {
            eprintln!("[settings] 损坏文件备份失败（{}）：{e}", bak.display());
        }
        Self {
            settings: Settings::default(),
            source: Source::Corrupted,
            data_dir: data_dir.to_path_buf(),
            path: path.to_path_buf(),
        }
    }

    /// 只读降级（未来 schema 版本）——set 路径调用方应拒绝。
    pub fn is_readonly(&self) -> bool {
        matches!(self.source, Source::FutureVersion(_))
    }

    /// 当前加载来源（诊断/测试用）。
    #[allow(dead_code)]
    pub fn source(&self) -> Source {
        self.source
    }

    /// 生效 workspace 根：settings.workspace.workspace_root 非空 → 它；
    /// 空 = 未设置 → 默认工作区（`<主目录>/PRTS-WorkSpace`，见
    /// [`crate::config::default_workspace_path`]）。
    ///
    /// 空值不是「无工作目录」：默认值即回退目标，因此不存在空态。
    /// 「清除」设置 = 写空串 = 回到默认。
    pub fn workspace_root(&self) -> PathBuf {
        let v = self.settings.workspace.workspace_root.trim();
        if v.is_empty() {
            crate::config::default_workspace_path()
        } else {
            PathBuf::from(v)
        }
    }

    /// data_dir（settings.json 所在目录）。
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// 引导「已看过」键集（快照拷贝）。
    pub fn guide_seen(&self) -> Vec<String> {
        self.settings.ui.guide_seen.clone()
    }

    /// 记一个引导项为「已看过」并落盘。幂等：已存在则直接返回（不重复写盘）。
    ///
    /// **不走 set_settings 的 busy 闸**：界面态不是配置，且它必须在对话处理中
    /// 也能落盘（设置页通知随时可能触发）。只读降级由 `save` 自己拒绝。
    pub fn mark_guide_seen(&mut self, key: &str) -> Result<(), String> {
        if self.settings.ui.guide_seen.iter().any(|k| k == key) {
            return Ok(());
        }
        self.settings.ui.guide_seen.push(key.to_string());
        self.save()
    }

    /// 把 patch 合并进内存 settings（mask 解析在此）。返回变化摘要。
    /// 不落盘、不触发 readonly 判定——调用方（命令层）先查 is_readonly。
    pub fn apply_patch(&mut self, patch: &SettingsPatch) -> PatchChanges {
        let mut changes = PatchChanges::default();
        let before_active = self.settings.llm.active_provider.clone();
        let before_active_model = self
            .settings
            .llm
            .providers
            .get(&self.settings.llm.active_provider)
            .map(|pc| pc.model.clone());

        if let Some(p) = &patch.llm {
            if let Some(v) = &p.active_provider {
                self.settings.llm.active_provider = v.trim().to_string();
            }
            if let Some(providers) = &p.providers {
                for (name, pp) in providers {
                    // 条目懒建：首次配某家 → 插入默认条目
                    let entry = self.settings.llm.providers.entry(name.clone()).or_default();
                    if let Some(v) = &pp.api_type {
                        entry.api_type = *v;
                    }
                    if let Some(v) = &pp.model {
                        entry.model = v.clone();
                    }
                    match &pp.api_key {
                        None => {}
                        // 掩码占位 → 保留该家现值（不上送明文）
                        Some(v) if v == MASKED_SECRET => {}
                        // 空串 = 清除；其他 = 明文。**明文一律不进 settings**：
                        // 记一条密钥操作交命令层落凭据层（见 crate::credentials）。
                        Some(v) => {
                            entry.api_key = String::new();
                            changes.key_ops.push(KeyOp {
                                provider: name.clone(),
                                plaintext: v.clone(),
                            });
                        }
                    }
                    if let Some(v) = &pp.base_url {
                        entry.base_url = if v.is_empty() { None } else { Some(v.clone()) };
                    }
                    if let Some(v) = &pp.headers {
                        entry.headers = if v.is_empty() { None } else { Some(v.clone()) };
                    }
                    if let Some(v) = &pp.max_tokens {
                        entry.max_tokens = *v;
                    }
                }
            }
            // 全局思考偏好：空串 = 清除回"跟随模型默认"
            if let Some(v) = &p.thinking_level {
                self.settings.llm.thinking_level =
                    if v.is_empty() { None } else { Some(v.clone()) };
            }
            if !p.is_empty() {
                changes.llm_changed = true;
            }
            // active 变化或 active 家 model 变化 → notices Model 维度
            let after_model = self
                .settings
                .llm
                .providers
                .get(&self.settings.llm.active_provider)
                .map(|pc| pc.model.clone());
            if self.settings.llm.active_provider != before_active
                || after_model != before_active_model
            {
                changes.model_changed = true;
            }
        }

        if let Some(p) = &patch.exec {
            if let Some(mode) = p.mode {
                self.settings.exec.mode = mode;
                changes.exec_changed = true;
            }
        }

        if let Some(p) = &patch.maa {
            let before = self.settings.maa.clone();
            if let Some(v) = &p.server {
                self.settings.maa.server = *v;
            }
            if let Some(v) = p.auto_detect {
                self.settings.maa.auto_detect = v;
            }
            if let Some(v) = p.screenshot_extras {
                self.settings.maa.screenshot_extras = v;
            }
            if let Some(v) = p.touch_extras {
                self.settings.maa.touch_extras = v;
            }
            if let Some(v) = &p.emulator {
                self.settings.maa.emulator = v.trim().to_string();
            }
            if let Some(v) = &p.emulator_path {
                self.settings.maa.emulator_path = v.trim().to_string();
            }
            if let Some(v) = &p.adb_path {
                self.settings.maa.adb_path = v.trim().to_string();
            }
            if let Some(v) = &p.address {
                self.settings.maa.address = v.trim().to_string();
            }
            if before != self.settings.maa {
                changes.maa_changed = true;
            }
        }

        if let Some(p) = &patch.workspace {
            let before = self.settings.workspace.workspace_root.clone();
            if let Some(v) = &p.workspace_root {
                self.settings.workspace.workspace_root = v.trim().to_string();
            }
            if before != self.settings.workspace.workspace_root {
                changes.workspace_changed = true;
            }
        }

        if let Some(p) = &patch.window {
            if let Some(v) = p.always_on_top {
                self.settings.window.always_on_top = v;
            }
        }

        changes
    }

    /// 整写 + tmp+rename 原子替换 + **写后重读校验**。失败 Err 上抛
    /// （内存已同步；盘保持旧值，下次变更兜底重写）。只读降级拒绝写。
    pub fn save(&self) -> Result<(), String> {
        if self.is_readonly() {
            return Err(format!(
                "设置文件 schemaVersion 高于当前（{SCHEMA_VERSION}），拒绝写入（需升级应用）"
            ));
        }
        // 写前确保版本归一（load 时 normalize 过的低版本文件首次保存即升级）
        let mut for_write = self.settings.clone();
        for_write.schema_version = SCHEMA_VERSION;
        // 密钥永不随设置落盘（结构性保证，不依赖调用方自觉）：
        // 明文归 crate::credentials 管，这里一律写空。
        for pc in for_write.llm.providers.values_mut() {
            pc.api_key.clear();
        }
        let json = serde_json::to_string_pretty(&for_write)
            .map_err(|e| format!("settings 序列化失败: {e}"))?;

        std::fs::create_dir_all(&self.data_dir)
            .map_err(|e| format!("无法创建数据目录（{}）：{e}", self.data_dir.display()))?;
        let tmp = self.path.with_file_name(SETTINGS_TMP);
        if let Err(e) = std::fs::write(&tmp, json) {
            return Err(format!("settings 临时文件写入失败: {e}"));
        }
        if let Err(e) = std::fs::rename(&tmp, &self.path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("settings 原子替换失败: {e}"));
        }

        // 写后重读校验：盘上读回必须可 parse
        match std::fs::read_to_string(&self.path)
            .map_err(|e| format!("settings 写后重读失败: {e}"))
            .and_then(|raw| {
                serde_json::from_str::<Settings>(&raw)
                    .map(|_| ())
                    .map_err(|e| format!("settings 写后校验 parse 失败: {e}"))
            }) {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-settings-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_missing_file_returns_default_source() {
        let dir = unique_temp_dir("missing");
        let s = SettingsStore::load(&dir);
        assert_eq!(s.source(), Source::Default);
        assert_eq!(s.settings.llm, LlmSection::default());
        assert_eq!(s.settings.exec, ExecutionSettings::default());
        assert!(s.settings.window.always_on_top);
        assert!(!dir.join(SETTINGS_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_roundtrip_partitioned_llm() {
        let dir = unique_temp_dir("roundtrip");
        let mut s = SettingsStore::load(&dir);
        let patch: SettingsPatch = serde_json::from_str(
            r#"{
                "llm": {
                    "active_provider": "deepseek",
                    "providers": {
                        "deepseek": { "model": "deepseek-chat", "api_key": "sk-test", "base_url": "" }
                    },
                    "thinking_level": "high"
                },
                "exec": { "mode": "auto" },
                "window": { "always_on_top": false }
            }"#,
        )
        .unwrap();
        let changes = s.apply_patch(&patch);
        assert!(changes.llm_changed);
        assert!(changes.model_changed);
        assert!(changes.exec_changed);
        // 密钥不进 settings：apply_patch 只记密钥操作，钥值由凭据层承接。
        assert_eq!(
            changes.key_ops,
            vec![KeyOp {
                provider: "deepseek".to_string(),
                plaintext: "sk-test".to_string(),
            }]
        );
        s.save().unwrap();
        assert!(dir.join(SETTINGS_FILE).exists());

        // 结构性保证：明文绝不落到 settings.json 上
        let raw = std::fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap();
        assert!(!raw.contains("sk-test"), "settings.json 不应出现明文密钥");

        let loaded = SettingsStore::load(&dir);
        assert_eq!(loaded.source(), Source::File);
        assert_eq!(loaded.settings.llm.active_provider, "deepseek");
        let pc = loaded.settings.llm.providers.get("deepseek").unwrap();
        assert_eq!(pc.model, "deepseek-chat");
        assert_eq!(pc.api_key, "", "settings 侧只留空值");
        assert_eq!(pc.base_url, None);
        // 运行时值经凭据层取（此处以桩 lookup 模拟凭据层）
        let resolved = loaded
            .settings
            .llm
            .resolve_active_with(|p| (p == "deepseek").then(|| "sk-test".to_string()));
        assert_eq!(resolved.api_key, "sk-test");
        assert_eq!(loaded.settings.llm.thinking_level.as_deref(), Some("high"));
        assert_eq!(
            loaded.settings.exec.mode,
            crate::exec_settings::ExecutionMode::Auto
        );
        assert!(!loaded.settings.window.always_on_top);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn guide_seen_roundtrips_and_is_idempotent() {
        let dir = unique_temp_dir("guide");
        let mut s = SettingsStore::load(&dir);
        assert!(s.guide_seen().is_empty());

        s.mark_guide_seen("welcome.new-chat").unwrap();
        s.mark_guide_seen("welcome.new-chat").unwrap(); // 幂等：不重复登记
        s.mark_guide_seen("settings.space").unwrap();
        assert_eq!(
            s.guide_seen(),
            vec!["welcome.new-chat".to_string(), "settings.space".to_string()]
        );

        // 落回盘上：界面态随 settings.json 存活（重装不清数据时引导不重放）
        let reloaded = SettingsStore::load(&dir);
        assert_eq!(reloaded.source(), Source::File);
        assert_eq!(reloaded.guide_seen(), s.guide_seen());
        // 与配置互不干扰：ui 写入不动 llm
        assert_eq!(reloaded.settings.llm, LlmSection::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ui_section_defaults_when_absent() {
        // 旧文件无 ui 段 → serde default 补空（不升 schema 版本，不炸）
        let dir = unique_temp_dir("ui-absent");
        std::fs::write(
            dir.join(SETTINGS_FILE),
            r#"{ "schema_version": 2, "exec": { "mode": "auto" } }"#,
        )
        .unwrap();
        let s = SettingsStore::load(&dir);
        assert_eq!(s.source(), Source::File);
        assert!(s.guide_seen().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrate_v1_single_llm_to_partition() {
        let dir = unique_temp_dir("migrate");
        std::fs::write(
            dir.join(SETTINGS_FILE),
            r#"{
                "schema_version": 1,
                "llm": {
                    "provider": "deepseek", "api_type": "openai-completions",
                    "model": "deepseek-chat", "api_key": "sk-v1",
                    "base_url": null, "headers": null, "max_tokens": 4096,
                    "thinking_level": "low"
                },
                "exec": { "mode": "ask" }
            }"#,
        )
        .unwrap();
        let s = SettingsStore::load(&dir);
        assert_eq!(s.source(), Source::File);
        assert_eq!(s.settings.llm.active_provider, "deepseek");
        let pc = s
            .settings
            .llm
            .providers
            .get("deepseek")
            .expect("迁移应建条目");
        assert_eq!(pc.model, "deepseek-chat");
        assert_eq!(pc.api_key, "sk-v1");
        assert_eq!(pc.base_url, None);
        assert_eq!(s.settings.llm.thinking_level.as_deref(), Some("low"));
        assert_eq!(
            s.settings.exec.mode,
            crate::exec_settings::ExecutionMode::Ask,
            "exec 保留"
        );
        let c = s.settings.llm.resolve_active();
        assert_eq!(c.provider, "deepseek");
        assert_eq!(c.model, "deepseek-chat");
        assert_eq!(c.api_key, "sk-v1");
        assert_eq!(c.thinking_level.as_deref(), Some("low"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrate_v1_unconfigured_provider_keeps_empty() {
        let dir = unique_temp_dir("migrate-empty");
        std::fs::write(
            dir.join(SETTINGS_FILE),
            r#"{ "schema_version": 1, "llm": { "provider": "", "model": "", "api_key": "" } }"#,
        )
        .unwrap();
        let s = SettingsStore::load(&dir);
        assert_eq!(s.source(), Source::File);
        assert!(s.settings.llm.active_provider.is_empty());
        assert!(s.settings.llm.providers.is_empty());
        assert_eq!(s.settings.llm.resolve_active(), LlmConfig::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn v2_partition_patch_deep_merge_and_lazy_create() {
        let dir = unique_temp_dir("lazy");
        let mut s = SettingsStore::load(&dir);
        let patch: SettingsPatch = serde_json::from_str(
            r#"{ "llm": { "active_provider": "kimi", "providers": { "kimi": { "api_key": "sk-kimi", "model": "kimi-k2.7-code" } } } }"#,
        )
        .unwrap();
        let changes = s.apply_patch(&patch);
        let kimi = s.settings.llm.providers.get("kimi").expect("懒建条目");
        assert_eq!(kimi.model, "kimi-k2.7-code");
        assert_eq!(kimi.api_key, "", "密钥不进 settings 持久层");
        assert_eq!(changes.key_ops.len(), 1);
        assert_eq!(changes.key_ops[0].provider, "kimi");
        assert_eq!(changes.key_ops[0].plaintext, "sk-kimi");
        assert_eq!(
            kimi.api_type,
            ApiType::OpenAiCompletions,
            "缺省字段走 default"
        );
        assert_eq!(s.settings.llm.active_provider, "kimi");
        let patch2: SettingsPatch =
            serde_json::from_str(r#"{ "llm": { "providers": { "kimi": { "model": "" } } } }"#)
                .unwrap();
        s.apply_patch(&patch2);
        assert_eq!(s.settings.llm.providers.get("kimi").unwrap().model, "");
        assert_eq!(s.settings.llm.providers.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn patch_api_key_detection_ignores_mask_only() {
        let plain: SettingsPatch = serde_json::from_str(
            r#"{ "llm": { "providers": { "deepseek": { "api_key": "sk-plain" } } } }"#,
        )
        .unwrap();
        let clear: SettingsPatch = serde_json::from_str(
            r#"{ "llm": { "providers": { "deepseek": { "api_key": "" } } } }"#,
        )
        .unwrap();
        let masked: SettingsPatch = serde_json::from_str(
            r#"{ "llm": { "providers": { "deepseek": { "api_key": "********" } } } }"#,
        )
        .unwrap();
        let no_key: SettingsPatch =
            serde_json::from_str(r#"{ "llm": { "providers": { "deepseek": { "model": "m" } } } }"#)
                .unwrap();

        assert!(plain.touches_api_key());
        assert!(clear.touches_api_key());
        assert!(!masked.touches_api_key(), "掩码占位不产生密钥操作");
        assert!(!no_key.touches_api_key());
    }

    #[test]
    fn api_key_three_state_per_provider() {
        // 三态语义现落在"密钥操作"层：缺席/掩码不产生操作、空串 = 清除、
        // 其他 = 明文。settings 字段本身恒空——钥值归凭据层（crate::credentials）。
        let dir = unique_temp_dir("key3");
        let mut s = SettingsStore::load(&dir);
        let seed: SettingsPatch = serde_json::from_str(
            r#"{ "llm": { "providers": { "deepseek": { "api_key": "sk-original" } } } }"#,
        )
        .unwrap();
        let c = s.apply_patch(&seed);
        assert_eq!(c.key_ops.len(), 1);
        assert_eq!(c.key_ops[0].plaintext, "sk-original");
        assert_eq!(
            s.settings.llm.providers.get("deepseek").unwrap().api_key,
            ""
        );

        let masked: SettingsPatch = serde_json::from_str(
            r#"{ "llm": { "providers": { "deepseek": { "api_key": "********" } } } }"#,
        )
        .unwrap();
        let c = s.apply_patch(&masked);
        assert!(
            c.key_ops.is_empty(),
            "掩码占位不产生密钥操作（保留该家现值）"
        );
        let absent: SettingsPatch = serde_json::from_str(
            r#"{ "llm": { "providers": { "deepseek": { "model": "m2" } } } }"#,
        )
        .unwrap();
        let c = s.apply_patch(&absent);
        assert!(c.key_ops.is_empty(), "字段缺席不产生密钥操作");
        let clear: SettingsPatch = serde_json::from_str(
            r#"{ "llm": { "providers": { "deepseek": { "api_key": "" } } } }"#,
        )
        .unwrap();
        let c = s.apply_patch(&clear);
        assert_eq!(c.key_ops.len(), 1);
        assert_eq!(c.key_ops[0].plaintext, "", "空串 = 清除");
        let fresh: SettingsPatch = serde_json::from_str(
            r#"{ "llm": { "providers": { "deepseek": { "api_key": "sk-new" } } } }"#,
        )
        .unwrap();
        let c = s.apply_patch(&fresh);
        assert_eq!(c.key_ops[0].plaintext, "sk-new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn switch_active_keeps_other_provider_config() {
        let dir = unique_temp_dir("switch");
        let mut s = SettingsStore::load(&dir);
        let seed: SettingsPatch = serde_json::from_str(
            r#"{
                "llm": {
                    "active_provider": "deepseek",
                    "providers": {
                        "deepseek": { "model": "deepseek-v4-flash", "api_key": "sk-ds" },
                        "openai": { "model": "gpt-5", "api_key": "sk-oa" }
                    }
                }
            }"#,
        )
        .unwrap();
        let changes = s.apply_patch(&seed);
        assert_eq!(changes.key_ops.len(), 2, "两家密钥各一条操作");

        let switch: SettingsPatch =
            serde_json::from_str(r#"{ "llm": { "active_provider": "openai" } }"#).unwrap();
        s.apply_patch(&switch);
        // 切家后：运行态取该家的钥值（以柱 lookup 模拟凭据层）
        let lookup = |p: &str| match p {
            "deepseek" => Some("sk-ds".to_string()),
            "openai" => Some("sk-oa".to_string()),
            _ => None,
        };
        let c = s.settings.llm.resolve_active_with(lookup);
        assert_eq!(c.provider, "openai");
        assert_eq!(c.model, "gpt-5");
        assert_eq!(c.api_key, "sk-oa");
        // 另一家的配置（model）原样保留；密钥本就不在 settings 里
        let ds = s.settings.llm.providers.get("deepseek").unwrap();
        assert_eq!(ds.model, "deepseek-v4-flash");
        assert_eq!(ds.api_key, "");

        let back: SettingsPatch =
            serde_json::from_str(r#"{ "llm": { "active_provider": "deepseek" } }"#).unwrap();
        s.apply_patch(&back);
        let c2 = s.settings.llm.resolve_active_with(lookup);
        assert_eq!(c2.model, "deepseek-v4-flash");
        assert_eq!(c2.api_key, "sk-ds");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn global_thinking_clear_and_resolve() {
        let dir = unique_temp_dir("think");
        let mut s = SettingsStore::load(&dir);
        let set: SettingsPatch = serde_json::from_str(
            r#"{ "llm": { "thinking_level": "low", "providers": { "deepseek": { "model": "m" } } } }"#,
        )
        .unwrap();
        s.apply_patch(&set);
        s.settings.llm.active_provider = "deepseek".into();
        let c = s.settings.llm.resolve_active();
        assert_eq!(c.thinking_level.as_deref(), Some("low"));
        let clear: SettingsPatch =
            serde_json::from_str(r#"{ "llm": { "thinking_level": "" } }"#).unwrap();
        s.apply_patch(&clear);
        assert_eq!(s.settings.llm.resolve_active().thinking_level, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_active_missing_entry_falls_back_default() {
        let dir = unique_temp_dir("resolve-none");
        let s = SettingsStore::load(&dir);
        assert_eq!(s.settings.llm.resolve_active(), LlmConfig::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupted_file_backed_up_and_defaulted() {
        let dir = unique_temp_dir("corrupt");
        std::fs::write(dir.join(SETTINGS_FILE), "{ not json !").unwrap();
        let s = SettingsStore::load(&dir);
        assert_eq!(s.source(), Source::Corrupted);
        assert_eq!(s.settings, Settings::default());
        assert!(dir.join(SETTINGS_FILE).exists());
        assert!(dir.join(SETTINGS_BAK).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn future_schema_readonly_degrade() {
        let dir = unique_temp_dir("future");
        std::fs::write(
            dir.join(SETTINGS_FILE),
            r#"{ "schema_version": 99, "llm": { "active_provider": "x", "providers": {} } }"#,
        )
        .unwrap();
        let s = SettingsStore::load(&dir);
        assert!(s.is_readonly());
        assert_eq!(s.settings, Settings::default());
        assert!(s.save().is_err());
        let raw = std::fs::read_to_string(dir.join(SETTINGS_FILE)).unwrap();
        assert!(raw.contains("\"schema_version\": 99"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn v2_unknown_fields_ignored_and_missing_normalized() {
        let dir = unique_temp_dir("normalize");
        std::fs::write(
            dir.join(SETTINGS_FILE),
            r#"{ "schema_version": 2, "llm": { "active_provider": "deepseek", "providers": { "deepseek": { "model": "m", "unknownFutureField": 1 } } }, "someTopFutureField": "x" }"#,
        )
        .unwrap();
        let s = SettingsStore::load(&dir);
        assert_eq!(s.source(), Source::File);
        let pc = s.settings.llm.providers.get("deepseek").unwrap();
        assert_eq!(pc.model, "m");
        assert_eq!(s.settings.exec, ExecutionSettings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workspace_root_falls_back_to_default_workspace_when_empty() {
        let dir = unique_temp_dir("ws");
        let s = SettingsStore::load(&dir);
        assert_eq!(
            s.workspace_root(),
            crate::config::default_workspace_path(),
            "空 = 默认工作区回退"
        );
        assert_ne!(
            s.workspace_root(),
            dir.clone(),
            "空值不再回退数据根（旧语义已撤）"
        );

        // 显式值优先
        let mut s2 = SettingsStore::load(&dir);
        let patch: SettingsPatch =
            serde_json::from_str(r#"{ "workspace": { "workspace_root": "F:/custom" } }"#).unwrap();
        s2.apply_patch(&patch);
        assert_eq!(s2.workspace_root(), std::path::PathBuf::from("F:/custom"));

        // 清除（写空串）= 回到默认，而不是「无工作目录」
        let clear: SettingsPatch =
            serde_json::from_str(r#"{ "workspace": { "workspace_root": "" } }"#).unwrap();
        s2.apply_patch(&clear);
        assert_eq!(s2.workspace_root(), crate::config::default_workspace_path());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
