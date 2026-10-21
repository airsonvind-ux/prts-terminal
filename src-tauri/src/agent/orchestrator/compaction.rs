//! 上下文压缩 · 摘要 run（模块 D · 上下文衰减）
//!
//! 压缩 = 前端发起的一次对话级状态转换（手动、单档）。本模块只承载后端一半：
//! 对前端折叠区的派生态历史（`folded_history`）做一次**静默独立 LLM 摘要调用**，
//! 返回结构化 checkpoint 正文（## Goal / Progress / Key Decisions / Next Steps…）。
//! 压缩记录（摘要 + 折叠边界）由前端写入会话元数据——本模块**不产生
//! SessionEvent、不 emit IPC、不触碰 session log 词汇表**（约束：session.rs
//! 零改动；摘要不是对话轮，无事件流形态）。
//!
//! ## 静默语义
//!
//! 摘要 run 与主对话共享同一 provider/模型（由命令层传入已路由的 client），
//! 但：system（含记忆区）、工具面、折叠区消息与主请求**逐字节同源**：请求即
//! 主请求真前缀，复用提供方 KV cache（DSH 形态）；流式事件全部丢弃（不推 IPC）、输出不进入
//! 任何 log。推理链策略与主对话一致（`Preserve`——裁决：思考强度设置挂账，
//! 本模块不引入第二策略面）。
//!
//! ## 输入超窗降级链（裁决 D，实现顺序微调为可测的线性链）
//!
//! 预算 = `0.6 × 当前模型 contextWindow`。estimate 超预算时依次降级：
//! 1. 丢弃全部推理链（reasoning_content → None，低损降级）；
//! 2. 单条超长消息头尾截断（各留 40%，中间省略标记）；
//! 3. 仍超 → **规则降级**：不调 LLM，把折叠区渲染为规则抽取的大纲式
//!    checkpoint（user 提问要点 + assistant 结论 + 工具调用清单），文本内
//!    自带「规则生成，可能遗漏细节」标记，`degraded=true` 回传前端提示。
//!
//! 折叠区本身受前端保留段预算（20K token 常数）约束，超窗属异常路径；
//! 降级保证压缩动作不被 LLM 输入限制阻塞（失败可重试语义在产品决策层）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::debug;
use crate::events::FrontendMessage;
use crate::llm::{ChatMessage, LlmClient, LlmConfig, MessageRole, ReasoningPolicy, StreamEvent, ToolDef, TokenUsage};
use crate::session::SessionLog;
use crate::turn_registry::AbortSignal;

use super::context::estimate_messages;
use super::{is_retryable, RetryPolicy};

/// 摘要 run 输入占窗口的最大比例（超窗触发降级链）。
pub const MAX_SUMMARY_INPUT_RATIO: f64 = 0.6;

/// 摘要输出期望上限（写入指令约束模型；非硬限制，指令层约束）。
const SUMMARY_TARGET_CHARS: usize = 900;

/// 单条消息硬截断的字符上限（超过则头尾各留 40%，中间省略）。
const MAX_SINGLE_MESSAGE_CHARS: usize = 8_000;

/// 截断保留比例（头尾各 40%）。
const TRUNCATE_KEEP_RATIO: f64 = 0.4;

/// 未知模型（不在 models.json）时的窗口兜底：deepseek 历史最小档。
pub const DEFAULT_CONTEXT_WINDOW: u32 = 65_536;

/// 播种占位 session_id（摘要 run 的 log 是临时内存镜像，id 无路由语义）。
const SUMMARY_SESSION_ID: &str = "compaction-summary";

/// 规则降级摘要在文本内自带的标记（重启后仅读 meta 也能识别降级产物）。
const FALLBACK_MARKER: &str = "（规则生成摘要——输入超窗未走 LLM，可能遗漏细节）";

/// 摘要 run 结果（回传命令层 → 前端写入会话元数据）。
#[derive(Debug)]
pub struct SummaryOutcome {
    /// 结构化 checkpoint 正文（不含定界块——壳由前端合成）
    pub summary: String,
    /// true = 规则降级产出（无 LLM 调用）
    pub degraded: bool,
    /// 折叠区压缩前估算 token（仅折叠消息本身）
    pub tokens_before: Option<u64>,
    /// 本次摘要 LLM 调用的用量（降级产出时为 None）。
    pub usage: Option<TokenUsage>,
}

/// 执行一次摘要 run。
///
/// # 参数
/// * `llm` — 命令层已按 config.api_type 路由的 client（与主对话同 provider/模型）
/// * `config` — LLM 连接配置（turn 起始快照）
/// * `folded_history` — 前端折叠区的派生态历史（含链式压缩时的旧摘要消息）
/// * `context_window` — 当前模型 contextWindow（命令层经 model_registry 查得，同源）
/// * `system_prompt` — 对话冻结的完整前缀全文（与主对话同源，批 5 起由命令层
///   从 `prefix_snapshot.rs` 取；人格 + 界面 + 目录 + 记忆区）
/// * `tool_defs` — 主对话工具面快照（同一冻结前缀；仅保请求前缀同源）
/// * `abort` — 压缩注册项的中止信号（取消通道）
///
/// 返回 Err(String)：错误文案面向用户（toast 直显）。
pub async fn run_summary(
    llm: &dyn LlmClient,
    config: &LlmConfig,
    folded_history: Vec<FrontendMessage>,
    context_window: u32,
    system_prompt: &str,
    tool_defs: &[ToolDef],
    abort: &AbortSignal,
) -> Result<SummaryOutcome, String> {
    // ── 1. 播种 + 派生（Preserve：与主对话推理链策略一致）──
    // 复用既有纯函数链：折叠区前端已过滤 error 与结构事件，此处为形态规范化
    // 与防御（未知 role 兜底），与 agent_process 的播种路径同构。
    let mut log = SessionLog::new();
    log.seed_from_history(SUMMARY_SESSION_ID, folded_history);
    let folded_messages = log.derive_messages(ReasoningPolicy::Preserve);
    if folded_messages.is_empty() {
        return Err("没有可压缩的内容".into());
    }
    let tokens_before = estimate_messages(&folded_messages) as u64;

    // ── 2. system（与主对话前缀逐字节一致）+ 指令 ──
    // 前缀用对话冻结快照（命令层取）：压缩请求必须复用主请求真前缀的
    // 全部字节（人格 / 界面 / 目录 / 记忆区）——现拼会在任一段变化时
    // 自断 KV cache 复用。
    let system = system_prompt.to_string();
    let instruction = ChatMessage::user(build_summary_instruction());
    let mut messages: Vec<ChatMessage> = Vec::with_capacity(2 + folded_messages.len());
    messages.push(ChatMessage::system(system));
    messages.extend(folded_messages);
    messages.push(instruction);

    // ── 3. 超窗预检与降级链（裁决 D）──
    let budget = ((context_window as u32 as f64) * MAX_SUMMARY_INPUT_RATIO).floor() as usize;
    if estimate_messages(&messages) > budget {
        strip_reasoning(&mut messages);
        if estimate_messages(&messages) > budget {
            truncate_long_messages(&mut messages);
        }
    }
    let degraded = estimate_messages(&messages) > budget;

    // ── 4. 产出 ──
    let (summary, usage) = if degraded {
        (build_fallback_summary(&messages), None)
    } else {
        call_summary_llm(llm, config, &messages, tool_defs, abort).await?
    };

    Ok(SummaryOutcome {
        summary,
        degraded,
        tokens_before: Some(tokens_before),
        usage,
    })
}

/// 摘要指令（user 形态，置于输入末尾；要求模型只输出 checkpoint 正文）。
fn build_summary_instruction() -> String {
    format!(
        "\
[内部压缩指令——非用户消息，无需回应]\n\
你是 PRTS 终端的对话压缩器。上方是一场较早的历史对话（含工具调用与推理过程），\
它将被压缩归档、彻底移出模型上下文。请把它压缩为一份结构化任务摘要，\
供后续对话作为新起点使用。\n\n\
要求：\n\
- 只输出下面的摘要正文本身，不输出任何多余说明、mood 或思考过程；\n\
- 不调用任何工具；\n\
- 只总结事实与任务状态，不编造摘要中不存在的信息；\n\
- 区分「已完成 / 进行中 / 受阻」，保留关键结论、数据出处线索与后续步骤；\n\
- 工具查询类结果不罗列原始数据，标注「（可重查）」即可；\n\
- 保留与原文一致的关键词（干员名、关卡名、机制术语），总量控制在 {SUMMARY_TARGET_CHARS} 字内。\n\n\
## Goal\n\
[用户正在达成什么目标；可多项]\n\n\
## Constraints & Preferences\n\
- [约束 / 偏好 / 要求；无则写（无）]\n\n\
## Progress\n\
### Done\n\
- [已完成]\n\n\
### In Progress\n\
- [进行中]\n\n\
### Blocked\n\
- [受阻问题；无则写（无）]\n\n\
## Key Decisions\n\
- [关键决策与理由]\n\n\
## Next Steps\n\
1. [下一步]",
        SUMMARY_TARGET_CHARS = SUMMARY_TARGET_CHARS
    )
}

/// 降级链 1：丢弃全部推理链（reasoning_content → None）。
/// 输入超窗时的低损降级——推理是思考过程，checkpoint 只关心结论。
fn strip_reasoning(messages: &mut [ChatMessage]) {
    for m in messages.iter_mut() {
        m.reasoning_content = None;
    }
}

/// 降级链 2：单条超长消息头尾截断（各留 40%，中间省略标记）。
fn truncate_long_messages(messages: &mut [ChatMessage]) {
    for m in messages.iter_mut() {
        if m.content.chars().count() > MAX_SINGLE_MESSAGE_CHARS {
            m.content = truncate_head_tail(&m.content, MAX_SINGLE_MESSAGE_CHARS);
        }
    }
}

/// UTF-8 安全的头尾截断（按 char 边界，中文安全）。
fn truncate_head_tail(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let keep = ((max_chars as f64) * TRUNCATE_KEEP_RATIO) as usize;
    let head: String = text.chars().take(keep).collect();
    let tail: String = text.chars().skip(total - keep).collect();
    let omitted = total - keep * 2;
    format!("{head}\n\n[… {omitted} 字符已省略 …]\n\n{tail}")
}

/// 降级链 3：规则降级摘要（无 LLM）。折叠区消息渲染为大纲式 checkpoint，
/// 文本自带标记（FALLBACK_MARKER），重启后仅读 meta 也能识别降级产物。
fn build_fallback_summary(messages: &[ChatMessage]) -> String {
    let mut tool_counts: Vec<(String, usize)> = Vec::new();
    let mut done_items: Vec<String> = Vec::new();
    let mut user_questions: Vec<String> = Vec::new();

    for m in messages {
        match m.role {
            MessageRole::User => {
                if let Some(line) = first_line(&m.content, 80) {
                    user_questions.push(line);
                }
            }
            MessageRole::Assistant => {
                if let Some(calls) = &m.tool_calls {
                    for tc in calls {
                        let name = tc.function.name.clone();
                        if let Some(entry) = tool_counts.iter_mut().find(|(n, _)| *n == name) {
                            entry.1 += 1;
                        } else {
                            tool_counts.push((name, 1));
                        }
                    }
                }
                if let Some(text) = first_line(&m.content, 120) {
                    if done_items.len() < 8 {
                        done_items.push(text);
                    }
                }
            }
            // tool 结果与 system 不参与规则抽取（量大 / 前缀）
            _ => {}
        }
    }

    let mut out = String::new();
    out.push_str(FALLBACK_MARKER);
    out.push_str("\n\n## Goal\n");
    if user_questions.is_empty() {
        out.push_str("（无法可靠归纳——请以保留的近期对话为准）\n");
    } else {
        for q in user_questions.iter().take(6) {
            out.push_str(&format!("- 用户方向：{q}\n"));
        }
    }
    out.push_str("\n## Progress\n### Done\n");
    if done_items.is_empty() {
        out.push_str("- （无可靠条目——规则摘要不归纳细节）\n");
    } else {
        for item in &done_items {
            out.push_str(&format!("- {item}\n"));
        }
    }
    out.push_str("\n### In Progress\n- （规则摘要无法可靠判定，以最近对话为准）\n");
    out.push_str("\n## Key Decisions\n- （规则摘要不归纳决策）\n");
    out.push_str("\n## Next Steps\n1. 继续处理最近一条保留的用户请求。\n");
    if !tool_counts.is_empty() {
        out.push_str("\n## Critical Context\n- 折叠区工具调用：");
        let list: Vec<String> = tool_counts
            .iter()
            .map(|(n, c)| format!("{n}×{c}"))
            .collect();
        out.push_str(&list.join("、"));
        out.push_str("（结果数据可重新查询）\n");
    }
    out
}

/// 取文本首行（截断到 max_chars，中文安全）。
fn first_line(text: &str, max_chars: usize) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 取首个换行前的完整行；过长则截断加省略
    let line = trimmed.lines().next().unwrap_or(trimmed);
    if line.chars().count() > max_chars {
        let s: String = line.chars().take(max_chars).collect();
        Some(format!("{s}…"))
    } else {
        Some(line.to_string())
    }
}

/// 静默 LLM 摘要调用（retry 语义与主对话 call_llm_with_retry 对齐：
/// 可重试错误 + 流未开始 + 未超 max_attempts → 退避重试）。
///
/// 流式事件全部丢弃——摘要 run 无前端可见输出；`stream_started` 仅用于
/// 重试判定（与主对话同语义：流开始后不可重试）。
async fn call_summary_llm(
    llm: &dyn LlmClient,
    config: &LlmConfig,
    messages: &[ChatMessage],
    tool_defs: &[ToolDef],
    abort: &AbortSignal,
) -> Result<(String, Option<TokenUsage>), String> {
    let retry = RetryPolicy::default();
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;
        let stream_started = Arc::new(AtomicBool::new(false));
        let started = stream_started.clone();
        let on_event = Box::new(move |_: StreamEvent| {
            started.store(true, Ordering::SeqCst);
        });

        let run = llm.chat_stream(config, messages, tool_defs, on_event);
        let result = tokio::select! {
            biased;
            _ = abort.wait_abort() => return Err("压缩已取消".into()),
            res = run => res,
        };

        match result {
            Ok(sr) => {
                let text = sr.text.trim();
                if text.is_empty() {
                    return Err("摘要为空，请重试".into());
                }
                return Ok((text.to_string(), sr.usage));
            }
            Err(e) => {
                let retryable = is_retryable(&e);
                let can_retry = attempt < retry.max_attempts
                    && retryable
                    && !stream_started.load(Ordering::SeqCst);
                if can_retry {
                    let delay_ms = retry.base_delay_ms * attempt as u64;
                    debug!(
                        "action" => "summary_llm_retry",
                        "attempt" => attempt,
                        "delay_ms" => delay_ms,
                        "error" => e.to_string()
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    continue;
                }
                return Err(format!("摘要生成失败：{e}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
use super::super::context::assemble_system_prompt_with_memory as asm_system;
    use crate::events::FrontendMessage;
    use crate::llm::mock::{MockLlmClient, MockResponse};
    use crate::llm::{LlmError, StreamResult, ToolCall, ToolCallFunction};
    use std::sync::Mutex;

    // ── 测试基建 ──

    /// 记录型 client：捕获最近一次 chat_stream 收到的消息与工具面，返回固定文本。
    struct CaptureClient {
        last_messages: Mutex<Vec<ChatMessage>>,
        last_tools: Mutex<Vec<ToolDef>>,
    }

    impl LlmClient for CaptureClient {
        fn chat<'a>(
            &'a self,
            _config: &'a LlmConfig,
            _messages: &'a [ChatMessage],
            _tools: &'a [crate::llm::ToolDef],
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<crate::llm::LlmResponse, LlmError>> + Send + 'a>,
        > {
            Box::pin(async move { Ok(crate::llm::LlmResponse { text: None, tool_calls: None, usage: None }) })
        }

        fn chat_stream<'a>(
            &'a self,
            _config: &'a LlmConfig,
            messages: &'a [ChatMessage],
            tools: &'a [crate::llm::ToolDef],
            on_event: Box<dyn Fn(StreamEvent) + Send + Sync>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<StreamResult, LlmError>> + Send + 'a>,
        > {
            let captured = messages.to_vec();
            let captured_tools = tools.to_vec();
            Box::pin(async move {
                *self.last_messages.lock().unwrap() = captured;
                *self.last_tools.lock().unwrap() = captured_tools;
                on_event(StreamEvent::TextDelta {
                    content: "## Goal\n测试摘要".into(),
                });
                Ok(StreamResult {
                    text: "## Goal\n测试摘要".into(),
                    tool_calls: vec![],
                    reasoning: None,
                    usage: None,
                })
            })
        }
    }

    impl CaptureClient {
        fn new() -> Self {
            Self {
                last_messages: Mutex::new(Vec::new()),
                last_tools: Mutex::new(Vec::new()),
            }
        }
    }

    fn fm(role: &str, content: &str) -> FrontendMessage {
        FrontendMessage {
            role: role.into(),
            content: content.into(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            attachments: Vec::new(),
        }
    }

    fn cfg() -> LlmConfig {
        LlmConfig::default()
    }

    fn fresh_abort() -> AbortSignal {
        AbortSignal::new()
    }

    fn simple_folded() -> Vec<FrontendMessage> {
        vec![
            fm("user", "博士：计算能天使 3 技能总伤"),
            fm("assistant", "需要查询敌方数据，先确认目标关卡"),
        ]
    }

    /// 测试用 system 前缀：口径 = 无目录 + 无记忆态（与生产同源——
    /// 命令层把冻结后的 system 全文传进来，这里用同一个装配函数生成）。
    fn sys() -> String {
        asm_system("", "")
    }

    // ── 主路径 ──

    #[tokio::test]
    async fn run_summary_returns_llm_checkpoint_text() {
        let mock = MockLlmClient::simple_reply("## Goal\n达成");
        let out = run_summary(&mock, &cfg(), simple_folded(), 65_536, &sys(), &[], &fresh_abort())
            .await
            .unwrap();
        assert_eq!(out.summary, "## Goal\n达成");
        assert!(!out.degraded);
        assert!(out.tokens_before.unwrap() > 0);
    }

    #[tokio::test]
    async fn summary_input_is_system_plus_folded_plus_instruction() {
        let client = CaptureClient::new();
        let out = run_summary(&client, &cfg(), simple_folded(), 65_536, &sys(), &[], &fresh_abort())
            .await
            .unwrap();
        assert!(!out.degraded);
        let captured = client.last_messages.lock().unwrap().clone();
        assert!(captured.len() >= 3, "system + 折叠消息 + 指令");
        assert_eq!(captured[0].role, MessageRole::System);
        assert!(captured[0].content.contains("PRTS"), "system 应含角色人格");
        assert_eq!(captured[1].role, MessageRole::User);
        assert_eq!(captured[1].content, "博士：计算能天使 3 技能总伤");
        let last = captured.last().unwrap();
        assert_eq!(last.role, MessageRole::User);
        assert!(
            last.content.contains("压缩器") && last.content.contains("## Goal"),
            "末条应为摘要指令（含输出骨架）"
        );
        // 折叠区原始消息不得被改写
        assert_eq!(out.summary, "## Goal\n测试摘要");
    }

    #[tokio::test]
    async fn summary_input_replays_catalog_memory_and_tools_for_cache_prefix() {
        // 前缀同源（2026-09-25 裁决 · DSH 形态）：system 含冻结目录与记忆区、
        // 工具面同主请求
        let client = CaptureClient::new();
        let tools = vec![ToolDef::new(
            "query_enemy",
            "查询敌人数据",
            serde_json::json!({"type": "object", "properties": {}}),
        )];
        let catalog = "- 数值计算：计算规则\n- 行动推演：推演规则";
        let memory = "## 记忆\n### 关于博士\n- 偏好简洁回答";
        let system = asm_system(catalog, memory);
        let out = run_summary(
            &client,
            &cfg(),
            simple_folded(),
            65_536,
            &system,
            &tools,
            &fresh_abort(),
        )
        .await
        .unwrap();
        assert!(!out.degraded);
        let captured = client.last_messages.lock().unwrap().clone();
        assert!(
            captured[0].content.contains("- 数值计算：计算规则"),
            "system 应含冻结目录文本（与主对话同源）"
        );
        assert!(
            captured[0].content.contains("## 记忆"),
            "system 应含记忆区（与主对话同源）"
        );
        let captured_tools = client.last_tools.lock().unwrap().clone();
        assert_eq!(captured_tools.len(), 1, "工具面应与主请求一致传入");
        assert_eq!(captured_tools[0].function.name, "query_enemy");
    }

    #[tokio::test]
    async fn folded_history_derives_and_filters() {
        // 未知 role 防御兜底（seed 语义）不 panic；error role 被过滤
        let mut folded = simple_folded();
        folded.push(fm("error", "前端错误提示不应进入折叠区派生"));
        let client = CaptureClient::new();
        let out = run_summary(&client, &cfg(), folded, 65_536, &sys(), &[], &fresh_abort())
            .await
            .unwrap();
        assert!(!out.degraded);
        let captured = client.last_messages.lock().unwrap().clone();
        // error 不入 log → 不出现
        assert!(
            !captured.iter().any(|m| m.content.contains("前端错误提示")),
            "error role 消息不应进入摘要输入"
        );
    }

    #[tokio::test]
    async fn empty_folded_history_errors() {
        let mock = MockLlmClient::new(vec![]);
        let err = run_summary(&mock, &cfg(), vec![], 65_536, &sys(), &[], &fresh_abort())
            .await
            .unwrap_err();
        assert!(err.contains("没有可压缩"), "got: {err}");
    }

    // ── 降级链 ──

    #[tokio::test]
    async fn oversize_strips_reasoning_then_calls_llm() {
        // 降级链：含推理超预算 → 剥链 → 回落 LLM 路径。预算 = 0.6×10000 = 6000。
        // 目录与记忆区均显式传参（空串）→ system 尺寸确定、无环境依赖；
        // 场景两条不变式（含推理超预算 / 剥后落预算内）仍由下方 estimate 断言自检。
        // reasoning 3840 tok：两侧余量 ≥0.9K。
        let reasoning: String = "分析过程".repeat(1600); // 6400 CJK ≈ 3840 tok
        let reasoning_tok = crate::orchestrator::context::estimate_tokens(&reasoning);
        let mut m = fm("assistant", "结论");
        m.reasoning_content = Some(reasoning);
        let client = CaptureClient::new();
        let out = run_summary(
            &client,
            &cfg(),
            vec![fm("user", "q"), m],
            10_000,
            &sys(),
            &[],
            &fresh_abort(),
        )
        .await
        .unwrap();
        assert!(!out.degraded, "丢推理后应回落至 LLM 路径");
        let captured = client.last_messages.lock().unwrap().clone();
        let stripped_total = estimate_messages(&captured);
        assert!(
            stripped_total + reasoning_tok > 6000,
            "触发条件应成立：含推理 {} 应超预算 6000",
            stripped_total + reasoning_tok
        );
        assert!(
            stripped_total <= 6000,
            "剥后应落在预算内：{stripped_total}"
        );
        for msg in &captured {
            assert!(msg.reasoning_content.is_none(), "降级后不应携带推理链");
        }
    }

    #[tokio::test]
    async fn extreme_oversize_falls_back_to_rules_without_llm() {
        // 无预设 Mock：若误走 LLM 会返回 Err("预设不足")——degraded 必须不调
        let mock = MockLlmClient::new(vec![]);
        let long = "长文本".repeat(2_000);
        let out = run_summary(&mock, &cfg(), vec![fm("user", &long)], 100, &sys(), &[], &fresh_abort())
            .await
            .unwrap();
        assert!(out.degraded, "极端超窗应规则降级");
        assert!(out.summary.contains("规则生成"), "降级摘要应自带标记");
        assert!(out.summary.contains("## Goal"), "降级产物仍为 checkpoint 骨架");
    }

    #[test]
    fn fallback_summary_lists_user_and_tool_names() {
        let mut tool_msg = ChatMessage::assistant("准备查询");
        tool_msg.tool_calls = Some(vec![ToolCall {
            id: "c1".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "query_enemy".into(),
                arguments: "{}".into(),
            },
        }]);
        let folded = vec![
            ChatMessage::user("博士：计算能天使总伤"),
            tool_msg,
            ChatMessage::tool_result("c1", "rows…"),
            ChatMessage::user("博士：换成早露再算"),
        ];
        let text = build_fallback_summary(&folded);
        assert!(text.contains("能天使总伤"), "应含 user 首行");
        assert!(text.contains("query_enemy"), "应含工具名");
    }

    #[test]
    fn truncate_head_tail_keeps_marker_and_boundaries() {
        let text = "甲".repeat(100);
        let cut = truncate_head_tail(&text, 20);
        assert!(cut.contains("已省略"));
        assert!(cut.starts_with(&"甲".repeat(8)), "应保留头部 40%");
        assert!(cut.ends_with(&"甲".repeat(8)), "应保留尾部 40%");
        assert!(cut.contains("84 字符已省略"), "省略数 = 100 - 8*2");
    }

    // ── 取消 ──

    #[tokio::test]
    async fn aborted_summary_returns_cancel_message() {
        let mock = MockLlmClient::new_with_hang(vec![MockResponse {
            events: vec![],
            result: StreamResult {
                text: String::new(),
                tool_calls: vec![],
                reasoning: None,
                usage: None,
            },
        }], 0);
        let abort = AbortSignal::new();
        abort.abort(); // 预置：wait_abort 立即返回
        let err = run_summary(&mock, &cfg(), simple_folded(), 65_536, &sys(), &[], &abort)
            .await
            .unwrap_err();
        assert!(err.contains("取消"), "got: {err}");
    }

    #[tokio::test]
    async fn abort_mid_stream_cancels() {
        // 挂起 client：推完事件不返回 → 中止经 select 退出
        let mock = MockLlmClient::new_with_hang(vec![MockResponse {
            events: vec![StreamEvent::TextDelta { content: "半截".into() }],
            result: StreamResult {
                text: "半截".into(),
                tool_calls: vec![],
                reasoning: None,
                usage: None,
            },
        }], 0);
        let abort = AbortSignal::new();
        let handle = tokio::spawn({
            let abort = abort.clone();
            async move {
                run_summary(&mock, &cfg(), simple_folded(), 65_536, &sys(), &[], &abort).await
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        abort.abort();
        let res = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("run_summary 应在 abort 后退出（不超时）")
            .expect("任务不应 panic");
        assert!(res.is_err(), "abort 后应为 Err");
        assert!(res.unwrap_err().contains("取消"));
    }

    // ── 推理链策略与主对话一致（Preserve）──

    #[tokio::test]
    async fn reasoning_preserved_when_within_budget() {
        let mut m = fm("assistant", "结论");
        m.reasoning_content = Some("短推理".into());
        let client = CaptureClient::new();
        let _ = run_summary(
            &client,
            &cfg(),
            vec![fm("user", "q"), m],
            65_536,
            &sys(),
            &[],
            &fresh_abort(),
        )
        .await
        .unwrap();
        let captured = client.last_messages.lock().unwrap().clone();
        assert!(
            captured.iter().any(|msg| msg.reasoning_content == Some("短推理".into())),
            "预算内 Preserve：推理链应保留（与主对话一致）"
        );
    }

    // ── 指令 token 上限值渲染 ──

    #[test]
    fn instruction_contains_target_and_skel() {
        let text = build_summary_instruction();
        assert!(text.contains("900"));
        assert!(text.contains("## Constraints & Preferences"));
        assert!(text.contains("## Next Steps"));
        assert!(!text.contains("<mood>"), "指令禁止模型输出内部叙事标记");
        assert!(text.contains("不调用任何工具"), "指令应禁止工具调用");
    }
}
