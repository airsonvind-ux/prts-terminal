//! tools::web_fetch —— 网页抓取工具（M5 网络域）
//!
//! 输入 URL → 抓取页面 → 按 content-type 分流提取正文 → 截断返回。
//!
//! ## 安全边界（对齐 M5 定稿）
//! - 仅 http/https（其余协议 Parse 拒绝）
//! - SSRF：逐跳（含重定向目标）host 校验——IP 字面量分类 + 域名全量解析
//!   复查（解析失败按私有保守拒）；解析结果固定到本跳客户端，连接阶段
//!   不再走 DNS（防重绑定）；白名单注入点仅供测试 loopback 服务器
//!   （生产默认空 = 全拒内网/本机/链路本地）
//! - 重定向 manual 逐跳复查，≤ 5 跳，单跳超时 15s（测试可注入短超时）
//! - 响应体 1MiB 流式字节硬上限（超限断报"页面过大"，不落全量内存）
//! - UA `Mozilla/5.0 (compatible; PRTS-Terminal/0.1)`；禁代理（不读环境变量）
//!
//! ## 内容管线
//! - `text/html`：readabilityrs 提取正文（文章页 → markdown，格式=网页正文；
//!   parse None / 非文章页 → 降级 standardize_all + html_to_markdown，再不行
//!   正则剥标签兜底，格式=网页文本）；markdown 链接经 absolutize 后处理
//!   （相对链接 join base，javascript:/data: 等危险协议剥成纯文本）
//! - `application/json`：pretty 格式化（格式=JSON）
//! - 其余：原样文本（格式=纯文本）
//!
//! 超时/HTTP 错误/体积超限均按 M5 定稿文案返回中文 ToolError。

use std::io::Read;
use std::net::SocketAddr;
use std::time::Duration;

use serde_json::Value;

use crate::tools::web_common::{
    absolutize_markdown_links, parse_http_url, resolve_host_all, resolve_public_ip, truncate_chars,
    HostReject, UrlReject,
};
use crate::tools::{DisplayLink, Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

// ═══════════════════════════════════════════════════════
// 常量（M5 定稿数值）
// ═══════════════════════════════════════════════════════

/// 单跳超时（生产默认）。
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
/// 重定向最大跳数（≤5 跳）。
const MAX_REDIRECTS: usize = 5;
/// 响应体字节硬上限：1MiB（流式读取，超限即断）。
const MAX_BODY_BYTES: usize = 1024 * 1024;
/// 返回正文默认最大字符数。
const DEFAULT_MAX_LEN: usize = 12000;
/// maxLength 双向钳制（schema 声明 + 执行内再钳）。
const MIN_MAX_LEN: usize = 200;
const MAX_MAX_LEN: usize = 50000;
/// 传输错误原因截断（防异常串撑爆 summary）。
const MAX_CAUSE_CHARS: usize = 200;

const USER_AGENT: &str = "Mozilla/5.0 (compatible; PRTS-Terminal/0.1)";
const ACCEPT: &str = "text/html,application/xhtml+xml,application/json,text/plain,*/*";
const ACCEPT_LANGUAGE: &str = "zh-CN,zh;q=0.9,en;q=0.8";
const REDIRECT_CODES: [u16; 5] = [301, 302, 303, 307, 308];

// ═══════════════════════════════════════════════════════
// 工具定义
// ═══════════════════════════════════════════════════════

/// 抓取选项（测试注入点：短超时 + loopback 白名单）。
#[derive(Debug, Clone)]
pub struct FetchOptions {
    /// 单跳超时。
    pub timeout: Duration,
    /// SSRF 显式放行主机（精确字面匹配；生产默认空 = 全拒私有地址）。
    pub allowed_hosts: Vec<String>,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            allowed_hosts: Vec::new(),
        }
    }
}

pub struct WebFetchTool {
    opts: FetchOptions,
}

impl WebFetchTool {
    pub fn new(opts: FetchOptions) -> Self {
        Self { opts }
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new(FetchOptions::default())
    }
}

impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "抓取指定 URL 的网页并提取正文文本。仅支持 http/https；自动跟随重定向（最多 5 跳，逐跳复查目标地址）；单跳超时 15 秒，页面体积上限 1MB。HTML 页面按可读性算法提取正文（文章页输出 Markdown，非文章页降级为纯文本），JSON 响应格式化返回，其余按纯文本返回；拒绝访问内网/本机/链路本地地址（防 SSRF）。maxLength 控制返回字符上限，超长自动截断并标注；失败返回中文错误信息（超时/HTTP 状态/页面过大/私网拒绝）"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "minLength": 1,
                    "description": "要抓取的完整链接，需以 http:// 或 https:// 开头（含 https://）"
                },
                "maxLength": {
                    "type": "integer",
                    "minimum": 200,
                    "maximum": 50000,
                    "description": "返回正文的最大字符数，默认 12000，下限 200、上限 50000（自动钳制）"
                }
            },
            "required": ["url"]
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(params) else {
            return ToolDisplay::default();
        };
        match v
            .get("url")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            // URL 链接（前端经 open_url 交系统浏览器打开）
            Some(u) => ToolDisplay::link(DisplayLink::url(u)),
            None => ToolDisplay::default(),
        }
    }

    fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        // ── 1. 解析参数 ──
        let v: Value = serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;
        let raw_url = v["url"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::Parse("缺少必填参数: url".into()))?;
        let max_len = clamp_max_len(v.get("maxLength").and_then(|x| x.as_i64()));

        // ── 2. URL 守卫（协议白名单）──
        let url = match parse_http_url(raw_url) {
            Ok(u) => u,
            Err(UrlReject::UnsupportedScheme(_)) => {
                return Err(ToolError::Parse("仅支持 http/https 链接".into()))
            }
            Err(UrlReject::Invalid) => {
                return Err(ToolError::Parse(format!(
                    "无效的链接：{raw_url}（需以 http:// 或 https:// 开头）"
                )))
            }
        };

        // ── 3. 抓取（逐跳 SSRF 复查 + 重定向 ≤5 跳 + 流式体积上限）──
        let (final_url, content_type, body) =
            self.fetch_pipeline(&url).map_err(|e| e.into_tool_error())?;

        // ── 4. 按 content-type 分流提取 ──
        let (text, label) = render_body(&content_type, &body, &final_url);

        // ── 5. 字符截断（双向钳制后的上限）──
        let text = if text.chars().count() > max_len {
            let mark = format!("\n…（正文过长，已截断至 {max_len} 字符）");
            format!("{}{}", truncate_chars(&text, max_len), mark)
        } else {
            text
        };

        let host = final_url.host_str().unwrap_or("");
        let path = final_url.path();
        let summary = format!("来源：{host}{path}（{label}）\n{text}");
        Ok(ToolOutput::text(summary))
    }
}

// ═══════════════════════════════════════════════════════
// 抓取管线（内部错误 → 工具错误文案）
// ═══════════════════════════════════════════════════════

/// 抓取内部错误：已含完整文案主体（无"工具执行失败："前缀——
/// Display 由 ToolError::Execution 统一加）。
#[derive(Debug)]
struct FetchError(String);

impl FetchError {
    fn into_tool_error(self) -> ToolError {
        ToolError::Execution(self.0)
    }
}

fn private_host_msg(host: &str) -> String {
    format!("已拒绝访问内网/本机地址（{host}），仅允许公网地址")
}

impl WebFetchTool {
    /// 抓取主循环：返回 (最终 URL, content-type, 完整响应体)。
    fn fetch_pipeline(
        &self,
        url: &reqwest::Url,
    ) -> Result<(reqwest::Url, String, Vec<u8>), FetchError> {
        let mut current = url.clone();
        let mut hops: usize = 0;

        let mut resp =
            loop {
                // 逐跳 SSRF 复查（首跳与每次重定向目标都校验）
                let host = current
                    .host_str()
                    .ok_or_else(|| FetchError("请求失败：链接缺少主机名".into()))?;
                // 解析一次并固定到本跳客户端：连接阶段不再走 DNS，封死重绑定窗口。
                let ip = resolve_public_ip(host, &self.opts.allowed_hosts, resolve_host_all)
                    .map_err(|reject| match reject {
                        HostReject::Private => FetchError(private_host_msg(host)),
                        HostReject::ResolveFailed | HostReject::Empty => {
                            FetchError(format!("请求失败：无法解析主机名（{host}）"))
                        }
                    })?;
                let port = current.port_or_known_default().unwrap_or(80);
                let is_literal = host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse::<std::net::IpAddr>()
                    .is_ok();
                let pin = (!is_literal).then(|| (host, SocketAddr::new(ip, port)));
                let client = build_client(self.opts.timeout, pin)
                    .map_err(|_| FetchError(format!("请求失败：{url}", url = current.as_str())))?;
                let attempt = client.get(current.as_str()).send().map_err(|e| {
                    classify_transport_error(&e, current.as_str(), self.opts.timeout)
                })?;
                let status = attempt.status().as_u16();

                // 非重定向状态：交主流程判定成败
                if !REDIRECT_CODES.contains(&status) {
                    break attempt;
                }
                // 重定向：跳数上限检查 → 取 Location → join 后复查下一跳
                if hops >= MAX_REDIRECTS {
                    return Err(FetchError(format!(
                        "重定向次数过多（超过 {MAX_REDIRECTS} 跳）：{url}",
                        url = current.as_str()
                    )));
                }
                let Some(location) = attempt
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|h| h.to_str().ok())
                else {
                    // 3xx 无 Location：无从跟随，按最终响应交给状态判定
                    break attempt;
                };
                current = current
                    .join(location)
                    .map_err(|_| FetchError(format!("请求失败：无效的重定向目标 {location}")))?;
                hops += 1;
            };

        // ── 状态判定（非 2xx 一律按 HTTP 错误返回，不读 body）──
        let status = resp.status();
        if !status.is_success() {
            return Err(FetchError(format!(
                "HTTP {code}：{url}",
                code = status.as_u16(),
                url = current.as_str()
            )));
        }

        // ── 流式读取，1MiB 硬上限（content-length 预检 + 边读边断）──
        if let Some(cl) = resp.content_length() {
            if cl > MAX_BODY_BYTES as u64 {
                return Err(FetchError(format!(
                    "页面过大（超过 1MiB 上限）：{url}",
                    url = current.as_str()
                )));
            }
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_string();
        let mut body: Vec<u8> = Vec::new();
        let mut buf = [0u8; 8192];
        let mut total = 0usize;
        loop {
            let n = resp
                .read(&mut buf)
                .map_err(|e| FetchError(format!("请求失败：读取响应中断（{e}）")))?;
            if n == 0 {
                break;
            }
            total += n;
            if total > MAX_BODY_BYTES {
                return Err(FetchError(format!(
                    "页面过大（超过 1MiB 上限）：{url}",
                    url = current.as_str()
                )));
            }
            body.extend_from_slice(&buf[..n]);
        }

        Ok((current, content_type, body))
    }
}

/// 单跳客户端：超时（生产 15s）+ manual 重定向 + 固定 UA/头 + 禁代理。
/// 注意：blocking 的 ClientBuilder 无 header()，头一律经 default_headers。
fn build_client(
    timeout: Duration,
    pin: Option<(&str, SocketAddr)>,
) -> Result<reqwest::blocking::Client, reqwest::Error> {
    use reqwest::header::{
        HeaderMap, HeaderValue, ACCEPT as HDR_ACCEPT, ACCEPT_LANGUAGE as HDR_ACCEPT_LANG,
    };
    let mut headers = HeaderMap::new();
    headers.insert(HDR_ACCEPT, HeaderValue::from_static(ACCEPT));
    headers.insert(HDR_ACCEPT_LANG, HeaderValue::from_static(ACCEPT_LANGUAGE));
    let mut builder = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(USER_AGENT)
        .default_headers(headers)
        .no_proxy();
    if let Some((host, addr)) = pin {
        builder = builder.resolve(host, addr);
    }
    builder.build()
}

/// 传输错误分类：超时 → 定稿文案（单跳超时秒数 + 原始目标 URL）；
/// 其余 → 请求失败 + 原因。
fn classify_transport_error(e: &reqwest::Error, url: &str, timeout: Duration) -> FetchError {
    if e.is_timeout() {
        return FetchError(format!("请求超时（{} 秒）：{url}", timeout.as_secs()));
    }
    let cause: String = e.to_string().chars().take(MAX_CAUSE_CHARS).collect();
    FetchError(format!("请求失败：{url}（{cause}）"))
}

// ═══════════════════════════════════════════════════════
// 内容分流渲染
// ═══════════════════════════════════════════════════════

/// content-type → (提取文本, 格式标签)。格式标签随 summary 头部展示。
fn render_body(content_type: &str, body: &[u8], base_url: &reqwest::Url) -> (String, &'static str) {
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("json") {
        // JSON：pretty 格式化（解析失败保底原样）
        match serde_json::from_slice::<Value>(body) {
            Ok(v) => (serde_json::to_string_pretty(&v).unwrap_or_default(), "JSON"),
            Err(_) => (String::from_utf8_lossy(body).into_owned(), "JSON"),
        }
    } else if ct.contains("html") {
        extract_html(&String::from_utf8_lossy(body), base_url)
    } else {
        (String::from_utf8_lossy(body).into_owned(), "纯文本")
    }
}

// ═══════════════════════════════════════════════════════
// HTML 正文提取（readabilityrs + 双保险降级）
// ═══════════════════════════════════════════════════════

/// HTML → (正文, 格式标签)：
/// - 文章页：readabilityrs 解析成功 → markdown 正文（格式=网页正文）
/// - 非文章页 / parse None：降级 = crate 公开的 elements::standardize_all +
///   markdown::html_to_markdown；仍为空 → 自带正则剥标签兜底（格式=网页文本）
///
/// markdown 形态统一过 absolutize 后处理（相对链接 join + 危险协议剥除）。
fn extract_html(html: &str, base_url: &reqwest::Url) -> (String, &'static str) {
    use readabilityrs::{is_probably_readerable, Readability, ReadabilityOptions};

    // 前置判断：明显非文章页直接降级（省一次完整 parse）
    if !is_probably_readerable(html, None) {
        return degrade_html(html, base_url);
    }
    let options = ReadabilityOptions::builder()
        .output_markdown(true)
        .sanitize_content(true)
        .build();
    match Readability::new(html, Some(base_url.as_str()), Some(options)) {
        Ok(rd) => match rd.parse() {
            Some(article) => match article.markdown_content {
                Some(md) if !md.trim().is_empty() => {
                    (absolutize_markdown_links(&md, base_url), "网页正文")
                }
                _ => degrade_html(html, base_url),
            },
            None => degrade_html(html, base_url),
        },
        Err(_) => degrade_html(html, base_url),
    }
}

/// 降级路径 1（crate 公开转换）：标准 HTML 全量 → markdown。
fn degrade_html(html: &str, base_url: &reqwest::Url) -> (String, &'static str) {
    let standardized = readabilityrs::elements::standardize_all(html, None);
    let md = readabilityrs::markdown::html_to_markdown(
        &standardized,
        &readabilityrs::MarkdownOptions::default(),
    );
    if md.trim().is_empty() {
        // 降级路径 2（双保险）：正则剥标签兜底
        (strip_html_to_text(html), "网页文本")
    } else {
        (absolutize_markdown_links(&md, base_url), "网页文本")
    }
}

/// 正则剥标签兜底（readability 系转换均失败时的最后防线）：
/// 去 script/style/head/注释 → 块级标签换行 → 去剩余标签 → 实体解码 → 空白归一。
fn strip_html_to_text(html: &str) -> String {
    use std::sync::OnceLock;

    use regex::Regex;

    fn regexes() -> &'static [Regex; 9] {
        static CACHE: OnceLock<[Regex; 9]> = OnceLock::new();
        CACHE.get_or_init(|| {
            [
                Regex::new(r"(?is)<script\b[^>]*>.*?</script\s*>").expect("静态正则合法"),
                Regex::new(r"(?is)<style\b[^>]*>.*?</style\s*>").expect("静态正则合法"),
                Regex::new(r"(?is)<noscript\b[^>]*>.*?</noscript\s*>").expect("静态正则合法"),
                Regex::new(r"(?is)<template\b[^>]*>.*?</template\s*>").expect("静态正则合法"),
                Regex::new(r"(?is)<head\b[^>]*>.*?</head\s*>").expect("静态正则合法"),
                Regex::new(r"(?s)<!--.*?-->").expect("静态正则合法"),
                Regex::new(
                    r"(?i)</?(?:p|div|br|h[1-6]|li|tr|th|td|table|thead|tbody|section|article|blockquote|header|footer|nav|ul|ol|pre)[^>]*>",
                )
                .expect("静态正则合法"),
                Regex::new(r"<[^>]+>").expect("静态正则合法"),
                Regex::new(r"&#(\d+);").expect("静态正则合法"),
            ]
        })
    }

    let (scripts, styles, noscripts, templates, head, comments, blocks, tags, numeric) = {
        let r = regexes();
        (
            &r[0], &r[1], &r[2], &r[3], &r[4], &r[5], &r[6], &r[7], &r[8],
        )
    };
    let mut text = html.to_string();
    text = scripts.replace_all(&text, "").into_owned();
    text = styles.replace_all(&text, "").into_owned();
    text = noscripts.replace_all(&text, "").into_owned();
    text = templates.replace_all(&text, "").into_owned();
    text = head.replace_all(&text, "").into_owned();
    text = comments.replace_all(&text, "").into_owned();
    text = blocks.replace_all(&text, "\n").into_owned();
    text = tags.replace_all(&text, "").into_owned();
    text = numeric
        .replace_all(&text, |caps: &regex::Captures<'_>| {
            caps[1]
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .map(|c| c.to_string())
                .unwrap_or_default()
        })
        .into_owned();
    for (from, to) in [
        ("&nbsp;", " "),
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&apos;", "'"),
    ] {
        text = text.replace(from, to);
    }
    // 空白归一（去 \r、行内多空格、3+ 空行压成 1）
    let mut out = String::with_capacity(text.len());
    let mut prev_space = false;
    let mut blank_lines = 0usize;
    for ch in text.chars() {
        match ch {
            ' ' | '\t' => {
                if !prev_space && !out.ends_with('\n') {
                    out.push(' ');
                }
                prev_space = true;
            }
            '\r' => {}
            '\n' => {
                if out.ends_with('\n') {
                    blank_lines += 1;
                    if blank_lines > 1 {
                        continue;
                    }
                } else {
                    blank_lines = 0;
                }
                out.push('\n');
                prev_space = false;
            }
            c => {
                out.push(c);
                prev_space = false;
                blank_lines = 0;
            }
        }
    }
    out.trim().to_string()
}

/// maxLength 双向钳制（schema 声明 + 执行内再钳，防 LLM 传超大/负值）。
fn clamp_max_len(v: Option<i64>) -> usize {
    match v {
        Some(n) if n > 0 => (n as usize).clamp(MIN_MAX_LEN, MAX_MAX_LEN),
        Some(_) => MIN_MAX_LEN,
        None => DEFAULT_MAX_LEN,
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════
// （测试模块经两次 edit 追加：网络层用例 + 清洗用例，见下方锚点）
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    // ── 罐头应答服务器（loopback，零新依赖）──

    struct Canned {
        status: u16,
        content_type: &'static str,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    fn canned(status: u16, content_type: &'static str, body: impl Into<Vec<u8>>) -> Canned {
        Canned {
            status,
            content_type,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    fn status_text(code: u16) -> &'static str {
        match code {
            200 => "OK",
            301 => "Moved Permanently",
            302 => "Found",
            303 => "See Other",
            307 => "Temporary Redirect",
            308 => "Permanent Redirect",
            404 => "Not Found",
            _ => "OK",
        }
    }

    /// 顺序应答服务器：每次请求调 handler(idx, 请求行)；None = 结束。
    /// 返回基础地址 `http://127.0.0.1:{port}`（子线程 detach，测试进程
    /// 退出即回收；读超时 3s 防请求缺失时永久阻塞）。
    fn spawn_canned<F>(mut handler: F) -> String
    where
        F: FnMut(u32, &str) -> Option<Canned> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback 绑定失败");
        let port = listener.local_addr().expect("取端口失败").port();
        thread::spawn(move || {
            let mut idx = 0u32;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
                let mut request_line = String::new();
                {
                    let mut reader = BufReader::new(&mut stream);
                    if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                        break;
                    }
                    // 丢弃剩余请求头
                    loop {
                        let mut rest = String::new();
                        if reader.read_line(&mut rest).unwrap_or(0) == 0 || rest.trim().is_empty() {
                            break;
                        }
                    }
                }
                let Some(resp) = handler(idx, request_line.trim()) else {
                    break;
                };
                let mut head = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    resp.status,
                    status_text(resp.status),
                    resp.content_type,
                    resp.body.len()
                );
                for (k, v) in &resp.headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                head.push_str("\r\n");
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&resp.body);
                let _ = stream.flush();
                idx += 1;
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    /// 测试工具：允许 loopback + 可注入超时。
    fn tool(timeout_secs: u64) -> WebFetchTool {
        WebFetchTool::new(FetchOptions {
            timeout: Duration::from_secs(timeout_secs),
            allowed_hosts: vec!["127.0.0.1".to_string()],
        })
    }

    fn exec(tool: &WebFetchTool, params: &str) -> Result<ToolOutput, ToolError> {
        tool.execute(&ToolContext::new(), params)
    }

    fn err_text(e: &ToolError) -> String {
        e.to_string()
    }

    // ── clamp_max_len ──

    #[test]
    fn max_length_clamped_both_ways() {
        assert_eq!(clamp_max_len(None), 12000);
        assert_eq!(clamp_max_len(Some(120)), 200);
        assert_eq!(clamp_max_len(Some(200)), 200);
        assert_eq!(clamp_max_len(Some(12000)), 12000);
        assert_eq!(clamp_max_len(Some(50000)), 50000);
        assert_eq!(clamp_max_len(Some(9_999_999)), 50000);
        assert_eq!(clamp_max_len(Some(-5)), 200);
        assert_eq!(clamp_max_len(Some(0)), 200);
    }

    // ── 协议/URL 守卫 ──

    #[test]
    fn rejects_non_http_schemes() {
        let t = tool(5);
        for u in [
            "ftp://example.com/file",
            "javascript:alert(1)",
            "file:///C:/secret.txt",
        ] {
            let err = exec(&t, &format!(r#"{{"url":"{u}"}}"#)).unwrap_err();
            assert_eq!(
                err_text(&err),
                "参数解析失败：仅支持 http/https 链接",
                "url: {u}"
            );
        }
    }

    #[test]
    fn rejects_invalid_or_missing_url() {
        let t = tool(5);
        let err = exec(&t, r#"{"url":"不是链接"}"#).unwrap_err();
        assert!(
            err_text(&err).contains("参数解析失败：无效的链接"),
            "got: {err}"
        );
        let err = exec(&t, r#"{}"#).unwrap_err();
        assert!(err_text(&err).contains("缺少必填参数: url"), "got: {err}");
        let err = exec(&t, r#"{"url":"  "}"#).unwrap_err();
        assert!(err_text(&err).contains("缺少必填参数: url"), "got: {err}");
    }

    // ── SSRF：生产默认（空白名单）连本机都拒，无需建连 ──

    #[test]
    fn production_default_blocks_loopback_without_connecting() {
        let t = WebFetchTool::default();
        let err = exec(&t, r#"{"url":"http://127.0.0.1:9/x"}"#).unwrap_err();
        assert_eq!(
            err_text(&err),
            "工具执行失败：已拒绝访问内网/本机地址（127.0.0.1），仅允许公网地址"
        );
        let err = exec(&t, r#"{"url":"http://[::1]:8080/"}"#).unwrap_err();
        assert!(err_text(&err).contains("（[::1]）"), "got: {err}");
        let err = exec(&t, r#"{"url":"http://localhost:8080/"}"#).unwrap_err();
        assert!(err_text(&err).contains("（localhost）"), "got: {err}");
    }

    // ── 重定向 ──

    #[test]
    fn follows_redirect_chain_with_hop_validation() {
        let base = spawn_canned(|idx, _| {
            if idx == 0 {
                let mut c = canned(302, "text/html", Vec::new());
                c.headers.push(("Location".into(), "/docs/article".into()));
                Some(c)
            } else {
                Some(canned(200, "text/plain; charset=utf-8", "重定向后正文到达"))
            }
        });
        let t = tool(5);
        let out = exec(&t, &format!(r#"{{"url":"{base}/start"}}"#)).unwrap();
        let head: String = out.summary.chars().take(40).collect();
        assert!(
            out.summary
                .starts_with("来源：127.0.0.1/docs/article（纯文本）"),
            "头部应展示最终 URL, got: {head}"
        );
        assert!(out.summary.contains("重定向后正文到达"));
    }

    #[test]
    fn redirect_to_private_target_rejected() {
        let base = spawn_canned(|_, _| {
            let mut c = canned(302, "text/html", Vec::new());
            c.headers
                .push(("Location".into(), "http://10.0.0.5:8080/secret".into()));
            Some(c)
        });
        let t = tool(5);
        let err = exec(&t, &format!(r#"{{"url":"{base}/start"}}"#)).unwrap_err();
        assert_eq!(
            err_text(&err),
            "工具执行失败：已拒绝访问内网/本机地址（10.0.0.5），仅允许公网地址"
        );
    }

    #[test]
    fn redirect_over_five_hops_rejected() {
        let base = spawn_canned(|idx, _| {
            let mut c = canned(302, "text/html", Vec::new());
            c.headers.push(("Location".into(), format!("/step{idx}")));
            Some(c)
        });
        let t = tool(5);
        let err = exec(&t, &format!(r#"{{"url":"{base}/hop0"}}"#)).unwrap_err();
        assert!(
            err_text(&err).contains("工具执行失败：重定向次数过多（超过 5 跳）"),
            "got: {err}"
        );
    }

    // ── HTTP 错误 ──

    #[test]
    fn http_404_reports_status() {
        let base = spawn_canned(|_, _| {
            Some(canned(
                404,
                "text/html",
                "<html><body>missing</body></html>",
            ))
        });
        let t = tool(5);
        let err = exec(&t, &format!(r#"{{"url":"{base}/missing"}}"#)).unwrap_err();
        assert!(
            err_text(&err).contains(&format!("工具执行失败：HTTP 404：{base}/missing")),
            "got: {err}"
        );
    }

    // ── 超时（注入 1s 短超时 + 静默服务器）──

    #[test]
    fn slow_server_times_out_with_injected_timeout() {
        let base = spawn_canned(|_, _| {
            thread::sleep(Duration::from_secs(3)); // 慢于客户端 1s 超时
            Some(canned(200, "text/plain", "迟到响应"))
        });
        let t = tool(1);
        let url = format!("{base}/slow");
        let err = exec(&t, &format!(r#"{{"url":"{url}"}}"#)).unwrap_err();
        assert!(
            err_text(&err).contains(&format!("工具执行失败：请求超时（1 秒）：{url}")),
            "got: {err}"
        );
    }

    // ── HTML 文章页 → readability 正文 ──

    /// 仿真文章页：chrome（nav/script/style/页脚/侧栏）+ <article> 正文
    /// （中文长句、表格、相对链接、长段落——覆盖清洗断言集）。
    fn article_html() -> String {
        let sentence = "源石技艺的现代研究最早可追溯到维多利亚工业革命时期，其能量转化效率取决于施术者的精神强度与源石晶体纯度，两者共同决定了所谓的天赋上限。";
        let mut paras = String::new();
        for i in 0..6 {
            paras.push_str(&format!("<p>{i}号段落：{sentence}{sentence}</p>"));
        }
        format!(
            r#"<!DOCTYPE html>
<html lang="zh-CN">
<head><meta charset="utf-8"><title>泰拉能源应用综述 - 示例站</title>
<style>.nav-item{{color:red}}</style></head>
<body>
<script>var track = "全站埋点脚本内容";</script>
<nav>全站导航目录在这里：<a href="/">首页</a><a href="/news">新闻</a><a href="/about">关于</a></nav>
<div id="sidebar">侧栏推荐位与广告横幅文字不属正文内容</div>
<article>
<h1>泰拉能源应用综述</h1>
<p>本文概述泰拉各国对源石能源的应用历史与现状，供罗德岛档案部门参考。</p>
{paras}
<p>干员档案简表如下：</p>
<table>
<thead><tr><th>干员</th><th>职业</th><th>源石适应性</th></tr></thead>
<tbody><tr><td>阿米娅</td><td>术师</td><td>卓越</td></tr><tr><td>煌</td><td>近卫</td><td>优良</td></tr></tbody>
</table>
<p>关于个体差异的进一步讨论，参见<a href="/docs/talent">天赋系统详解</a>。</p>
<p>{sentence}</p>
</article>
<footer>页脚链接与版权声明文字不应进入正文</footer>
</body></html>"#
        )
    }

    #[test]
    fn html_article_extracts_clean_markdown() {
        let base =
            spawn_canned(|_, _| Some(canned(200, "text/html; charset=utf-8", article_html())));
        let t = tool(5);
        let out = exec(&t, &format!(r#"{{"url":"{base}/news/energy"}}"#)).unwrap();
        assert!(
            out.summary
                .starts_with("来源：127.0.0.1/news/energy（网页正文）\n"),
            "got head: {}",
            out.summary.chars().take(40).collect::<String>()
        );
        assert!(
            out.summary.contains("源石技艺的现代研究最早可追溯到"),
            "中文长句应完整保留"
        );
        assert!(
            out.summary.contains("阿米娅") && out.summary.contains("术师"),
            "表格行应保留"
        );
        assert!(out.summary.contains('|'), "表格应转 markdown 管道符形态");
        assert!(out.summary.contains("天赋系统详解"), "链接文本应保留");
        assert!(
            out.summary.contains(&format!("]({base}/docs/talent)")),
            "相对链接应 absolutize（summary len={}）",
            out.summary.len()
        );
        for banned in [
            "全站导航目录在这里",
            "全站埋点脚本内容",
            "侧栏推荐位",
            "页脚链接与版权声明",
            "<script",
            "<style",
            "<nav",
        ] {
            assert!(
                !out.summary.contains(banned),
                "chrome 特征不应残留: {banned}"
            );
        }
    }

    // ── 非文章页 → 降级非空 ──

    #[test]
    fn non_article_html_degrades_nonempty() {
        let mut body = String::from(
            "<html><body><div class=\"banner\">促销横幅文字在这里滚动展示</div><div class=\"link-grid\">",
        );
        for i in 0..40 {
            body.push_str(&format!(
                "<div class=\"cell\"><a href=\"/c{i}\">条目卡片{i}</a><span>短说明{i}，都不足一长段。</span></div>"
            ));
        }
        body.push_str("</div><div>页面尾部统计文字</div></body></html>");
        let base = spawn_canned(move |_, _| Some(canned(200, "text/html", body.clone())));
        let t = tool(5);
        let out = exec(&t, &format!(r#"{{"url":"{base}/portal"}}"#)).unwrap();
        let head: String = out.summary.chars().take(40).collect();
        assert!(
            out.summary.contains("（网页文本）"),
            "降级格式标签应为网页文本, got: {head}"
        );
        assert!(out.summary.contains("促销横幅文字在这里"), "页面文本应保留");
        assert!(out.summary.contains("条目卡片3"), "降级输出非空且含内容");
        assert!(!out.summary.contains("<div"), "不应残留原始标签");
        assert!(!out.summary.contains("<a "), "不应残留原始标签");
    }

    #[test]
    fn strip_html_to_text_removes_scripts_styles_and_decodes_entities() {
        let html = r#"<html><head><style>.x{color:red}</style></head><body>
            <script>alert(1)</script>
            <noscript>无脚本提示</noscript>
            <template><p>模板内容</p></template>
            <!-- 注释 -->
            <p>正文内容 &amp; 数字实体 &#20320;</p>
        </body></html>"#;
        let text = strip_html_to_text(html);
        for banned in ["alert(1)", ".x{", "无脚本提示", "模板内容", "注释"] {
            assert!(!text.contains(banned), "应剥除: {banned}\n结果: {text}");
        }
        assert!(text.contains("正文内容"), "正文应保留\n结果: {text}");
        assert!(text.contains('你'), "数字实体应解码\n结果: {text}");
    }

    // ── JSON 分流 ──

    #[test]
    fn json_response_is_pretty_printed() {
        let body =
            r#"{"status":"ok","rows":[{"name":"罗德岛","type":"制药"}],"count":1}"#.to_string();
        let base = spawn_canned(move |_, _| {
            Some(canned(
                200,
                "application/json; charset=utf-8",
                body.as_bytes().to_vec(),
            ))
        });
        let t = tool(5);
        let out = exec(&t, &format!(r#"{{"url":"{base}/api/org"}}"#)).unwrap();
        let head: String = out.summary.chars().take(40).collect();
        assert!(out.summary.contains("（JSON）"), "got: {head}");
        assert!(out.summary.contains("\n  \"rows\""), "应 pretty 缩进");
        assert!(out.summary.contains("罗德岛"));
    }

    #[test]
    fn malformed_json_falls_back_to_raw() {
        let base = spawn_canned(|_, _| {
            Some(canned(
                200,
                "application/json",
                b"{ not valid json !!!".to_vec(),
            ))
        });
        let t = tool(5);
        let out = exec(&t, &format!(r#"{{"url":"{base}/api/bad"}}"#)).unwrap();
        assert!(
            out.summary.contains("{ not valid json !!!"),
            "解析失败应原样返回"
        );
    }

    // ── 纯文本分流 ──

    #[test]
    fn plain_text_passthrough() {
        let base = spawn_canned(|_, _| {
            Some(canned(
                200,
                "text/plain; charset=utf-8",
                "凯尔希：请保持理性。",
            ))
        });
        let t = tool(5);
        let out = exec(&t, &format!(r#"{{"url":"{base}/note.txt"}}"#)).unwrap();
        assert!(out.summary.contains("（纯文本）"));
        assert!(out.summary.contains("凯尔希：请保持理性。"));
    }

    // ── 体积上限 ──

    #[test]
    fn oversized_body_aborted_streaming() {
        let big = vec![b'x'; 1024 * 1024 + 64 * 1024]; // 1MiB + 64KiB
        let base = spawn_canned(move |_, _| Some(canned(200, "text/plain", big.clone())));
        let t = tool(5);
        let err = exec(&t, &format!(r#"{{"url":"{base}/huge"}}"#)).unwrap_err();
        assert!(
            err_text(&err).contains("工具执行失败：页面过大（超过 1MiB 上限）"),
            "got: {err}"
        );
    }

    // ── 截断 ──

    #[test]
    fn truncates_overlong_text_with_marker() {
        let body = "正文内容行。".repeat(3000); // 约 18000 字 > 默认 12000
        let base =
            spawn_canned(move |_, _| Some(canned(200, "text/plain; charset=utf-8", body.clone())));
        let t = tool(5);
        let out = exec(&t, &format!(r#"{{"url":"{base}/long.txt"}}"#)).unwrap();
        assert!(
            out.summary.contains("（正文过长，已截断至 12000 字符）"),
            "缺截断尾标"
        );
        assert!(out.summary.chars().count() < 12400, "截断应生效");
        assert!(out.summary.ends_with("字符）"), "尾标应在末尾");
    }
}
