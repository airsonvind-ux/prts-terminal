//! tools::web_search —— 关键词搜索工具（M5 网络域）
//!
//! 输入关键词 → 免费匿名搜索 API → 结果列表（标题 + URL + 摘要）。
//! 定位中性：agent 基本自由检索能力，描述不绑定领域用途。
//!
//! ## 架构
//! - `WebSearchProvider` trait：搜索后端薄接口（注入形态——测试用 fake
//!   后端离线确定性覆盖命中/空/限流/错误四型，不触碰真实网络）
//! - `AnysearchFreeBackend`：免费匿名 anysearch_free 薄封装（仿既有实现
//!   search-providers.ts / web-search.ts；端点 api.anysearch.com/v1/search，
//!   POST JSON {query, max_results, language:"zh-CN"}，无鉴权头）
//! - 请求级超时 30s；结果解析为纯函数（可离线单测真实 provider 的
//!   envelope/扁平/错误三种响应结构）
//!
//! 输出：canonical {query, results:[{title,url,snippet}]}；summary 编号列表
//! 中文渲染。错误统一中文文案（限流/HTTP/服务错误/网络失败）。

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};

// ═══════════════════════════════════════════════════════
// 常量
// ═══════════════════════════════════════════════════════

/// 请求超时（M5 定稿 30s）。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// 默认返回条数。
const DEFAULT_MAX_RESULTS: u32 = 10;
/// maxResults 钳制（schema 声明 + 执行内再钳）。
const MIN_MAX_RESULTS: u32 = 1;
const MAX_MAX_RESULTS: u32 = 20;
/// summary 中单条摘要的最大字符数（渲染层裁剪，canonical 保持完整）。
const SNIPPET_RENDER_CHARS: usize = 300;

/// anysearch_free 免费匿名端点（既有实现同源参数/结构）。
const ANYSEARCH_URL: &str = "https://api.anysearch.com/v1/search";
const ANYSEARCH_LANGUAGE: &str = "zh-CN";

// ═══════════════════════════════════════════════════════
// 数据结构与后端 trait
// ═══════════════════════════════════════════════════════

/// 单条搜索结果（canonical 序列化字段名 = 设计定稿 title/url/snippet）。
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

impl SearchResult {
    fn from_anysearch_item(item: &Value) -> Option<Self> {
        let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let url = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
        if url.is_empty() {
            return None; // 无目标 URL 的结果对 agent 无用
        }
        // snippet 优先 content（既有实现同规则），再退 description/snippet
        let snippet = ["content", "description", "snippet"]
            .iter()
            .find_map(|k| {
                item.get(*k)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or("");
        Some(Self {
            title: title.to_string(),
            url: url.to_string(),
            snippet: snippet.to_string(),
        })
    }
}

/// 搜索后端错误（中文文案在 Display/映射层，与消息模板同前缀）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    /// 限流（HTTP 429/402）
    RateLimited,
    /// 非 2xx
    HttpStatus(u16),
    /// 服务端业务错误（envelope code != 0）
    Api(String),
    /// 网络/解析失败
    Network(String),
}

impl SearchError {
    /// 转 ToolError（统一"工具执行失败："前缀 + 中文文案）。
    pub fn to_tool_error(&self) -> ToolError {
        let msg = match self {
            SearchError::RateLimited => "搜索服务限流（HTTP 429），请稍后重试".to_string(),
            SearchError::HttpStatus(code) => format!("搜索服务返回 HTTP {code}"),
            SearchError::Api(m) => format!("搜索服务错误：{m}"),
            SearchError::Network(m) => format!("搜索请求失败：{m}"),
        };
        ToolError::Execution(msg)
    }
}

/// 搜索后端薄接口（fake 注入测试 / anysearch_free 生产实现）。
/// Send + Sync：工具实例可跨线程并发执行（is_concurrency_safe=true 契约）。
pub trait WebSearchProvider: Send + Sync {
    fn search(&self, query: &str, max_results: u32) -> Result<Vec<SearchResult>, SearchError>;
}

// ═══════════════════════════════════════════════════════
// 工具定义
// ═══════════════════════════════════════════════════════

pub struct WebSearchTool {
    backend: Arc<dyn WebSearchProvider>,
}

impl WebSearchTool {
    pub fn new(backend: Arc<dyn WebSearchProvider>) -> Self {
        Self { backend }
    }

    /// 生产装配：免费匿名 anysearch_free 后端。
    pub fn anysearch_default() -> Self {
        Self::new(Arc::new(AnysearchFreeBackend::new()))
    }
}

impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "在互联网上搜索关键词并返回结果列表（标题+链接+摘要），用于获取实时信息：最新新闻、技术文档、时事、或任何不在本地知识库内的外部信息。独立通用检索能力，不限定领域。经免费匿名搜索服务查询（请求超时 30 秒），结果按相关度排序；无结果、服务限流或错误时返回中文提示"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "minLength": 1,
                    "description": "搜索关键词（1~100 字内更佳）"
                },
                "maxResults": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 20,
                    "description": "最大返回条数，默认 10，上限 20（自动钳制）"
                }
            },
            "required": ["query"]
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(params) else {
            return ToolDisplay::default();
        };
        match v
            .get("query")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
        {
            Some(q) => ToolDisplay::info(q),
            None => ToolDisplay::default(),
        }
    }

    fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        // ── 1. 解析参数（query 去空白后非空；maxResults 钳制 1..=20）──
        let v: Value = serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;
        let query = match v.get("query").and_then(|x| x.as_str()) {
            Some(q) if !q.trim().is_empty() => q.trim().to_string(),
            Some(_) => return Err(ToolError::Parse("参数 query 不能为空".into())),
            None => return Err(ToolError::Parse("缺少必填参数: query".into())),
        };
        let max_results = clamp_max_results(v.get("maxResults").and_then(|x| x.as_u64()));

        // ── 2. 调用后端 ──
        let results = self
            .backend
            .search(&query, max_results)
            .map_err(|e| e.to_tool_error())?;
        // 后端返回量再按 max_results 收敛（双保险）
        let results: Vec<SearchResult> = results.into_iter().take(max_results as usize).collect();

        // ── 3. 渲染输出 ──
        let canonical = serde_json::json!({
            "query": query,
            "results": results,
        });
        if results.is_empty() {
            return Ok(ToolOutput::json(
                canonical,
                format!("未找到与「{query}」相关的结果"),
            ));
        }
        let items: Vec<String> = results
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let snippet: String = r
                    .snippet
                    .chars()
                    .take(SNIPPET_RENDER_CHARS)
                    .collect::<String>()
                    .trim_end()
                    .to_string();
                format!("{}. **{}**\n   {}\n   {}", i + 1, r.title, r.url, snippet)
            })
            .collect();
        let summary = format!("共 {} 条结果：\n{}", results.len(), items.join("\n"));
        Ok(ToolOutput::json(canonical, summary))
    }
}

// ═══════════════════════════════════════════════════════
// anysearch_free 免费匿名后端（生产薄封装）
// ═══════════════════════════════════════════════════════

pub struct AnysearchFreeBackend;

impl AnysearchFreeBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AnysearchFreeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl WebSearchProvider for AnysearchFreeBackend {
    fn search(&self, query: &str, max_results: u32) -> Result<Vec<SearchResult>, SearchError> {
        let (url, body) = build_anysearch_request(query, max_results);
        let client = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent("Mozilla/5.0 (compatible; PRTS-Terminal/0.1)")
            .no_proxy()
            .build()
            .map_err(|e| SearchError::Network(e.to_string()))?;
        let resp = client.post(&url).json(&body).send().map_err(|e| {
            if e.is_timeout() {
                SearchError::Network(format!("请求超时（{} 秒）", REQUEST_TIMEOUT.as_secs()))
            } else {
                SearchError::Network(e.to_string())
            }
        })?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .map_err(|e| SearchError::Network(format!("读取响应失败：{e}")))?;

        // HTTP 层错误优先（429/402 = 限流；其余非 2xx = 服务状态）
        if status.as_u16() == 429 || status.as_u16() == 402 {
            return Err(SearchError::RateLimited);
        }
        if !status.is_success() {
            return Err(SearchError::HttpStatus(status.as_u16()));
        }
        parse_anysearch_results(&bytes, max_results)
    }
}

// ═══════════════════════════════════════════════════════
// anysearch 请求/响应纯函数（离线可测）
// ═══════════════════════════════════════════════════════

/// 组装请求：(端点 URL, JSON body {query, max_results, language})。
fn build_anysearch_request(query: &str, max_results: u32) -> (String, Value) {
    let body = serde_json::json!({
        "query": query,
        "max_results": max_results,
        "language": ANYSEARCH_LANGUAGE,
    });
    (ANYSEARCH_URL.to_string(), body)
}

/// 解析 anysearch 响应体 → 结果列表。
/// 兼容两种结构（既有实现 anySearchResultsFrom 同规则）：
/// - envelope：{code, message?, data:{results:[...], metadata?}}
/// - 扁平：{results:[...]} / {code:0, results:[...]}
/// code 非 0（业务错误）→ Api(message 或 code)；结果字段 content 优先
/// description/snippet（既有实现同规则）；无 url 项丢弃；超出 limit 截断。
fn parse_anysearch_results(bytes: &[u8], limit: u32) -> Result<Vec<SearchResult>, SearchError> {
    let v: Value = serde_json::from_slice(bytes)
        .map_err(|e| SearchError::Network(format!("响应解析失败：{e}")))?;

    // envelope 业务码：显式非 0 → 服务错误
    if let Some(code) = v.get("code").and_then(|c| c.as_i64()) {
        if code != 0 {
            let msg = v
                .get("message")
                .and_then(|m| m.as_str())
                .filter(|m| !m.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("code {code}"));
            return Err(SearchError::Api(msg));
        }
    }

    // 结果源：data.results（envelope）或 results（扁平）
    let items = match v.get("data").and_then(|d| d.get("results")) {
        Some(arr) => arr,
        None => v.get("results").unwrap_or(&Value::Null),
    };
    let arr = items
        .as_array()
        .ok_or_else(|| SearchError::Network("响应结构异常：缺少 results 数组".into()))?;

    let out: Vec<SearchResult> = arr
        .iter()
        .filter_map(SearchResult::from_anysearch_item)
        .take(limit as usize)
        .collect();
    Ok(out)
}

/// maxResults 钳制（供测试直接断言）。
pub fn clamp_max_results(v: Option<u64>) -> u32 {
    match v {
        Some(n) => (n as u32).clamp(MIN_MAX_RESULTS, MAX_MAX_RESULTS),
        None => DEFAULT_MAX_RESULTS,
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════
// （fake 四型 + anysearch 解析用例经两次 edit 追加，见下方锚点）
#[cfg(test)]
mod tests {
    use super::*;

    // ── fake 后端（离线确定性：命中/空/限流/服务错误四型）──

    #[derive(Clone, Copy)]
    enum FakeCase {
        Hit,
        Empty,
        RateLimited,
        ApiError,
    }

    struct FakeBackend {
        case: FakeCase,
        /// 记录每次请求的 max_results（钳制断言用）。
        seen: Arc<std::sync::Mutex<Vec<u32>>>,
    }

    impl FakeBackend {
        fn new(case: FakeCase) -> Self {
            Self {
                case,
                seen: Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }
        fn recorded(&self) -> Vec<u32> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl WebSearchProvider for FakeBackend {
        fn search(&self, _query: &str, max_results: u32) -> Result<Vec<SearchResult>, SearchError> {
            self.seen.lock().unwrap().push(max_results);
            match self.case {
                FakeCase::Hit => Ok(vec![
                    SearchResult {
                        title: "明日方舟 泰拉能源综述".into(),
                        url: "https://example.com/a".into(),
                        snippet: "关于源石能源应用的中文综述页面摘要内容。".into(),
                    },
                    SearchResult {
                        title: "PRTS 百科 - 源石技艺".into(),
                        url: "https://example.com/b".into(),
                        snippet: "技能机制与数值说明。".into(),
                    },
                ]),
                FakeCase::Empty => Ok(Vec::new()),
                FakeCase::RateLimited => Err(SearchError::RateLimited),
                FakeCase::ApiError => Err(SearchError::Api("临时限流额度耗尽".into())),
            }
        }
    }

    fn exec(tool: &WebSearchTool, params: &str) -> Result<ToolOutput, ToolError> {
        tool.execute(&ToolContext::new(), params)
    }

    fn hit_tool() -> WebSearchTool {
        WebSearchTool::new(Arc::new(FakeBackend::new(FakeCase::Hit)))
    }

    // ── 命中：canonical + 编号列表 summary ──

    #[test]
    fn hit_returns_canonical_and_numbered_summary() {
        let t = hit_tool();
        let out = exec(&t, r#"{"query":"明日方舟 能源"}"#).unwrap();
        // canonical 结构
        assert_eq!(out.canonical["query"], "明日方舟 能源");
        let results = out.canonical["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["title"], "明日方舟 泰拉能源综述");
        assert_eq!(results[0]["url"], "https://example.com/a");
        assert_eq!(
            results[0]["snippet"],
            "关于源石能源应用的中文综述页面摘要内容。"
        );
        // summary：中文编号列表，含标题/链接/摘要
        assert!(
            out.summary.starts_with("共 2 条结果："),
            "got: {}",
            out.summary
        );
        assert!(
            out.summary.contains("1. **明日方舟 泰拉能源综述**"),
            "got: {}",
            out.summary
        );
        assert!(out.summary.contains("https://example.com/b"));
        assert!(out.summary.contains("技能机制与数值说明。"));
    }

    #[test]
    fn hit_respects_max_results() {
        let t = hit_tool();
        // 后端只给 2 条，maxResults=1 → canonical 只保留 1 条
        let out = exec(&t, r#"{"query":"明日方舟","maxResults":1}"#).unwrap();
        assert_eq!(out.canonical["results"].as_array().unwrap().len(), 1);
        assert!(out.summary.contains("共 1 条结果："));
    }

    // ── 空结果 ──

    #[test]
    fn empty_results_reports_not_found() {
        let t = WebSearchTool::new(Arc::new(FakeBackend::new(FakeCase::Empty)));
        let out = exec(&t, r#"{"query":"不存在的关键词xyz"}"#).unwrap();
        assert_eq!(out.canonical["results"], serde_json::json!([]));
        assert_eq!(out.summary, "未找到与「不存在的关键词xyz」相关的结果");
    }

    // ── 错误型：限流 / 服务错误 → 中文 ToolError ──

    #[test]
    fn rate_limited_maps_to_chinese_error() {
        let t = WebSearchTool::new(Arc::new(FakeBackend::new(FakeCase::RateLimited)));
        let err = exec(&t, r#"{"query":"测试"}"#).unwrap_err();
        assert_eq!(
            err.to_string(),
            "工具执行失败：搜索服务限流（HTTP 429），请稍后重试"
        );
    }

    #[test]
    fn api_error_maps_to_chinese_error() {
        let t = WebSearchTool::new(Arc::new(FakeBackend::new(FakeCase::ApiError)));
        let err = exec(&t, r#"{"query":"测试"}"#).unwrap_err();
        assert_eq!(
            err.to_string(),
            "工具执行失败：搜索服务错误：临时限流额度耗尽"
        );
    }

    // ── 参数校验 ──

    #[test]
    fn missing_or_blank_query_rejected() {
        let t = hit_tool();
        let err = exec(&t, r#"{}"#).unwrap_err();
        assert!(
            err.to_string().contains("缺少必填参数: query"),
            "got: {err}"
        );
        let err = exec(&t, r#"{"query":"   "}"#).unwrap_err();
        assert!(err.to_string().contains("query 不能为空"), "got: {err}");
    }

    // ── maxResults 钳制（记录注入断言）──

    #[test]
    fn max_results_clamped_and_forwarded() {
        let backend = Arc::new(FakeBackend::new(FakeCase::Hit));
        let t = WebSearchTool::new(backend.clone()); // Arc<FakeBackend> → Arc<dyn> 参数位自动 unsize
                                                     // 缺省 → 10；超上限 999 → 20；下限 0 → 1
        let _ = exec(&t, r#"{"query":"a"}"#).unwrap();
        let _ = exec(&t, r#"{"query":"a","maxResults":999}"#).unwrap();
        let _ = exec(&t, r#"{"query":"a","maxResults":0}"#).unwrap();
        let seen = backend.recorded();
        assert_eq!(seen, vec![10, 20, 1], "钳制后传给后端：缺省10/上限20/下限1");
    }

    // ── anysearch 请求/响应纯函数 ──

    #[test]
    fn anysearch_request_body_shape() {
        let (url, body) = build_anysearch_request("明日方舟 攻略", 10);
        assert_eq!(url, "https://api.anysearch.com/v1/search");
        assert_eq!(body["query"], "明日方舟 攻略");
        assert_eq!(body["max_results"], 10);
        assert_eq!(body["language"], "zh-CN");
    }

    #[test]
    fn anysearch_parse_envelope_with_data_results() {
        // 既有实现主结构：{code:0, data:{results:[{title,url,content,...}], metadata}}
        let body = r#"{
            "code": 0,
            "message": "ok",
            "data": {
                "results": [
                    {"title": "泰拉能源综述", "url": "https://a.example/x", "content": "正文摘要甲"},
                    {"title": "无URL条目", "snippet": "应被过滤"},
                    {"title": "条目乙", "url": "https://b.example/y", "description": "只有描述字段"}
                ],
                "metadata": {"total_results": 100}
            }
        }"#
        .as_bytes();
        let results = parse_anysearch_results(body, 10).unwrap();
        assert_eq!(results.len(), 2, "无 url 项应过滤");
        assert_eq!(results[0].title, "泰拉能源综述");
        assert_eq!(results[0].snippet, "正文摘要甲", "content 优先");
        assert_eq!(results[1].snippet, "只有描述字段", "description 退路");
    }

    #[test]
    fn anysearch_parse_flat_results_with_snippet_field() {
        // 扁平结构：{results:[...]}；snippet 字段退路
        let body = r#"{"code":0,"results":[{"title":"T","url":"https://c.example/z","snippet":"摘要丙"}]}"#
            .as_bytes();
        let results = parse_anysearch_results(body, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].snippet, "摘要丙");
    }

    #[test]
    fn anysearch_parse_limit_truncates() {
        let body = br#"{"results":[
            {"title":"1","url":"https://x/1"},
            {"title":"2","url":"https://x/2"},
            {"title":"3","url":"https://x/3"}
        ]}"#;
        assert_eq!(parse_anysearch_results(body, 2).unwrap().len(), 2);
        assert_eq!(parse_anysearch_results(body, 99).unwrap().len(), 3);
    }

    #[test]
    fn anysearch_parse_business_error_code() {
        let body = br#"{"code":429,"message":"quota exceeded for free tier"}"#;
        let err = parse_anysearch_results(body, 10).unwrap_err();
        assert_eq!(err, SearchError::Api("quota exceeded for free tier".into()));
        // 无 message → code 兜底
        let body = br#"{"code":500}"#;
        let err = parse_anysearch_results(body, 10).unwrap_err();
        assert_eq!(err, SearchError::Api("code 500".into()));
    }

    #[test]
    fn anysearch_parse_garbage_or_wrong_shape() {
        let err = parse_anysearch_results(b"not json at all", 10).unwrap_err();
        assert!(matches!(err, SearchError::Network(_)), "got: {err:?}");
        let err = parse_anysearch_results(br#"{"data":{"foo":1}}"#, 10).unwrap_err();
        assert!(
            matches!(err, SearchError::Network(_)),
            "缺 results 应为结构错误"
        );
    }
}
