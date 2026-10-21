//! Tauri IPC 薄层 —— 前端 ←→ 后端命令适配
//! 各命令按职责拆到子模块；本文件只保留共享导入与 re-export。

mod agent;
mod conversation;
mod prelude;
mod settings;
mod system;

pub use agent::*;
pub use conversation::*;
pub use settings::*;
pub use system::*;

#[cfg(test)]
use settings::{guard_config_write, CONFIG_WRITE_GUARD_MSG};

use prelude::*;


// ═══════════════════════════════════════════════════════
// 附件上传（通用通道）
// ═══════════════════════════════════════════════════════

/// 上传本地文件为会话附件（快照复制到会话落点）。
///
/// 纯搬运：不做内容解析/识别。`conversationId` 为空 → 落临时 uploads/。
/// 逐项校验（绝对路径 / 非符号链接 / 是文件 / 体积上限）；任一不通过即整体报错。
#[tauri::command]
pub fn upload_attachment(
    paths: Vec<String>,
    conversation_id: Option<String>,
) -> Result<Vec<crate::attachments::Attachment>, String> {
    use crate::attachments as att;
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    if paths.len() > att::MAX_FILES {
        return Err(format!("一次最多上传 {} 个文件", att::MAX_FILES));
    }
    let dir = att::session_files_dir(&conversation_id.unwrap_or_default());
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建附件目录失败: {e}"))?;

    let mut out = Vec::with_capacity(paths.len());
    for raw in &paths {
        let src = std::path::Path::new(raw);
        if !src.is_absolute() {
            return Err(format!("附件路径必须是绝对路径：{raw}"));
        }
        let meta = std::fs::symlink_metadata(src)
            .map_err(|e| format!("附件不存在或不可读：{raw}（{e}）"))?;
        if meta.file_type().is_symlink() {
            return Err(format!("不支持符号链接：{raw}"));
        }
        if !meta.is_file() {
            return Err(format!("仅支持文件（暂不支持目录）：{raw}"));
        }
        if meta.len() > att::MAX_FILE_BYTES {
            return Err(format!(
                "文件超过体积上限（{} MB）：{raw}",
                att::MAX_FILE_BYTES / 1024 / 1024
            ));
        }
        out.push(att::copy_into(src, &dir)?);
    }
    Ok(out)
}

/// 上传 base64 blob（粘贴图片等无路径来源）。仅接受白名单图片 MIME。
#[tauri::command]
pub fn upload_attachment_blob(
    name: String,
    base64_data: String,
    mime_type: String,
    conversation_id: Option<String>,
) -> Result<crate::attachments::Attachment, String> {
    use crate::attachments as att;
    if !att::is_allowed_image_mime(&mime_type) {
        return Err(format!("不支持的图片类型：{mime_type}"));
    }
    // 粗估解码后体积（base64 4 字符 ≈ 3 字节），超限直接拒。
    if (base64_data.len() as u64) / 4 * 3 > att::MAX_FILE_BYTES {
        return Err(format!(
            "图片超过体积上限（{} MB）",
            att::MAX_FILE_BYTES / 1024 / 1024
        ));
    }
    let dir = att::session_files_dir(&conversation_id.unwrap_or_default());
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建附件目录失败: {e}"))?;
    att::write_blob_into(&dir, &name, &base64_data, &mime_type)
}

/// 技能清单条目（空间页只读展示）：来源 skills 目录 md 的 frontmatter。
/// file_path = 源 md 绝对路径（None = 索引未记录，异常态）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    pub file_path: Option<String>,
    /// 来源分层（frontmatter source 原样透出；缺省 = 用户）
    pub source: Option<String>,
}

/// 技能清单（空间页技能节只读展示）：name + description，按 name 排序。
/// 读技能运行根（data_dir()/skills，内置经启动同步进入）。
/// 加载失败（目录不可读）→ 描述性 Err。只读命令不设 busy 闸。
#[tauri::command]
pub fn list_skills() -> Result<Vec<SkillEntry>, String> {
    list_skills_from(&crate::skills_index::skills_dir())
}

/// list_skills 实现体（显式目录参数：命令层传运行根，测试传夹具）。
fn list_skills_from(dir: &std::path::Path) -> Result<Vec<SkillEntry>, String> {
    let idx = crate::skills_index::SkillIndex::load(dir)
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
            source: idx.resolve(name).and_then(|m| m.source.clone()),
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
/// 读取随包 LICENSE 全文（关于页“许可证”块展开时调用）。
///
/// 薄命令：按解析出的路径读文本，不改写、不缓存。正文永远来自随包文件——
/// 不在前端硬编码（Hanako 的“许可证全文”其实是硬编码的短声明，此处避开）。
#[tauri::command]
pub fn read_license() -> Result<String, String> {
    let path = crate::config::license_path();
    std::fs::read_to_string(&path)
        .map_err(|e| format!("读取许可证文件失败（{}）：{e}", path.display()))
}

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
/// 前端 sendMessage 时调用：新对话（游标为空）取**值维度**快照（`kind == "value"`，
/// 由 `DimKey::kind()` 分类、前端按标记筛）；进行中对话取增量（stamp/seq > 游标的
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

// ══════════════════════════════════════════════════════
// 技能安装 / 移除（批 3 · 设置页扩展）
// ══════════════════════════════════════════════════════

/// 打开系统文件选择器选一个技能文件（.md）。
///
/// 实现形态同 `choose_workspace_directory`：后端命令 + dialog Rust API，
/// 非阻塞 pick_file 内部经 run_on_main_thread 弹窗，oneshot 桥回 async 命令；
/// 用户取消 → `Ok(None)`。安装动作独立成 `install_skill`（纯路径入参、可测）。
#[tauri::command]
pub async fn pick_skill_file(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    app.dialog()
        .file()
        .set_title("选择技能文件（.md）")
        .add_filter("Markdown", &["md"])
        .pick_file(move |picked| {
            let value = picked.and_then(|fp| {
                fp.as_path()
                    .map(|p| p.to_string_lossy().into_owned())
            });
            let _ = tx.send(value);
        });
    rx.await.map_err(|e| format!("文件选择失败: {e}"))
}

/// 打开系统目录选择器选一个技能文件夹（根级需含 SKILL.md）。
#[tauri::command]
pub async fn pick_skill_folder(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    app.dialog()
        .file()
        .set_title("选择技能文件夹（含 SKILL.md）")
        .pick_folder(move |picked| {
            let value = picked.and_then(|fp| {
                fp.as_path()
                    .map(|p| p.to_string_lossy().into_owned())
            });
            let _ = tx.send(value);
        });
    rx.await.map_err(|e| format!("文件夹选择失败: {e}"))
}

/// 安装技能（单 .md 文件 / 技能包文件夹，自动识别；参照 Hanako 安装器移植）。
///
/// 校验与拷贝在 `skills_install` 模块（纯路径入参，可测）；成功后重算技能
/// 目录 diff 并入提醒台账——下一条消息经游标机制送达模型（目录冻结视图
/// 不变，变化走提醒通道，批 2 定案）。
#[tauri::command]
pub fn install_skill(
    source_path: String,
    ledger: State<'_, Arc<Mutex<Notices>>>,
) -> Result<SkillEntry, String> {
    let source = std::path::PathBuf::from(&source_path);
    let root = crate::skills_index::skills_dir();
    let installed = crate::skills_install::install_skill(&source, &root)?;
    diff_skills_now(&root, &ledger);
    Ok(SkillEntry {
        name: installed.name,
        description: installed.description,
        file_path: Some(installed.file_path.to_string_lossy().into_owned()),
        source: installed.source,
    })
}

/// 移除用户技能（内置拒绝；文件夹包整目录删除；确认在前端行内两步完成）。
#[tauri::command]
pub fn remove_skill(
    file_path: String,
    ledger: State<'_, Arc<Mutex<Notices>>>,
) -> Result<String, String> {
    let path = std::path::PathBuf::from(&file_path);
    let root = crate::skills_index::skills_dir();
    let name = crate::skills_install::remove_skill(&path, &root)?;
    diff_skills_now(&root, &ledger);
    Ok(name)
}

/// 重算技能目录 diff 并记账（best-effort：记账失败仅终端留痕，不改写安装 /
/// 移除的结果——动作本身已成功，提醒属增量信息）。
fn diff_skills_now(root: &std::path::Path, ledger: &Arc<Mutex<Notices>>) {
    let pairs = crate::skills_index::SkillIndex::load(root)
        .map(|idx| idx.catalog_pairs())
        .unwrap_or_default();
    match ledger.lock() {
        Ok(mut l) => l.diff_skill_catalog(&pairs),
        Err(e) => eprintln!("[commands] 技能目录提醒记账失败：{e}"),
    }
}

/// 环境维度 DTO（`value` 为空串的维度不下发）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderDim {
    pub key: String,
    pub title: String,
    /// 类别：`"value"`（值维度，入首轮快照）／`"catalog"`（目录维度，不入）。
    pub kind: String,
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
            .filter(|(_, d)| !d.value.is_empty())
            .map(|(k, d)| ReminderDim {
                key: k.key().to_string(),
                title: k.title().to_string(),
                kind: k.kind().as_str().to_string(),
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
    /// 本次摘要 LLM 调用用量（前端随压缩记录一并提交）；降级产出时 None。
    pub usage: Option<UsageStat>,
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
/// - 失败语义：Err(String) 文案面向用户直显；失败不改变任何状态，幂等可重试；
/// - 请求形态（2026-09-25 · DSH 形态）：system 与主对话同源（含冻结目录与
///   记忆区）、工具面同主请求、指令置折叠区末尾——请求成为主请求的真前缀，
///   复用 KV cache。
/// - 成功语义（批 4）：以活体重刷该对话两份冻结快照（有差才写；best-effort）——
///   后续轮次的 system 基线切到最新。
#[tauri::command]
pub async fn compact_conversation(
    request: CompactRequest,
    llm: State<'_, Arc<ClientRegistry>>,
    config: State<'_, Arc<Mutex<LlmConfig>>>,
    model_registry: State<'_, Arc<ModelRegistry>>,
    turn_registry: State<'_, Arc<TurnRegistry>>,
    memory_store: State<'_, Arc<Mutex<MemoryStore>>>,
    snapshot_store: State<'_, Arc<Mutex<SnapshotStore>>>,
    skills_snapshot_store: State<'_, Arc<Mutex<SkillSnapshotStore>>>,
    prefix_snapshot_store: State<'_, Arc<Mutex<PrefixSnapshotStore>>>,
    tools: State<'_, Arc<ToolRegistry>>,
) -> Result<CompactionOutcome, String> {
    if request.folded_history.is_empty() {
        return Err("没有可压缩的内容".into());
    }
    if turn_registry.active_count() > 0 {
        return Err("正在处理对话，请稍后再压缩".into());
    }
    // ── 请求前缀同源（2026-09-25 裁决 · DSH 形态）：system（含冻结目录与
    // 记忆区）与工具面同主对话 → 摘要请求成为主请求真前缀（KV cache 复用）。──
    let conversation_key = if request.conversation_id.is_empty() {
        None
    } else {
        Some(request.conversation_id.as_str())
    };
    let memory_block = {
        let mem = memory_store
            .lock()
            .map_err(|e| format!("记忆存储锁获取失败: {e}"))?;
        let mut snaps = snapshot_store
            .lock()
            .map_err(|e| format!("快照存储锁获取失败: {e}"))?;
        snaps.get_or_assemble(conversation_key, &mem)
    };
    // 技能目录冻结快照（批 2）：压缩读旧快照——请求须复用主请求真前缀。
    let skill_catalog = {
        let live = crate::skills_index::SkillIndex::load(&crate::skills_index::skills_dir())
            .map(|idx| idx.catalog_text())
            .map_err(|e| eprintln!("[commands] 技能目录加载失败（本次不冻结）：{e}"))
            .ok();
        let mut snaps = skills_snapshot_store
            .lock()
            .map_err(|e| format!("技能快照存储锁获取失败: {e}"))?;
        snaps.get_or_freeze(conversation_key, live.as_deref())
    };
    // 完整前缀冻结快照（批 5）：压缩请求须与主请求前缀逐字节一致。
    let (system_prompt, tool_defs) = {
        let live_system = crate::orchestrator::context::assemble_system_prompt_with_memory(
            &skill_catalog,
            &memory_block,
        );
        let live_tools = tools.list_defs();
        let mut snaps = prefix_snapshot_store
            .lock()
            .map_err(|e| format!("前缀快照存储锁获取失败: {e}"))?;
        let snap = snaps.get_or_freeze(conversation_key, &live_system, &live_tools);
        (snap.system, snap.tool_defs)
    };
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
        &system_prompt,
        &tool_defs,
        &abort,
    )
    .await;
    turn_registry.unregister(&request.session_id);
    result.map(|o| {
        // 批 4 · 压缩并更新：摘要成功（含降级规则产出——上下文同样被替换）→
        // 以活体重刷两份冻结快照（有差才写；best-effort，失败不改变压缩结果）。
        let live = crate::skills_index::SkillIndex::load(&crate::skills_index::skills_dir())
            .map(|idx| idx.catalog_text())
            .map_err(|e| eprintln!("[commands] 技能目录加载失败（压缩后不刷新技能快照）：{e}"))
            .ok();
        let live_tools = tools.list_defs();
        refresh_frozen_snapshots(
            &request.conversation_id,
            live.as_deref(),
            &live_tools,
            &memory_store,
            &snapshot_store,
            &skills_snapshot_store,
            &prefix_snapshot_store,
        );
        CompactionOutcome {
            summary: o.summary,
            degraded: o.degraded,
            tokens_before: o.tokens_before,
            usage: o.usage.map(|u| usage_stat(&u, cfg.api_type)),
        }
    })
}

/// 压缩成功后刷新三份冻结快照（批 4 / 批 5 · 压缩并更新）。
///
/// 顺序有依赖：记忆区与技能目录先各自刷新，再用刷新后的两者重组装活体前缀，
/// 刷新完整前缀快照（system 全文 + 工具面）。三者均有差才写。本函数
/// best-effort：锁失败仅终端留痕，不改变压缩结果（调用方照常返回成功）。
fn refresh_frozen_snapshots(
    conversation_id: &str,
    live_catalog: Option<&str>,
    live_tools: &[crate::llm::ToolDef],
    memory_store: &Arc<Mutex<MemoryStore>>,
    snapshot_store: &Arc<Mutex<SnapshotStore>>,
    skills_snapshot_store: &Arc<Mutex<SkillSnapshotStore>>,
    prefix_snapshot_store: &Arc<Mutex<PrefixSnapshotStore>>,
) {
    if let Ok(mem) = memory_store.lock() {
        match snapshot_store.lock() {
            Ok(mut snaps) => {
                snaps.refresh(conversation_id, &mem);
            }
            Err(e) => eprintln!("[commands] 记忆快照刷新跳过（快照锁失败）：{e}"),
        }
    } else {
        eprintln!("[commands] 记忆快照刷新跳过（记忆锁失败）");
    }
    match skills_snapshot_store.lock() {
        Ok(mut snaps) => {
            snaps.refresh(conversation_id, live_catalog);
        }
        Err(e) => eprintln!("[commands] 技能快照刷新跳过（锁失败）：{e}"),
    }
    // 完整前缀：目录不可读（None）时保持旧前缀不刷（无法确认新字节）。
    let Some(catalog) = live_catalog else {
        return;
    };
    let live_system = match (memory_store.lock(), snapshot_store.lock()) {
        (Ok(mem), Ok(mut snaps)) => {
            let block = snaps.get_or_assemble(Some(conversation_id), &mem);
            crate::orchestrator::context::assemble_system_prompt_with_memory(catalog, &block)
        }
        _ => {
            eprintln!("[commands] 前缀快照刷新跳过（记忆/记忆快照锁失败）");
            return;
        }
    };
    match prefix_snapshot_store.lock() {
        Ok(mut snaps) => {
            snaps.refresh(conversation_id, &live_system, live_tools);
        }
        Err(e) => eprintln!("[commands] 前缀快照刷新跳过（锁失败）：{e}"),
    }
}

/// 删除会话：先删持久文件（jsonl + index 条目），再清冻结快照 sidecar
/// （记忆区 + 技能目录 + 完整前缀三份，同生共死）。
#[tauri::command]
pub fn delete_conversation(
    store: State<'_, Arc<Mutex<SessionStore>>>,
    snapshot_store: State<'_, Arc<Mutex<SnapshotStore>>>,
    skills_snapshot_store: State<'_, Arc<Mutex<SkillSnapshotStore>>>,
    prefix_snapshot_store: State<'_, Arc<Mutex<PrefixSnapshotStore>>>,
    conversation_id: String,
) -> Result<(), String> {
    // 先删会话（失败上抛，不静默）；再清 sidecar（冻结快照同生共死；
    // 裁决 1：先写快照后归档 → 孤儿在删除侧清扫）。
    {
        let mut s = store.lock().map_err(|e| format!("会话存储锁获取失败: {e}"))?;
        s.delete(&conversation_id)?;
    }
    {
        let mut snaps = snapshot_store
            .lock()
            .map_err(|e| format!("快照存储锁获取失败: {e}"))?;
        snaps.remove(&conversation_id);
    }
    let mut snaps = skills_snapshot_store
        .lock()
        .map_err(|e| format!("技能快照存储锁获取失败: {e}"))?;
    snaps.remove(&conversation_id);
    let mut snaps = prefix_snapshot_store
        .lock()
        .map_err(|e| format!("前缀快照存储锁获取失败: {e}"))?;
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

/// 推送记忆用量事件（前端镜像进投影；后端已累加，前端不回写）。
fn emit_memory_usage(app_handle: &AppHandle, conversation_id: &str, usage: UsageStat) {
    let event = IpcEvent::Agent(AgentEvent::memory_usage(conversation_id, usage));
    if let Err(e) = app_handle.emit("agent-event", event) {
        eprintln!("[commands] 记忆用量事件推送失败: {e}");
    }
}

/// 触发经历管线（fire-and-forget）：对指定对话"水位线之后的增量"做提炼。
///
/// 前端在轮末 / 切换 / 删除 / 启动补偿时调用；后端自判阈值
/// （增量 < [`orchestrator::memory::INCREMENTAL_TURNS`] → no-op）。后台 turn：
/// 注册 TurnRegistry（可 abort）但**不计 busy**（不拦用户操作）。失败/整轮报废
/// → 记录并静默返回 Ok（前端不 await）。
#[tauri::command]
pub async fn finalize_memory(
    app_handle: AppHandle,
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
    match result {
        Ok(o) => {
            if let Some(u) = o.usage {
                let stat = usage_stat(&u, llm_config.api_type);
                if let Ok(mut s) = store.lock() {
                    if let Err(e) =
                        s.accumulate_usage(&conversation_id, UsageCategory::Memory, &stat)
                    {
                        eprintln!("[commands] 记忆用量累加跳过（{conversation_id}）：{e}");
                    }
                }
                emit_memory_usage(&app_handle, &conversation_id, stat);
            }
        }
        Err(e) => eprintln!("[commands] 记忆管线整轮报废（{conversation_id}）：{e}"),
    }
    Ok(())
}

/// 启动补偿：返回需要补提炼的对话 id（水位落后于总轮数）。
///
/// 最近更新优先、最多 limit 条（缺省 [`MEMORY_CATCH_UP_LIMIT`]）；前端逐个
/// invoke finalize_memory。仅扫描 updatedAt 前 [`MEMORY_CATCH_UP_SCAN`] 条，
/// 控制启动开销。
///
/// 异步执行（线程池）：扫描不占窗口线程；轮数用轻量计数
/// （[`SessionStore::count_turns`]——数行不解析）。
#[tauri::command(async)]
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
        let total = match s.count_turns(&m.id) {
            Ok(n) => n as u64,
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

/// 记忆库展示排序：批次（`ts`）新在前；同批次内按创建序**倒序**（批内新的在上），
/// 使批内方向与批间一致（此前批内保持文件创建升序，导致最新批的顶部反而是最旧条目）。
/// 只服务展示，不改存储顺序（文件仍是创建序）。
fn experience_list_rows(
    entries: &[crate::memory::schema::ExperienceEntry],
) -> Vec<ExperienceItemDto> {
    let mut rows: Vec<(u64, usize, ExperienceItemDto)> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.ts, i, experience_item(e)))
        .collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    rows.into_iter().map(|(_, _, dto)| dto).collect()
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
    Ok(experience_list_rows(m.experiences()))
}

/// 清除经历记忆：清经历本体＋长期梗概；水位抬平到各对话当前总轮数
/// （防旧轮次被补提炼挖回）；不动冻结文件。
///
/// 异步执行（线程池）＋轻量计数（[`SessionStore::count_turns`]）：
/// 抬水位不再为一声轮数整读档案。
#[tauri::command(async)]
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
            let total = match s.count_turns(&meta.id) {
                Ok(n) => n as u64,
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
    app_handle: AppHandle,
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
    if let Ok(o) = &result {
        if let Some(u) = o.usage {
            let stat = usage_stat(&u, llm_config.api_type);
            if let Ok(mut s) = store.lock() {
                let _ = s.accumulate_usage(&conversation_id, UsageCategory::Memory, &stat);
            }
            emit_memory_usage(&app_handle, &conversation_id, stat);
        }
    }
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
    turn_registry: State<'_, Arc<TurnRegistry>>,
    session_id: String,
) -> Result<bool, String> {
    // 中止语义（2026-09-25 修订）：中止 = 立断当前段；插话队列不再清空——
    // 未交付条目由 agent_process 收尾置保留态，同对话下一轮领取交付
    // （中止不丢字；前端 pending 气泡留存、可点击取消）。
    Ok(turn_registry.abort(&session_id))
}

// ═══════════════════════════════════════════════════════
// 插话（steer）——生成中入队（设计定稿）
// ═══════════════════════════════════════════════════════

/// 生成中提交插话：入队待注入（不打断当前段）。
///
/// - 仅该 session 有活动 turn 时可入队；空闲返回明确拒绝（前端据拒绝文案
///   走「关窗降级」= 退化为普通发送，见 ChatView.handleSteer 同步口径）；
/// - 返回条目定位 id（cancel_steer / 前端 pending 气泡取消用）；
/// - 队列消费（2026-09-25 修订）：dispatch 循环每个 step 的请求发出前领取
///   队首注入为轮内下一条 user 消息（步边界领取——工具步之间不再等回答段
///   收尾）；assistant 段收尾出口复查队列，有留存则续跑、空才发 turn/end。
///   同轮 streaming 不断，turn/end 只在队列空时发一次；
/// - 拒绝文案「当前没有处理中的对话，请等待处理结束后直接发送」是前端
///   关窗降级的判定标记，改动需同步（ChatView.handleSteer）。
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
    fn read_license_returns_full_text_in_dev() {
        // 开发构建下应能读到仓库根的 LICENSE 全文（不是简短声明）
        let text = read_license().expect("开发构建的 LICENSE 应可读");
        assert!(text.contains("Apache License"), "应是 Apache 许可证");
        assert!(
            text.len() > 5000,
            "应是全文而非简短声明，实际 {} 字节",
            text.len()
        );
    }

    fn exp_entry(id: &str, ts: u64, title: &str) -> crate::memory::schema::ExperienceEntry {
        crate::memory::schema::ExperienceEntry {
            id: id.into(),
            ts,
            conversation_id: "c1".into(),
            line_from: String::new(),
            turn_from: 0,
            turn_to: 0,
            level: "short".into(),
            title: title.into(),
            summary: String::new(),
            tags: Vec::new(),
            revision_count: 0,
            absorbed_in: None,
        }
    }

    #[test]
    fn experience_list_orders_newest_first_within_batch() {
        // 批间新在前（ts 降序）；批内按创建序倒序（同一 ts 的末条在上）。
        let entries = vec![
            exp_entry("a", 100, "旧批"),
            exp_entry("b", 200, "批2-1"),
            exp_entry("c", 200, "批2-2"),
            exp_entry("d", 200, "批2-3"),
        ];
        let rows = experience_list_rows(&entries);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["d", "c", "b", "a"]);
    }

    #[test]
    fn validate_longterm_bounds() {
        let max = crate::orchestrator::memory::MAX_LONGTERM_CHARS;
        assert!(validate_longterm("").is_ok(), "空串允许（＝清空）");
        assert!(validate_longterm(&"字".repeat(max)).is_ok(), "上限内应放行");
        let err = validate_longterm(&"字".repeat(max + 1)).unwrap_err();
        assert!(err.contains("上限"), "超限应报错而不静默截断: {err}");
    }

    // ── list_skills：临时夹具目录（运行根语义；不再依赖 dev 源目录）──

    #[test]
    fn list_skills_from_returns_entries_with_description() {
        let root = std::env::temp_dir().join(format!(
            "prts-cmd-skills-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("calc.md"),
            "---\nname: 数值计算\ndescription: 计算规则\n---\n\n## 正文",
        )
        .unwrap();
        std::fs::write(
            root.join("tui.md"),
            "---\nname: 行动推演\ndescription: 推演规则\n---\n\n## 正文",
        )
        .unwrap();

        let skills = list_skills_from(&root).expect("夹具目录应可读");
        assert_eq!(skills.len(), 2);
        let calc = skills
            .iter()
            .find(|s| s.name == "数值计算")
            .expect("应含数值计算技能");
        assert_eq!(calc.description, "计算规则", "frontmatter description 应被解析");
        assert!(calc.source.is_none(), "fixture 无 source → None（用户态）");
        // file_path：绝对路径且在目标目录内（打开链接用；文件名≠name）
        let fp = calc.file_path.as_ref().expect("应记录源文件路径");
        let p = std::path::Path::new(fp);
        assert!(p.is_absolute() && p.starts_with(&root));
        assert!(p.extension().and_then(|e| e.to_str()) == Some("md"));
        // 按 name 排序（SkillIndex.names 语义）
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "清单应按 name 排序");

        let _ = std::fs::remove_dir_all(&root);
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
            source: Some("builtin".into()),
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
        assert_eq!(
            v.get("source").and_then(|x| x.as_str()),
            Some("builtin"),
            "source 应原样透出（前端据此显示来源与删除门控）"
        );
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
        // 技能源文件（真实存在，包形态）→ 同样放行（不设白名单）
        let skill_md = crate::skills_index::bundled_skills_dir()
            .join("数值计算")
            .join("SKILL.md");
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

    // ── 压缩后快照刷新（批 4 / 批 5：压缩并更新）──

    #[test]
    fn refresh_frozen_snapshots_refreshes_all_stores() {
        let data = space_root("snap-refresh");
        let sessions = data.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let memory = Arc::new(Mutex::new(MemoryStore::load(&data).unwrap()));
        let snapshots = Arc::new(Mutex::new(SnapshotStore::new(&sessions)));
        let skills = Arc::new(Mutex::new(SkillSnapshotStore::new(&sessions)));
        let prefix = Arc::new(Mutex::new(PrefixSnapshotStore::new(&sessions)));

        // 冻结初始态：记忆为空块、技能目录 A
        {
            let mem = memory.lock().unwrap();
            snapshots.lock().unwrap().get_or_assemble(Some("c1"), &mem);
        }
        skills.lock().unwrap().get_or_freeze(Some("c1"), Some("目录 A"));
        prefix
            .lock()
            .unwrap()
            .get_or_freeze(Some("c1"), "系统 A", &[]);

        // 产生差异：记忆写入 + 技能目录 B
        {
            let mut mem = memory.lock().unwrap();
            mem.growth_update(PROFILE_NAME, Some("画像"), None, None, SOURCE_USER)
                .unwrap();
        }
        refresh_frozen_snapshots("c1", Some("目录 B"), &[], &memory, &snapshots, &skills, &prefix);

        assert_eq!(
            std::fs::read_to_string(sessions.join("c1.skills.txt")).unwrap(),
            "目录 B",
            "技能快照应刷为新目录"
        );
        assert!(
            std::fs::read_to_string(sessions.join("c1.memory.txt"))
                .unwrap()
                .contains("画像"),
            "记忆快照应含新写入"
        );

        // 完整前缀也应刷为新目录（含新记忆）
        let prefix_json = std::fs::read_to_string(sessions.join("c1.prefix.json")).unwrap();
        assert!(
            prefix_json.contains("目录 B"),
            "前缀快照应含新目录：{prefix_json}"
        );
        assert!(prefix_json.contains("画像"), "前缀快照应含新记忆");

        // 活体不可读 → 跳过（保持旧值）
        refresh_frozen_snapshots("c1", None, &[], &memory, &snapshots, &skills, &prefix);
        assert_eq!(
            std::fs::read_to_string(sessions.join("c1.skills.txt")).unwrap(),
            "目录 B",
            "不可读时保持"
        );

        let _ = std::fs::remove_dir_all(&data);
    }
}
