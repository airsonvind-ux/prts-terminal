//! 视图沙箱的独立源。
//!
//! 视图 HTML 走自定义协议而不是 `srcdoc`：`srcdoc` 继承主窗口的 CSP，
//! 内联脚本会被 `script-src 'self'` 拦掉（spike 结论）。自定义协议是独立
//! 源，文档策略只来自本模块返回的响应头——父级 CSP 不下传到跨源 iframe。
//!
//! URL 形态分平台：Windows 上 `prts-view://` 被映射成
//! `http://prts-view.localhost/…`，macOS/Linux 保持 `prts-view://localhost/…`。
//! 前端按 `viewOrigin()` 拼，宿主只认路径。

use tauri::http::{Response, StatusCode};
use tauri::Runtime;

/// 自定义协议名。
pub const SCHEME: &str = "prts-view";

/// 沙箱文档的 CSP：脚本可跑、样式可内联，但拿不到同源、发不出请求。
///
/// `sandbox` 是 CSP 的 sandbox 指令，效果等同 iframe 的 `sandbox` 属性；
/// 没有 `allow-same-origin`，所以 `document.cookie` / `localStorage` /
/// `parent.document` 一律抛 `SecurityError`（spike 已逐条验证）。
const SANDBOX_CSP: &str = "sandbox allow-scripts; default-src 'none'; \
     style-src 'unsafe-inline'; script-src 'unsafe-inline'; \
     img-src data: blob:; font-src data:; connect-src 'none'";

/// 协议入口：解析路径、读文件、套上沙箱策略。
pub fn handle<R: Runtime>(
    ctx: tauri::UriSchemeContext<'_, R>,
    request: tauri::http::Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    let _ = ctx.app_handle();
    let path = request.uri().path().to_string();
    tauri::async_runtime::spawn(async move {
        let response = match serve(&path) {
            Ok((mime, bytes)) => build(StatusCode::OK, mime, bytes),
            Err(detail) => build(StatusCode::NOT_FOUND, "text/plain; charset=utf-8", detail.into_bytes()),
        };
        responder.respond(response);
    });
}

/// 只服务 `session-files/<会话id>/views/**`；其它路径一律当成不存在。
fn serve(path: &str) -> Result<(&'static str, Vec<u8>), String> {
    let trimmed = path.trim_start_matches('/');
    let mut segments = trimmed.splitn(2, '/');
    let conversation_id = segments.next().unwrap_or_default();
    let relative = segments.next().unwrap_or_default();
    if conversation_id.is_empty() || !relative.starts_with("views/") {
        return Err("视图不存在".into());
    }
    let absolute = crate::storage::views::resolve_session_path(conversation_id, relative)?;
    let path = std::path::Path::new(&absolute);
    let bytes = std::fs::read(path).map_err(|_| "视图文件读不到".to_string())?;
    Ok((mime_of(path), bytes))
}

fn mime_of(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn build(status: StatusCode, mime: &str, body: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header("Content-Type", mime)
        .header("Content-Security-Policy", SANDBOX_CSP)
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "no-store")
        .body(body)
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_views_under_a_conversation_are_served() {
        assert_eq!(serve("/s1/other/x.html").unwrap_err(), "视图不存在");
        assert_eq!(serve("/s1").unwrap_err(), "视图不存在");
        assert_eq!(serve("/").unwrap_err(), "视图不存在");
        // 会话 id 带 `..`：存储层的词法校验拒绝，不落到文件系统。
        assert!(serve("/../views/x.html").is_err());
    }

    #[test]
    fn mime_follows_extension() {
        assert_eq!(mime_of(std::path::Path::new("a.html")), "text/html; charset=utf-8");
        assert_eq!(mime_of(std::path::Path::new("a.PNG")), "image/png");
        assert_eq!(mime_of(std::path::Path::new("a.bin")), "application/octet-stream");
    }
}
