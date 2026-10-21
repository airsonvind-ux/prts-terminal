//! 标题生成 · 主模型静默独立 run（会话自动标题）
//!
//! 语义对齐上下文压缩模块（`compaction.rs`）：与主对话共享同一 provider/模型
//! （由命令层传入已路由的 client），但**无工具、非流式、不 emit IPC、不写
//! session log、不产生对话轮事件**——标题是会话级元数据，不是轮事实。
//!
//! 请求级覆盖（本模块职责，返回前不改动调用方 config）：
//! - **禁思考**：`thinking_level = Some("off")`。现网全部有档位声明的模型声明集
//!   均含 off；未声明 reasoning 模型走推断集（恒含 off）；非 reasoning 模型
//!   天然不注入（`llm/thinking.rs::resolve_requested`）。
//! - **压小输出上限**：`max_tokens = min(现值, 64)`（标题极短，三族无兼容问题）。
//! - **15s 超时**：`tokio::time::timeout` 包裹（LlmConfig 无超时字段；适配器级
//!   HTTP_TIMEOUT 为 600s，对标题过宽）。
//!
//! 失败语义：任何失败（无 client / 超时 / 空文本 / 报错）→ 返回 None，调用方
//! 静默降级（保留默认标题），不重试。

use std::time::Duration;

use crate::debug;
use crate::llm::{ChatMessage, LlmClient, LlmConfig};

/// 标题素材单侧最大字符数（用户首问 / 首答可见文本各截 500）。
pub const MAX_MATERIAL_CHARS: usize = 500;

/// 标题输出 token 上限（压小；标题极短）。
pub const TITLE_MAX_TOKENS: u32 = 64;

/// 单次标题生成超时。
pub const TITLE_TIMEOUT: Duration = Duration::from_secs(15);

/// 标题硬上限（清洗后兜底截断；与前端改名 30 字上限对齐）。
pub const MAX_TITLE_CHARS: usize = 30;

/// 按字符（Unicode scalar）截取上限。
fn slice_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// 组装标题生成的 system + user 消息。
///
/// system 语义对齐既有实现 `summarizeTitle`（约 10 字、语言跟随用户首句、无标点）；
/// user 为 `用户：{首问}\n助手：{首答}`，各截 500 字符。
pub fn build_title_messages(user_text: &str, assistant_text: &str) -> Vec<ChatMessage> {
    let system =
        "你是一个对话标题生成器。根据用户和助手的第一轮对话，用一句极短的话概括对话主题。\n\n\
规则：\n\
1. 标题目标约 10 个字（中文）或 5 个单词（英文），保持极短\n\
2. 语言必须和用户说的第一句话一致：用户说中文就用中文，用户说英文就用英文\n\
3. 不要加引号、句号或其他标点\n\
4. 直接输出标题，不要解释";
    let u = slice_chars(user_text, MAX_MATERIAL_CHARS);
    let a = slice_chars(assistant_text, MAX_MATERIAL_CHARS);
    vec![
        ChatMessage::system(system),
        ChatMessage::user(format!("用户：{u}\n助手：{a}")),
    ]
}

/// 清洗模型输出：trim → 取首行 → 去首尾包裹标点/引号 → 硬上限 30 字。
///
/// 空串（清洗后）→ None（调用方按失败处理）。
pub fn clean_title(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 取首行（防模型输出多行/前后缀解释）
    let first = trimmed.lines().next().unwrap_or(trimmed).trim();
    // 去首尾包裹标点（引号/书名号/括号/句读）
    let stripped = first.trim_matches(|c: char| {
        matches!(
            c,
            '"' | '\''
                | '“'
                | '”'
                | '‘'
                | '’'
                | '《'
                | '》'
                | '「'
                | '」'
                | '『'
                | '』'
                | '【'
                | '】'
                | '（'
                | '）'
                | '('
                | ')'
                | '['
                | ']'
                | '。'
                | '，'
                | ','
                | '.'
                | '、'
                | '！'
                | '!'
                | '？'
                | '?'
                | ':'
                | '：'
                | '；'
                | ';'
        )
    });
    let stripped = stripped.trim();
    if stripped.is_empty() {
        return None;
    }
    Some(slice_chars(stripped, MAX_TITLE_CHARS))
}

/// 生成标题（默认 15s 超时）。
///
/// 失败（无文本 / 超时 / 报错）→ None。
#[allow(dead_code)] // 当前仅测试引用：生产走 generate_title_full
pub async fn generate_title(
    llm: &dyn LlmClient,
    config: &LlmConfig,
    user_text: &str,
    assistant_text: &str,
) -> Option<String> {
    generate_title_full(llm, config, user_text, assistant_text, TITLE_TIMEOUT)
        .await
        .title
}

/// 生成标题（可指定超时；测试用）。
#[allow(dead_code)] // 当前仅测试引用：生产走 generate_title_full
pub async fn generate_title_with_timeout(
    llm: &dyn LlmClient,
    config: &LlmConfig,
    user_text: &str,
    assistant_text: &str,
    timeout: Duration,
) -> Option<String> {
    generate_title_full(llm, config, user_text, assistant_text, timeout)
        .await
        .title
}

/// 标题生成结果（标题文本 + 本次调用用量）。
pub struct TitleResult {
    pub title: Option<String>,
    pub usage: Option<crate::llm::TokenUsage>,
}

/// 生成标题（返回标题 + 用量；命令层用于用量统计）。
pub async fn generate_title_full(
    llm: &dyn LlmClient,
    config: &LlmConfig,
    user_text: &str,
    assistant_text: &str,
    timeout: Duration,
) -> TitleResult {
    // 请求级覆盖：禁思考 + 压小输出上限（不改动调用方 config）
    let mut cfg = config.clone();
    cfg.thinking_level = Some("off".to_string());
    let capped = cfg.max_tokens.min(TITLE_MAX_TOKENS);
    cfg.max_tokens = if capped == 0 {
        TITLE_MAX_TOKENS
    } else {
        capped
    };

    let messages = build_title_messages(user_text, assistant_text);
    let run = llm.chat(&cfg, &messages, &[]);
    let resp = match tokio::time::timeout(timeout, run).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            debug!("action" => "title_generation_failed", "error" => e.to_string());
            return TitleResult {
                title: None,
                usage: None,
            };
        }
        Err(_) => {
            debug!("action" => "title_generation_timeout");
            return TitleResult {
                title: None,
                usage: None,
            };
        }
    };

    let usage = resp.usage;
    let text = resp.text.unwrap_or_default();
    let cleaned = clean_title(&text);
    if cleaned.is_none() {
        debug!("action" => "title_generation_empty");
    }
    TitleResult {
        title: cleaned,
        usage,
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::mock::MockLlmClient;
    use crate::llm::{LlmError, LlmResponse, StreamEvent, StreamResult, ToolDef};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;

    /// 记录 config 的 client：捕获 chat 收到的 config，返回固定文本。
    struct CaptureConfigClient {
        last_config: Mutex<Option<LlmConfig>>,
        reply: String,
    }

    impl CaptureConfigClient {
        fn new(reply: &str) -> Self {
            Self {
                last_config: Mutex::new(None),
                reply: reply.to_string(),
            }
        }
    }

    impl LlmClient for CaptureConfigClient {
        fn chat<'a>(
            &'a self,
            config: &'a LlmConfig,
            _messages: &'a [ChatMessage],
            _tools: &'a [ToolDef],
        ) -> Pin<Box<dyn Future<Output = Result<LlmResponse, LlmError>> + Send + 'a>> {
            Box::pin(async move {
                *self.last_config.lock().unwrap() = Some(config.clone());
                Ok(LlmResponse {
                    text: Some(self.reply.clone()),
                    tool_calls: None,
                    usage: None,
                })
            })
        }

        fn chat_stream<'a>(
            &'a self,
            config: &'a LlmConfig,
            _messages: &'a [ChatMessage],
            _tools: &'a [ToolDef],
            _on_event: Box<dyn Fn(StreamEvent) + Send + Sync>,
        ) -> Pin<Box<dyn Future<Output = Result<StreamResult, LlmError>> + Send + 'a>> {
            Box::pin(async move {
                *self.last_config.lock().unwrap() = Some(config.clone());
                Ok(StreamResult {
                    text: self.reply.clone(),
                    tool_calls: vec![],
                    reasoning: None,
                    usage: None,
                })
            })
        }
    }

    /// 永不返回的 client（超时测试用）。
    struct PendingClient;

    impl LlmClient for PendingClient {
        fn chat<'a>(
            &'a self,
            _config: &'a LlmConfig,
            _messages: &'a [ChatMessage],
            _tools: &'a [ToolDef],
        ) -> Pin<Box<dyn Future<Output = Result<LlmResponse, LlmError>> + Send + 'a>> {
            Box::pin(async move {
                std::future::pending::<()>().await;
                unreachable!()
            })
        }

        fn chat_stream<'a>(
            &'a self,
            _config: &'a LlmConfig,
            _messages: &'a [ChatMessage],
            _tools: &'a [ToolDef],
            _on_event: Box<dyn Fn(StreamEvent) + Send + Sync>,
        ) -> Pin<Box<dyn Future<Output = Result<StreamResult, LlmError>> + Send + 'a>> {
            Box::pin(async move {
                std::future::pending::<()>().await;
                unreachable!()
            })
        }
    }

    #[test]
    fn build_messages_truncates_material_to_500_each() {
        let long_user = "用".repeat(800);
        let long_assistant = "答".repeat(700);
        let msgs = build_title_messages(&long_user, &long_assistant);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, crate::llm::MessageRole::System);
        assert_eq!(msgs[1].role, crate::llm::MessageRole::User);
        // user 内容 = "用户：{500字}\n助手：{500字}"
        let body = &msgs[1].content;
        assert!(body.starts_with("用户："));
        assert!(body.contains("\n助手："));
        let after = body.split("\n助手：").nth(1).unwrap();
        assert_eq!(after.chars().count(), 500);
        // 前缀 "用户：" + 500 字
        let user_part = body
            .trim_start_matches("用户：")
            .split("\n助手：")
            .next()
            .unwrap();
        assert_eq!(user_part.chars().count(), 500);
        // system 含关键规则
        assert!(msgs[0].content.contains("约 10 个字"));
        assert!(msgs[0].content.contains("不要加引号"));
    }

    #[test]
    fn clean_title_strips_wrapping_punctuation_and_multiline() {
        assert_eq!(clean_title("  源石虫查询  ").as_deref(), Some("源石虫查询"));
        assert_eq!(clean_title("“伤害计算”").as_deref(), Some("伤害计算"));
        assert_eq!(clean_title("《集成战略》").as_deref(), Some("集成战略"));
        assert_eq!(clean_title("标题。").as_deref(), Some("标题"));
        // 多行 → 取首行
        assert_eq!(
            clean_title("Buff机制\n解释：...").as_deref(),
            Some("Buff机制")
        );
        // 空 / 纯标点 → None
        assert_eq!(clean_title("   "), None);
        assert_eq!(clean_title("\"\""), None);
    }

    #[test]
    fn clean_title_caps_at_30_chars() {
        let long = "标".repeat(50);
        let out = clean_title(&long).unwrap();
        assert_eq!(out.chars().count(), 30);
    }

    #[tokio::test]
    async fn generate_title_success_and_config_override() {
        let client = CaptureConfigClient::new("  源石虫数值查询！ ");
        let cfg = LlmConfig::default();
        let title = generate_title(&client, &cfg, "查源石虫", "源石虫攻击力是 350").await;
        assert_eq!(title.as_deref(), Some("源石虫数值查询"));
        // 请求级覆盖断言
        let captured = client.last_config.lock().unwrap().clone().unwrap();
        assert_eq!(captured.thinking_level.as_deref(), Some("off"));
        assert_eq!(captured.max_tokens, TITLE_MAX_TOKENS);
        // 调用方 config 未被改动
        assert_eq!(cfg.thinking_level, None);
        assert_eq!(cfg.max_tokens, 4096);
    }

    #[tokio::test]
    async fn generate_title_empty_text_returns_none() {
        let mock = MockLlmClient::new(vec![crate::llm::mock::MockResponse {
            events: vec![],
            result: StreamResult {
                text: "".into(),
                tool_calls: vec![],
                reasoning: None,
                usage: None,
            },
        }]);
        let out = generate_title(&mock, &LlmConfig::default(), "问", "答").await;
        assert_eq!(out, None);
    }

    #[tokio::test]
    async fn generate_title_error_returns_none() {
        // Mock 无预设 → chat 返回 Err
        let mock = MockLlmClient::new(vec![]);
        let out = generate_title(&mock, &LlmConfig::default(), "问", "答").await;
        assert_eq!(out, None);
    }

    #[tokio::test]
    async fn generate_title_timeout_returns_none() {
        let out = generate_title_with_timeout(
            &PendingClient,
            &LlmConfig::default(),
            "问",
            "答",
            Duration::from_millis(50),
        )
        .await;
        assert_eq!(out, None);
    }
}
