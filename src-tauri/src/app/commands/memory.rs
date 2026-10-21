use super::prelude::*;

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
pub(super) fn experience_list_rows(
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
pub(super) fn validate_longterm(text: &str) -> Result<(), String> {
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
