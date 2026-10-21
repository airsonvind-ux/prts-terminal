use super::prelude::*;

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
    let dir = att::session_files_dir(&conversation_id.unwrap_or_default())?;
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
    let dir = att::session_files_dir(&conversation_id.unwrap_or_default())?;
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
pub(super) fn list_skills_from(dir: &std::path::Path) -> Result<Vec<SkillEntry>, String> {
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
/// 不在前端硬编码（既有实现的“许可证全文”其实是硬编码的短声明，此处避开）。
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
