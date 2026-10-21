use super::*;

// ═══════════════════════════════════════════════════════
// 工具执行
// ═══════════════════════════════════════════════════════

// ═══════════════════════════════════════════════════════
// 审批预检（M3-A §4.4）
// ═══════════════════════════════════════════════════════

/// 工具调用预检判定（按 tool_calls 索引对齐）。
///
/// 中止分支（M3-B）：对被 await 的本张卡补 Denied{aborted}——dispatch 复查
/// is_aborted 后直接返回，不消费本分支产物（仅保 preflight 自身完整性）。
pub(super) enum ToolVerdict {
    /// 放行：进入正常执行（含 auto-low 直放 / 用户已确认）
    Run,
    /// 拦截/拒绝：不执行，execute_tools_and_append 直接产出失败 tool/result
    Denied {
        summary: String,
        reason_code: &'static str,
    },
}

/// 审批请求事件中的命令回显截断长度（M3-A §5.1：command ≤ 512 字符）。
const MAX_COMMAND_EVENT_CHARS: usize = 512;

/// 从工具参数 JSON 提取命令串（解析失败/缺 command → None，交给执行段自然报错）。
fn extract_command(args: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(args).ok()?;
    let cmd = v.get("command")?.as_str()?.trim();
    if cmd.is_empty() {
        None
    } else {
        Some(cmd.to_string())
    }
}

/// 从工具参数 JSON 解析执行 cwd（与 run_command 工具层同源规则）：
/// 缺省 → workspace_root；相对路径相对 workspace_root join；绝对路径原样。
/// 不做目录存在性校验（执行段对不存在的 cwd 会自然 Parse 拒绝；
/// gate 判定对无法解析的 cwd 落 region-cwd-external）。
fn resolve_cwd(args: &str, workspace_root: &Path) -> PathBuf {
    let v: Option<serde_json::Value> = serde_json::from_str(args).ok();
    let Some(v) = v else {
        return workspace_root.to_path_buf();
    };
    match v.get("cwd") {
        Some(raw) if raw.is_string() => {
            let p = PathBuf::from(raw.as_str().unwrap());
            if p.is_absolute() {
                p
            } else {
                workspace_root.join(p)
            }
        }
        _ => workspace_root.to_path_buf(),
    }
}

// ═══════════════════════════════════════════════════════
// M4 执行身份：permission / justification 校验（preflight 内）
// ═══════════════════════════════════════════════════════

/// run_command `permission` 参数判定（M4 批 2 定稿）。
/// - 缺省 / "default" → [`RunPerm::Default`]（受限身份）
/// - "full" → 必须携带非空 justification，否则 [`RunPerm::Invalid`]（不弹卡）
/// - 其余值 → [`RunPerm::Invalid`]（不弹卡）
#[derive(Debug, Clone, PartialEq, Eq)]
enum RunPerm {
    Default,
    /// 完整权限 + 一句话理由（确认卡展示）
    Full(String),
    /// 参数错误（Denied 直接拒绝，不产生确认卡）
    Invalid(&'static str),
}

fn check_run_permission(args: &str) -> RunPerm {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        // JSON 损坏：execute 段自然 Parse 报错，此处按 default 放行不拦
        return RunPerm::Default;
    };
    match v.get("permission") {
        None => RunPerm::Default,
        Some(p) if p.as_str() == Some("default") => RunPerm::Default,
        Some(p) if p.as_str() == Some("full") => {
            let j = v
                .get("justification")
                .and_then(|x| x.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            match j {
                Some(j) => RunPerm::Full(j.to_string()),
                None => RunPerm::Invalid("permission=full 必须携带非空 justification"),
            }
        }
        Some(_) => RunPerm::Invalid("permission 仅支持 default/full"),
    }
}

/// 预检：地域性信任门判定 + 审批询问（M3-A 设计 §1/§8）。
///
/// 仅对 run_command 消费 gate；其余工具直放。消费点从旧 classify_risk
/// 换为 `exec::gate::evaluate`：
/// - Blocked（crit 恒拦，任何模式）：log_only blocked result + Denied
/// - Auto + 自动化模式：log_only request + auto-approved result 审计直放
/// - Auto + 询问模式 / Ask 判定（任何模式）：询问流（emit request → await
///   用户决策，oneshot 300s 超时）——询问模式不消费门的放行语义
///   （模式 Ask 下 Auto 判定同样弹卡）；reason 码经 request/result 的
///   rule_id 通道携带（前端确认卡类别短语）。
///
/// 询问串行（一次一张卡）；审批通过的命令回原执行段（safe 组并行/串行不受影响）。
///
/// M3-B：询问等待改三路 select（rx / abort / 300s sleep）；中止分支对本卡
/// 补 Rejected/Drop result（收前端卡保配对）后**提前返回整个 preflight**——
/// dispatch 复查 is_aborted 后不进 execute（余卡收束于 Aborted 出口）。
pub(super) async fn preflight_tool_calls(
    session_id: &str,
    tool_calls: &[crate::llm::ToolCall],
    mode: ExecutionMode,
    sink: &EventSink,
    approvals: &ApprovalRegistry,
    abort: &AbortSignal,
    workspace_root: &Path,
) -> Vec<ToolVerdict> {
    let mut verdicts = Vec::with_capacity(tool_calls.len());
    for tc in tool_calls {
        // 非 run_command 直放（审批仅执行类工具消费；未来执行类在此扩展）
        if tc.function.name != "run_command" {
            verdicts.push(ToolVerdict::Run);
            continue;
        }
        let Some(command) = extract_command(&tc.function.arguments) else {
            // 参数缺 command：放行，执行段自然报 Parse 错误
            verdicts.push(ToolVerdict::Run);
            continue;
        };
        // cwd：与 run_command 工具解析同源（缺省 = workspace_root；
        // 相对参数相对 workspace_root join）
        let cwd = resolve_cwd(&tc.function.arguments, workspace_root);
        let gate = crate::exec::gate::evaluate(&command, &cwd, workspace_root);
        let event_risk = match gate.decision {
            crate::exec::gate::GateDecision::Blocked => ApprovalRisk::Crit,
            crate::exec::gate::GateDecision::Ask => ApprovalRisk::High,
            crate::exec::gate::GateDecision::Auto => ApprovalRisk::Low,
        };
        let command_short: String = command.chars().take(MAX_COMMAND_EVENT_CHARS).collect();

        // ── M4 执行身份：permission=default/full + justification 校验 ──
        // full 且无 justification / 非法值 → Denied（参数错误，不弹卡）；
        // privilege 随 request/result 事件携带（前端确认卡档位渲染）。
        let perm = check_run_permission(&tc.function.arguments);
        if let RunPerm::Invalid(msg) = &perm {
            verdicts.push(ToolVerdict::Denied {
                summary: msg.to_string(),
                reason_code: "invalid_params",
            });
            continue;
        }
        let is_full = matches!(&perm, RunPerm::Full(_));
        let privilege = if is_full {
            ApprovalPrivilege::Full
        } else {
            ApprovalPrivilege::Restricted
        };
        let justification: Option<&str> = match &perm {
            RunPerm::Full(j) => Some(j),
            _ => None,
        };

        // ① Blocked：crit 恒拦（任何模式、任何档位——含 full；log_only result）
        if gate.decision == crate::exec::gate::GateDecision::Blocked {
            sink.log_only(SessionEvent::tool_approval_result(
                session_id,
                &tc.id,
                ApprovalDecision::Blocked,
                event_risk,
                mode,
                Some(gate.reason),
                None,
                privilege,
            ));
            verdicts.push(ToolVerdict::Denied {
                summary: format!(
                    "命令被安全策略拦截（规则 {}）：{}",
                    gate.reason, command_short
                ),
                reason_code: "blocked_by_policy",
            });
            continue;
        }

        // ② Auto 判定 + 自动化模式 + 非 full：自动放行
        // （full 跳过直放——即使 gate Auto 也恒走询问；log_only 审计两事件，
        // reason 码进 approval-result 事件 rule_id 通道；不询问、不推 IPC）
        if mode == ExecutionMode::Auto
            && gate.decision == crate::exec::gate::GateDecision::Auto
            && !is_full
        {
            sink.log_only(SessionEvent::tool_approval_request(
                session_id,
                &tc.id,
                &command_short,
                event_risk,
                mode,
                Some(gate.reason),
                ApprovalPrivilege::Restricted,
                None,
            ));
            sink.log_only(SessionEvent::tool_approval_result(
                session_id,
                &tc.id,
                ApprovalDecision::AutoApproved,
                event_risk,
                mode,
                Some(gate.reason),
                None,
                ApprovalPrivilege::Restricted,
            ));
            verdicts.push(ToolVerdict::Run);
            continue;
        }

        // ③ 询问路径（Ask 模式全量——含 gate Auto 判定；Auto 模式非 Auto 判定
        //    + 任何 full）：emit request → await 用户决策 → emit result。
        // reason 码随 request 事件携带（前端确认卡类别短语），随 result 落审计。
        // M3-B 三路 select：rx / abort / 300s sleep（等宽语义同原 timeout）。
        sink.emit(SessionEvent::tool_approval_request(
            session_id,
            &tc.id,
            &command_short,
            event_risk,
            mode,
            Some(gate.reason),
            privilege,
            justification,
        ));
        let rx = approvals.register(session_id, &tc.id);
        let (approved, reject_reason) = tokio::select! {
            biased;
            _ = abort.wait_abort() => {
                // 责任表③：对本张已 emit request 的卡补 Rejected/Drop（收前端
                // 卡保配对）→ 提前返回整个 preflight（余卡不审——dispatch
                // 复查 is_aborted 后不进 execute，剩余槽位由 Aborted 出口收束）
                sink.emit(SessionEvent::tool_approval_result(
                    session_id,
                    &tc.id,
                    ApprovalDecision::Rejected,
                    event_risk,
                    mode,
                    Some(gate.reason),
                    Some(ApprovalRejectReason::Drop),
                    privilege,
                ));
                verdicts.push(ToolVerdict::Denied {
                    summary: "处理已中止，工具未执行".to_string(),
                    reason_code: "aborted",
                });
                return verdicts;
            }
            decision = rx => match decision {
                Ok(UserDecision::Approved) => (true, None),
                Ok(UserDecision::Rejected) => (false, Some(ApprovalRejectReason::User)),
                Err(_) => (false, Some(ApprovalRejectReason::Drop)),
            },
            _ = tokio::time::sleep(APPROVAL_WAIT_TIMEOUT) => (false, Some(ApprovalRejectReason::Timeout)),
        };
        sink.emit(SessionEvent::tool_approval_result(
            session_id,
            &tc.id,
            if approved {
                ApprovalDecision::Approved
            } else {
                ApprovalDecision::Rejected
            },
            event_risk,
            mode,
            Some(gate.reason),
            reject_reason,
            privilege,
        ));
        if approved {
            verdicts.push(ToolVerdict::Run);
        } else {
            let reason_text = match reject_reason {
                Some(ApprovalRejectReason::Timeout) => "用户确认超时（300s），视为拒绝",
                Some(ApprovalRejectReason::Drop) => "确认通道已关闭，命令未执行",
                _ => "用户拒绝了该命令执行",
            };
            verdicts.push(ToolVerdict::Denied {
                summary: reason_text.to_string(),
                reason_code: "user_rejected",
            });
        }
    }
    verdicts
}

/// 执行本轮 LLM 请求的全部工具调用，结果追加到消息列表，
/// 并将每次执行的指纹记录到 TurnState。
///
/// 执行策略（模块 3 P2）：并发安全工具（`is_concurrency_safe()` = true）
/// 分组并行执行（`std::thread::scope`——工具为同步查询，线程池摊薄耗时）；
/// 非安全工具严格串行。所有结果按**调用顺序**收集后统一 emit/入 log，
/// 保证 log 事件顺序确定（回放/派生不依赖执行调度）。
///
/// 上下文变更指令（context_ops）已停用（2026-08：动态层移除，
/// 工具不再携带变更标记，ActiveContext 已注释）。
#[allow(clippy::too_many_arguments)] // 与 process/dispatch_turn 相同的全链路参数集合
pub(super) fn execute_tools_and_append(
    session_id: &str,
    messages: &mut Vec<ChatMessage>,
    result: &StreamResult,
    tools: &ToolRegistry,
    ctx: &ToolContext,
    sink: &EventSink,
    state: &mut TurnState,
    verdicts: Option<&[ToolVerdict]>,
    abort: &AbortSignal,
) {
    // 1. 构造 assistant 消息（正文 + tool_calls + reasoning_content），写入上下文。
    //    正文必须回填（2026-09 修复）：模型需要看到自己此前的输出——与
    //    derive_messages 的既有设计（文本挂到带工具调用的消息上）同一口径；
    //    修复前此处为 String::new()，模型轮内对自身输出失明，自指任务会退化成
    //    工具调用死循环（实测 21 步）。
    //    RequireToolCall 策略仅丢弃"无工具轮次"的推理链，工具轮次保留。
    let assistant_msg = ChatMessage::assistant_with_tools(
        result.text.clone(),
        result.tool_calls.clone(),
        result.reasoning.clone(),
    );
    messages.push(assistant_msg);

    // 2. 批量记录 tool/call 入 log：LLM 一次返回的多个工具调用属于
    //    同一条 assistant 消息，先全部落账再逐个执行，保证派生顺序
    //    （chunk → tool/call × N → tool/result × N）与流式路径一致。
    //    （IPC 已在流式回调实时推送过，此处只写 log。）
    for tc in &result.tool_calls {
        sink.log_only(SessionEvent::tool_call_with_display(
            session_id,
            &tc.id,
            &tc.function.name,
            &tc.function.arguments,
            tools.display_for(&tc.function.name, &tc.function.arguments, ctx),
        ));
    }

    // 3. 执行全部工具调用（统一走 registry 执行管线），结果按调用顺序收集
    let n = result.tool_calls.len();
    let mut outputs: Vec<Option<Result<crate::tools::ToolOutput, crate::tools::ToolError>>> =
        vec![None; n];

    // 并发安全组：并行执行（thread::scope 保证线程不逃逸）。
    // M3-B：spawn 后不中途停——join 等完（查询毫秒级，可接受）；
    // 长 run_command 由 runner 轮询 cancel（kill 路径）覆盖，本组不消费 abort。
    {
        let safe_indices: Vec<usize> = result
            .tool_calls
            .iter()
            .enumerate()
            .filter_map(|(idx, tc)| {
                // preflight 拦截/拒绝的调用不进入执行（Denied 槽位由 emit 阶段直出结果）
                if verdicts.is_some_and(|v| matches!(v[idx], ToolVerdict::Denied { .. })) {
                    return None;
                }
                tools
                    .get(&tc.function.name)
                    .filter(|t| t.is_concurrency_safe())
                    .map(|_| idx)
            })
            .collect();

        std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(safe_indices.len());
            for &idx in &safe_indices {
                let tc = &result.tool_calls[idx];
                handles.push(s.spawn(move || {
                    let r = tools.execute(&tc.function.name, ctx, &tc.function.arguments);
                    (idx, r)
                }));
            }
            // 全部 spawn 后统一 join（真正并行）；结果按索引回填
            for h in handles {
                let (idx, r) = h.join().expect("工具执行线程 panic");
                outputs[idx] = Some(r);
            }
        });
    }

    // 非安全组：严格串行。
    // M3-B：每槽执行前检查 abort——命中后剩余槽位不执行（保持 None），
    // 由下方 emit 阶段的 None 分支补「处理已中止」型 tool/result（保配对）。
    // 正在执行的槽位（同步工具无法预占）正常收尾后统一 emit。
    for (idx, tc) in result.tool_calls.iter().enumerate() {
        if outputs[idx].is_some() {
            continue;
        }
        // Denied 槽位不执行（emit 阶段直出失败结果）
        if verdicts.is_some_and(|v| matches!(v[idx], ToolVerdict::Denied { .. })) {
            continue;
        }
        if abort.is_aborted() {
            break;
        }
        outputs[idx] = Some(tools.execute(&tc.function.name, ctx, &tc.function.arguments));
    }

    // 4. 按调用顺序统一 emit/入 log/追加消息/录指纹（顺序确定，与调度无关）
    for (idx, tc) in result.tool_calls.iter().enumerate() {
        debug!("tool" => &tc.function.name, "action" => "start", "params" => &tc.function.arguments);
        let t0 = std::time::Instant::now();
        match outputs[idx].take() {
            Some(Ok(output)) => {
                debug!(
                    "tool" => &tc.function.name,
                    "action" => "done",
                    "elapsed_ms" => t0.elapsed().as_millis(),
                    "result_len" => output.summary.len()
                );
                // 工具产物里的图片走附件通路：附件引用随 tool/result 落账，
                // 跟轮 derive 重建出同一条带图 user 消息（消息数不变）。
                let image_attachments: Vec<crate::attachments::Attachment> = output
                    .images
                    .iter()
                    .map(|p| crate::attachments::reference_existing(std::path::Path::new(p)))
                    .collect();
                // 双写：IPC tool/result + log tool/result（presentation 用 summary）
                if image_attachments.is_empty() {
                    sink.emit(SessionEvent::tool_result(
                        session_id,
                        &tc.id,
                        Some(&tc.function.name),
                        &output.summary,
                        true,
                    ));
                } else {
                    sink.emit(SessionEvent::tool_result_with_attachments(
                        session_id,
                        &tc.id,
                        Some(&tc.function.name),
                        &output.summary,
                        true,
                        image_attachments.clone(),
                    ));
                }
                messages.push(ChatMessage::tool_result(&tc.id, output.summary.clone()));
                // 工具声明了图片（如 MAA 截图）：另起一条 user 消息送进模型。
                // 不塞进 tool 消息——OpenAI 的 tool 角色只吃文本，Anthropic 的
                // tool_result 才带图；追加一条 user 消息是三家协议都成立的落法。
                // 像素只在当前轮送达（prepare_request_images 按区间决定），
                // 但消息本身落账、跟轮重建，序列不缺项。
                if let Some(msg) = crate::session::SessionLog::tool_image_message(
                    Some(&tc.function.name),
                    &image_attachments,
                ) {
                    messages.push(msg);
                }
                // 缓存操作落账已停用（2026-08：缓存工具取消注册，
                // CacheEventMark 无生产者；未来记忆系统复用时可恢复）
                for mark in &output.skill_events {
                    match mark {
                        SkillEventMark::Activate { name } => {
                            sink.log_only(SessionEvent::skill_activate(session_id, name));
                        }
                    }
                }
                // 录指纹：同一工具+同一参数+同一结果 → 同一哈希
                state.record_fingerprint(
                    &tc.function.name,
                    &tc.function.arguments,
                    &output.summary,
                );
            }
            Some(Err(err)) => {
                let err_msg = format!("{err}");
                debug!(
                    "tool" => &tc.function.name,
                    "action" => "error",
                    "elapsed_ms" => t0.elapsed().as_millis(),
                    "error" => err.to_string()
                );
                sink.emit(SessionEvent::tool_result(
                    session_id,
                    &tc.id,
                    Some(&tc.function.name),
                    &err_msg,
                    false,
                ));
                // 工具失败不中断循环：LLM 看到错误结果后可自行决定
                // 重试（修正参数）、换用其他工具、或承认无法完成
                messages.push(ChatMessage::tool_result(&tc.id, err_msg.clone()));
                // 失败也录指纹：报错重试是四种停滞模式之一
                state.record_fingerprint(&tc.function.name, &tc.function.arguments, &err_msg);
            }
            None => {
                // 未执行槽位：Denied（preflight 拦截/拒绝）按其文案直出；
                // 中止后剩余槽位按「未执行」型补（M3-B 责任表⑤）——
                // 保证 tool/call ↔ tool/result 配对不变量。
                let (summary, reason_code) = match verdicts.and_then(|v| v.get(idx)) {
                    Some(ToolVerdict::Denied {
                        summary,
                        reason_code,
                    }) => (summary.clone(), *reason_code),
                    _ if abort.is_aborted() => ("处理已中止，工具未执行".to_string(), "aborted"),
                    _ => ("工具调用未执行".to_string(), "not_executed"),
                };
                debug!(
                    "tool" => &tc.function.name,
                    "action" => "denied",
                    "reason" => reason_code
                );
                sink.emit(SessionEvent::tool_result(
                    session_id,
                    &tc.id,
                    Some(&tc.function.name),
                    &summary,
                    false,
                ));
                messages.push(ChatMessage::tool_result(&tc.id, summary.clone()));
                // 拦截/拒绝也录指纹：LLM 若原样重试会被停滞检测捕获
                state.record_fingerprint(&tc.function.name, &tc.function.arguments, &summary);
            }
        }
    }
}
