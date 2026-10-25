use super::*;

use crate::session::{ImageDeliveryFact, ImageOmitReason};

/// 模型视觉能力的三态判定。
///
/// `Unknown` 与 `Unsupported` 分开：未收录模型不能被写成"不支持"，
/// 否则历史会留下一句无法证伪的假话。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VisionSupport {
    Supported,
    Unsupported,
    Unknown,
}

/// 请求前图像准备（**每次** LLM 请求前调用）。
///
/// 判据：只给「最后一条 assistant **之后**」的消息物化像素，
/// 之前的（历史）一律不读字节——它们只保留文本标记（`[附件：名，路径：落点]`、
/// 工具结果文本里的路径），模型据此知道曾有附件、也能按路径自己读。
///
/// 返回值是本轮实际图片投递事实：只看 `msg.images` 的最终状态，
/// 不看模型能力声明。调用方把它写成 `image/delivery` 事件后，
/// 后续轮次就能从日志稳定重建同一条历史。
pub(super) fn prepare_request_images(
    messages: &mut [ChatMessage],
    vision: VisionSupport,
) -> Vec<ImageDeliveryFact> {
    let last_assistant_idx = messages
        .iter()
        .rposition(|m| matches!(m.role, crate::llm::MessageRole::Assistant));
    let mut facts = Vec::new();

    for (i, msg) in messages.iter_mut().enumerate() {
        let current_turn = match last_assistant_idx {
            Some(idx) => i > idx,
            None => true,
        };

        // 历史消息只去掉像素，保留文本标记；不在这里补投递事实。
        if !current_turn {
            msg.image_paths = None;
            msg.images = None;
            continue;
        }

        let had_image = msg.images.is_some() || msg.image_paths.is_some();
        if !had_image {
            continue;
        }

        let mut reason = None;
        let included = match vision {
            VisionSupport::Supported => {
                if msg.images.is_none() {
                    if let Some(paths) = msg.image_paths.take() {
                        let parts = crate::attachments::materialize_images(&paths);
                        if !parts.is_empty() {
                            msg.images = Some(parts);
                        }
                    }
                }
                let included = msg.images.as_ref().is_some_and(|v| !v.is_empty());
                if !included {
                    reason = Some(ImageOmitReason::MaterializeFailed);
                }
                included
            }
            VisionSupport::Unsupported => {
                reason = Some(ImageOmitReason::UnsupportedVision);
                false
            }
            VisionSupport::Unknown => {
                reason = Some(ImageOmitReason::UnknownVision);
                false
            }
        };

        msg.image_paths = None;
        if !included {
            msg.images = None;
            SessionLog::append_image_omitted_notice(&mut msg.content);
        }

        facts.push(ImageDeliveryFact {
            user_event_offset: msg.image_event_seq,
            tool_call_id: msg.image_tool_call_id.clone(),
            included,
            reason,
        });
    }

    facts
}

/// 带通用重试的 LLM 调用（缺陷 3）。
///
/// 请求阶段失败直接重试；流已经开始但只产出正文/推理时，先写
/// `assistant/reset` 撤回这段半成品，再重试。工具调用一旦出现就不再重试——
/// 工具卡已经可见，撤回会把用户刚看到的执行事实抹掉。
#[allow(clippy::too_many_arguments)] // 调用面由调度循环固定，拆参数结构会扩散到所有调用点
#[allow(dead_code)] // 测试兼容包装；生产走 call_llm_with_retry_collect
pub(super) async fn call_llm_with_retry(
    session_id: &str,
    messages: &[ChatMessage],
    tool_defs: &[crate::llm::ToolDef],
    llm: &dyn LlmClient,
    config: &LlmConfig,
    sink: &EventSink,
    tools: &Arc<ToolRegistry>,
    tool_ctx: &Arc<ToolContext>,
    policy: RetryPolicy,
) -> Result<crate::llm::StreamResult, OrchestratorError> {
    call_llm_with_retry_collect(
        session_id,
        messages,
        tool_defs,
        llm,
        config,
        sink,
        tools,
        tool_ctx,
        policy,
        Arc::new(std::sync::Mutex::new(Vec::new())),
        Arc::new(std::sync::Mutex::new(None)),
    )
    .await
}

/// 同 `call_llm_with_retry`，额外收集本次调用的 provider 原始块（thinking）。
/// 收集器用于把 anthropic thinking 挂到本轮内存消息上（同一 turn 的下一步请求
/// 需要原样回传）；log 记录由事件通道独立完成。
#[allow(clippy::too_many_arguments)]
pub(super) async fn call_llm_with_retry_collect(
    session_id: &str,
    messages: &[ChatMessage],
    tool_defs: &[crate::llm::ToolDef],
    llm: &dyn LlmClient,
    config: &LlmConfig,
    sink: &EventSink,
    tools: &Arc<ToolRegistry>,
    tool_ctx: &Arc<ToolContext>,
    policy: RetryPolicy,
    provider_blocks: Arc<std::sync::Mutex<Vec<crate::llm::ProviderBlock>>>,
    usage_snapshot: Arc<std::sync::Mutex<Option<crate::llm::TokenUsage>>>,
) -> Result<crate::llm::StreamResult, OrchestratorError> {
    let mut attempt: u32 = 0;
    // 本轮是否已经产出过需要撤回的流内容（正文/推理/供应商块）；
    // 用于决定重试前是否写撤回事件。
    let partial_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
    // 工具调用一旦出现就不能撤回：UI 卡片与后续执行阶段都以它为准。
    let tool_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));

    loop {
        attempt += 1;
        // 每次尝试独立收集；失败尝试的 provider 块和用量不能污染重试结果。
        if let Ok(mut blocks) = provider_blocks.lock() {
            blocks.clear();
        }
        if let Ok(mut slot) = usage_snapshot.lock() {
            *slot = None;
        }

        match call_llm_and_stream(
            session_id,
            messages,
            tool_defs,
            llm,
            config,
            sink,
            Arc::clone(&partial_seen),
            Arc::clone(&tool_seen),
            tools,
            tool_ctx,
            Arc::clone(&provider_blocks),
            Arc::clone(&usage_snapshot),
        )
        .await
        {
            Ok(result) => return Ok(result),
            Err(e) => {
                let delay_ms = match &e {
                    OrchestratorError::Llm(llm_err) if is_retryable(llm_err) => {
                        policy.delay_for_error(attempt, llm_err)
                    }
                    _ => None,
                };
                let can_retry = attempt < policy.max_attempts
                    && delay_ms.is_some()
                    && !tool_seen.load(std::sync::atomic::Ordering::SeqCst);
                if can_retry {
                    if partial_seen.load(std::sync::atomic::Ordering::SeqCst) {
                        sink.emit(SessionEvent::assistant_reset(session_id));
                    }
                    let delay_ms = delay_ms.expect("can_retry 已确认存在延迟");
                    debug!(
                        "action" => "llm_retry",
                        "attempt" => attempt,
                        "delay_ms" => delay_ms,
                        "error" => e.to_string()
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    continue;
                }
                return Err(e);
            }
        }
    }
}

/// 调用 chat_stream()，在回调中将 StreamEvent 映射为持久事实（IPC + log 双写）。
/// 返回聚合的 StreamResult。
///
/// 回调内部职责：
/// 1. Phase 跟踪：phase 变化时 emit Phase 事件（避免重复推送）
/// 2. StreamEvent → 双写映射：TextDelta/ReasoningDelta → assistant/chunk，
///    ToolCall → 仅 IPC（实时卡片，log 由执行阶段批量写）
///
/// `partial_seen` / `tool_seen` 由调用方传入：分别记录正文/推理是否出现、
/// 工具调用是否出现；重试前据此决定是否撤回半成品。
#[allow(clippy::too_many_arguments)] // 调用面由调度循环固定，拆参数结构会扩散到所有调用点
async fn call_llm_and_stream(
    session_id: &str,
    messages: &[ChatMessage],
    tool_defs: &[crate::llm::ToolDef],
    llm: &dyn LlmClient,
    config: &LlmConfig,
    sink: &EventSink,
    partial_seen: Arc<std::sync::atomic::AtomicBool>,
    tool_seen: Arc<std::sync::atomic::AtomicBool>,
    tools: &Arc<ToolRegistry>,
    tool_ctx: &Arc<ToolContext>,
    provider_blocks: Arc<std::sync::Mutex<Vec<crate::llm::ProviderBlock>>>,
    usage_snapshot: Arc<std::sync::Mutex<Option<crate::llm::TokenUsage>>>,
) -> Result<crate::llm::StreamResult, OrchestratorError> {
    // ── 用 Arc 共享可变状态到回调闭包中 ──
    // Mutex 保护的原因：chat_stream 的 on_event 回调是 Box<dyn Fn + Send + Sync>，
    // 内部可能在多个异步任务间传递（取决于具体 LlmClient 实现），
    // Mutex 保证跨线程访问的安全。实际场景中回调是同步依次调用的，
    // 锁争用成本为零，不影响性能。
    let phase_state = Arc::new(std::sync::Mutex::new(AgentPhase::Reasoning));

    // 初始 Phase：进入 reasoning 状态
    sink.phase(session_id, AgentPhase::Reasoning);

    // ── 构造回调闭包（双写：IPC + log）──
    let session_id_owned = session_id.to_string();
    let phase_state_clone = phase_state.clone();
    let sink_for_closure = sink.clone();
    let partial_seen_for_closure = Arc::clone(&partial_seen);
    let partial_seen_tail = Arc::clone(&partial_seen);
    let tool_seen_for_closure = Arc::clone(&tool_seen);
    let tool_seen_tail = Arc::clone(&tool_seen);
    // Registry / ToolContext 以 Arc 克隆入 'static 回调：tool/call 事件在流式
    // 阶段（工具执行之前）发出，需在此解析展示切片（Tool::display）。
    let tools_for_closure = Arc::clone(tools);
    let tool_ctx_for_closure = Arc::clone(tool_ctx);
    // 协议清洗状态：闭包与收尾都要用，故共享（匿名工具调用拦在此处）。
    let guard = Arc::new(std::sync::Mutex::new(
        crate::llm::protocol_guard::ProtocolGuard::new(),
    ));
    let guard_for_closure = Arc::clone(&guard);
    // 收尾回写要用的一套句柄（上面那套会被闭包 move 走，这里留一份）。
    let session_id_tail = session_id.to_string();
    let phase_state_tail = Arc::clone(&phase_state);
    let sink_tail = sink.clone();
    let tools_tail = Arc::clone(tools);
    let tool_ctx_tail = Arc::clone(tool_ctx);
    let provider_blocks_tail = Arc::clone(&provider_blocks);
    let usage_snapshot_for_closure = Arc::clone(&usage_snapshot);

    let on_event = Box::new(move |event: StreamEvent| {
        // Usage 只是计费快照，不代表用户已经看到内容；它不能阻止空响应重试。
        match &event {
            StreamEvent::TextDelta { .. }
            | StreamEvent::ReasoningDelta { .. }
            | StreamEvent::ProviderBlock { .. } => {
                partial_seen_for_closure.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            StreamEvent::ToolCall { .. } => {
                tool_seen_for_closure.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            _ => {}
        }
        if let StreamEvent::Usage { usage } = &event {
            if let Ok(mut slot) = usage_snapshot_for_closure.lock() {
                *slot = Some(*usage);
            }
        }
        let passed = match guard_for_closure.lock() {
            Ok(mut guard) => guard.push(event),
            // 锁中毒（持锁 panic）按"原样放行"处理：清洗是加分项，不能因为锁坏了吞事件。
            Err(_) => vec![event],
        };
        for event in passed {
            handle_stream_event_collect(
                &session_id_owned,
                event,
                &phase_state_clone,
                &sink_for_closure,
                &tools_for_closure,
                &tool_ctx_for_closure,
                &provider_blocks,
            );
        }
    });

    // ── 调用 LLM ──
    let outcome = llm.chat_stream(config, messages, tool_defs, on_event).await;

    // 收尾：把匿名调用缓冲住的正文尽力回写出去。成功与失败都要发——用户已经看到的
    // 部分不能因为一次协议异常就被吞掉。
    let tail = match guard.lock() {
        Ok(mut guard) => guard.finish(),
        Err(_) => Vec::new(),
    };
    for event in tail {
        // finish 回写的正文/推理/块同样属于这次尝试的事实；失败重试时必须撤回。
        match &event {
            StreamEvent::TextDelta { .. }
            | StreamEvent::ReasoningDelta { .. }
            | StreamEvent::ProviderBlock { .. } => {
                partial_seen_tail.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            StreamEvent::ToolCall { .. } => {
                tool_seen_tail.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            _ => {}
        }
        handle_stream_event_collect(
            &session_id_tail,
            event,
            &phase_state_tail,
            &sink_tail,
            &tools_tail,
            &tool_ctx_tail,
            &provider_blocks_tail,
        );
    }

    let mut result = outcome?;
    if let Ok(mut guard) = guard.lock() {
        guard.sanitize_result(&mut result);
    }
    if let Some(error) = stream_result_error(&result) {
        return Err(OrchestratorError::Llm(error));
    }
    Ok(result)
}

/// 把“流正常结束但用户没有可用回答”分类成可解释的错误。
///
/// 只有推理、没有最终正文的结果不在这里报错：模型偶尔只输出思考过程，
/// 这是可接受的模型行为，不应伪装成系统故障。
fn stream_result_error(result: &crate::llm::StreamResult) -> Option<crate::llm::LlmError> {
    if result.finish_reason == Some(crate::llm::FinishReason::ContentFilter)
        && result.text.trim().is_empty()
        && result.tool_calls.is_empty()
    {
        return Some(crate::llm::LlmError::ContentFilter);
    }
    if result.is_empty_response() {
        return Some(crate::llm::LlmError::EmptyResponse);
    }
    None
}

/// 单个 StreamEvent 的处理逻辑（双写：IPC 事件 + session log）。
///
/// 进来之前已经过 `llm::protocol_guard`：匿名工具调用已被拦下，它回写出的正文
/// 会以 `TextDelta` 的形式到这里。
///
/// 从 call_llm_and_stream 提取为独立函数，便于阅读和测试。
///
/// 词汇映射（模块 1）：
/// - ReasoningDelta → assistant/chunk(Reasoning)（双写）
/// - TextDelta       → assistant/chunk(Text)（双写）
/// - ToolCall        → tool/call（仅 IPC：实时工具卡片；log 由执行阶段批量写，
///   保证"一次 assistant 消息携带全部工具调用"的派生顺序）
/// - phase 变化      → Phase（仅 IPC，纯 UI 信号）
///
/// reasoning_done 已删除：推理结束由后续事件（text chunk / tool/call /
/// turn/end）隐式表达，前端从事件序列推断。
#[allow(dead_code)] // 测试兼容包装；生产走 handle_stream_event_collect
pub(super) fn handle_stream_event(
    session_id: &str,
    event: StreamEvent,
    phase_state: &std::sync::Mutex<AgentPhase>,
    sink: &EventSink,
    tools: &ToolRegistry,
    tool_ctx: &ToolContext,
) {
    let provider_blocks = Arc::new(std::sync::Mutex::new(Vec::new()));
    handle_stream_event_collect(
        session_id,
        event,
        phase_state,
        sink,
        tools,
        tool_ctx,
        &provider_blocks,
    );
}

pub(super) fn handle_stream_event_collect(
    session_id: &str,
    event: StreamEvent,
    phase_state: &std::sync::Mutex<AgentPhase>,
    sink: &EventSink,
    tools: &ToolRegistry,
    tool_ctx: &ToolContext,
    provider_blocks: &Arc<std::sync::Mutex<Vec<crate::llm::ProviderBlock>>>,
) {
    match event {
        StreamEvent::ReasoningDelta { content } => {
            // Phase 切换（仅在变化时 emit，避免重复推送）
            switch_phase(phase_state, AgentPhase::Reasoning, session_id, sink);
            sink.emit(SessionEvent::assistant_chunk(
                session_id,
                ChunkKind::Reasoning,
                &content,
            ));
        }

        StreamEvent::Usage { .. } => {
            // 用量快照只供中止路径兜底；正常完成以 StreamResult.usage 为准。
        }

        StreamEvent::ProviderBlock { block } => {
            // 只进 log，不推 IPC：thinking 块是回传凭据，不是 UI 内容。
            if let Ok(mut collector) = provider_blocks.lock() {
                collector.push(block.clone());
            }
            sink.log_only(SessionEvent::assistant_provider_blocks(
                session_id,
                vec![block],
            ));
        }

        StreamEvent::ToolCall {
            id,
            name,
            arguments,
        } => {
            // Phase 切换
            switch_phase(phase_state, AgentPhase::Executing, session_id, sink);
            // 展示切片：在此（tool/call 发出前）解析——用户于调用发起时即见对象/
            // 文件链接；失败同链保留（挂在 call 侧数据上）
            let display = tools.display_for(&name, &arguments, tool_ctx);
            let event =
                SessionEvent::tool_call_with_display(session_id, &id, &name, &arguments, display);
            if sink.persists_tool_calls() {
                // primary：流式阶段就落 log——即使 preflight 中止，UI 卡片事实
                // 也在官方日志里；投影会补齐/丢弃不完整配对（R-26）。
                sink.emit(event);
            } else {
                // legacy：执行阶段批量写 log，这里只推 IPC（保持旧测试口径）。
                sink.ipc_only(event);
            }
        }

        StreamEvent::TextDelta { content } => {
            // Phase 切换
            switch_phase(phase_state, AgentPhase::Generating, session_id, sink);
            sink.emit(SessionEvent::assistant_chunk(
                session_id,
                ChunkKind::Text,
                &content,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FinishReason, LlmError, StreamResult};

    fn result(finish_reason: FinishReason, text: &str, reasoning: Option<&str>) -> StreamResult {
        StreamResult {
            text: text.into(),
            tool_calls: Vec::new(),
            reasoning: reasoning.map(str::to_string),
            usage: None,
            finish_reason: Some(finish_reason),
        }
    }

    #[test]
    fn content_filter_without_content_is_classified() {
        let result = result(FinishReason::ContentFilter, "", None);
        assert!(matches!(
            stream_result_error(&result),
            Some(LlmError::ContentFilter)
        ));
    }

    #[test]
    fn content_filter_with_text_is_not_treated_as_empty() {
        let result = result(FinishReason::ContentFilter, "半截回答", None);
        assert!(stream_result_error(&result).is_none());
    }

    #[test]
    fn normal_empty_response_is_classified() {
        let result = result(FinishReason::Normal, "", None);
        assert!(matches!(
            stream_result_error(&result),
            Some(LlmError::EmptyResponse)
        ));
    }

    #[test]
    fn reasoning_only_is_allowed() {
        let result = result(FinishReason::Normal, "", Some("只想了没回答"));
        assert!(stream_result_error(&result).is_none());
    }
}
