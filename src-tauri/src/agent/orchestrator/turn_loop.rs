use super::llm_call::{call_llm_with_retry_collect, prepare_request_images};
use super::*;

/// 调度循环：turn 内反复执行 step（调 LLM → 判断是否需要工具 → 执行工具 → 继续），
/// 直到 LLM 不再请求工具或停滞检测触发。
///
/// 主循环体，行为与拆分前保持一致；调用方由 `mod.rs` 的 `dispatch_turn` 薄包装
/// 或 `process_with_memory` 直接进入。
#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_turn_with_memory(
    session_id: &str,
    sink: &EventSink,
    messages: &mut Vec<ChatMessage>,
    skill_index: &Arc<crate::skills_index::SkillIndex>,
    tools: &Arc<ToolRegistry>,
    tool_defs: &[crate::llm::ToolDef],
    llm: &dyn LlmClient,
    config: &LlmConfig,
    db: Option<Arc<Database>>,
    policy: ReasoningPolicy,
    mode: ExecutionMode,
    approvals: &ApprovalRegistry,
    abort: &AbortSignal,
    steer_queue: &SteerQueue,
    workspace_root: &Path,
    system_prompt: &str,
    supports_vision: bool,
) -> Result<TurnOutcome, OrchestratorError> {
    // tool_ctx 构造：有 db 用 with_db（生产路径），无 db 用 new（Mock 场景）。
    // 缓存通道已移除（2026-08：缓存工具取消注册）；Skill 索引注入
    // （Arc::clone 共享 process 的 turn 级快照，索引小、工具只读，零拷贝）；
    // abort 注入（M3-B：run_command execute 读 ctx.abort → RunOptions.cancel）。
    let mut ctx = match db {
        Some(ref db) => ToolContext::with_db(Arc::clone(db)),
        None => ToolContext::new(),
    };
    ctx.skill_index = Some(Arc::clone(skill_index));
    ctx.abort = Some(abort.clone());
    // Arc 共享：流式回调（'static）需持有 Registry/ToolContext 以在 tool/call
    // 事件发出**之前**解析展示切片（Tool::display）——进行中即显示。
    let tool_ctx = Arc::new(ctx);
    let mut state = TurnState::new();
    let mut step: u32 = 0;

    loop {
        step += 1;
        // ── 插话领取（步边界，2026-09-25 语义修订）──
        // 每个 step 的请求发出前领取队首：上一 step 结束后（工具结果已回填）
        // 到达的插话，在本 step 第一时间进入模型视野——工具连跑时不再等到
        // 回答段收尾（对齐上游「步骤之间只领 next-step」）。轮内首个 step
        // 同样领取：承接上一轮中止/错误保留的未交付插话（轮次边界领取）。
        // 每次只取一条：一条走完其回答段再注入下一条（FIFO）。
        // 中止已置位时不领取（未交付条目由命令层保留，同对话下一轮领取）；
        // 注入口径与既有收尾出口一致：log+IPC 双写，messages 与
        // derive(log) 一致性不变量保持。
        if !abort.is_aborted() {
            if let Some(entry) = steer_queue.take_next(session_id) {
                messages.push(ChatMessage::user(entry.text.clone()));
                sink.emit(SessionEvent::user_message(session_id, &entry.text));
                debug!(
                    "step" => step,
                    "action" => "steer_injected",
                    "chars" => entry.text.chars().count()
                );
            }
        }

        // 前缀由命令层冻结后传入（批 5）：整个对话内逐字节不变；
        // 仍按 step 重写 messages[0] 保持不变量（首条恒为 system），
        // push_header 去重后不追加（prompt 无变化）
        let system = system_prompt.to_string();
        messages[0] = ChatMessage::system(system.clone());
        sink.push_header(session_id, &system, tool_defs);

        // ── 缓存前缀契约观测（2026-10-03）：请求前输出 system / 工具定义
        // 字节指纹，供 PRTS_DEBUG 终端比对“跨轮 / 跨重启是否漂移”——
        // 只出哈希与字节数，绝不出正文（request/header 全文照旧不进终端）。──
        {
            let system_hash = crate::observability::fnv1a64_hex(system.as_bytes());
            let tools_json = serde_json::to_string(tool_defs).unwrap_or_default();
            let tools_hash = crate::observability::fnv1a64_hex(tools_json.as_bytes());
            debug!(
                "step" => step,
                "action" => "cache_contract",
                "model" => config.model.as_str(),
                "systemHash" => system_hash,
                "systemBytes" => system.len(),
                "toolsHash" => tools_hash,
                "toolsBytes" => tools_json.len()
            );
        }

        // 请求前图像准备：当前轮（最后一条 assistant 之后）的图物化为像素，
        // 历史图只留文本标记。必须**每次请求前**跑——工具图生于轮内。
        prepare_request_images(messages.as_mut_slice(), supports_vision);

        // 请求前估算（模块 8 双锚：estimate 兜底锚入 log，
        // 与 step/end 的 provider usage 基线锚对齐）
        let estimated_tokens = context::estimate_messages(messages) as u64;
        sink.log_only(SessionEvent::step_start(session_id, step, estimated_tokens));

        // ── 调 LLM，流式推送事件（持久事实双写 IPC + log；通用重试）──
        // M3-B 取消点：biased select —— abort 已置位时确定性选中止分支
        // （drop llm future → reqwest 流断开；重试 sleep 同理被取消）。
        debug!("step" => step, "action" => "llm_start");
        let t0 = std::time::Instant::now();
        // Anthropic thinking 原始块：本轮收集后挂到内存 assistant 消息，
        // 同 turn 的下一步工具续跑必须原序回传（R-27）。
        let provider_blocks = Arc::new(std::sync::Mutex::new(Vec::new()));
        let result = tokio::select! {
            biased;
            _ = abort.wait_abort() => {
                debug!("step" => step, "action" => "llm_aborted");
                return Ok(TurnOutcome::Aborted(state.stat));
            }
            res = call_llm_with_retry_collect(
                session_id,
                messages.as_slice(),
                tool_defs,
                llm,
                config,
                sink,
                tools,
                &tool_ctx,
                RetryPolicy::default(),
                Arc::clone(&provider_blocks),
            ) => res?,
        };
        let step_provider_blocks = provider_blocks
            .lock()
            .map(|blocks| blocks.clone())
            .unwrap_or_default();
        debug!(
            "step" => step,
            "action" => "llm_done",
            "elapsed_ms" => t0.elapsed().as_millis(),
            "tool_calls" => result.tool_calls.len(),
            "provider_blocks" => step_provider_blocks.len()
        );

        // 记录调用：usage 快照（缺陷 4）+ 推理链
        state.record_call(result.usage, result.reasoning.clone());
        if let Some(u) = result.usage {
            state.add_stat(usage_stat(&u, config.api_type));
        }

        // ── 终止判断：tool_calls 为空 → completed ──
        if result.tool_calls.is_empty() {
            // 最终回答归档进上下文（与 log 的 chunk 序列一致；推理链按策略附载，
            // 保持 messages 与 derive(log) 的一致性不变量）
            if !result.text.is_empty() {
                let final_reasoning = if policy == ReasoningPolicy::RequireToolCall {
                    None
                } else {
                    result.reasoning.clone()
                };
                let mut final_msg = ChatMessage::assistant(result.text.clone());
                final_msg.reasoning_content = final_reasoning;
                if !step_provider_blocks.is_empty() {
                    final_msg.provider_blocks = Some(step_provider_blocks.clone());
                }
                messages.push(final_msg);
            }
            debug!(
                "action" => "turn_completed",
                "steps" => step,
                "total_input_tokens" => state.total_input_tokens,
                "total_output_tokens" => state.total_output_tokens
            );
            // 聚合事件（供前端归档：content + reasoning + usage 快照）+ 边界闭合
            let (reasoning, usage) = state.snapshot();
            sink.emit(SessionEvent::assistant_message(
                session_id,
                &result.text,
                reasoning,
                usage,
            ));
            let elapsed_ms = t0.elapsed().as_millis() as u64;
            sink.emit(SessionEvent::step_end(
                session_id,
                step,
                result.usage,
                elapsed_ms,
            ));

            // ── 插话关轮重读（2026-09-25 语义修订）──
            // 本段已收尾（assistant 消息归档、step/end 已闭）：重读队列——
            // 有未交付条目 → continue 续跑（领取在 loop 顶统一进行：下一次
            // step 请求前注入队首；messages 已含本段最终 assistant 消息，
            // 下轮 LLM 看到「已完成回答段 + 插话指令」，FIFO 一条一条来）。
            // 队列空 → 走既有 turn/end 收尾（后端零行为变化）。
            // 中止已置位且队列非空时：续跑由 loop 顶守卫短路为 Aborted
            // （条目保留，下一轮领取）——不向中止轮注入。
            if steer_queue.len(session_id) > 0 {
                continue;
            }

            sink.emit(SessionEvent::turn_end_with_usage(
                session_id,
                if result.finish_reason == Some(crate::llm::FinishReason::MaxTokens) {
                    TurnEndReason::MaxTokens
                } else {
                    TurnEndReason::Completed
                },
                Some(state.stat),
            ));
            return Ok(TurnOutcome::Completed);
        }

        // ── 审批预检（M3-A §4.4）：危险度分类 + 询问等待，先审后跑 ──
        // 仅 run_command 消费；crit 恒拦 / auto-low 直放 / 其余询问（一次一张卡）。
        // 审批只影响本 step 的工具调用放行与否，不进入执行段（并行语义不变）。
        let verdicts = preflight_tool_calls(
            session_id,
            &result.tool_calls,
            mode,
            sink,
            approvals,
            abort,
            workspace_root,
            &tool_ctx,
        )
        .await;

        // ── M3-B：中止优先于执行段（preflight 中止出口已补本卡 result；不进 execute）──
        if abort.is_aborted() {
            return Ok(TurnOutcome::Aborted(state.stat));
        }

        // ── 执行工具，结果追加到上下文，记录指纹 ──
        execute_tools_and_append_with_blocks(
            session_id,
            messages,
            &result,
            tools,
            &tool_ctx,
            sink,
            &mut state,
            Some(&verdicts),
            abort,
            &step_provider_blocks,
        );

        // ── M3-B 中止出口：不写 step/end（与 error 路径一致，log 容忍悬挂 step/start）──
        if abort.is_aborted() {
            return Ok(TurnOutcome::Aborted(state.stat));
        }

        // step/end：工具执行完成后闭合 step 边界
        let elapsed_ms = t0.elapsed().as_millis() as u64;
        sink.emit(SessionEvent::step_end(
            session_id,
            step,
            result.usage,
            elapsed_ms,
        ));

        // ── 停滞检测：升格为终止原因（turn/end reason=stagnation）──
        // M3-B：停滞与中止并发 → 中止优先（中止先于停滞判定）
        if abort.is_aborted() {
            return Ok(TurnOutcome::Aborted(state.stat));
        }
        if state.check_stagnation() {
            // R-33：只发 turn/end(stagnation)，不再先发终止 error。
            // 终止 error 会让前端按错误轮收尾，丢掉半截内容、工具记录和 usage；
            // 轮尾说明由前端按 reason 合成。
            sink.emit(SessionEvent::turn_end_with_usage(
                session_id,
                TurnEndReason::Stagnation,
                Some(state.stat),
            ));
            debug!(
                "action" => "turn_stagnated",
                "steps" => step
            );
            return Ok(TurnOutcome::Stagnation);
        }
    }
}
