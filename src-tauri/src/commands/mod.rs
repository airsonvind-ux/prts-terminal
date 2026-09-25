//! Tauri IPC 薄层 —— 前端 ←→ 后端命令适配
//!
//! 职责：
//!   1. agent_process 命令：反序列化请求 → 包装 emit 回调 → 调用调度中枢
//!   2. get_config / set_config 命令：配置读写，为设置页提供 API key 填入通道
//!   3. 所有 Tauri State 通过 Arc 管理，异步命令通过 clone Arc 移入 async 上下文

use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

use crate::db::Database;
use crate::notices::Notices;
use crate::events::{AgentEvent, FrontendMessage, IpcEvent};
use crate::llm::{default_base_url, ApiType, ClientRegistry, LlmConfig, ModelInfo, ModelRegistry, ProviderInfo, TokenUsage};
use crate::memory::schema::{PROFILE_NAME, REVISION_SEALED, SOURCE_USER};
use crate::memory::{MemoryStore, SnapshotStore};
use crate::orchestrator;
use crate::settings::{SettingsPatch, SettingsStore};
use crate::session_store::{
    AppendTurnOutcome, AppendTurnReq, CompactionMeta, ConversationMeta, ConversationRecord, MetaPatch,
    SessionStore, TitleWriteOutcome,
};
use crate::steer_queue::SteerQueue;
use crate::tools::ToolRegistry;
use crate::turn_registry::TurnRegistry;

// ═══════════════════════════════════════════════════════
// 请求类型
// ═══════════════════════════════════════════════════════

/// 前端 agent_process invoke 的请求体。
///
/// 对应 `types.ts` 的 AgentRequest：
///   input            — 用户原始输入
///   history          — 完整对话历史（含本轮 user 消息）
///   sessionId        — 本次请求的唯一标识，随所有 AgentEvent 回传
///   conversationId   — 对话标识（模块 C：缓存区按此分区；可选字段，
///                      旧前端不传时后端用 sessionId 兜底）
///
/// rename_all = "camelCase" 将 Rust 的 snake_case 字段名
/// 自动映射为前端传来的 camelCase JSON key，无需手动 rename。
/// 2026-08：input / conversation_id 当前未被后端消费（input 为协议预留字段；
/// conversation_id 随缓存系统停用暂不消费），保留以兼容前端协议。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct AgentRequest {
    pub input: String,
    pub history: Vec<FrontendMessage>,
    pub session_id: String,
    /// 对话标识（可选：旧前端不传 → 反序列化为空串，命令层兜底为 session_id）
    #[serde(default)]
    pub conversation_id: String,
}

// ═══════════════════════════════════════════════════════
// 活动 turn RAII 收尾
// ═══════════════════════════════════════════════════════

/// 活动 turn 的 RAII 收尾：无论正常返回、错误还是 `?` 提前退出，
/// Drop 都会注销该 session 的 busy 位（防忙位泄漏，批 1 审查修复 🟡1）。
struct TurnGuard {
    registry: Arc<TurnRegistry>,
    session_id: String,
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.registry.unregister(&self.session_id);
    }
}

// ═══════════════════════════════════════════════════════
// agent_process —— 核心命令
// ═══════════════════════════════════════════════════════

/// 处理一次完整的 Agent 交互。
///
/// 前端调用 `invoke("agent_process", { request })`，
/// 本命令同步返回 Ok(()) 表示"接收成功"，实际处理结果通过
/// `app_handle.emit("agent-event", ...)` 异步推送。
///
/// 异步命令在 Tauri 的多线程运行时上执行，
/// orchestrator::process 的 await 不会阻塞主线程。
/// 前端事件通过 listen("agent-event") 独立通道到达，
/// 不依赖命令返回值。
///
/// 参数管理方式：
///   AppHandle    — 命令函数自动注入，用于 emit 事件
///   State<Arc<T>> — lib.rs 装配时通过 app.manage() 注入的共享状态
///                   Arc 保证异步上下文中数据存活，Mutex 保护可变访问
#[tauri::command]
pub async fn agent_process(
    app_handle: AppHandle,
    request: AgentRequest,
    tools: State<'_, Arc<ToolRegistry>>,
    llm: State<'_, Arc<ClientRegistry>>,
    config: State<'_, Arc<Mutex<LlmConfig>>>,
    db: State<'_, Arc<Database>>,
    model_registry: State<'_, Arc<ModelRegistry>>,
    exec_settings: State<'_, Arc<Mutex<crate::exec_settings::ExecutionSettings>>>,
    approval_registry: State<'_, Arc<crate::approval::ApprovalRegistry>>,
    turn_registry: State<'_, Arc<TurnRegistry>>,
    steer_queue: State<'_, Arc<SteerQueue>>,
    settings_store: State<'_, Arc<Mutex<SettingsStore>>>,
    memory_store: State<'_, Arc<Mutex<MemoryStore>>>,
    snapshot_store: State<'_, Arc<Mutex<SnapshotStore>>>,
) -> Result<(), String> {
    // ── Clone Arc：从 State 引用中获得可移入 async 上下文的独立所有权 ──
    let tools = Arc::clone(&tools);
    let llm = Arc::clone(&llm);
    let config = Arc::clone(&config);
    let db = Some(Arc::clone(&db));
    let model_registry = Arc::clone(&model_registry);
    let turn_registry = Arc::clone(&turn_registry);
    let steer_queue = Arc::clone(&steer_queue);
    let memory_store = Arc::clone(&memory_store);
    let snapshot_store = Arc::clone(&snapshot_store);
    // 缓存 State 已注释（2026-08：缓存系统停用，待升级为记忆系统）

    // ── 包装 emit 回调：AppHandle.emit → 闭包 ──
    let emit_app_handle = app_handle.clone();
    let session_id = request.session_id.clone();
    let emit = move |event: IpcEvent| {
        // emit 失败不中断处理（仅日志记录）
        if let Err(e) = emit_app_handle.emit("agent-event", event) {
            eprintln!("[commands] emit 失败: {e}");
        }
    };

    // ── 读取配置（短暂持锁） ──
    let llm_config = {
        let guard = config.lock().map_err(|e| format!("配置锁获取失败: {e}"))?;
        guard.clone()
    };

    // ── 命令执行模式快照（M3-A）：turn 起始读一次 → 本 turn 内模式不变
    // （processing/streaming 期间锁定的语义底座；M3-B 在 set_settings 的
    // exec patch 上叠加运行时闸）。
    let mode = {
        let s = exec_settings
            .lock()
            .map_err(|e| format!("执行模式配置锁获取失败: {e}"))?;
        s.mode
    };

    // ── 注册活动 turn（M3-B §2.2）：取得 abort 句柄；
    // process 返回（含中止/错误）后无条件注销——busy 口径释放点。
    // conversation_id 为 AgentRequest 已收字段（serde 到位，此前未消费）。
    let conversation_id = request.conversation_id.clone();
    let abort_signal = turn_registry.register(&session_id, &conversation_id);
    // RAII 收尾：注册后任何错误出口（含 `?`）都会注销 busy 位
    let _turn_guard = TurnGuard {
        registry: Arc::clone(&turn_registry),
        session_id: session_id.clone(),
    };

    // ── workspace_root（地域性信任门信任根）：settings.workspace 段取一次
    // （与 lib.rs 装配 Policy/RunCommandTool 同源），owned 注入 process。
    let workspace_root = {
        let st = settings_store
            .lock()
            .map_err(|e| format!("设置锁获取失败: {e}"))?;
        st.workspace_root()
    };

    // ── 记忆区冻结快照（Phase 4 任务二）：按 conversationId 取 ──
    // 有真实 conversationId → 组装 + 落盘 sidecar（对话内冻结，唯一刷新点＝新对话）；
    // **缺 conversationId（旧前端）→ 只组装、不落盘**（sessionId 每请求唯一，
    // 用它当冻结键会名存实亡并产生垃圾快照，批 1 审查修复 🟡3）。
    let memory_key = if conversation_id.is_empty() {
        None
    } else {
        Some(conversation_id.as_str())
    };
    let memory_block = {
        let mem = memory_store
            .lock()
            .map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
        let mut snaps = snapshot_store
            .lock()
            .map_err(|e| format!("快照存储锁获取失败: {e}"))?;
        snaps.get_or_assemble(memory_key, &mem)
    };

    // ── 调用调度中枢（abort 透传：取消点消费；memory_block 注入 system 尾部）──
    let result = orchestrator::process_with_memory(
        &session_id,
        request.history,
        &tools, // State<Arc<ToolRegistry>> → &Arc<ToolRegistry>（deref 强制）
        &*llm, // Arc<dyn LlmClient> → &dyn LlmClient
        &llm_config,
        &model_registry,
        db,
        mode,
        approval_registry.as_ref(),
        &abort_signal,
        steer_queue.as_ref(),
        workspace_root,
        emit,
        memory_block,
    )
    .await;

    // ── 兜底清理插话队列（设计定稿）：正常轮队列在 turn/end 前已消费空；
    // error 终止轮（dispatch Err → process 补 error 事件）队列可能残留
    // 待注入条目——session_id 每轮唯一不复用，残留永不被消费，此处幂等清空。
    steer_queue.clear(&session_id);
    // busy 位由 `_turn_guard` Drop 统一注销（含上方所有错误出口）

    // ── 错误处理：调度中枢报错 → 推送错误事件到前端（新词汇）──
    if let Err(e) = result {
        app_handle
            .emit(
                "agent-event",
                crate::events::IpcEvent::Session(crate::session::SessionEvent::error(
                    &session_id,
                    &e.to_string(),
                    false,
                )),
            )
            .map_err(|e| format!("错误事件推送失败: {e}"))?;
    }

    Ok(())
}

// ═══════════════════════════════════════════════════════
// clear_conversation_cache —— 已注释（2026-08：缓存系统停用，
// 待升级为记忆系统。此前为 AI 自行添加、前端无对应功能，一并停用）
// ═══════════════════════════════════════════════════════

// /// 清空指定对话的缓存分区（前端删除对话时调用）。
// ///
// /// 缓存区是单对话工作记忆，生命周期绑定对话（见 cache.rs 顶部的
// /// 「边界（缓存 ≠ 记忆）」声明）：对话删除即整体失效。此处移除对应
// /// 分区，释放进程内内存；分区不存在时幂等（remove_partition 返回 false），
// /// 命令仍返回 Ok——重复清理不视为错误。
// #[tauri::command]
// pub fn clear_conversation_cache(
//     conversation_id: String,
//     cache_stores: State<'_, Arc<Mutex<crate::cache::CacheStores>>>,
// ) -> Result<(), String> {
//     let mut stores = cache_stores
//         .lock()
//         .map_err(|e| format!("缓存区锁获取失败: {e}"))?;
//     stores.remove_partition(&conversation_id);
//     Ok(())
// }

// ═══════════════════════════════════════════════════════
// 配置读写
// ═══════════════════════════════════════════════════════

/// M3-B 设置写闸错误文案（后端闸 + 前端 toast 同文案；现有命令中文风格，
/// 不 code 化——前端 catch String 直接 toast）。
const CONFIG_WRITE_GUARD_MSG: &str = "对话处理中不能修改设置，请等待当前处理结束";

/// M3-B 设置写闸（双保险）：对话处理中（active_count() > 0）禁止修改设置。
///
/// 调用先于配置锁获取——registry 短暂持锁即释放，再取配置锁（无嵌套锁序，
/// 不引入死锁面）。供 set_settings 写命令消费；
/// get_settings / reveal_api_key / get_app_meta / verify_connection /
/// list_models 等只读命令不拦截。
///
/// 快照隔离事实：轮内 config 是 turn 起始快照（agent_process 已 clone），
/// 本闸并非数据一致性必需，而是「busy 期间设置锁定」的 UX 规则强制面——
/// 勿按数据竞态防护理解而误删（防后人重构时顺手拿掉）。
fn guard_config_write(turn_registry: &TurnRegistry) -> Result<(), String> {
    if turn_registry.active_count() > 0 {
        return Err(CONFIG_WRITE_GUARD_MSG.into());
    }
    Ok(())
}

/// 读取设置全量（get_settings）。返回 Settings 结构（snake_case），
/// **逐家 api_key 掩码下发**（明文经 reveal_api_key 瞬态获取）。只读不设闸。
#[tauri::command]
pub fn get_settings(
    store: State<'_, Arc<Mutex<SettingsStore>>>,
) -> Result<crate::settings::Settings, String> {
    let s = store.lock().map_err(|e| format!("设置锁获取失败: {e}"))?;
    let mut out = s.settings.clone();
    for pc in out.llm.providers.values_mut() {
        if !pc.api_key.is_empty() {
            pc.api_key = crate::settings::MASKED_SECRET.to_string();
        }
    }
    Ok(out)
}

/// 显式读取某供应商 api_key 明文（瞬态查看用）。provider 缺省 = 当前
/// active 供应商。前端展示后即弃，不落持久明文态。
#[tauri::command]
pub fn reveal_api_key(
    store: State<'_, Arc<Mutex<SettingsStore>>>,
    provider: Option<String>,
) -> Result<String, String> {
    let s = store.lock().map_err(|e| format!("设置锁获取失败: {e}"))?;
    let name = provider.unwrap_or_else(|| s.settings.llm.active_provider.clone());
    Ok(s.settings
        .llm
        .providers
        .get(&name)
        .map(|pc| pc.api_key.clone())
        .unwrap_or_default())
}

/// 局部 patch 写设置（per-field：字段缺席 = 不更新；显式 null = 清除回默认；
/// api_key 三态：缺席/掩码保留现值、空串清除、其他明文写入）。
///
/// 同步职责：patch 合并进 SettingsStore（持久层 + 读取源）后，按变化摘要
/// 同步运行 State（llm_config / exec_settings），notices 记 Model 变化，
/// 最后落盘（写后重读校验；失败 Err 上抛 → 前端可见——内存已同步，
/// 下次变更兜底重写）。
///
/// M3-B busy 闸前置（锁序固定：store → config → exec_settings → ledger，
/// 无任何路径反向嵌套）。只读降级（高版本文件）拒绝写。
#[tauri::command]
pub fn set_settings(
    turn_registry: State<'_, Arc<TurnRegistry>>,
    store: State<'_, Arc<Mutex<SettingsStore>>>,
    config: State<'_, Arc<Mutex<LlmConfig>>>,
    exec_settings: State<'_, Arc<Mutex<crate::exec_settings::ExecutionSettings>>>,
    ledger: State<'_, Arc<Mutex<Notices>>>,
    patch: SettingsPatch,
) -> Result<(), String> {
    guard_config_write(&turn_registry)?;
    let mut st = store.lock().map_err(|e| format!("设置锁获取失败: {e}"))?;
    if st.is_readonly() {
        return Err(format!(
            "设置文件 schemaVersion 高于当前，拒绝写入（需升级应用）"
        ));
    }
    let changes = st.apply_patch(&patch);
    if changes.llm_changed {
        let mut c = config.lock().map_err(|e| format!("配置锁获取失败: {e}"))?;
        // active 供应商 resolve 成请求用 LlmConfig（provider = active 名）
        *c = st.settings.llm.resolve_active();
    }
    if changes.exec_changed {
        let mut es = exec_settings
            .lock()
            .map_err(|e| format!("执行模式配置锁获取失败: {e}"))?;
        *es = st.settings.exec;
    }
    if changes.model_changed {
        // Model 条目：active 供应商 + 其 model（与装配/ledger 同源 resolve）
        let resolved = st.settings.llm.resolve_active();
        let mut l = ledger.lock().map_err(|e| format!("台账锁获取失败: {e}"))?;
        l.note_model_change(&crate::notices::model_label(
            &resolved.provider,
            &resolved.model,
        ));
    }
    st.save()
}

/// 动态切换窗口置顶 + 写回 settings.json（window 段）。
/// 先应用窗口（即时生效），成功后写盘；save 失败 → 回滚窗口与内存（
/// 下次启动仍按盘上值）。
#[tauri::command]
pub fn set_always_on_top(
    app: AppHandle,
    turn_registry: State<'_, Arc<TurnRegistry>>,
    store: State<'_, Arc<Mutex<SettingsStore>>>,
    enabled: bool,
) -> Result<(), String> {
    guard_config_write(&turn_registry)?;
    let win = app
        .get_webview_window("main")
        .ok_or_else(|| "找不到主窗口".to_string())?;
    win.set_always_on_top(enabled)
        .map_err(|e| format!("窗口置顶设置失败: {e}"))?;

    let mut st = store.lock().map_err(|e| format!("设置锁获取失败: {e}"))?;
    if st.is_readonly() {
        return Err(format!(
            "设置文件 schemaVersion 高于当前，拒绝写入（需升级应用）"
        ));
    }
    let previous = st.settings.window.always_on_top;
    st.settings.window.always_on_top = enabled;
    if let Err(e) = st.save() {
        // 落盘失败：回滚内存与窗口（保持盘上/运行一致的可预期状态）
        st.settings.window.always_on_top = previous;
        let _ = win.set_always_on_top(previous);
        return Err(e);
    }
    Ok(())
}

/// 应用只读元信息（关于页 + 空间页共用）：版本 + 当前生效路径。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppMeta {
    pub version: &'static str,
    pub data_dir: String,
    pub workspace_root: String,
    pub db_path: String,
    pub knowledge_dir: String,
    pub skills_dir: String,
}

#[tauri::command]
pub fn get_app_meta(
    store: State<'_, Arc<Mutex<SettingsStore>>>,
) -> Result<AppMeta, String> {
    let st = store.lock().map_err(|e| format!("设置锁获取失败: {e}"))?;
    Ok(AppMeta {
        version: env!("CARGO_PKG_VERSION"),
        data_dir: st.data_dir().to_string_lossy().into_owned(),
        workspace_root: st.workspace_root().to_string_lossy().into_owned(),
        db_path: crate::config::resolve_db_path().to_string_lossy().into_owned(),
        knowledge_dir: crate::knowledge::index::knowledge_dir()
            .to_string_lossy()
            .into_owned(),
        skills_dir: crate::skills_index::skills_dir()
            .to_string_lossy()
            .into_owned(),
    })
}

/// 技能清单条目（空间页只读展示）：来源 skills 目录 md 的 frontmatter。
/// file_path = 源 md 绝对路径（None = 索引未记录，异常态）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    pub file_path: Option<String>,
}

/// 技能清单（空间页技能节只读展示）：name + description，按 name 排序。
/// 加载失败（目录不可读）→ 描述性 Err。只读命令不设 busy 闸。
#[tauri::command]
pub fn list_skills() -> Result<Vec<SkillEntry>, String> {
    let idx = crate::skills_index::SkillIndex::load(&crate::skills_index::skills_dir())
        .map_err(|e| format!("技能目录读取失败: {e}"))?;
    Ok(idx
        .names()
        .into_iter()
        .map(|name| SkillEntry {
            name: name.to_string(),
            description: idx
                .resolve(name)
                .map(|m| m.description.clone())
                .unwrap_or_default(),
            file_path: idx
                .path(name)
                .map(|p| p.to_string_lossy().into_owned()),
        })
        .collect())
}

/// open_path 目标校验（放宽口径，2026-09）：不设白名单，仅要求**绝对路径且
/// 目标存在（为文件）**。工具卡片文件链接需要打开任意位置文件（读/写/编辑的
/// 工作区文件、知识库 md、Skill md）；防误点仍拒绝相对路径与非文件（目录）。
pub fn is_openable_target(p: &std::path::Path) -> bool {
    p.is_absolute() && p.is_file()
}

/// 用系统默认应用打开一个文件（tauri-plugin-opener）。
/// 入参 = 工具卡片/设置页给出的文件绝对路径；校验存在后放行（无白名单）。
/// 失败 Err 上抛（前端 FileLink 弹可见 toast——点击必有可见结果契约）。
#[tauri::command]
pub fn open_path(app: AppHandle, path: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(&path);
    if !is_openable_target(&p) {
        return Err(format!("文件不存在或不是可打开的绝对路径：{path}"));
    }
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| format!("打开失败（{}）：{e}", p.display()))
}

/// 用系统默认浏览器打开一个 http/https 链接（web_fetch 的 URL 链接）。
/// 失败 Err 上抛（前端弹可见 toast——点击必有可见结果契约）。
#[tauri::command]
pub fn open_url(app: AppHandle, url: String) -> Result<(), String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("仅支持 http/https 链接".into());
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| format!("打开链接失败：{e}"))
}

/// 拉取提醒数据（快照＝值 + 有界记忆事件）。
///
/// 前端 sendMessage 时调用：新对话（游标为空）取环境值快照（仅
/// workspace/knowledge/model 的 value）；进行中对话取增量（stamp/seq > 游标的
/// 维度与事件）。后端**无消费状态**（纯只读）——游标在前端按对话维护。
#[tauri::command]
pub fn get_reminder_data(
    ledger: State<'_, Arc<Mutex<Notices>>>,
) -> Result<ReminderData, String> {
    let n = ledger
        .lock()
        .map_err(|e| format!("提醒数据锁获取失败: {e}"))?;
    Ok(ReminderData::from_notices(&n))
}

// ═══════════════════════════════════════════════════════
// 目录选择（设置页扩展 · 任务 D：工作空间根选择）
// ═══════════════════════════════════════════════════════

/// 打开系统目录选择器选一个目录（工作空间根）。
///
/// 实现形态选择：**后端命令 + dialog Rust API**（tauri_plugin_dialog），而非
/// 前端 JS API——弹窗与路径解析在后端、白名单无关（选择器返回任意用户目录
/// 均可作 workspace，属用户自有内容区），前端只收返回值；改动面 = 一个命令
/// + 一个按钮，符合 PRTS invoke 模式（其余设置命令同构）。
///
/// `current`：当前 workspace（选择器初始目录，可选）；用户取消 → `Ok(None)`。
/// 非阻塞 pick_folder 内部经 run_on_main_thread 弹窗，oneshot 桥回 async 命令。
#[tauri::command]
pub async fn choose_workspace_directory(
    app: AppHandle,
    current: Option<String>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    let mut builder = app.dialog().file().set_title("选择工作空间目录");
    if let Some(cur) = &current {
        if !cur.trim().is_empty() {
            builder = builder.set_directory(cur);
        }
    }
    builder.pick_folder(move |picked| {
        let value = picked.and_then(|fp| {
            fp.as_path()
                .map(|p| p.to_string_lossy().into_owned())
        });
        let _ = tx.send(value);
    });
    rx.await.map_err(|e| format!("目录选择失败: {e}"))
}

/// 环境维度 DTO（`value` 为空串的维度不下发）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderDim {
    pub key: String,
    pub title: String,
    pub value: String,
    pub note: String,
    pub stamp: u64,
}

/// 记忆事件 DTO。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderEvent {
    pub seq: u64,
    pub title: String,
    pub detail: String,
    pub ts: String,
}

/// get_reminder_data 载荷。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderData {
    pub seq: u64,
    pub dims: Vec<ReminderDim>,
    pub events: Vec<ReminderEvent>,
}

impl ReminderData {
    fn from_notices(n: &crate::notices::Notices) -> Self {
        let dims = n
            .dim_views()
            .into_iter()
            .filter(|(_, _, d)| !d.value.is_empty())
            .map(|(key, title, d)| ReminderDim {
                key: key.to_string(),
                title: title.to_string(),
                value: d.value.clone(),
                note: d.note.clone(),
                stamp: d.stamp,
            })
            .collect();
        let events = n
            .events()
            .iter()
            .map(|e| ReminderEvent {
                seq: e.seq,
                title: e.title.clone(),
                detail: e.detail.clone(),
                ts: e.ts.clone(),
            })
            .collect();
        Self {
            seq: n.seq(),
            dims,
            events,
        }
    }
}

// ═══════════════════════════════════════════════════════
// 会话持久化（架构升级 · 模块 1 · Phase 4：对话跨重启恢复）
// ═══════════════════════════════════════════════════════
//
// 命令层只做参数透传与锁获取，语义全部收敛在 session_store（纯存储层，
// 不解析业务）。写盘触发 = 前端轮末归档（turn/end 与错误轮都提交）；
// 空对话不落盘（createConversation 不触发 IPC）；删除 = 删持久文件。

/// 列出全部会话元数据（来源 index.json——列表不扫内容文件；附带孤儿清理）。
#[tauri::command]
pub fn list_conversations(
    store: State<'_, Arc<Mutex<SessionStore>>>,
) -> Result<Vec<ConversationMeta>, String> {
    let mut s = store.lock().map_err(|e| format!("会话存储锁获取失败: {e}"))?;
    s.list()
}

/// 读取单个会话：meta + 轮行（坏行已跳过；恢复时前端 JSON.parse 逐行重建）。
///
/// **读出侧合并流式 chunk**（2026-09）：归档按事件落盘，每个流式 chunk 一条，
/// 长对话单行可达数 MB；返回前把「连续且同 kind 的 assistant/chunk」合并为一条
/// （[`crate::session_store::coalesce_chunks_line`]），payload 与前端解析量下降
/// 数量级。**落盘原文不动**——历史冻结语义不变。
#[tauri::command]
pub fn get_conversation(
    store: State<'_, Arc<Mutex<SessionStore>>>,
    conversation_id: String,
) -> Result<ConversationRecord, String> {
    let mut s = store.lock().map_err(|e| format!("会话存储锁获取失败: {e}"))?;
    let mut rec = s.get(&conversation_id)?;
    for line in rec.turns.iter_mut() {
        if let Some(merged) = crate::session_store::coalesce_chunks_line(line) {
            *line = merged;
        }
    }
    Ok(rec)
}

/// 追加一轮（前端轮末归档触发）。首轮建会话文件（header + 轮行 + index 插入），
/// 后续轮追加轮行 + index 更新 updatedAt 与随轮提交的 contextTokens/lastNoticeSeq。
/// 可缺省字段：JS 侧 null/不传 = 缺省（Option）。
#[tauri::command]
pub fn append_turn(
    store: State<'_, Arc<Mutex<SessionStore>>>,
    conversation_id: String,
    events_json: String,
    title: Option<String>,
    created_at: Option<u64>,
    model_name: Option<String>,
    context_length: Option<u64>,
    context_tokens: Option<TokenUsage>,
    last_notice_seq: Option<u64>,
) -> Result<AppendTurnOutcome, String> {
    let req = AppendTurnReq {
        conversation_id,
        events_json,
        title,
        created_at,
        model_name,
        context_length,
        context_tokens,
        last_notice_seq,
    };
    let mut s = store.lock().map_err(|e| format!("会话存储锁获取失败: {e}"))?;
    s.append_turn(&req)
}

/// 更新会话元数据（rename / contextTokens / lastNoticeSeq / compaction；
/// 只更新非 None 字段；clearContextTokens 为显式清空开关）。游标正常随轮
/// 提交，本命令是独立更新路径（低频；模块 D 压缩记录的落盘通道）。
///
/// `expectedTitle` 为生成标题的 CAS 守卫：Some 时仅当 index 当前 title 等于
/// 该值才应用 `title`（用户已改名 → 放弃写入，不报错）。
#[tauri::command]
pub fn update_conversation_meta(
    store: State<'_, Arc<Mutex<SessionStore>>>,
    conversation_id: String,
    title: Option<String>,
    expected_title: Option<String>,
    context_tokens: Option<TokenUsage>,
    last_notice_seq: Option<u64>,
    // 压缩记录（模块 D）：传对象 = 覆盖写入；null/缺省 = 不更新。
    // （tauri command 参数上的 doc 注释不合法，用普通注释）
    compaction: Option<CompactionMeta>,
    // 显式清空 contextTokens（模块 D：压缩归零的持久面——contextTokens=null
    // 与缺省同为 None 无法区分，故独立布尔；压缩完成时前端随 compaction 置 true）
    clear_context_tokens: Option<bool>,
) -> Result<(), String> {
    let patch = MetaPatch {
        title,
        expected_title,
        context_tokens,
        last_notice_seq,
        compaction,
        context_tokens_clear: clear_context_tokens.unwrap_or(false),
    };
    let mut s = store.lock().map_err(|e| format!("会话存储锁获取失败: {e}"))?;
    s.update_meta(&conversation_id, &patch)
}

// ── 会话自动标题（生成）───────────────────────────────

/// 生成会话标题（主模型静默 run）：从用户首问 + 首答可见文本生成短标题，
/// CAS 写入 index.json 后推送 `conversation/title` 事件。
///
/// 语义（与前端触发层配合）：
/// - **命令 fire-and-forget**：前端 invoke 不 await 返回值；结果经事件回推
///   （与 append_turn / agent-event 同模式）。
/// - **CAS 三态写入**（`try_set_title_if_expected`）：会话已存在且标题仍为
///   默认 → 写入；会话未落盘（首轮流式期）→ 不写但推送，由前端携带至其后
///   append_turn；用户已改名 → 放弃（不写不推）。
/// - **失败静默降级**：生成失败（无 client / 超时 / 空）→ 直接 Ok(())，
///   保留默认标题，不重试、不报错。
///
/// `expectedTitle` = 前端产出的默认标题哨兵（`deriveTitle(首问)`）——后端
/// 不复算，只做字符串相等比较（规避跨语言截取差异）。
#[tauri::command]
pub async fn generate_conversation_title(
    app_handle: AppHandle,
    llm: State<'_, Arc<ClientRegistry>>,
    config: State<'_, Arc<Mutex<LlmConfig>>>,
    store: State<'_, Arc<Mutex<SessionStore>>>,
    conversation_id: String,
    user_text: String,
    assistant_text: String,
    expected_title: String,
) -> Result<(), String> {
    // turn 起始配置快照（短暂持锁）
    let llm_config = {
        let guard = config.lock().map_err(|e| format!("配置锁获取失败: {e}"))?;
        guard.clone()
    };
    let client = llm
        .select(&llm_config.api_type)
        .ok_or_else(|| format!("协议 {:?} 暂未支持", llm_config.api_type))?;

    let title = match orchestrator::title::generate_title(
        &**client,
        &llm_config,
        &user_text,
        &assistant_text,
    )
    .await
    {
        Some(t) => t,
        None => return Ok(()), // 失败静默降级（保留默认标题）
    };

    let outcome = {
        let mut s = store.lock().map_err(|e| format!("会话存储锁获取失败: {e}"))?;
        s.try_set_title_if_expected(&conversation_id, &title, &expected_title)
    };

    match outcome {
        Ok(TitleWriteOutcome::TitleChanged) => {
            // 用户已改名/已生成过 → 放弃（不推送，避免覆盖用户标题）
            crate::debug!(
                "action" => "title_generation_skipped",
                "conversation_id" => conversation_id,
                "reason" => "title_changed"
            );
            Ok(())
        }
        Ok(outcome) => {
            // Written / NotExists → 推送事件（NotExists 由前端携带至 append_turn）
            let event = IpcEvent::Agent(AgentEvent::conversation_title(&conversation_id, &title));
            if let Err(e) = app_handle.emit("agent-event", event) {
                eprintln!("[commands] 标题事件推送失败: {e}");
            }
            if matches!(outcome, TitleWriteOutcome::NotExists) {
                crate::debug!(
                    "action" => "title_generation_not_persisted",
                    "conversation_id" => conversation_id
                );
            }
            Ok(())
        }
        Err(e) => {
            // 写盘错误（非致命）：不发事件（避免前端显示未落盘的标题）
            eprintln!("[commands] 标题写入失败: {e}");
            Ok(())
        }
    }
}

// ═══════════════════════════════════════════════════════
// 上下文压缩（模块 D · 上下文衰减）
// ═══════════════════════════════════════════════════════

/// 压缩请求体（compact_conversation 命令）。
///
/// `session_id` 由前端生成（约定 `ctxdecay-{conversationId}`），经
/// TurnRegistry 注册后即可走既有 `abort_turn` 通道取消摘要 run（压缩中
/// 再次点击圆环 → 取消确认 → abort 同一 id）。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactRequest {
    pub conversation_id: String,
    /// 压缩注册/取消用 session 标识（约定 ctxdecay-{conversationId}）
    pub session_id: String,
    /// 折叠区历史（前端派生；链式压缩时含旧摘要消息）
    pub folded_history: Vec<FrontendMessage>,
}

/// compact_conversation 成功载荷（camelCase 序列化给前端）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionOutcome {
    /// 结构化 checkpoint 正文（定界块由前端合成）
    pub summary: String,
    /// true = 输入超窗规则降级产出（无 LLM 调用）
    pub degraded: bool,
    /// 折叠区压缩前估算（信息用，随压缩记录落盘）
    pub tokens_before: Option<u64>,
}

/// 压缩对话（模块 D · 上下文衰减）：对前端折叠区做一次静默独立 LLM 摘要 run。
///
/// 语义要点：
/// - 压缩 = 前端发起的一次对话级状态转换；本命令只产出摘要正文，**不产生
///   对话轮事件、不 emit agent-event**（压缩不是轮；记录由前端
///   update_conversation_meta 写入会话元数据）；
/// - busy 闸：对话处理中（active_count > 0）拒绝——生成与压缩双向互斥，
///   拒绝而非排队（避免压缩跑在被改写历史上）；
/// - 取消：以 `ctxdecay-{conversationId}` 注册 TurnRegistry，前端可经
///   abort_turn 中止；注册计入 active_count（压缩中设置写闸同样生效）；
/// - 失败语义：Err(String) 文案面向用户直显；失败不改变任何状态，幂等可重试。
#[tauri::command]
pub async fn compact_conversation(
    request: CompactRequest,
    llm: State<'_, Arc<ClientRegistry>>,
    config: State<'_, Arc<Mutex<LlmConfig>>>,
    model_registry: State<'_, Arc<ModelRegistry>>,
    turn_registry: State<'_, Arc<TurnRegistry>>,
) -> Result<CompactionOutcome, String> {
    if request.folded_history.is_empty() {
        return Err("没有可压缩的内容".into());
    }
    if turn_registry.active_count() > 0 {
        return Err("正在处理对话，请稍后再压缩".into());
    }
    let cfg = config
        .lock()
        .map_err(|e| format!("配置锁获取失败: {e}"))?
        .clone();
    let client = llm.select(&cfg.api_type).ok_or_else(|| {
        format!("协议 {:?} 暂未支持", cfg.api_type)
    })?;
    // 窗口与主对话同源（model_registry）；自定义模型查不到时用兜底常量
    let window = model_registry
        .get_model(&cfg.provider, &cfg.model)
        .map(|m| m.context)
        .unwrap_or(orchestrator::compaction::DEFAULT_CONTEXT_WINDOW);
    let abort = turn_registry.register(&request.session_id, &request.conversation_id);
    let result = orchestrator::compaction::run_summary(
        &**client,
        &cfg,
        request.folded_history,
        window,
        &abort,
    )
    .await;
    turn_registry.unregister(&request.session_id);
    result.map(|o| CompactionOutcome {
        summary: o.summary,
        degraded: o.degraded,
        tokens_before: o.tokens_before,
    })
}

/// 删除会话：先删持久文件（jsonl + index 条目），再清记忆快照 sidecar。
#[tauri::command]
pub fn delete_conversation(
    store: State<'_, Arc<Mutex<SessionStore>>>,
    snapshot_store: State<'_, Arc<Mutex<SnapshotStore>>>,
    conversation_id: String,
) -> Result<(), String> {
    // 先删会话（失败上抛，不静默）；再清 sidecar（记忆区冻结快照同生共死；
    // 裁决 1：先写快照后归档 → 孤儿在删除侧清扫）。
    {
        let mut s = store.lock().map_err(|e| format!("会话存储锁获取失败: {e}"))?;
        s.delete(&conversation_id)?;
    }
    let mut snaps = snapshot_store
        .lock()
        .map_err(|e| format!("快照存储锁获取失败: {e}"))?;
    snaps.remove(&conversation_id);
    Ok(())
}

// ═══════════════════════════════════════════════════════
// 记忆系统（Phase 4 任务二）——经历管线 + 启动补偿
// ═══════════════════════════════════════════════════════

/// 启动补偿扫描的会话数上限（按 updatedAt 降序前 N 个）。
const MEMORY_CATCH_UP_SCAN: usize = 20;
/// 启动补偿单次返回的待补会话数上限。
const MEMORY_CATCH_UP_LIMIT: usize = 5;

/// 触发经历管线（fire-and-forget）：对指定对话"水位线之后的增量"做提炼。
///
/// 前端在轮末 / 切换 / 删除 / 启动补偿时调用；后端自判阈值
/// （增量 < [`orchestrator::memory::INCREMENTAL_TURNS`] → no-op）。后台 turn：
/// 注册 TurnRegistry（可 abort）但**不计 busy**（不拦用户操作）。失败/整轮报废
/// → 记录并静默返回 Ok（前端不 await）。
#[tauri::command]
pub async fn finalize_memory(
    llm: State<'_, Arc<ClientRegistry>>,
    config: State<'_, Arc<Mutex<LlmConfig>>>,
    store: State<'_, Arc<Mutex<SessionStore>>>,
    memory_store: State<'_, Arc<Mutex<MemoryStore>>>,
    ledger: State<'_, Arc<Mutex<Notices>>>,
    turn_registry: State<'_, Arc<TurnRegistry>>,
    conversation_id: String,
) -> Result<(), String> {
    if conversation_id.trim().is_empty() {
        return Ok(());
    }
    // turn 起始配置快照
    let llm_config = {
        let guard = config.lock().map_err(|e| format!("配置锁获取失败: {e}"))?;
        guard.clone()
    };
    let Some(client) = llm.select(&llm_config.api_type) else {
        return Ok(()); // 协议未支持 → 静默
    };
    // 读水位 + 增量（会话文件缺失/损坏 → 无可提炼）
    let (delta, turn_from, new_watermark) = {
        let mut s = store.lock().map_err(|e| format!("会话存储锁获取失败: {e}"))?;
        let rec = match s.get(&conversation_id) {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };
        let total = rec.turns.len() as u64;
        let wm = {
            let m = memory_store
                .lock()
                .map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
            m.watermark(&conversation_id)
        };
        if wm >= total {
            return Ok(());
        }
        (rec.turns[wm as usize..].to_vec(), wm, total)
    };
    let memory_store = Arc::clone(&memory_store);
    let ledger = Arc::clone(&ledger);
    let turn_registry = Arc::clone(&turn_registry);
    let bg_id = format!("memsync-{conversation_id}");
    let abort = turn_registry.register_background(&bg_id, &conversation_id);
    let result = orchestrator::memory::run_experience_pipeline(
        &**client,
        &llm_config,
        &memory_store,
        &ledger,
        &abort,
        orchestrator::memory::PipelineInput {
            conversation_id: conversation_id.clone(),
            delta_turns: delta,
            turn_from,
            new_watermark,
        },
    )
    .await;
    turn_registry.unregister(&bg_id);
    if let Err(e) = result {
        eprintln!("[commands] 记忆管线整轮报废（{conversation_id}）：{e}");
    }
    Ok(())
}

/// 启动补偿：返回需要补提炼的对话 id（水位落后于总轮数）。
///
/// 最近更新优先、最多 limit 条（缺省 [`MEMORY_CATCH_UP_LIMIT`]）；前端逐个
/// invoke finalize_memory。仅扫描 updatedAt 前 [`MEMORY_CATCH_UP_SCAN`] 条，
/// 控制启动开销。
#[tauri::command]
pub fn memory_catch_up(
    store: State<'_, Arc<Mutex<SessionStore>>>,
    memory_store: State<'_, Arc<Mutex<MemoryStore>>>,
    limit: Option<usize>,
) -> Result<Vec<String>, String> {
    let limit = limit.unwrap_or(MEMORY_CATCH_UP_LIMIT).max(1);
    let mut s = store.lock().map_err(|e| format!("会话存储锁获取失败: {e}"))?;
    let mut metas = s.list()?;
    metas.sort_by_key(|m| std::cmp::Reverse(m.updated_at));
    let mem = memory_store
        .lock()
        .map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
    let mut out = Vec::new();
    for m in metas.into_iter().take(MEMORY_CATCH_UP_SCAN) {
        let total = match s.get(&m.id) {
            Ok(r) => r.turns.len() as u64,
            Err(_) => continue,
        };
        if total > mem.watermark(&m.id) {
            out.push(m.id);
            if out.len() >= limit {
                break;
            }
        }
    }
    Ok(out)
}

// ═══════════════════════════════════════════════════════
// 记忆页（设置页「记忆」子页命令面 · Phase 4 任务二）
// ═══════════════════════════════════════════════════════

/// 成长页元数据（overview 载荷；不含正文）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrowthMetaDto {
    pub name: String,
    pub brief: String,
    pub tags: Vec<String>,
    pub updated_at: u64,
    /// 是否「关于博士」（前端置顶用）
    pub is_profile: bool,
}

/// 「近期」条目（与提示词渲染同源）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentItemDto {
    pub date: String,
    pub title: String,
    pub summary: String,
}

/// memory_overview 载荷（页面初次加载一次取齐）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryOverviewDto {
    pub growth: Vec<GrowthMetaDto>,
    pub recent: Vec<RecentItemDto>,
    /// 「先前」长期梗概全文（按需展开）
    pub previous: String,
    /// 记忆库条目数（全部经历条目，含已收纳）
    pub experience_count: usize,
}

/// 成长页详情（memory_get growth）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrowthPageDto {
    pub name: String,
    pub brief: String,
    pub body: String,
    pub tags: Vec<String>,
    pub source: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub is_profile: bool,
}

/// 经历条目（memory_get experience / list）。无正文层：状态句＋标签＋来源。
/// `absorbed`＝已蒸进长期；`sealed`＝未收纳且重写达封存线（供列表小标）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExperienceItemDto {
    pub id: String,
    pub date: String,
    pub title: String,
    pub summary: String,
    pub tags: Vec<String>,
    pub absorbed: bool,
    /// 未收纳且重写次数达封存线（REVISION_SEALED）
    pub sealed: bool,
    /// 来源：对话 id ＋ 轮次范围
    pub conversation_id: String,
    pub turn_from: u64,
    pub turn_to: u64,
}

/// memory_get 返回（kind 判别）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDetailDto {
    pub kind: String,
    pub page: Option<GrowthPageDto>,
    pub entry: Option<ExperienceItemDto>,
}

/// memory_set_longterm 返回。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySetLongtermDto {
    /// 写入后的长期记忆版本号
    pub version: u64,
    /// 写入字数
    pub chars: usize,
    /// 是否有实际改动（false＝原文逐字相同：未写盘、未推进版本、未推提醒）
    pub changed: bool,
}

/// memory_tidy 返回（界面反馈）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryTidyDto {
    pub status: String, // created | rewritten | compressed | no_delta | cooldown | failed
    pub message: String,
    pub created: usize,
    pub rewritten: usize,
    pub compressed: bool,
}

fn growth_meta(page: &crate::memory::schema::GrowthPage) -> GrowthMetaDto {
    GrowthMetaDto {
        name: page.name.clone(),
        brief: page.brief.clone(),
        tags: page.tags.clone(),
        updated_at: page.updated_at,
        is_profile: page.name == PROFILE_NAME,
    }
}

fn experience_item(e: &crate::memory::schema::ExperienceEntry) -> ExperienceItemDto {
    ExperienceItemDto {
        id: e.id.clone(),
        date: crate::memory::prompt::ymd_local(e.ts),
        title: e.title.clone(),
        summary: e.summary.clone(),
        tags: e.tags.clone(),
        absorbed: e.is_absorbed(),
        sealed: !e.is_absorbed() && e.revision_count >= REVISION_SEALED,
        conversation_id: e.conversation_id.clone(),
        turn_from: e.turn_from,
        turn_to: e.turn_to,
    }
}

/// 非查命令的 busy 闸（有前台轮在跑 → 拒绝；与 set_settings 同口径）。
fn ensure_not_busy(tr: &TurnRegistry) -> Result<(), String> {
    if tr.active_count() > 0 {
        Err("对话处理中，无法修改记忆".into())
    } else {
        Ok(())
    }
}

fn note_memory(ledger: &Arc<Mutex<Notices>>, detail: &str) {
    match ledger.lock() {
        Ok(mut l) => l.note_memory_change("记忆", detail),
        Err(e) => eprintln!("[commands] 台账锁获取失败（记忆变化未通知）: {e}"),
    }
}

/// 记忆总览（页面初次加载一次取齐：成长列表 + 当前注入 + 计数）。
#[tauri::command]
pub fn memory_overview(
    memory_store: State<'_, Arc<Mutex<MemoryStore>>>,
) -> Result<MemoryOverviewDto, String> {
    let m = memory_store
        .lock()
        .map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
    let mut growth = vec![growth_meta(m.profile())];
    growth.extend(m.topics().iter().map(growth_meta));
    let recent = crate::memory::prompt::recent_items(&m)
        .into_iter()
        .map(|(date, title, summary)| RecentItemDto {
            date,
            title,
            summary,
        })
        .collect();
    Ok(MemoryOverviewDto {
        growth,
        recent,
        previous: m.longterm().trim().to_string(),
        experience_count: m.experiences().len(),
    })
}

/// 单条详情：成长页全文（编辑前读取）／经历条目详情（记忆库展开）。
#[tauri::command]
pub fn memory_get(
    memory_store: State<'_, Arc<Mutex<MemoryStore>>>,
    kind: String,
    name: Option<String>,
    id: Option<String>,
) -> Result<MemoryDetailDto, String> {
    let m = memory_store
        .lock()
        .map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
    match kind.as_str() {
        "growth" => {
            let name = name
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "缺少参数 name".to_string())?;
            let page = m
                .growth_get(&name)
                .ok_or_else(|| format!("成长页「{name}」不存在"))?;
            Ok(MemoryDetailDto {
                kind: "growth".into(),
                page: Some(GrowthPageDto {
                    name: page.name.clone(),
                    brief: page.brief.clone(),
                    body: page.body.clone(),
                    tags: page.tags.clone(),
                    source: page.source.clone(),
                    created_at: page.created_at,
                    updated_at: page.updated_at,
                    is_profile: page.name == PROFILE_NAME,
                }),
                entry: None,
            })
        }
        "experience" => {
            let id = id
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| "缺少参数 id".to_string())?;
            let e = m
                .experience_get(&id)
                .ok_or_else(|| format!("经历条目「{id}」不存在"))?;
            Ok(MemoryDetailDto {
                kind: "experience".into(),
                page: None,
                entry: Some(experience_item(e)),
            })
        }
        other => Err(format!("未知 kind「{other}」（可用 growth / experience）")),
    }
}

/// 新建成长页（重名（含「关于博士」）报错）。
#[tauri::command]
pub fn memory_create(
    turn_registry: State<'_, Arc<TurnRegistry>>,
    memory_store: State<'_, Arc<Mutex<MemoryStore>>>,
    ledger: State<'_, Arc<Mutex<Notices>>>,
    name: String,
    brief: Option<String>,
    body: String,
    tags: Option<Vec<String>>,
) -> Result<(), String> {
    ensure_not_busy(&turn_registry)?;
    let brief = brief.unwrap_or_default();
    {
        let mut m = memory_store
            .lock()
            .map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
        m.growth_create(&name, &brief, &body, tags.unwrap_or_default(), SOURCE_USER)?;
    }
    let b = brief.trim();
    let detail = if b.is_empty() {
        format!("博士新建了成长页「{name}」")
    } else {
        format!("博士新建了成长页「{name}：{b}」")
    };
    note_memory(&ledger, &detail);
    Ok(())
}

/// 编辑成长页（name ＋ 简述/正文/标签任一；「关于博士」同走）。
///
/// 返回是否有实际改动：内容逐字相同 → 不写盘、不刷新时间、不推提醒，返回 `false`。
#[tauri::command]
pub fn memory_update(
    turn_registry: State<'_, Arc<TurnRegistry>>,
    memory_store: State<'_, Arc<Mutex<MemoryStore>>>,
    ledger: State<'_, Arc<Mutex<Notices>>>,
    name: String,
    brief: Option<String>,
    body: Option<String>,
    tags: Option<Vec<String>>,
) -> Result<bool, String> {
    ensure_not_busy(&turn_registry)?;
    if brief.is_none() && body.is_none() && tags.is_none() {
        return Err("至少提供 brief / body / tags 之一".into());
    }
    let (changed, is_profile, full, brief_after) = {
        let mut m = memory_store
            .lock()
            .map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
        let changed =
            m.growth_update(&name, body.as_deref(), brief.as_deref(), tags, SOURCE_USER)?;
        let p = m
            .growth_get(&name)
            .ok_or_else(|| format!("成长页「{name}」不存在"))?;
        (
            changed,
            p.name == PROFILE_NAME,
            p.body.trim().to_string(),
            p.brief.trim().to_string(),
        )
    };
    if !changed {
        return Ok(false);
    }
    let detail = if is_profile {
        format!("博士更新了成长页「{name}」\n{full}")
    } else if brief_after.is_empty() {
        format!("博士更新了成长页「{name}」")
    } else {
        format!("博士更新了成长页「{name}：{brief_after}」")
    };
    note_memory(&ledger, &detail);
    Ok(true)
}

/// 删除成长页（「关于博士」删＝复位）；不含经历。
#[tauri::command]
pub fn memory_delete(
    turn_registry: State<'_, Arc<TurnRegistry>>,
    memory_store: State<'_, Arc<Mutex<MemoryStore>>>,
    ledger: State<'_, Arc<Mutex<Notices>>>,
    name: String,
) -> Result<(), String> {
    ensure_not_busy(&turn_registry)?;
    {
        let mut m = memory_store
            .lock()
            .map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
        m.growth_delete(&name)?;
    }
    note_memory(&ledger, &format!("博士删除了成长页「{name}」"));
    Ok(())
}

/// 记忆库全量条目（含已收录；id/日期/标题/摘要/是否已收纳）。
#[tauri::command]
pub fn memory_experience_list(
    memory_store: State<'_, Arc<Mutex<MemoryStore>>>,
) -> Result<Vec<ExperienceItemDto>, String> {
    let m = memory_store
        .lock()
        .map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
    let mut rows: Vec<(u64, ExperienceItemDto)> = m
        .experiences()
        .iter()
        .map(|e| (e.ts, experience_item(e)))
        .collect();
    rows.sort_by_key(|(ts, _)| std::cmp::Reverse(*ts));
    Ok(rows.into_iter().map(|(_, dto)| dto).collect())
}

/// 清除经历记忆：清经历本体＋长期梗概；水位抬平到各对话当前总轮数
/// （防旧轮次被补提炼挖回）；不动冻结文件。
#[tauri::command]
pub fn memory_experience_clear(
    turn_registry: State<'_, Arc<TurnRegistry>>,
    memory_store: State<'_, Arc<Mutex<MemoryStore>>>,
    store: State<'_, Arc<Mutex<SessionStore>>>,
    ledger: State<'_, Arc<Mutex<Notices>>>,
) -> Result<(), String> {
    ensure_not_busy(&turn_registry)?;
    {
        let mut m = memory_store
            .lock()
            .map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
        m.clear_experiences()?;
    }
    let now = crate::memory::schema::now_ms();
    {
        let mut s = store.lock().map_err(|e| format!("会话存储锁获取失败: {e}"))?;
        let metas = s.list()?;
        let mut m = memory_store
            .lock()
            .map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
        for meta in metas {
            let total = match s.get(&meta.id) {
                Ok(r) => r.turns.len() as u64,
                Err(_) => continue,
            };
            m.raise_watermark(&meta.id, total, now)?;
        }
    }
    note_memory(&ledger, "博士清除了经历记忆");
    Ok(())
}

/// 重写长期记忆：把「先前」正文覆盖为给定文本（版本 +1，走台账提醒）。
///
/// 定位：**人工纠偏入口**（与成长页编辑同构）——自动沉淀跑偏时由用户改写。
/// 上限与压缩硬闸同源（[`MAX_LONGTERM_CHARS`]）；空串允许（＝清空）。
/// 无改动（逐字相同）→ 不写盘、不推进版本、不推提醒（见 `set_longterm_manual`）。
/// 写入对**新对话**生效；正在进行的对话靠提醒看到新版全文（快照对话内冻结）。
#[tauri::command]
pub fn memory_set_longterm(
    turn_registry: State<'_, Arc<TurnRegistry>>,
    memory_store: State<'_, Arc<Mutex<MemoryStore>>>,
    ledger: State<'_, Arc<Mutex<Notices>>>,
    text: String,
) -> Result<MemorySetLongtermDto, String> {
    ensure_not_busy(&turn_registry)?;
    validate_longterm(&text)?;
    let now = crate::memory::schema::now_ms();
    let (version, changed) = {
        let mut m = memory_store
            .lock()
            .map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
        m.set_longterm_manual(text.clone(), now)?
    };
    if changed {
        let detail = format!("长期记忆已重写（版本 {version}）\n{}", text.trim());
        note_memory(&ledger, &detail);
    }
    Ok(MemorySetLongtermDto {
        version,
        chars: text.chars().count(),
        changed,
    })
}

/// 长期记忆写入校验（人工编辑入口专用；与压缩硬闸同源）。
/// 空串放行（＝清空）；超限报错，不静默截断。
fn validate_longterm(text: &str) -> Result<(), String> {
    let n = text.chars().count();
    let max = crate::orchestrator::memory::MAX_LONGTERM_CHARS;
    if n > max {
        return Err(format!("长期记忆过长：{n} 字，上限 {max} 字"));
    }
    Ok(())
}

/// 整理当下记忆：对指定对话跑一次经历管线（**忽略 6 轮阈值**；冷却不静默）。
#[tauri::command]
pub async fn memory_tidy(
    llm: State<'_, Arc<ClientRegistry>>,
    config: State<'_, Arc<Mutex<LlmConfig>>>,
    store: State<'_, Arc<Mutex<SessionStore>>>,
    memory_store: State<'_, Arc<Mutex<MemoryStore>>>,
    ledger: State<'_, Arc<Mutex<Notices>>>,
    turn_registry: State<'_, Arc<TurnRegistry>>,
    conversation_id: String,
) -> Result<MemoryTidyDto, String> {
    if conversation_id.trim().is_empty() {
        return Err("缺少 conversationId".into());
    }
    ensure_not_busy(&turn_registry)?;
    let llm_config = {
        let guard = config.lock().map_err(|e| format!("配置锁获取失败: {e}"))?;
        guard.clone()
    };
    let Some(client) = llm.select(&llm_config.api_type) else {
        return Ok(MemoryTidyDto {
            status: "failed".into(),
            message: "当前协议不支持后台整理".into(),
            created: 0,
            rewritten: 0,
            compressed: false,
        });
    };
    // 读水位 + 增量
    let (delta, turn_from, new_watermark) = {
        let mut s = store.lock().map_err(|e| format!("会话存储锁获取失败: {e}"))?;
        let rec = match s.get(&conversation_id) {
            Ok(r) => r,
            Err(_) => {
                return Ok(MemoryTidyDto {
                    status: "no_delta".into(),
                    message: "该对话尚无归档内容".into(),
                    created: 0,
                    rewritten: 0,
                    compressed: false,
                })
            }
        };
        let total = rec.turns.len() as u64;
        let wm = {
            let m = memory_store
                .lock()
                .map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
            m.watermark(&conversation_id)
        };
        (rec.turns[wm.min(total) as usize..].to_vec(), wm, total)
    };
    let memory_store = Arc::clone(&memory_store);
    let ledger = Arc::clone(&ledger);
    let turn_registry = Arc::clone(&turn_registry);
    let bg_id = format!("memsync-{conversation_id}");
    let abort = turn_registry.register_background(&bg_id, &conversation_id);
    let result = orchestrator::memory::run_experience_pipeline_with(
        &**client,
        &llm_config,
        &memory_store,
        &ledger,
        &abort,
        orchestrator::memory::PipelineInput {
            conversation_id: conversation_id.clone(),
            delta_turns: delta,
            turn_from,
            new_watermark,
        },
        true, // 忽略 6 轮阈值
    )
    .await;
    turn_registry.unregister(&bg_id);
    Ok(match result {
        Err(e) => MemoryTidyDto {
            status: "failed".into(),
            message: format!("整理失败：{e}"),
            created: 0,
            rewritten: 0,
            compressed: false,
        },
        Ok(o) => {
            let (status, message) = if o.skipped.as_deref() == Some("cooldown") {
                ("cooldown", "冷却中，请稍后再试".to_string())
            } else if o.created + o.rewritten > 0 {
                let mut msg = format!("已整理：新建 {} 条、重写 {} 条", o.created, o.rewritten);
                if o.compressed {
                    msg.push_str("，并压缩长期记忆");
                }
                (if o.rewritten > 0 { "rewritten" } else { "created" }, msg)
            } else if o.compressed {
                ("compressed", "已压缩长期记忆".to_string())
            } else {
                ("no_delta", "没有可整理的新增量".to_string())
            };
            MemoryTidyDto {
                status: status.into(),
                message,
                created: o.created,
                rewritten: o.rewritten,
                compressed: o.compressed,
            }
        }
    })
}

/// 从本地 ModelRegistry 查询模型信息（不上网）。
///
/// 仅在 models.json 中的预置模型可查。自定义供应商的模型不在注册表中，返回 None。
/// 供前端启动时初始化模型名称和上下文窗口大小，避免依赖设置页手动点选。
#[tauri::command]
pub fn get_local_model_info(
    model_registry: State<'_, Arc<ModelRegistry>>,
    provider: String,
    model_id: String,
) -> Result<Option<ModelInfo>, String> {
    let entry = model_registry.get_model(&provider, &model_id);
    Ok(entry.map(|e| ModelInfo {
        id: model_id,
        display_name: e.name.clone(),
        context_length: e.context,
        supports_reasoning: e.reasoning,
        supports_vision: e.supports_vision,
        compat: e.compat.clone().unwrap_or_default(),
        thinking_levels: e.thinking_levels.clone(),
        default_thinking_level: e.default_thinking_level.clone(),
        enriched: true,
    }))
}

// ═══════════════════════════════════════════════════════
// quit_app —— 关闭应用
// ═══════════════════════════════════════════════════════

/// 关闭终端应用。
///
/// 设置页"通用"标签中的"关闭终端"按钮调用此命令。
/// 无人机悬浮窗无原生窗口控件（关闭/最小化/最大化），
/// 关闭入口统一收在设置页内。
#[tauri::command]
pub fn quit_app(app_handle: AppHandle) {
    app_handle.exit(0);
}

// ═══════════════════════════════════════════════════════
// verify_connection —— 验证 LLM 连接
// ═══════════════════════════════════════════════════════

/// 验证 LLM 供应商连接是否可用。
///
/// 用传入的 LlmConfig 发起一次 GET /v1/models 请求（5 秒超时）。
/// 适用于 OpenAI 兼容 API（DeepSeek、OpenAI、Ollama）。
/// 注意：Anthropic API 不兼容 OpenAI 协议，不存在 /v1/models 端点，
/// provider="anthropic" 时验证必然返回 404。此为预期行为——
/// Anthropic 适配器尚未实现，provider 选项仅为未来扩展预留。
/// 成功：返回 Ok。失败：返回可读的错误描述。
#[tauri::command]
pub async fn verify_connection(config: LlmConfig) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let base_url = config
        .base_url
        .as_deref()
        .or_else(|| default_base_url(&config.provider));

    let Some(base_url) = base_url else {
        return Err("未配置 Base URL，请填写或选择供应商".into());
    };

    // 按 API 类型构造验证请求：不同协议的 /models 端点路径和鉴权方式不同
    let (url, auth_header): (String, Option<(_, _)>) = match config.api_type {
        ApiType::AnthropicMessages => {
            // Anthropic 协议：用 x-api-key 鉴权，剥离尾部路径后请求 /v1/models
            let root = strip_api_path(base_url);
            (format!("{}/v1/models", root), Some(("x-api-key", config.api_key.clone())))
        }
        _ => {
            // OpenAI 兼容协议：用 Bearer 鉴权
            let url = format!("{}/models", base_url.trim_end_matches('/'));
            let auth = if config.api_key.is_empty() {
                None
            } else {
                Some(("authorization", format!("Bearer {}", config.api_key)))
            };
            (url, auth.map(|(k, v)| (k, v)))
        }
    };

    let mut request = client.get(&url);
    if let Some((key, value)) = auth_header {
        request = request.header(key, &value);
    }

    // 解析并附加用户自定义 Headers
    if let Some(ref headers_str) = config.headers {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(headers_str) {
            if let Some(obj) = json.as_object() {
                for (k, v) in obj {
                    if let Some(val) = v.as_str() {
                        request = request.header(k.as_str(), val);
                    }
                }
            }
        }
    }

    let response = request.send().await.map_err(|e| {
        if e.is_timeout() {
            "连接超时：请检查 Base URL 和网络连接".into()
        } else if e.is_connect() {
            "无法连接服务器：请检查 Base URL 是否正确".into()
        } else {
            format!("请求失败: {e}")
        }
    })?;

    match response.status().as_u16() {
        200 => Ok(()),
        401 | 403 => Err("API Key 无效".into()),
        404 => Err("API 端点不存在：请检查 Base URL".into()),
        s => Err(format!("服务器返回异常状态 ({s})")),
    }
}

/// 剥离 base_url 中的 API 路径后缀，返回供应商根 URL。
/// 例如：https://api.deepseek.com/anthropic → https://api.deepseek.com
fn strip_api_path(url: &str) -> String {
    let url = url.trim_end_matches('/');
    // 尝试解析为 URL，失败则简单处理
    if let Ok(parsed) = reqwest::Url::parse(url) {
        let host = parsed.host_str().unwrap_or("");
        let scheme = parsed.scheme();
        // 对已知供应商，返回根路径
        if host.contains("api.deepseek.com") {
            return format!("{scheme}://api.deepseek.com");
        }
        if host.contains("api.anthropic.com") {
            return format!("{scheme}://api.anthropic.com");
        }
        // 其他情况：保留 host，去掉路径
        format!("{scheme}://{host}")
    } else {
        url.to_string()
    }
}

/// 列出所有可用供应商。
///
/// 从内存中的 ModelRegistry 枚举供应商信息，返回 id、展示名、
/// api_type、base_url、默认 compat。前端供应商下拉渲染此列表。
#[tauri::command]
pub fn list_providers(
    model_registry: State<'_, Arc<ModelRegistry>>,
) -> Vec<ProviderInfo> {
    model_registry.list_providers()
}

/// 获取指定供应商的可用模型列表。
///
/// 按 api_type 分流：
/// - OpenAI 兼容协议：GET {base_url}/models + Bearer
/// - Anthropic 协议：GET {root}/v1/models + x-api-key
///
/// 解析远端模型 ID 列表，逐个在 ModelRegistry 中匹配能力信息。
#[tauri::command]
pub async fn list_models(
    provider: String,
    api_key: String,
    base_url: Option<String>,
    api_type: Option<String>,
    model_registry: State<'_, Arc<ModelRegistry>>,
) -> Result<Vec<ModelInfo>, String> {
    let base = base_url
        .as_deref()
        .or_else(|| model_registry.get_base_url(&provider));

    let Some(base) = base else {
        return Err("未找到该供应商的 Base URL，请先选择供应商".into());
    };

    let is_anthropic = api_type.as_deref() == Some("anthropic-messages");

    let (url, auth_header): (String, Option<(&str, String)>) = if is_anthropic {
        // Anthropic 协议：剥离路径回到根 URL，请求 /v1/models，用 x-api-key
        let root = strip_api_path(base);
        let url = format!("{}/v1/models", root);
        let auth = if api_key.is_empty() {
            None
        } else {
            Some(("x-api-key", api_key.clone()))
        };
        (url, auth)
    } else {
        // OpenAI 兼容协议
        let url = format!("{}/models", base.trim_end_matches('/'));
        let auth = if api_key.is_empty() {
            None
        } else {
            Some(("authorization", format!("Bearer {}", api_key)))
        };
        (url, auth)
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let mut request = client.get(&url);
    if let Some((key, value)) = auth_header {
        request = request.header(key, &value);
    }

    let response = request.send().await.map_err(|e| {
        if e.is_timeout() {
            "获取模型列表超时：请检查网络连接".into()
        } else if e.is_connect() {
            "无法连接服务器：请检查 Base URL".into()
        } else {
            format!("请求失败: {e}")
        }
    })?;

    if !response.status().is_success() {
        return Err(format!(
            "服务器返回 HTTP {}，请检查 API Key 和 Base URL",
            response.status().as_u16()
        ));
    }

    // 解析 OpenAI /models 响应格式：{ "data": [{ "id": "...", ... }, ...] }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("解析模型列表失败: {e}"))?;

    let data = body["data"]
        .as_array()
        .ok_or("模型列表格式异常：缺少 data 数组")?;

    let models: Vec<ModelInfo> = data
        .iter()
        .filter_map(|entry| {
            let id = entry["id"].as_str()?;
            let matched = model_registry.get_model(&provider, id);

            Some(ModelInfo {
                id: id.to_string(),
                display_name: matched
                    .map(|m| m.name.clone())
                    .unwrap_or_else(|| id.to_string()),
                context_length: matched.map(|m| m.context).unwrap_or(0),
                supports_reasoning: matched.map(|m| m.reasoning).unwrap_or(false),
                supports_vision: matched.map(|m| m.supports_vision).unwrap_or(false),
                compat: matched
                    .and_then(|m| m.compat.clone())
                    .unwrap_or_default(),
                thinking_levels: matched
                    .map(|m| m.thinking_levels.clone())
                    .unwrap_or_default(),
                default_thinking_level: matched
                    .and_then(|m| m.default_thinking_level.clone()),
                enriched: matched.is_some(),
            })
        })
        .collect();

    Ok(models)
}

// ═══════════════════════════════════════════════════════
// 命令执行模式（M3-A §4.2）——已并入 settings.json exec 段
// （get_exec_settings / set_exec_settings 由 get_settings / set_settings
// 的 exec patch 取代，2026-09 设置页扩展批 A）
// ═══════════════════════════════════════════════════════

// ═══════════════════════════════════════════════════════
// 中止当前对话轮（M3-B §2.1）
// ═══════════════════════════════════════════════════════

/// 中止一次活动对话轮次（幂等）。
///
/// - `Ok(true)` = 找到活动 turn 并已置位中止（进程经取消点收敛退出）；
/// - `Ok(false)` = 无该 session 的活动 turn（前端幂等处理，非错误）。
///
/// 命令**只置标志**，不碰 approvals/进程——清理收敛在 orchestrator::process
/// 的 Aborted 出口（clear_session + turn/end(aborted) emit），避免双头清理。
/// 置位后 busy 仍保持（快照未删），直到 process 返回、命令层 unregister。
#[tauri::command]
pub fn abort_turn(
    steer_queue: State<'_, Arc<SteerQueue>>,
    turn_registry: State<'_, Arc<TurnRegistry>>,
    session_id: String,
) -> Result<bool, String> {
    // 中止语义（插话补充，设计定稿）：中止 = 停当前段 + 清空该 session
    // 插话队列——待注入条目随中止作废（前端 pending 由 abort 反馈侧清除，
    // 不残留僵尸注入）。
    steer_queue.clear(&session_id);
    Ok(turn_registry.abort(&session_id))
}

// ═══════════════════════════════════════════════════════
// 插话（steer）——生成中入队（设计定稿）
// ═══════════════════════════════════════════════════════

/// 生成中提交插话：入队待注入（不打断当前段）。
///
/// - 仅该 session 有活动 turn 时可入队；空闲返回明确拒绝（前端走普通发送）；
/// - 返回条目定位 id（cancel_steer / 前端 pending 气泡取消用）；
/// - 队列消费：dispatch_turn 的 Completed 出口（assistant 段完成、无工具调用）
///   每次 take 队首注入为轮内下一条 user 消息，自动继续生成（同轮 streaming
///   不断，turn/end 只在队列空时发一次）。
#[tauri::command]
pub fn steer_message(
    steer_queue: State<'_, Arc<SteerQueue>>,
    turn_registry: State<'_, Arc<TurnRegistry>>,
    session_id: String,
    text: String,
) -> Result<String, String> {
    if text.trim().is_empty() {
        return Err("插话内容不能为空".into());
    }
    if !turn_registry.is_active(&session_id) {
        return Err("当前没有处理中的对话，请等待处理结束后直接发送".into());
    }
    steer_queue
        .enqueue(&session_id, &text)
        .ok_or_else(|| "插话入队失败（内容为空）".into())
}

/// 取消一条待注入插话（pending 气泡点击取消）。
///
/// Ok(true) = 已移除；Ok(false) = 无此条目（已注入 / 已取消 / 会话无队列）——
/// 前端幂等处理（气泡消失即反馈，不回填输入）。
#[tauri::command]
pub fn cancel_steer(
    steer_queue: State<'_, Arc<SteerQueue>>,
    session_id: String,
    entry_id: String,
) -> Result<bool, String> {
    Ok(steer_queue.cancel(&session_id, &entry_id))
}

// ═══════════════════════════════════════════════════════
// 审批决策回传（M3-A §4.3/§5.2：确认卡 → 后端决策通道）
// ═══════════════════════════════════════════════════════

/// 前端确认卡按钮的决策载荷（invoke resolve_tool_approval）。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalResolutionRequest {
    pub session_id: String,
    pub call_id: String,
    /// "approved" | "rejected"（其余值命令层拒绝）
    pub decision: String,
}

/// 用户对一次工具审批的决策回传。
///
/// 返回 true = 决策已送达等待方（agent turn 继续）；false = 无此 pending
/// （已超时 / 已决 / 会话已结束）——前端应幂等处理（按事件终态收起卡片）。
#[tauri::command]
pub fn resolve_tool_approval(
    approval_registry: State<'_, Arc<crate::approval::ApprovalRegistry>>,
    request: ApprovalResolutionRequest,
) -> Result<bool, String> {
    let decision = match request.decision.as_str() {
        "approved" => crate::approval::UserDecision::Approved,
        "rejected" => crate::approval::UserDecision::Rejected,
        other => return Err(format!("decision 仅支持 approved/rejected，got: {other}")),
    };
    Ok(approval_registry.resolve(&request.session_id, &request.call_id, decision))
}

// ═══════════════════════════════════════════════════════
// 空间页 · 工作区查看（目录树 / 按名搜索 / 资源管理器）
// ═══════════════════════════════════════════════════════
//
// 定位：让用户能“舒服地查看当前的工作目录”。树按需逐层读取（点开哪级读哪级）；
// 搜索递归按名匹配并跳过隐藏项与依赖/构建目录。两侧口径有意不同：树照实列
// （＝真实目录，也与 list_dir 一致），搜索降噪（结果才有用）。

/// 树节点 DTO（list_workspace_dir 返回）。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEntry {
    pub name: String,
    pub is_dir: bool,
    /// 绝对路径（普通形式，非 canonical）
    pub path: String,
}

/// 搜索命中 DTO（search_workspace_files 返回）。
///
/// 为什么要带 `path` 与 `ancestorDirs`：前端不拼路径（与 FileLink 同口径）——
/// 命中后要在树里展开祖先并选中，展开链由后端给出，前端只做状态合并。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSearchHit {
    pub name: String,
    /// 绝对路径（点击打开 / 树选中定位）
    pub path: String,
    /// 相对工作区根，'/' 分隔（展示）
    pub relative_path: String,
    /// 相对根的父目录（根级命中 = 空串；展示 + 兜底）
    pub parent_subdir: String,
    /// 从根下一级到命中项所在目录的绝对路径链（展开用，根级命中为空）
    pub ancestor_dirs: Vec<String>,
    pub is_dir: bool,
}

/// 搜索跳过的目录名（隐藏项另按 '.' 前缀跳过）。
const WORKSPACE_SEARCH_SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "vendor",
    "__pycache__",
];

/// 搜索结果上限（照 Hanako 工作台的 WORKSPACE_SEARCH_LIMIT = 80）。
const WORKSPACE_SEARCH_LIMIT: usize = 80;
/// 搜索递归深度上限（根 = 0）。
const WORKSPACE_SEARCH_MAX_DEPTH: usize = 12;
/// 搜索扫描条目预算（防超大目录树把界面拖住）。
const WORKSPACE_SEARCH_SCAN_BUDGET: usize = 20_000;

/// 解析工作区内的目标路径。
///
/// `requested` 为空 → 根自身；非空必须是绝对路径，且（canonical 化后）落在
/// 工作区根内——越界一律拒绝。返回值保持传入口径（非 canonical），供前端回用。
fn resolve_workspace_target(
    root: &std::path::Path,
    requested: Option<&str>,
) -> Result<std::path::PathBuf, String> {
    let target = match requested.map(str::trim).filter(|p| !p.is_empty()) {
        None => root.to_path_buf(),
        Some(p) => {
            let pb = std::path::PathBuf::from(p);
            if !pb.is_absolute() {
                return Err(format!("需要绝对路径：{p}"));
            }
            pb
        }
    };
    let canon_root = root
        .canonicalize()
        .map_err(|e| format!("工作区根不可访问：{e}"))?;
    let canon_target = target
        .canonicalize()
        .map_err(|e| format!("路径不可访问：{e}"))?;
    if !canon_target.starts_with(&canon_root) {
        return Err("路径不在工作区内".to_string());
    }
    Ok(target)
}

/// 列一层目录：目录在前，各自按名排序（大小写不敏感，末位比原串保确定）。
fn list_dir_entries(dir: &std::path::Path) -> Result<Vec<WorkspaceEntry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("读取目录失败：{e}"))?;
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        out.push(WorkspaceEntry {
            name,
            is_dir,
            path: entry.path().to_string_lossy().into_owned(),
        });
    }
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(out)
}

/// 递归按名搜索（大小写不敏感子串）。
///
/// 跳过点开头项与依赖/构建目录；深度 ≤ MAX_DEPTH；扫描条目 ≤ BUDGET；
/// 命中数达上限即停。结果按相对路径排序（确定序）。
fn search_workspace_entries(root: &std::path::Path, query: &str) -> Vec<WorkspaceSearchHit> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<WorkspaceSearchHit> = Vec::new();
    let mut budget = WORKSPACE_SEARCH_SCAN_BUDGET;
    let mut stack: Vec<(std::path::PathBuf, String, Vec<String>, usize)> =
        vec![(root.to_path_buf(), String::new(), Vec::new(), 0)];
    'outer: while let Some((dir, rel, chain, depth)) = stack.pop() {
        if depth > WORKSPACE_SEARCH_MAX_DEPTH {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut subdirs: Vec<(std::path::PathBuf, String, Vec<String>, usize)> = Vec::new();
        for entry in rd.flatten() {
            if budget == 0 {
                break 'outer;
            }
            budget -= 1;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let child_rel = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let child_abs = entry.path().to_string_lossy().into_owned();
            if name.to_lowercase().contains(&needle) {
                hits.push(WorkspaceSearchHit {
                    name: name.clone(),
                    path: child_abs.clone(),
                    relative_path: child_rel.clone(),
                    parent_subdir: rel.clone(),
                    ancestor_dirs: chain.clone(),
                    is_dir,
                });
                if hits.len() >= WORKSPACE_SEARCH_LIMIT {
                    break 'outer;
                }
            }
            if is_dir && !WORKSPACE_SEARCH_SKIP_DIRS.contains(&name.to_lowercase().as_str()) {
                let mut child_chain = chain.clone();
                child_chain.push(child_abs);
                subdirs.push((entry.path(), child_rel, child_chain, depth + 1));
            }
        }
        for item in subdirs.into_iter().rev() {
            stack.push(item);
        }
    }
    hits.sort_by(|a, b| {
        a.relative_path
            .to_lowercase()
            .cmp(&b.relative_path.to_lowercase())
            .then_with(|| a.relative_path.cmp(&b.relative_path))
    });
    hits
}

/// 列出工作区内一层目录（path 省略 = 根）。
#[tauri::command]
pub fn list_workspace_dir(
    store: State<'_, Arc<Mutex<SettingsStore>>>,
    path: Option<String>,
) -> Result<Vec<WorkspaceEntry>, String> {
    let root = {
        let st = store.lock().map_err(|e| format!("设置锁获取失败：{e}"))?;
        st.workspace_root()
    };
    let dir = resolve_workspace_target(&root, path.as_deref())?;
    list_dir_entries(&dir)
}

/// 在工作区内按名字递归搜索（跳过隐藏项与依赖/构建目录）。
#[tauri::command]
pub fn search_workspace_files(
    store: State<'_, Arc<Mutex<SettingsStore>>>,
    query: String,
) -> Result<Vec<WorkspaceSearchHit>, String> {
    let root = {
        let st = store.lock().map_err(|e| format!("设置锁获取失败：{e}"))?;
        st.workspace_root()
    };
    Ok(search_workspace_entries(&root, &query))
}

/// 用系统文件管理器打开工作区根。
///
/// 目录不能走 open_path（那里只放行文件），故单开一条命令；目标固定为工作区根、
/// 不经前端传参，边界最清楚。
#[tauri::command]
pub fn open_workspace_in_explorer(
    store: State<'_, Arc<Mutex<SettingsStore>>>,
) -> Result<(), String> {
    let root = {
        let st = store.lock().map_err(|e| format!("设置锁获取失败：{e}"))?;
        st.workspace_root()
    };
    let dir = resolve_workspace_target(&root, None)?;
    #[cfg(target_os = "windows")]
    let spawned = std::process::Command::new("explorer").arg(&dir).spawn();
    #[cfg(target_os = "macos")]
    let spawned = std::process::Command::new("open").arg(&dir).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let spawned = std::process::Command::new("xdg-open").arg(&dir).spawn();
    spawned.map_err(|e| format!("打开文件管理器失败：{e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ApiType;

    #[test]
    fn validate_longterm_bounds() {
        let max = crate::orchestrator::memory::MAX_LONGTERM_CHARS;
        assert!(validate_longterm("").is_ok(), "空串允许（＝清空）");
        assert!(validate_longterm(&"字".repeat(max)).is_ok(), "上限内应放行");
        let err = validate_longterm(&"字".repeat(max + 1)).unwrap_err();
        assert!(err.contains("上限"), "超限应报错而不静默截断: {err}");
    }

    // ── list_skills：dev 目录真实条目（数值计算/行动推演已落盘）──

    #[test]
    fn list_skills_returns_dev_entries_with_description() {
        let skills = list_skills().expect("dev 技能目录应可读");
        assert!(!skills.is_empty());
        // dev 目录强断言：数值计算.md 存在（skills_index 测试同款先例）
        let calc = skills
            .iter()
            .find(|s| s.name == "数值计算")
            .expect("应含数值计算技能");
        assert!(!calc.description.is_empty(), "frontmatter description 应被解析");
        // file_path：绝对路径且在 skills_dir 内（打开链接用；文件名≠name）
        let fp = calc.file_path.as_ref().expect("应记录源文件路径");
        let p = std::path::Path::new(fp);
        assert!(p.is_absolute() && p.starts_with(crate::skills_index::skills_dir()));
        assert!(p.extension().and_then(|e| e.to_str()) == Some("md"));
        // 按 name 排序（SkillIndex.names 语义）
        let mut names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "清单应按 name 排序");
    }

    // ── open_path 放行校验（纯函数；不触发真实 opener）──

    #[test]
    fn skill_entry_serializes_camel_case_file_path() {
        // 锁定 wire 契约：SkillEntry `#[serde(rename_all = "camelCase")]` →
        // 输出 filePath；前端 types.ts 按 filePath 读取（防字段契约断裂回归——
        // 曾因前端读 file_path 恒 undefined 致点击静默失效）
        let entry = SkillEntry {
            name: "数值计算".into(),
            description: "d".into(),
            file_path: Some("F:/skills/数值计算.md".into()),
        };
        let v = serde_json::to_value(&entry).unwrap();
        assert!(
            v.get("filePath").is_some(),
            "应序列化为 camelCase filePath，got: {v}"
        );
        assert_eq!(
            v.get("filePath").and_then(|x| x.as_str()),
            Some("F:/skills/数值计算.md")
        );
        assert!(v.get("file_path").is_none(), "不应输出 snake_case file_path");
        assert!(v.get("name").is_some());
        assert!(v.get("description").is_some());
    }

    #[test]
    fn open_target_accepts_existing_file_rejects_others() {
        // 绝对路径 + 存在（任意位置，白名单已放开）→ 放行
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let f = std::env::temp_dir().join(format!("prts-open-{nanos}.md"));
        std::fs::write(&f, "x").unwrap();
        assert!(is_openable_target(&f));
        // 技能目录内真实文件 → 同样放行（不再受白名单限制）
        let skill_md = crate::skills_index::skills_dir().join("数值计算.md");
        assert!(is_openable_target(&skill_md));
        // 相对路径 → 拒绝
        assert!(!is_openable_target(std::path::Path::new("数值计算.md")));
        // 不存在的绝对路径 → 拒绝
        assert!(!is_openable_target(std::path::Path::new(
            "C:/definitely/not/exist-prts-xyz.md"
        )));
        // 目录（非文件）→ 拒绝
        assert!(!is_openable_target(&std::env::temp_dir()));
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn agent_request_deserializes_camelcase() {
        let json = r#"{
            "input": "你好",
            "history": [
                { "role": "user", "content": "之前的问题" },
                { "role": "assistant", "content": "之前的回答" }
            ],
            "sessionId": "abc-123"
        }"#;

        let req: AgentRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.input, "你好");
        assert_eq!(req.session_id, "abc-123");
        assert_eq!(req.history.len(), 2);
        assert_eq!(req.history[0].role, "user");
        assert_eq!(req.history[1].role, "assistant");
    }

    #[test]
    fn agent_request_deserializes_with_error_messages() {
        // error 消息不做过滤——那是 orchestrator 的职责
        let json = r#"{
            "input": "重试",
            "history": [
                { "role": "user", "content": "你好" },
                { "role": "error", "content": "超时了" },
                { "role": "user", "content": "重试" }
            ],
            "sessionId": "xyz-456"
        }"#;

        let req: AgentRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.history.len(), 3);
        assert_eq!(req.history[1].role, "error");
    }

    #[test]
    fn agent_request_empty_history() {
        let json = r#"{
            "input": "第一句话",
            "history": [],
            "sessionId": "new-001"
        }"#;

        let req: AgentRequest = serde_json::from_str(json).unwrap();
        assert!(req.history.is_empty());
        assert_eq!(req.input, "第一句话");
    }

    #[test]
    fn agent_request_with_conversation_id() {
        // 新前端：携带 conversationId（缓存区按对话分区的维度）
        let json = r#"{
            "input": "你好",
            "history": [],
            "sessionId": "abc-123",
            "conversationId": "conv-7"
        }"#;

        let req: AgentRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.conversation_id, "conv-7");
        assert_eq!(req.session_id, "abc-123");
    }

    #[test]
    fn agent_request_without_conversation_id_defaults_empty() {
        // 旧前端：不传 conversationId → serde default 空串（命令层兜底为 sessionId）
        let json = r#"{
            "input": "你好",
            "history": [],
            "sessionId": "abc-123"
        }"#;

        let req: AgentRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.conversation_id, "");
    }

    // ── verify_connection 空 base_url ──

    #[tokio::test]
    async fn verify_connection_empty_base_url_rejected() {
        // provider 为空 + base_url 为 None → 无映射可用，应返回错误
        let config = LlmConfig {
            provider: String::new(),
            api_type: ApiType::OpenAiCompletions,
            api_key: String::new(),
            base_url: None,
            headers: None,
            model: String::new(),
            max_tokens: 4096,
            thinking_level: None,
        };
        let result = verify_connection(config).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Base URL"));
    }

    // ── M3-B 设置写闸 ──

    #[test]
    fn config_write_guard_allows_when_idle_blocks_when_active() {
        let reg = TurnRegistry::new();
        // 空闲 → Ok
        assert!(guard_config_write(&reg).is_ok());

        // 有活动 turn → Err（文案为命令层同款中文）
        let signal = reg.register("s1", "conv-1");
        assert_eq!(
            guard_config_write(&reg).unwrap_err(),
            CONFIG_WRITE_GUARD_MSG
        );

        // abort 不释放 busy：快照仍在，直到 process 返回后 unregister
        assert!(reg.abort("s1"));
        assert!(guard_config_write(&reg).is_err(), "中止后仍 busy（轮未收尾）");

        reg.unregister("s1");
        assert!(guard_config_write(&reg).is_ok(), "注销（轮收尾）后放行");
        let _ = signal;
    }

    #[test]
    fn config_write_guard_is_session_scope_safe() {
        // busy 按全局口径（任一对话处理中即拦截）——多会话注册任一即拦
        let reg = TurnRegistry::new();
        reg.register("s1", "c1");
        assert!(guard_config_write(&reg).is_err());
        reg.unregister("s1");
        assert!(guard_config_write(&reg).is_ok());
    }

    // ── 空间页 · 工作区查看（树 / 搜索） ──

    fn space_root(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("prts-space-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn list_dir_entries_dirs_first_then_name() {
        let root = space_root("list");
        std::fs::create_dir_all(root.join("zeta")).unwrap();
        std::fs::create_dir_all(root.join("Alpha")).unwrap();
        std::fs::write(root.join("b.txt"), "x").unwrap();
        std::fs::write(root.join("a.txt"), "x").unwrap();

        let entries = list_dir_entries(&root).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "zeta", "a.txt", "b.txt"], "目录在前，各自按名");
        assert!(entries[0].is_dir, "目录标记");
        assert!(!entries[2].is_dir, "文件标记");
        // 路径为拼接形式（非 canonical，不带 \\?\ 前缀）——直接回传给前端用
        assert!(entries[0].path.ends_with("Alpha"));
        assert!(!entries[0].path.contains("\\\\?\\"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_workspace_target_guards_escape() {
        let root = space_root("guard");
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        assert!(
            resolve_workspace_target(&root, Some(&sub.to_string_lossy())).is_ok(),
            "根内子目录放行"
        );
        assert_eq!(resolve_workspace_target(&root, None).unwrap(), root, "省略 = 根自身");
        assert_eq!(
            resolve_workspace_target(&root, Some("  ")).unwrap(),
            root,
            "空白项等同省略"
        );
        assert!(
            resolve_workspace_target(&root, Some("sub")).is_err(),
            "相对路径拒绝"
        );
        let outside = std::env::temp_dir();
        assert!(
            resolve_workspace_target(&root, Some(&outside.to_string_lossy())).is_err(),
            "根外目录拒绝"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn search_skips_hidden_and_dependency_dirs() {
        let root = space_root("search");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("notes/崖.md"), "x").unwrap();
        std::fs::create_dir_all(root.join("node_modules/hit")).unwrap();
        std::fs::write(root.join("node_modules/hit/崖.js"), "x").unwrap();
        std::fs::create_dir_all(root.join(".git/hit")).unwrap();
        std::fs::write(root.join(".git/hit/崖"), "x").unwrap();
        std::fs::write(root.join(".hidden崖.md"), "x").unwrap();

        let hits = search_workspace_entries(&root, "崖");
        let paths: Vec<&str> = hits.iter().map(|h| h.relative_path.as_str()).collect();
        assert_eq!(paths, vec!["notes/崖.md"], "命中只来自可见目录");

        // 嵌套命中的展开链：根下一级 → 命中所在目录（绝对路径，前端不拼路径）
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        std::fs::write(root.join("a/b/c/崖深处.md"), "x").unwrap();
        let deep = search_workspace_entries(&root, "崖深处");
        assert_eq!(deep.len(), 1);
        assert_eq!(deep[0].relative_path, "a/b/c/崖深处.md");
        assert_eq!(deep[0].parent_subdir, "a/b/c");
        assert_eq!(deep[0].ancestor_dirs.len(), 3, "a → b → c 三层展开链");
        assert!(deep[0].ancestor_dirs[0].ends_with("a"));
        assert!(deep[0].ancestor_dirs[2].ends_with("c"));

        assert!(search_workspace_entries(&root, "  ").is_empty(), "空查询不搜");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn search_caps_results_and_matches_case_insensitively() {
        let root = space_root("cap");
        std::fs::create_dir_all(root.join("d")).unwrap();
        for i in 0..100 {
            std::fs::write(root.join(format!("d/Note{i}.md")), "x").unwrap();
        }

        let hits = search_workspace_entries(&root, "note");
        assert_eq!(hits.len(), WORKSPACE_SEARCH_LIMIT, "结果上限 80");
        assert_eq!(hits[0].parent_subdir, "d", "父目录相对根");
        assert!(!hits[0].is_dir);
        assert!(
            hits[0].path.ends_with("d") || hits[0].path.contains("d"),
            "命中带绝对路径（供树定位）"
        );
        assert_eq!(hits[0].ancestor_dirs.len(), 1, "展开链：命中所在目录");

        let dir_hits = search_workspace_entries(&root, "d");
        let root_dir_hit = dir_hits
            .iter()
            .find(|h| h.is_dir && h.relative_path == "d")
            .expect("根级目录命中");
        assert!(root_dir_hit.parent_subdir.is_empty(), "根级 parentSubdir 为空串");
        assert!(root_dir_hit.ancestor_dirs.is_empty(), "根级命中无祖先目录");
        let _ = std::fs::remove_dir_all(&root);
    }
}
