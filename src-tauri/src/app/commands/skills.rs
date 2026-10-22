use super::prelude::*;
use super::SkillEntry;

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
            let value =
                picked.and_then(|fp| fp.as_path().map(|p| p.to_string_lossy().into_owned()));
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
            let value =
                picked.and_then(|fp| fp.as_path().map(|p| p.to_string_lossy().into_owned()));
            let _ = tx.send(value);
        });
    rx.await.map_err(|e| format!("文件夹选择失败: {e}"))
}

/// 安装技能（单 .md 文件 / 技能包文件夹，自动识别）。
///
/// 校验与拷贝在 `skills_install` 模块（纯路径入参，可测）；成功后重算技能
/// 目录 diff 并入提醒台账——下一条消息经游标机制送达模型（目录冻结视图
/// 不变，变化走提醒通道，批 2 定案）。
#[tauri::command]
pub fn install_skill(
    source_path: String,
    confirm_scripts: bool,
    ledger: State<'_, Arc<Mutex<Notices>>>,
) -> Result<SkillEntry, String> {
    let source = std::path::PathBuf::from(&source_path);
    let root = crate::skills_index::skills_dir();
    let installed = crate::skills_install::install_skill(&source, &root, confirm_scripts)?;
    diff_skills_now(&root, &ledger);
    Ok(SkillEntry {
        name: installed.name,
        description: installed.description,
        file_path: Some(installed.file_path.to_string_lossy().into_owned()),
        source: installed.source,
        trust: installed.trust.as_str().to_string(),
        package_hash: Some(installed.package_hash),
        resource_dir: installed
            .resource_dir
            .map(|p| p.to_string_lossy().into_owned()),
    })
}

/// 安装前预览：只读扫描，不落盘；前端据此决定是否显示脚本确认块。
#[tauri::command]
pub fn preview_skill_source(
    source_path: String,
) -> Result<crate::skills_install::SkillPreview, String> {
    let source = std::path::PathBuf::from(&source_path);
    let root = crate::skills_index::skills_dir();
    crate::skills_install::preview_skill(&source, &root)
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
