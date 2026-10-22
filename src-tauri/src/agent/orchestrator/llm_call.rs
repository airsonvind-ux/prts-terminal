use super::*;

/// 请求前图像准备（**每次** LLM 请求前调用）。
///
/// 判据：只给「最后一条 assistant **之后**」的消息物化像素，
/// 之前的（历史）一律不读字节——它们只保留文本标记（`[附件：名，路径：落点]`、
/// 工具结果文本里的路径），模型据此知道曾有附件、也能按路径自己读。
///
/// 必须每次请求前跑，而不是轮起点：工具返回的图（如 MAA 截图）是轮内
/// 追加的，轮起点那次看不到它。
///
/// 模型没有视觉时，图进不去——但得把「图没进去」这件事说清楚，
/// 否则读者会当自己看过了（见 [`NO_VISION_NOTICE`]）。
pub(super) fn prepare_request_images(messages: &mut [ChatMessage], supports_vision: bool) {
    let last_assistant_idx = messages
        .iter()
        .rposition(|m| matches!(m.role, crate::llm::MessageRole::Assistant));
    for (i, msg) in messages.iter_mut().enumerate() {
        let current_turn = match last_assistant_idx {
            Some(idx) => i > idx,
            None => true,
        };
        if current_turn && supports_vision {
            if msg.images.is_none() {
                if let Some(paths) = msg.image_paths.take() {
                    let parts = crate::attachments::materialize_images(&paths);
                    if !parts.is_empty() {
                        msg.images = Some(parts);
                    }
                }
            }
        } else {
            // 本轮带图、但模型看不见：把这件事说清楚，别的都不说。
            // 只补事实，不写行为指令（要不要描述画面由读者自己决定）。
            // 历史消息的图早该退化成文本标记，不补——否则每轮都会堆一句。
            if current_turn && (msg.images.is_some() || msg.image_paths.is_some()) {
                push_notice(&mut msg.content, NO_VISION_NOTICE);
            }
            msg.image_paths = None;
            msg.images = None;
        }
    }
}

/// 模型没有视觉时，对带图消息追加的唯一一句话。
///
/// 只陈述事实（模型看不见），不给行为指令。
pub(super) const NO_VISION_NOTICE: &str = "[当前模型不支持视觉，无法查看图片内容]";

/// 往正文末尾补一句提示；已出现过就不再补（同一轮可被多次请求，会重进这里）。
fn push_notice(content: &mut String, notice: &str) {
    if content.contains(notice) {
        return;
    }
    if !content.is_empty() {
        content.push('\n');
    }
    content.push_str(notice);
}

/// 带通用重试的 LLM 调用（缺陷 3）。
///
/// 仅当**请求尚未产出任何流式事件**（连接/建流阶段失败）时重试——
/// 一旦流已开始（chunk/tool 已双写入 log 与 IPC），重试会造成
/// 重复事件污染回放，此时直接返回错误（turn/end reason=error）。
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
) -> Result<crate::llm::StreamResult, OrchestratorError> {
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;
        // 流是否已开始的观测标志：回调首个事件置位
        let stream_started = Arc::new(std::sync::atomic::AtomicBool::new(false));

        match call_llm_and_stream(
            session_id,
            messages,
            tool_defs,
            llm,
            config,
            sink,
            stream_started.clone(),
            tools,
            tool_ctx,
            Arc::clone(&provider_blocks),
        )
        .await
        {
            Ok(result) => return Ok(result),
            Err(e) => {
                let retryable =
                    matches!(&e, OrchestratorError::Llm(llm_err) if is_retryable(llm_err));
                let can_retry = attempt < policy.max_attempts
                    && retryable
                    && !stream_started.load(std::sync::atomic::Ordering::SeqCst);
                if can_retry {
                    let delay_ms = policy.base_delay_ms * attempt as u64;
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
/// `stream_started` 由调用方传入：首个事件到达时置位，
/// 供重试决策判断"流是否已开始"（开始后不可重试）。
#[allow(clippy::too_many_arguments)] // 调用面由调度循环固定，拆参数结构会扩散到所有调用点
async fn call_llm_and_stream(
    session_id: &str,
    messages: &[ChatMessage],
    tool_defs: &[crate::llm::ToolDef],
    llm: &dyn LlmClient,
    config: &LlmConfig,
    sink: &EventSink,
    stream_started: Arc<std::sync::atomic::AtomicBool>,
    tools: &Arc<ToolRegistry>,
    tool_ctx: &Arc<ToolContext>,
    provider_blocks: Arc<std::sync::Mutex<Vec<crate::llm::ProviderBlock>>>,
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
    let stream_started_for_closure = stream_started;
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

    let on_event = Box::new(move |event: StreamEvent| {
        stream_started_for_closure.store(true, std::sync::atomic::Ordering::SeqCst);
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
    Ok(result)
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
