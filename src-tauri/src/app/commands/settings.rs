use super::prelude::*;

// ═══════════════════════════════════════════════════════
// 配置读写
// ═══════════════════════════════════════════════════════

/// M3-B 设置写闸错误文案（后端闸 + 前端 toast 同文案；现有命令中文风格，
/// 不 code 化——前端 catch String 直接 toast）。
pub(super) const CONFIG_WRITE_GUARD_MSG: &str = "对话处理中不能修改设置，请等待当前处理结束";

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
pub(super) fn guard_config_write(turn_registry: &TurnRegistry) -> Result<(), String> {
    if turn_registry.active_count() > 0 {
        return Err(CONFIG_WRITE_GUARD_MSG.into());
    }
    Ok(())
}

/// MAA 设置页的**只读**状态（不触发连接、不起桥、不改状态）。
///
/// 同样走 `async`：它要往桥问一句 "status"（带 20s 超时），同步命令会在
/// 主线程上等这句回话——桥忙的时候设置页就会跟着僵住。
#[tauri::command]
pub async fn maa_status() -> crate::maa::manager::StatusView {
    tauri::async_runtime::spawn_blocking(crate::maa::manager::status_view)
        .await
        .unwrap_or_default()
}

/// 「测试连接」：桥在但没连上时**真的重连一次**，完了回最新状态。
///
/// `async` 不是为了并发，是为了**别冻界面**：Tauri 的同步命令在主线程执行，
/// 而起桥 + 连接是几十秒的活儿，同步会把这几秒整段卡在 UI 上。
/// 阻塞段丢进 spawn_blocking，界面照常响应，按钮的「连接中…」也才刷得出来。
#[tauri::command]
pub async fn maa_test_connection() -> Result<crate::maa::manager::StatusView, String> {
    tauri::async_runtime::spawn_blocking(crate::maa::manager::test_connection)
        .await
        .map_err(|e| format!("连接任务失败: {e}"))??;
    Ok(crate::maa::manager::status_view())
}

/// 扩展页「选择」模拟器安装目录。
///
/// 按**目录**选，不是选 exe：这个值是模拟器的安装目录（MAA 侧同一个位置也是
/// 目录），用来启用模拟器自带的截图/触控通道。形态同
/// `choose_workspace_directory`：后端命令 + dialog Rust API，非阻塞 pick_folder
/// 内部经 run_on_main_thread 弹窗，oneshot 桥回 async 命令；用户取消 → `Ok(None)`。
#[tauri::command]
pub async fn pick_emulator_directory(
    app: AppHandle,
    current: Option<String>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    let mut builder = app.dialog().file().set_title("选择模拟器安装目录");
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

/// 扩展页「选择」adb 可执行文件（模拟器自带的 adb.exe）。
#[tauri::command]
pub async fn pick_adb_file(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    app.dialog()
        .file()
        .set_title("选择 adb 可执行文件")
        .add_filter("adb", &["exe"])
        .pick_file(move |picked| {
            let value = picked.and_then(|fp| {
                fp.as_path()
                    .map(|p| p.to_string_lossy().into_owned())
            });
            let _ = tx.send(value);
        });
    rx.await.map_err(|e| format!("文件选择失败: {e}"))
}

/// 读取设置全量（get_settings）。返回 Settings 结构（snake_case），
/// **逐家 api_key 掩码下发**（明文经 reveal_api_key 瞬态获取）。只读不设闸。
#[tauri::command]
pub fn get_settings(
    store: State<'_, Arc<Mutex<SettingsStore>>>,
    credentials: State<'_, Arc<Mutex<crate::credentials::CredentialsStore>>>,
) -> Result<crate::settings::Settings, String> {
    let s = store.lock().map_err(|e| format!("设置锁获取失败: {e}"))?;
    let creds = credentials
        .lock()
        .map_err(|e| format!("凭据锁获取失败: {e}"))?;
    let mut out = s.settings.clone();
    // 掩码按"凭据层里有没有这家钥值"判定：settings 侧一律是空串
    // （钥值已搬到加密存储，见 crate::credentials）。
    for (name, pc) in out.llm.providers.iter_mut() {
        if creds.has(&crate::credentials::provider_key_ref(name)) {
            pc.api_key = crate::settings::MASKED_SECRET.to_string();
        } else {
            pc.api_key.clear();
        }
    }
    Ok(out)
}

/// 引导「已看过」键集（界面态，只读；不设闸）。见 `settings::UiSection`。
#[tauri::command]
pub fn get_guide_seen(
    store: State<'_, Arc<Mutex<SettingsStore>>>,
) -> Result<Vec<String>, String> {
    let s = store.lock().map_err(|e| format!("设置锁获取失败: {e}"))?;
    Ok(s.guide_seen())
}

/// 记一个引导项为「已看过」（界面态写入）。
///
/// **不受 set_settings 的 busy 闸约束**（维护者定案）：界面态不是配置，
/// 且它必须在对话处理中也能落盘——设置页通知随时会触发。落点仍是 settings.json
/// 的 `ui` 段，tmp+rename 原子替换 + 写后重读校验由 `SettingsStore::save` 保证。
#[tauri::command]
pub fn mark_guide_seen(
    store: State<'_, Arc<Mutex<SettingsStore>>>,
    key: String,
) -> Result<(), String> {
    let mut s = store.lock().map_err(|e| format!("设置锁获取失败: {e}"))?;
    s.mark_guide_seen(&key)
}

/// 显式读取某供应商 api_key 明文（瞬态查看用）。provider 缺省 = 当前
/// active 供应商。前端展示后即弃，不落持久明文态。
#[tauri::command]
pub fn reveal_api_key(
    store: State<'_, Arc<Mutex<SettingsStore>>>,
    credentials: State<'_, Arc<Mutex<crate::credentials::CredentialsStore>>>,
    provider: Option<String>,
) -> Result<String, String> {
    let name = {
        let s = store.lock().map_err(|e| format!("设置锁获取失败: {e}"))?;
        provider.unwrap_or_else(|| s.settings.llm.active_provider.clone())
    };
    let creds = credentials
        .lock()
        .map_err(|e| format!("凭据锁获取失败: {e}"))?;
    Ok(creds
        .get(&crate::credentials::provider_key_ref(&name))
        .unwrap_or_default()
        .to_string())
}

/// 局部 patch 写设置（per-field：字段缺席 = 不更新；显式 null = 清除回默认；
/// api_key 三态：缺席/掩码保留现值、空串清除、其他明文写入——但明文不进
/// settings，落凭据层（crate::credentials），settings 侧只留"配没配"）。
///
/// 同步职责：patch 合并进 SettingsStore（持久层 + 读取源）后，按变化摘要
/// 同步运行 State（llm_config / exec_settings / **workspace 共享句柄**，
/// 工作目录热生效见 crate::workspace），notices 记 Model 变化，
/// 最后落盘（写后重读校验；失败 Err 上抛 → 前端可见——内存已同步，
/// 下次变更兜底重写）。
///
/// M3-B busy 闸前置（锁序固定：store → credentials → config → exec_settings
/// → maa_settings → ledger，无任何路径反向嵌套）。只读降级（高版本文件）拒绝写。
/// `async` 只为不占主线程：它会把 MAA 设置推给管理器，而管理器锁在连接期间
/// 是被占着的——同步命令会把这份等待摊到 UI 上（连接测试中改设置会卡住）。
#[tauri::command]
pub async fn set_settings(
    turn_registry: State<'_, Arc<TurnRegistry>>,
    store: State<'_, Arc<Mutex<SettingsStore>>>,
    credentials: State<'_, Arc<Mutex<crate::credentials::CredentialsStore>>>,
    config: State<'_, Arc<Mutex<LlmConfig>>>,
    exec_settings: State<'_, Arc<Mutex<crate::exec_settings::ExecutionSettings>>>,
    maa_settings: State<'_, Arc<Mutex<crate::maa::settings::MaaSettings>>>,
    ledger: State<'_, Arc<Mutex<Notices>>>,
    root: State<'_, crate::workspace::WorkspaceRoot>,
    patch: SettingsPatch,
) -> Result<(), String> {
    guard_config_write(&turn_registry)?;
    let mut st = store.lock().map_err(|e| format!("设置锁获取失败: {e}"))?;
    if st.is_readonly() {
        return Err(format!(
            "设置文件 schemaVersion 高于当前，拒绝写入（需升级应用）"
        ));
    }
    // MAA 运行期间拒绝任何带 maa 段的写入：这项改动会作废连接、关掉桥，
    // 把正在跑的任务腰斩（长任务出轮后仍在跑，属常见情形）。扩展页的只读锁定
    // 是前端那一层，这里是后端那一层。
    // 判据用请求里的键，而不是 apply_patch 之后的变化摘要——必须在改动内存态
    // 之前拒绝，否则会出现「设置半数落进 store、却没落盘」。
    if patch.maa.is_some() && crate::maa::manager::is_running() {
        return Err("MAA 正在执行任务，暂不能修改连接设置；等它跑完，或先中止它".to_string());
    }
    let changes = st.apply_patch(&patch);
    // 密钥操作：先落凭据层（可回滚），再存 settings（不可回滚）。锁序固定
    // store → credentials（无反向路径）。
    if !changes.key_ops.is_empty() {
        let mut creds = credentials
            .lock()
            .map_err(|e| format!("凭据锁获取失败: {e}"))?;
        // 操作前快照：set/save 任一步失败都能回滚到原状。
        let undo: Vec<(String, Option<String>)> = changes
            .key_ops
            .iter()
            .map(|op| {
                let r = crate::credentials::provider_key_ref(&op.provider);
                let prev = creds.get(&r).map(|s| s.to_string());
                (r, prev)
            })
            .collect();
        let applied = (|| -> Result<(), String> {
            for op in &changes.key_ops {
                creds.set(
                    &crate::credentials::provider_key_ref(&op.provider),
                    &op.plaintext,
                )?;
            }
            creds.save()
        })();
        if let Err(e) = applied {
            for (r, prev) in &undo {
                let _ = creds.set(r, prev.as_deref().unwrap_or(""));
            }
            let _ = creds.save();
            return Err(format!("密钥保存失败：{e}"));
        }
    }
    if changes.llm_changed {
        let mut c = config.lock().map_err(|e| format!("配置锁获取失败: {e}"))?;
        // active 供应商 resolve 成请求用 LlmConfig（provider = active 名）；
        // 钥值从凭据层取（settings 侧不留明文）。
        let creds = credentials
            .lock()
            .map_err(|e| format!("凭据锁获取失败: {e}"))?;
        *c = st.settings.llm.resolve_active_with(|p| {
            creds
                .get(&crate::credentials::provider_key_ref(p))
                .map(|s| s.to_string())
        });
    }
    if changes.exec_changed {
        let mut es = exec_settings
            .lock()
            .map_err(|e| format!("执行模式配置锁获取失败: {e}"))?;
        *es = st.settings.exec;
    }
    if changes.workspace_changed {
        // 热生效：把新工作根推给共享句柄——策略、路径守卫、执行工具、仿真
        // 工具随之跟随，无需重启（见 crate::workspace）。新根若解析为默认
        // 路径，顺手确保它存在（用户可能删过）；用户自选路径**不**重建
        // ——目录消失应当显式暴露，而不是被静默重建。
        let resolved = st.workspace_root();
        if resolved == crate::config::default_workspace_path() {
            if let Err(e) = crate::config::ensure_default_workspace() {
                eprintln!(
                    "[settings] 默认工作区创建失败（{}）：{e}",
                    resolved.display()
                );
            }
        }
        // 台账同步（提醒模型环境变了）：对话前缀冻结，环境变化只能靠 reminder
        // 通知；少了这一步，模型会拿着旧工作目录做事，新对话还会拿到旧值。
        {
            let mut l = ledger.lock().map_err(|e| format!("台账锁获取失败: {e}"))?;
            l.note_workspace_change(&resolved.to_string_lossy());
        }
        root.set(resolved);
    }
    if changes.maa_changed {
        let mut ms = maa_settings
            .lock()
            .map_err(|e| format!("MAA 设置锁获取失败: {e}"))?;
        *ms = st.settings.maa.clone();
        // 推给运行期权威（桥管理器）：连接类改动在那里即作废现有连接。
        crate::maa::manager::set_settings(ms.clone());
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
    _turn_registry: State<'_, Arc<TurnRegistry>>,
    store: State<'_, Arc<Mutex<SettingsStore>>>,
    enabled: bool,
) -> Result<(), String> {
    // 不受“对话处理中”限制（2026-10-08）：置顶是纯窗口属性，不参与任何轮次
    // 快照，与 guard_config_write 的立意（锁住“本次运行所依据的配置”）无关。
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

/// 启动自检事实（lib.rs 装配期写入，只读命令 `get_boot_status` 消费）。
///
/// 只报事实、不参与业务判定：数据库可用性从 `Database` 自身读，这里带的是
/// 装配期一次性得到的知识库 / 记忆结果。
#[derive(Debug, Clone)]
pub struct BootFacts {
    /// 知识库索引建起来的条目数（加载失败 = 0）
    pub knowledge_count: usize,
    /// 知识库索引加载失败原因（正常 = None）
    pub knowledge_error: Option<String>,
    /// 记忆存储加载失败原因（正常 = None）
    pub memory_error: Option<String>,
}

/// 启动自检单件的报告：ok + 一行注记（成功时是事实，失败时是原因）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootComponent {
    pub ok: bool,
    pub note: String,
}

/// 启动自检报告（启动演出消费；不参与业务判定）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootStatus {
    pub database: BootComponent,
    pub knowledge: BootComponent,
    pub memory: BootComponent,
}

/// 启动自检：三件内置资源是否就位。只读、无副作用。
///
/// 演出在载入时按这个报告翻牌（`[ OK ]` / `[FAIL]`）——载入是真检测，
/// 不是纯动画。`note` 在成功时是事实（如「已接入 · 128 条」），
/// 失败时是原因原文（给提示悬停与排查用）。
#[tauri::command]
pub fn get_boot_status(
    db: State<'_, Arc<Database>>,
    facts: State<'_, Arc<BootFacts>>,
) -> Result<BootStatus, String> {
    let database = match db.unavailable_reason() {
        None => BootComponent {
            ok: true,
            note: format!("已接入 · {} 张表", db.present_table_count()),
        },
        Some(reason) => BootComponent {
            ok: false,
            note: reason.to_string(),
        },
    };
    let knowledge = match &facts.knowledge_error {
        None => BootComponent {
            ok: true,
            note: format!("已接入 · {} 条", facts.knowledge_count),
        },
        Some(e) => BootComponent {
            ok: false,
            note: e.clone(),
        },
    };
    let memory = match &facts.memory_error {
        None => BootComponent {
            ok: true,
            note: "已接入".into(),
        },
        Some(e) => BootComponent {
            ok: false,
            note: e.clone(),
        },
    };
    Ok(BootStatus {
        database,
        knowledge,
        memory,
    })
}

/// 应用只读元信息（关于页 + 空间页共用）：版本 + **静态**路径。
///
/// 这里的路径必须都是静态事实（数据根 / 默认工作区 / 种子库 / 随包资源）——
/// 它们是面板挂载时拉一次的快照。**不得放随设置变化的派生值**（如「当前生效
/// 工作区」）：那会变成启动时的旧值，改设置后前端读到过期路径（2026-10-11 实测踩到）。
/// 当前生效工作区由前端自算：设置值非空则用它，否则用默认工作区。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppMeta {
    pub version: &'static str,
    pub data_dir: String,
    /// 默认工作区路径（`<主目录>/PRTS-WorkSpace`）：恒定的，与设置无关。
    pub default_workspace_root: String,
    pub db_path: String,
    pub knowledge_dir: String,
    pub skills_dir: String,
    /// 《使用说明》文档路径（随包资源）。
    pub user_guide_path: String,
}

#[tauri::command]
pub fn get_app_meta(
    store: State<'_, Arc<Mutex<SettingsStore>>>,
) -> Result<AppMeta, String> {
    let st = store.lock().map_err(|e| format!("设置锁获取失败: {e}"))?;
    Ok(AppMeta {
        version: env!("CARGO_PKG_VERSION"),
        data_dir: st.data_dir().to_string_lossy().into_owned(),
        default_workspace_root: crate::config::default_workspace_path()
            .to_string_lossy()
            .into_owned(),
        db_path: crate::config::resolve_db_path().to_string_lossy().into_owned(),
        knowledge_dir: crate::knowledge::index::knowledge_dir()
            .to_string_lossy()
            .into_owned(),
        skills_dir: crate::skills_index::skills_dir()
            .to_string_lossy()
            .into_owned(),
        user_guide_path: crate::config::bundled_docs_dir()
            .join("使用说明.md")
            .to_string_lossy()
            .into_owned(),
    })
}
