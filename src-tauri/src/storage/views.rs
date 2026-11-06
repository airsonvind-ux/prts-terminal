//! views —— 工具视图的落盘与回读
//!
//! 视图是「工具结果的第二形态」：正文 `content` 进模型上下文，`view` 只给人看。
//! 落点与会话附件同根：`data_dir/session-files/<会话id>/views/<callId>.<ext>`，
//! 事件里只存**会话目录内的相对路径**（`views/<file>`）——数据目录整体搬家、
//! 归档复制到别处后，视图仍能跟着走。
//!
//! 写入只在 `tool_loop` 的成功分支发生（失败轮不带视图）；回读由前端经
//! [`resolve_session_path`] 换成绝对路径后走 asset 协议。
//!
//! 安全边界：文件名只取净化后的 `callId`，扩展名只取源文件的扩展名；
//! 回读时先做词法越界检查，再 canonicalize 比对前缀（防软链接逃逸）。

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::storage::attachments::{mime_for_path, sanitize_file_name, session_files_dir};
use crate::tools::{ToolView, ToolViewKind};

/// 会话目录下的视图子目录名。
pub const VIEWS_DIR: &str = "views";

/// 随 `tool/result` 事件落账的视图引用（不含内容本体）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultView {
    pub kind: ToolViewKind,
    /// 视图标题（卡片头）。
    pub title: String,
    /// 建议高度（已 clamp 到 40–160）；旧事件缺省时前端按 120 处理。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u16>,
    /// 会话目录内相对路径，形如 `views/<callId>.html`。
    pub path: String,
    /// `doc` 的 MIME（打开时用），`html` / `image` 不写。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
}

/// 把工具声明的视图落到会话目录，返回可随事件落账的引用。
pub fn save_view(
    conversation_id: &str,
    call_id: &str,
    view: &ToolView,
) -> Result<ToolResultView, String> {
    let session_dir = session_files_dir(conversation_id)?;
    save_view_in(&session_dir, call_id, view)
}

/// [`save_view`] 的落点显式版本（测试与调用方自定目录时用）。
pub fn save_view_in(
    session_dir: &Path,
    call_id: &str,
    view: &ToolView,
) -> Result<ToolResultView, String> {
    let dir = session_dir.join(VIEWS_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建视图目录失败: {e}"))?;

    let stem = file_stem(call_id);
    let (file_name, mime) = match view.kind {
        ToolViewKind::Html => {
            let html = view
                .html
                .as_deref()
                .ok_or_else(|| "html 视图缺少内容".to_string())?;
            let name = format!("{stem}.html");
            std::fs::write(dir.join(&name), html.as_bytes())
                .map_err(|e| format!("写入视图失败: {e}"))?;
            (name, None)
        }
        ToolViewKind::Image | ToolViewKind::Doc => {
            let source = view
                .source
                .as_deref()
                .ok_or_else(|| "视图缺少源文件".to_string())?;
            let ext = source
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "bin".into());
            let name = format!("{stem}.{ext}");
            std::fs::copy(source, dir.join(&name)).map_err(|e| format!("复制视图失败: {e}"))?;
            let mime = match view.kind {
                ToolViewKind::Doc => {
                    Some(view.mime.clone().unwrap_or_else(|| mime_for_path(source)))
                }
                _ => None,
            };
            (name, mime)
        }
    };

    Ok(ToolResultView {
        kind: view.kind,
        title: view.title.clone(),
        height: Some(view.height_px()),
        path: format!("{VIEWS_DIR}/{file_name}"),
        mime,
    })
}

/// 把事件里的相对路径解析成绝对路径（越界或不存在返回可读错误）。
pub fn resolve_session_path(conversation_id: &str, relative: &str) -> Result<String, String> {
    let session_dir = session_files_dir(conversation_id)?;
    let path = resolve_in(&session_dir, relative)?;
    Ok(path.to_string_lossy().into_owned())
}

/// [`resolve_session_path`] 的落点显式版本。
pub fn resolve_in(session_dir: &Path, relative: &str) -> Result<PathBuf, String> {
    let rel = Path::new(relative);
    if rel.is_absolute()
        || rel.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(format!("路径越出会话目录：{relative}"));
    }
    let base = session_dir
        .canonicalize()
        .map_err(|e| format!("会话目录不可用: {e}"))?;
    let resolved = session_dir
        .join(rel)
        .canonicalize()
        .map_err(|_| format!("视图文件不存在：{relative}"))?;
    if !resolved.starts_with(&base) {
        return Err(format!("路径越出会话目录：{relative}"));
    }
    Ok(resolved)
}

/// 落盘文件名的主体：净化后的 callId，空则回退 `view`。
fn file_stem(call_id: &str) -> String {
    let cleaned = sanitize_file_name(call_id);
    if cleaned.is_empty() {
        "view".into()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolView;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "prts-views-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_micros())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn html_view_is_written_to_the_session_dir() {
        let dir = scratch("html");
        let view = ToolView::html("地图", "<p>hi</p>", Some(160));
        let saved = save_view_in(&dir, "call_1", &view).expect("落盘");

        assert_eq!(saved.kind, ToolViewKind::Html);
        assert_eq!(saved.path, "views/call_1.html");
        assert_eq!(saved.height, Some(160));
        assert_eq!(saved.mime, None);
        let body = std::fs::read_to_string(dir.join("views/call_1.html")).expect("读回");
        assert_eq!(body, "<p>hi</p>");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn image_view_is_copied_and_path_is_relative() {
        let dir = scratch("image");
        let source = dir.join("shot.png");
        std::fs::write(&source, b"\x89PNG\r\n").expect("写源文件");
        let view = ToolView::image("截图", &source, None);

        let saved = save_view_in(&dir, "call-2", &view).expect("落盘");
        assert_eq!(saved.path, "views/call-2.png");
        assert!(dir.join("views/call-2.png").exists(), "复制到 views/");
        assert_eq!(saved.height, Some(120), "缺省高度 120");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn doc_view_keeps_mime() {
        let dir = scratch("doc");
        let source = dir.join("report.pdf");
        std::fs::write(&source, b"%PDF-1.4").expect("写源文件");
        let view = ToolView::doc("报告", &source, "application/pdf");

        let saved = save_view_in(&dir, "call:3", &view).expect("落盘");
        assert_eq!(saved.path, "views/call3.pdf", "冒号被净化掉");
        assert_eq!(saved.mime.as_deref(), Some("application/pdf"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolving_rejects_traversal_and_reports_missing() {
        let dir = scratch("resolve");
        std::fs::create_dir_all(dir.join("views")).expect("建 views");
        std::fs::write(dir.join("views/a.html"), "<p/>").expect("写文件");

        let abs = resolve_in(&dir, "views/a.html").expect("正常路径");
        assert!(abs.ends_with("a.html"));
        assert!(resolve_in(&dir, "../escape.html").is_err(), "拒绝上跳");
        assert!(resolve_in(&dir, "C:/Windows/win.ini").is_err(), "拒绝盘符");
        assert!(resolve_in(&dir, "views/missing.html").is_err(), "报不存在");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
