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
//! 主请求真前缀，复用提供方 KV cache（上游形态）；流式事件全部丢弃（不推 IPC）、输出不进入
//! 任何 log。推理链策略由当前模型的 replay 合同决定，不再固定 `Preserve`；
//! `require_tool_call` 且 `clearable=false` 时，超窗压缩 fail closed。
//!
//! ## 输入超窗降级链（裁决 D，实现顺序微调为可测的线性链）
//!
//! 预算 = `0.6 × 当前模型 contextWindow`。estimate 超预算时依次降级：
//! 1. 丢弃推理链；若模型要求工具轮推理且声明不可清空，则直接 fail closed；
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
use crate::llm::{
    ChatMessage, LlmClient, LlmConfig, MessageRole, ReasoningPolicy, ResolvedCompat, StreamEvent,
    TokenUsage, ToolDef,
};
use crate::session::{SessionEvent, SessionLog};
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
/// * `replay` — 当前模型的效果层回放合同（policy / clearable / assistant_reasoning）
/// * `system_prompt` — 对话冻结的完整前缀全文（与主对话同源，批 5 起由命令层
///   从 `prefix_snapshot.rs` 取；人格 + 界面 + 目录 + 记忆区）
/// * `tool_defs` — 主对话工具面快照（同一冻结前缀；仅保请求前缀同源）
/// * `abort` — 压缩注册项的中止信号（取消通道）
///
/// 返回 Err(String)：错误文案面向用户（toast 直显）。
#[allow(clippy::too_many_arguments)]
pub async fn run_summary_with_replay(
    llm: &dyn LlmClient,
    config: &LlmConfig,
    folded_history: Vec<FrontendMessage>,
    context_window: u32,
    replay: &ResolvedCompat,
    system_prompt: &str,
    tool_defs: &[ToolDef],
    abort: &AbortSignal,
) -> Result<SummaryOutcome, String> {
    // 兼容入口：前端消息没有 provider_blocks，不能被当作 Anthropic
    // 工具轮的完整历史；think-block 校验会在真正请求前 fail closed。
    let mut log = SessionLog::new();
    log.seed_from_history(SUMMARY_SESSION_ID, folded_history);
    summarize_log(
        llm,
        config,
        log,
        context_window,
        replay,
        system_prompt,
        tool_defs,
        abort,
    )
    .await
}

/// 从后端官方 log 的折叠轮事件做摘要。
///
/// C-lite primary 下这是生产入口：事件里保留 `assistant/provider-blocks`，
/// 因而 Anthropic 的签名 thinking block 不会在压缩输入处丢失。
#[allow(clippy::too_many_arguments)]
pub async fn run_summary_with_events(
    llm: &dyn LlmClient,
    config: &LlmConfig,
    folded_turns: Vec<Vec<SessionEvent>>,
    prior_summary: Option<String>,
    context_window: u32,
    replay: &ResolvedCompat,
    system_prompt: &str,
    tool_defs: &[ToolDef],
    abort: &AbortSignal,
) -> Result<SummaryOutcome, String> {
    let mut log = SessionLog::new();
    // 链式压缩：上一次摘要必须作为折叠区首条 user 消息参与新摘要。
    if let Some(summary) = prior_summary {
        log.append(SessionEvent::user_message(
            SUMMARY_SESSION_ID,
            &SessionLog::compaction_summary_message(&summary),
        ));
    }
    for turn in folded_turns {
        log.seed_events(turn);
    }
    summarize_log(
        llm,
        config,
        log,
        context_window,
        replay,
        system_prompt,
        tool_defs,
        abort,
    )
    .await
}

/// 公共摘要执行体：输入已经是带完整事件的 SessionLog。
#[allow(clippy::too_many_arguments)]
async fn summarize_log(
    llm: &dyn LlmClient,
    config: &LlmConfig,
    log: SessionLog,
    context_window: u32,
    replay: &ResolvedCompat,
    system_prompt: &str,
    tool_defs: &[ToolDef],
    abort: &AbortSignal,
) -> Result<SummaryOutcome, String> {
    // ── 1. 派生（与主对话同一个 replay policy）──
    let folded_messages = log.derive_messages(replay.policy);
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
    let budget = ((context_window as f64) * MAX_SUMMARY_INPUT_RATIO).floor() as usize;
    if estimate_messages(&messages) > budget {
        strip_reasoning(&mut messages, replay)?;
        if estimate_messages(&messages) > budget {
            truncate_long_messages(&mut messages);
        }
    }
    let degraded = estimate_messages(&messages) > budget;

    // ── 4. 产出 ──
    // 历史里可能有 thinking 关闭时产生的工具轮（本来就没有签名 block）。
    // 这类轮次不能按“缺块”拒绝；摘要请求改用 thinking off，避免把合法历史判死。
    let summary_config = if should_disable_thinking_for_missing_blocks(&messages, replay, config) {
        // 摘要不需要回放历史签名；若仍携带部分 thinking block，而请求又声明
        // thinking=off，混合请求可能被 Anthropic 拒绝（F-11）。降级时一并清块。
        for message in &mut messages {
            message.provider_blocks = None;
        }
        let mut cfg = config.clone();
        cfg.thinking_level = Some("off".into());
        cfg
    } else {
        config.clone()
    };
    let (summary, usage) = if degraded {
        (build_fallback_summary(&messages), None)
    } else {
        call_summary_llm(llm, &summary_config, &messages, tool_defs, abort).await?
    };

    Ok(SummaryOutcome {
        summary,
        degraded,
        tokens_before: Some(tokens_before),
        usage,
    })
}

/// 历史里存在无 provider block 的 Anthropic 工具轮时，摘要请求改用 thinking off。
///
/// 无法从旧日志判断该轮当时是否真的开启了 thinking：关闭 thinking 产生的
/// 工具轮本来就没有签名 block。把这类合法历史直接判死会把用户卡在“只能新建会话”。
fn should_disable_thinking_for_missing_blocks(
    messages: &[ChatMessage],
    replay: &ResolvedCompat,
    config: &LlmConfig,
) -> bool {
    let thinking_enabled = !matches!(config.thinking_level.as_deref(), Some("off") | Some("none"));
    if !thinking_enabled || replay.carrier.as_deref() != Some("thinking_blocks") {
        return false;
    }
    messages.iter().any(|m| {
        let has_tool_call = m.role == MessageRole::Assistant
            && m.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty());
        has_tool_call
            && m.provider_blocks
                .as_ref()
                .is_none_or(|blocks| blocks.is_empty())
    })
}

/// 测试兼容入口：使用默认 replay 合同。生产命令必须显式传当前模型合同。
#[cfg(test)]
pub async fn run_summary(
    llm: &dyn LlmClient,
    config: &LlmConfig,
    folded_history: Vec<FrontendMessage>,
    context_window: u32,
    system_prompt: &str,
    tool_defs: &[ToolDef],
    abort: &AbortSignal,
) -> Result<SummaryOutcome, String> {
    run_summary_with_replay(
        llm,
        config,
        folded_history,
        context_window,
        &ResolvedCompat::default(),
        system_prompt,
        tool_defs,
        abort,
    )
    .await
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
- 只写上方这段较早对话中明确出现的事实；不补写、不推测；\n\
- 保留可追溯证据：工具名、关键查询参数、数据版本、条目名、数值和错误串；结果只写关键结论，不搬运原始数据；\n\
- 工具结果明确失败或未返回时，按原文保留失败状态；\n\
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
1. [下一步]\n\n\
## Critical Context\n\
- [继续任务所需的旧对话上下文：决策、约束、用户偏好、未解决问题]\n\
- [工具证据：工具名 + 关键参数 → 结果结论；数据版本或条目名如有则保留]\n\
- [无则写（无）]",
        SUMMARY_TARGET_CHARS = SUMMARY_TARGET_CHARS
    )
}

/// 降级链 1：丢弃全部推理链（reasoning_content → None）。
///
/// `require_tool_call` 且 `clearable=false` 的模型不能静默清空工具轮推理；
/// 这类历史直接 fail closed，让用户新建会话，而不是发出一个缺少推理的请求。
fn strip_reasoning(messages: &mut [ChatMessage], replay: &ResolvedCompat) -> Result<(), String> {
    if replay.policy == ReasoningPolicy::RequireToolCall && !replay.clearable {
        let has_tool_reasoning = messages.iter().any(|m| {
            m.role == MessageRole::Assistant
                && m.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty())
                && m.reasoning_content
                    .as_deref()
                    .is_some_and(|text| !text.is_empty())
        });
        if has_tool_reasoning {
            return Err(
                "当前模型要求工具调用轮回传推理，且该模型的推理不可清空；请新建会话后再压缩。"
                    .into(),
            );
        }
    }
    for m in messages.iter_mut() {
        // 空串是合法的“已记录为空”carrier，不是缺失；它也不占 token。
        // 把它清成 None 会让摘要请求在本地被 ensure_reasoning_replay
        // 拦成 Config 错误，等于压缩自己制造了历史缺口。
        let tool_turn = m.role == MessageRole::Assistant
            && m.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty());
        if tool_turn && m.reasoning_content.as_deref() == Some("") {
            continue;
        }
        m.reasoning_content = None;
    }
    Ok(())
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
    // 工具证据按 call_id 配对：调用先挂起，结果到达时合成一条可追溯锚点。
    // 规则降级不能只留工具名和次数，否则用户追问依据时只剩“查过”这一层。
    let mut pending_calls: Vec<(String, String, String)> = Vec::new();
    let mut evidence: Vec<String> = Vec::new();
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
                        pending_calls.push((
                            tc.id.clone(),
                            tc.function.name.clone(),
                            tc.function.arguments.clone(),
                        ));
                    }
                }
                if let Some(text) = first_line(&m.content, 120) {
                    if done_items.len() < 8 {
                        done_items.push(text);
                    }
                }
            }
            MessageRole::Tool => {
                let call_id = m.tool_call_id.as_deref().unwrap_or("");
                if let Some(pos) = pending_calls.iter().position(|(id, _, _)| id == call_id) {
                    let (_, name, args) = pending_calls.remove(pos);
                    evidence.push(format!(
                        "- {name} {} → {}",
                        compact_inline(&args, 80),
                        compact_inline(&m.content, 140)
                    ));
                } else if !m.content.trim().is_empty() {
                    evidence.push(format!(
                        "- 工具结果 {call_id}：{}",
                        compact_inline(&m.content, 140)
                    ));
                }
            }
            // system 不参与规则抽取（前缀）
            _ => {}
        }
    }

    for (_, name, args) in pending_calls {
        evidence.push(format!(
            "- {name} {} → （未返回结果）",
            compact_inline(&args, 80)
        ));
    }
    evidence.truncate(8);

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
    out.push_str("\n## Critical Context\n");
    if evidence.is_empty() {
        out.push_str("- （无工具调用或结果）\n");
    } else {
        for item in &evidence {
            out.push_str(item);
            out.push('\n');
        }
    }
    out
}

/// 把多行文本压成单行，并截断到 max_chars（中文安全）。
/// 规则摘要只保留证据锚点，不搬运原始数据。
fn compact_inline(text: &str, max_chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return "（空）".into();
    }
    if flat.chars().count() > max_chars {
        let s: String = flat.chars().take(max_chars).collect();
        format!("{s}…")
    } else {
        flat
    }
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
                let delay_ms = if is_retryable(&e) {
                    retry.delay_for_error(attempt, &e)
                } else {
                    None
                };
                let can_retry = attempt < retry.max_attempts
                    && delay_ms.is_some()
                    && !stream_started.load(Ordering::SeqCst);
                if can_retry {
                    let delay_ms = delay_ms.expect("can_retry 已确认存在延迟");
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
    use super::super::context::assemble_system_prompt_with_memory as asm_system;
    use super::*;
    use crate::events::FrontendMessage;
    use crate::llm::mock::{MockLlmClient, MockResponse};
    use crate::llm::{LlmError, ProviderBlock, StreamResult, ToolCall, ToolCallFunction};
    use crate::session::{ChunkKind, SessionEvent};
    use std::sync::Mutex;

    // ── 测试基建 ──

    /// 记录型 client：捕获最近一次 chat_stream 收到的消息与工具面，返回固定文本。
    struct CaptureClient {
        last_messages: Mutex<Vec<ChatMessage>>,
        last_tools: Mutex<Vec<ToolDef>>,
        last_config: Mutex<Option<LlmConfig>>,
    }

    impl LlmClient for CaptureClient {
        fn chat<'a>(
            &'a self,
            _config: &'a LlmConfig,
            _messages: &'a [ChatMessage],
            _tools: &'a [crate::llm::ToolDef],
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<crate::llm::LlmResponse, LlmError>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                Ok(crate::llm::LlmResponse {
                    text: None,
                    tool_calls: None,
                    usage: None,
                })
            })
        }

        fn chat_stream<'a>(
            &'a self,
            config: &'a LlmConfig,
            messages: &'a [ChatMessage],
            tools: &'a [crate::llm::ToolDef],
            on_event: Box<dyn Fn(StreamEvent) + Send + Sync>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<StreamResult, LlmError>> + Send + 'a>,
        > {
            let captured = messages.to_vec();
            let captured_tools = tools.to_vec();
            let captured_config = config.clone();
            Box::pin(async move {
                *self.last_messages.lock().unwrap() = captured;
                *self.last_tools.lock().unwrap() = captured_tools;
                *self.last_config.lock().unwrap() = Some(captured_config);
                on_event(StreamEvent::TextDelta {
                    content: "## Goal\n测试摘要".into(),
                });
                Ok(StreamResult {
                    text: "## Goal\n测试摘要".into(),
                    tool_calls: vec![],
                    reasoning: None,
                    usage: None,
                    finish_reason: None,
                })
            })
        }
    }

    impl CaptureClient {
        fn new() -> Self {
            Self {
                last_messages: Mutex::new(Vec::new()),
                last_tools: Mutex::new(Vec::new()),
                last_config: Mutex::new(None),
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
        let out = run_summary(
            &mock,
            &cfg(),
            simple_folded(),
            65_536,
            &sys(),
            &[],
            &fresh_abort(),
        )
        .await
        .unwrap();
        assert_eq!(out.summary, "## Goal\n达成");
        assert!(!out.degraded);
        assert!(out.tokens_before.unwrap() > 0);
    }

    #[tokio::test]
    async fn summary_input_is_system_plus_folded_plus_instruction() {
        let client = CaptureClient::new();
        let out = run_summary(
            &client,
            &cfg(),
            simple_folded(),
            65_536,
            &sys(),
            &[],
            &fresh_abort(),
        )
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
    async fn backend_events_preserve_anthropic_provider_blocks_for_compaction() {
        let client = CaptureClient::new();
        let events = vec![
            SessionEvent::user_message("s1", "查一下敌人"),
            SessionEvent::assistant_chunk("s1", ChunkKind::Text, "先查数据"),
            SessionEvent::assistant_provider_blocks(
                "s1",
                vec![ProviderBlock::AnthropicThinking {
                    thinking: "需要工具".into(),
                    signature: "sig-1".into(),
                }],
            ),
            SessionEvent::tool_call("s1", "c1", "query_enemy", "{}"),
            SessionEvent::tool_result("s1", "c1", Some("query_enemy"), "结果", true),
        ];
        let replay = ResolvedCompat {
            carrier: Some("thinking_blocks".into()),
            policy: ReasoningPolicy::Preserve,
            clearable: false,
            ..Default::default()
        };
        let config = LlmConfig {
            thinking_level: Some("medium".into()),
            ..Default::default()
        };
        let out = run_summary_with_events(
            &client,
            &config,
            vec![events],
            None,
            65_536,
            &replay,
            &sys(),
            &[],
            &fresh_abort(),
        )
        .await
        .unwrap();
        assert!(!out.degraded);
        let captured = client.last_messages.lock().unwrap().clone();
        let assistant = captured
            .iter()
            .find(|m| m.role == MessageRole::Assistant && m.tool_calls.is_some())
            .expect("应保留工具轮 assistant 消息");
        let blocks = assistant
            .provider_blocks
            .as_ref()
            .expect("provider blocks 应随事件进入摘要请求");
        assert!(matches!(
            blocks.first(),
            Some(ProviderBlock::AnthropicThinking { signature, .. }) if signature == "sig-1"
        ));
    }

    #[tokio::test]
    async fn anthropic_tool_turn_without_provider_blocks_downgrades_to_thinking_off() {
        let client = CaptureClient::new();
        let events = vec![
            SessionEvent::user_message("s1", "查一下敌人"),
            SessionEvent::tool_call("s1", "c1", "query_enemy", "{}"),
            SessionEvent::tool_result("s1", "c1", Some("query_enemy"), "结果", true),
        ];
        let replay = ResolvedCompat {
            carrier: Some("thinking_blocks".into()),
            policy: ReasoningPolicy::Preserve,
            clearable: false,
            ..Default::default()
        };
        let config = LlmConfig {
            thinking_level: Some("medium".into()),
            ..Default::default()
        };
        let out = run_summary_with_events(
            &client,
            &config,
            vec![events],
            None,
            65_536,
            &replay,
            &sys(),
            &[],
            &fresh_abort(),
        )
        .await
        .unwrap();
        assert!(!out.degraded);
        assert!(
            !client.last_messages.lock().unwrap().is_empty(),
            "缺块历史不应直接拒绝；摘要请求应降级为 thinking off"
        );
        let captured_config = client.last_config.lock().unwrap().clone().unwrap();
        assert_eq!(captured_config.thinking_level.as_deref(), Some("off"));
    }

    #[tokio::test]
    async fn mixed_history_thinking_off_summary_drops_all_provider_blocks() {
        let client = CaptureClient::new();
        let events = vec![
            SessionEvent::user_message("s1", "先查敌人"),
            SessionEvent::assistant_chunk("s1", ChunkKind::Text, "先查数据"),
            SessionEvent::assistant_provider_blocks(
                "s1",
                vec![ProviderBlock::AnthropicThinking {
                    thinking: "需要工具".into(),
                    signature: "sig-1".into(),
                }],
            ),
            SessionEvent::tool_call("s1", "c1", "query_enemy", "{}"),
            SessionEvent::tool_result("s1", "c1", Some("query_enemy"), "结果", true),
            // 第二轮来自 thinking 关闭的旧历史，本来就没有 provider block。
            SessionEvent::assistant_chunk("s1", ChunkKind::Text, "再查一次"),
            SessionEvent::tool_call("s1", "c2", "query_enemy", "{}"),
            SessionEvent::tool_result("s1", "c2", Some("query_enemy"), "结果", true),
        ];
        let replay = ResolvedCompat {
            carrier: Some("thinking_blocks".into()),
            policy: ReasoningPolicy::Preserve,
            clearable: false,
            ..Default::default()
        };
        let config = LlmConfig {
            thinking_level: Some("medium".into()),
            ..Default::default()
        };
        run_summary_with_events(
            &client,
            &config,
            vec![events],
            None,
            65_536,
            &replay,
            &sys(),
            &[],
            &fresh_abort(),
        )
        .await
        .unwrap();

        let captured = client.last_messages.lock().unwrap().clone();
        assert!(
            captured
                .iter()
                .all(|message| message.provider_blocks.is_none()),
            "混合历史降级时必须清掉全部 provider block"
        );
        let captured_config = client.last_config.lock().unwrap().clone().unwrap();
        assert_eq!(captured_config.thinking_level.as_deref(), Some("off"));
    }

    #[tokio::test]
    async fn summary_input_replays_catalog_memory_and_tools_for_cache_prefix() {
        // 前缀同源（2026-09-25 裁决 · 上游形态）：system 含冻结目录与记忆区、
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
        assert!(stripped_total <= 6000, "剥后应落在预算内：{stripped_total}");
        for msg in &captured {
            assert!(msg.reasoning_content.is_none(), "降级后不应携带推理链");
        }
    }

    #[tokio::test]
    async fn oversize_tool_reasoning_fails_closed_when_not_clearable() {
        let mut assistant = fm("assistant", "结论");
        assistant.reasoning_content = Some("工具轮推理".repeat(2000));
        assistant.tool_calls = Some(vec![ToolCall {
            id: "c1".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "lookup".into(),
                arguments: "{}".into(),
            },
        }]);
        let mock = MockLlmClient::new(vec![]);
        let replay = ResolvedCompat {
            policy: ReasoningPolicy::RequireToolCall,
            clearable: false,
            ..Default::default()
        };
        let err = run_summary_with_replay(
            &mock,
            &cfg(),
            vec![fm("user", "q"), assistant],
            1_000,
            &replay,
            &sys(),
            &[],
            &fresh_abort(),
        )
        .await
        .unwrap_err();
        assert!(err.contains("不可清空"), "got: {err}");
    }

    #[test]
    fn strip_reasoning_keeps_empty_tool_carrier() {
        // 空串是“已记录为空”的合法 carrier，不是缺失；压缩不能把它清成 None。
        let mut assistant = ChatMessage::assistant("结论");
        assistant.reasoning_content = Some(String::new());
        assistant.tool_calls = Some(vec![ToolCall {
            id: "c1".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "lookup".into(),
                arguments: "{}".into(),
            },
        }]);
        let mut messages = vec![ChatMessage::user("q"), assistant];
        let replay = ResolvedCompat {
            policy: ReasoningPolicy::RequireToolCall,
            clearable: false,
            ..Default::default()
        };

        strip_reasoning(&mut messages, &replay).unwrap();

        assert_eq!(
            messages[1].reasoning_content.as_deref(),
            Some(""),
            "工具轮空 carrier 必须保留"
        );
    }

    #[tokio::test]
    async fn oversize_empty_tool_carrier_survives_summary_request() {
        let mut assistant = fm("assistant", "结论");
        assistant.reasoning_content = Some(String::new());
        assistant.tool_calls = Some(vec![ToolCall {
            id: "c1".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "lookup".into(),
                arguments: "{}".into(),
            },
        }]);
        let mut tool_result = fm("tool", "查询结果");
        tool_result.tool_call_id = Some("c1".into());
        let client = CaptureClient::new();
        let replay = ResolvedCompat {
            policy: ReasoningPolicy::RequireToolCall,
            clearable: false,
            ..Default::default()
        };
        let long = "长文本".repeat(10_000);

        let out = run_summary_with_replay(
            &client,
            &cfg(),
            vec![fm("user", &long), assistant, tool_result],
            20_000,
            &replay,
            &sys(),
            &[],
            &fresh_abort(),
        )
        .await
        .unwrap();
        assert!(!out.degraded, "截断后应回到 LLM 摘要路径");

        let captured = client.last_messages.lock().unwrap().clone();
        let tool_msg = captured
            .iter()
            .find(|m| {
                m.role == MessageRole::Assistant
                    && m.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty())
            })
            .expect("摘要请求应仍包含工具轮");
        assert_eq!(
            tool_msg.reasoning_content.as_deref(),
            Some(""),
            "超预算压缩后，工具轮空 carrier 仍应随请求发出"
        );
    }

    #[tokio::test]
    async fn extreme_oversize_falls_back_to_rules_without_llm() {
        // 无预设 Mock：若误走 LLM 会返回 Err("预设不足")——degraded 必须不调
        let mock = MockLlmClient::new(vec![]);
        let long = "长文本".repeat(2_000);
        let out = run_summary(
            &mock,
            &cfg(),
            vec![fm("user", &long)],
            100,
            &sys(),
            &[],
            &fresh_abort(),
        )
        .await
        .unwrap();
        assert!(out.degraded, "极端超窗应规则降级");
        assert!(out.summary.contains("规则生成"), "降级摘要应自带标记");
        assert!(
            out.summary.contains("## Goal"),
            "降级产物仍为 checkpoint 骨架"
        );
    }

    #[test]
    fn fallback_summary_keeps_tool_evidence_anchors() {
        let mut tool_msg = ChatMessage::assistant("准备查询");
        tool_msg.tool_calls = Some(vec![ToolCall {
            id: "c1".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "query_enemy".into(),
                arguments: r#"{"operator":"能天使"}"#.into(),
            },
        }]);
        let folded = vec![
            ChatMessage::user("博士：计算能天使总伤"),
            tool_msg,
            ChatMessage::tool_result("c1", "攻击力 630，数据版本 2026-10"),
            ChatMessage::user("博士：换成早露再算"),
        ];
        let text = build_fallback_summary(&folded);
        assert!(text.contains("能天使总伤"), "应含 user 首行");
        assert!(text.contains("## Critical Context"), "应含续接上下文段");
        assert!(text.contains("query_enemy"), "应含工具名");
        assert!(text.contains("operator"), "应含关键参数");
        assert!(text.contains("攻击力 630"), "应含结果锚点");
        assert!(text.contains("数据版本 2026-10"), "应保留结果里的版本线索");
    }

    #[test]
    fn fallback_summary_marks_tool_call_without_result() {
        let mut tool_msg = ChatMessage::assistant("准备查询");
        tool_msg.tool_calls = Some(vec![ToolCall {
            id: "c1".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "query_stage".into(),
                arguments: r#"{"stage":"1-7"}"#.into(),
            },
        }]);
        let text = build_fallback_summary(&[tool_msg]);
        assert!(text.contains("query_stage"), "应含工具名");
        assert!(text.contains("未返回结果"), "应标明证据不完整");
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
        let mock = MockLlmClient::new_with_hang(
            vec![MockResponse {
                events: vec![],
                result: StreamResult {
                    text: String::new(),
                    tool_calls: vec![],
                    reasoning: None,
                    usage: None,
                    finish_reason: None,
                },
            }],
            0,
        );
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
        let mock = MockLlmClient::new_with_hang(
            vec![MockResponse {
                events: vec![StreamEvent::TextDelta {
                    content: "半截".into(),
                }],
                result: StreamResult {
                    text: "半截".into(),
                    tool_calls: vec![],
                    reasoning: None,
                    usage: None,
                    finish_reason: None,
                },
            }],
            0,
        );
        let abort = AbortSignal::new();
        let handle = tokio::spawn({
            let abort = abort.clone();
            async move { run_summary(&mock, &cfg(), simple_folded(), 65_536, &sys(), &[], &abort).await }
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
            captured
                .iter()
                .any(|msg| msg.reasoning_content == Some("短推理".into())),
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
        assert!(text.contains("## Critical Context"));
        assert!(text.contains("工具名"), "应要求保留工具名");
        assert!(text.contains("关键查询参数"), "应要求保留关键参数");
        assert!(text.contains("不补写、不推测"), "应禁止补写缺失事实");
        assert!(!text.contains("<mood>"), "指令禁止模型输出内部叙事标记");
        assert!(text.contains("不调用任何工具"), "指令应禁止工具调用");
        assert!(!text.contains("可重查"), "不应把工具证据降级为可重查");
        assert!(!text.contains("未验证"), "不应引入验证状态标签");
        assert!(!text.contains("折叠区"), "不应使用前端内部结构术语");
    }
}
