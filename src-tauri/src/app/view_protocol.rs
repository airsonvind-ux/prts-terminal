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
            // 错误不能把系统原文（「会话目录不可用: os error 2」）当正文吐回去：
            // iframe 会把它当内容渲染出来，用户以为那是插件输出的卡片。
            // 这里统一换成一段中性占位，详细原因留给后端日志。
            Err(detail) => {
                eprintln!("[views] 视图读取失败（{path}）：{detail}");
                build(
                    StatusCode::NOT_FOUND,
                    "text/html; charset=utf-8",
                    placeholder_html().into_bytes(),
                )
            }
        };
        responder.respond(response);
    });
}

/// 读不到视图时给用户看的中性占位（沙箱内只有内联样式可用）。
///
/// 同时向宿主发一条 `view-unavailable`：卡片据此退化成占位并收掉「展开」，
/// 否则用户点开的是一个空面板，背后压着一整块黑蒙版。
fn placeholder_html() -> String {
    concat!(
        "<!doctype html><meta charset=\"utf-8\">",
        "<style>",
        "html,body{margin:0;height:100%;background:transparent}",
        "body{display:flex;align-items:center;justify-content:center;",
        "font:14px/1.6 system-ui,'Microsoft YaHei',sans-serif;color:#8b939d}",
        "</style>",
        "<body>这块视图已不可用</body>",
        "<script>parent.postMessage({source:'prts-view',type:'view-unavailable'},'*')</script>"
    )
    .to_string()
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
    let mime = mime_of(path);
    if mime.starts_with("text/html") {
        return Ok((mime, with_view_chrome(&bytes)));
    }
    Ok((mime, bytes))
}

/// 宿主给沙箱文档补的一层「壳」样式。
///
/// 跨源 iframe 的滚动条没法从父页面改，而 WebView2 的默认滚动条是浅色实心条，
/// 压在深色视图上非常刺眼。作者自己的样式写在后面，会正常覆盖这些默认值。
const VIEW_CHROME_CSS: &str = concat!(
    // 作者没写底色时，文档画布默认是白的——放在深色面板里就是一块刺眼的
    // 白板。这里把画布也让出来，露出宿主的底板；作者自己写背景仍然生效。
    "html,body{background:transparent;color-scheme:dark}",
    "html{scrollbar-width:thin;scrollbar-color:rgba(255,255,255,.16) transparent}",
    "::-webkit-scrollbar{width:8px;height:8px}",
    "::-webkit-scrollbar-track{background:transparent}",
    "::-webkit-scrollbar-thumb{background:rgba(255,255,255,.16);border-radius:4px}",
    "::-webkit-scrollbar-thumb:hover{background:rgba(255,255,255,.26)}",
);

/// 把 `VIEW_CHROME_CSS` 插进作者 HTML 的 `<head>` 开头；没有 head 就插在
/// doctype 之后，再没有就直接前置（仍在文档最前面，不影响渲染）。
fn with_view_chrome(html: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(html);
    let style = format!("<style data-prts-view-chrome>{VIEW_CHROME_CSS}</style>");
    let lower = text.to_ascii_lowercase();
    let insert_at = lower
        .find("<head")
        .and_then(|start| lower[start..].find('>').map(|offset| start + offset + 1))
        .or_else(|| {
            lower
                .find("<!doctype")
                .and_then(|start| lower[start..].find('>').map(|offset| start + offset + 1))
        })
        .unwrap_or(0);
    let mut out = String::with_capacity(text.len() + style.len());
    out.push_str(&text[..insert_at]);
    out.push_str(&style);
    out.push_str(&text[insert_at..]);
    out.into_bytes()
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
    fn missing_view_gets_a_neutral_placeholder() {
        let html = placeholder_html();
        assert!(html.starts_with("<!doctype html>"), "是一个完整文档");
        assert!(html.contains("这块视图已不可用"));
        assert!(html.contains("color:#8b939d"), "字色取次级正文色");
        assert!(
            html.contains("view-unavailable"),
            "占位页要通知宿主，卡片才能收掉展开"
        );
        assert!(!html.contains("os error"), "不把系统错误原文吐给用户");
    }

    #[test]
    fn view_chrome_goes_into_the_document_head() {
        let with_head = with_view_chrome(
            b"<!doctype html><html><head><title>x</title></head><body>hi</body></html>",
        );
        let text = String::from_utf8_lossy(&with_head);
        let style_at = text.find("data-prts-view-chrome").expect("注入了样式");
        assert!(style_at > text.find("<head>").unwrap(), "在 head 之内");
        assert!(
            style_at < text.find("<title>").unwrap(),
            "尽量靠前，作者样式可覆盖"
        );
        assert!(text.contains("::-webkit-scrollbar"), "带滚动条样式");

        let no_head =
            String::from_utf8_lossy(&with_view_chrome(b"<!doctype html><p>hi</p>")).into_owned();
        assert!(
            no_head.starts_with("<!doctype html><style"),
            "没有 head 时插在 doctype 之后"
        );
    }

    #[test]
    fn mime_follows_extension() {
        assert_eq!(
            mime_of(std::path::Path::new("a.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(mime_of(std::path::Path::new("a.PNG")), "image/png");
        assert_eq!(
            mime_of(std::path::Path::new("a.bin")),
            "application/octet-stream"
        );
    }
}
