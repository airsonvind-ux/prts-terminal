use super::*;
use crate::skills::trust::SkillTrust;

// ═══════════════════════════════════════════════════════
// 工具执行
// ═══════════════════════════════════════════════════════

// ═══════════════════════════════════════════════════════
// 工具预检（M3-A 审批 + R-02b taint）
// ═══════════════════════════════════════════════════════

/// 工具调用预检判定（按 tool_calls 索引对齐）。
///
/// 中止分支（M3-B）：对被 await 的本张卡补 Denied{aborted}——dispatch 复查
/// is_aborted 后直接返回，不消费本分支产物（仅保 preflight 自身完整性）。
pub(super) enum ToolVerdict {
    /// 放行：进入正常执行（含 auto-low 直放 / 用户已确认）。
    /// `outside_read` 为预检阶段判定的“本次成功后会读到区外内容”——
    /// 执行成功后据此给本轮 taint 落位（失败/拒绝不落位）。
    Run { outside_read: bool },
    /// 拦截/拒绝：不执行，execute_tools_and_append 直接产出失败 tool/result
    Denied {
        summary: String,
        reason_code: &'static str,
    },
}

/// 外部内容工具（成功返回后视为“本轮已摄入外部资料”）。
const EXTERNAL_CONTENT_TOOLS: &[&str] = &["web_fetch", "web_search"];
/// 本地读取/命令工具（同批外部内容时受限；区外目标受 taint 限制）。
const LOCAL_ACCESS_TOOLS: &[&str] = &["read_file", "list_dir", "grep", "run_command"];

fn is_external_content_tool(name: &str) -> bool {
    EXTERNAL_CONTENT_TOOLS.contains(&name)
}

fn is_local_access_tool(name: &str) -> bool {
    LOCAL_ACCESS_TOOLS.contains(&name)
}

/// 激活目标是否为不可信 Skill（R-29；同批 activate_skill + run_command 兜底）。
fn activation_target_untrusted(ctx: &ToolContext, args: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return false;
    };
    let Some(name) = v.get("name").and_then(|x| x.as_str()) else {
        return false;
    };
    ctx.skill_index
        .as_ref()
        .is_some_and(|idx| idx.trust(name) == SkillTrust::Untrusted)
}

/// 命令/cwd 是否引用不可信 Skill 包目录（R-29）。
///
/// 先看 cwd 是否已在包内，再对词法 token 做路径规范化；扫描失败时用
/// 带分隔符边界的原始字符串兜底，避免 `cd 包目录; .\run.ps1` 这类形态漏判。
fn command_hits_untrusted_skill(command: &str, cwd: &Path, dirs: &[PathBuf]) -> bool {
    if dirs.is_empty() {
        return false;
    }
    let cwd_c = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let dirs_c: Vec<PathBuf> = dirs
        .iter()
        .map(|dir| dir.canonicalize().unwrap_or_else(|_| dir.clone()))
        .collect();
    if dirs_c
        .iter()
        .any(|dir| crate::exec::gate::inside_workspace(&cwd_c, dir))
    {
        return true;
    }
    let scan = crate::exec::gate::scan_tokens(command);
    for token in scan.tokens {
        if let Some(target) = crate::exec::gate::normalize_path(&token.value, cwd) {
            if dirs_c
                .iter()
                .any(|dir| crate::exec::gate::inside_workspace(&target, dir))
            {
                return true;
            }
        }
    }
    // 词法扫描失败时用原始字符串兜底；统一分隔符后再比较，避免 `\` / `/` 漏判。
    let lower = command.replace('\\', "/").to_lowercase();
    dirs.iter().any(|dir| {
        let prefix = dir.to_string_lossy().replace('\\', "/").to_lowercase();
        let prefix = prefix.trim_end_matches('/');
        lower.contains(&format!("{prefix}/"))
    })
}

/// 文件工具（read_file/list_dir/grep）的目标是否落在工作区外；
/// run_command 不走这里（由 gate.touched_external 判）。
fn local_target_outside(name: &str, args: &str, workspace_root: &Path) -> bool {
    if !matches!(name, "read_file" | "list_dir" | "grep") {
        return false;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(args) else {
        return false;
    };
    let raw = v
        .get("path")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .unwrap_or("");
    let raw = if raw.is_empty() { "." } else { raw };
    let ws = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    match crate::exec::gate::normalize_path(raw, &ws) {
        Some(target) => !target.starts_with(&ws),
        None => false,
    }
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

/// 预检：taint 判定 + 地域性信任门判定 + 审批询问（M3-A §1/§8 + R-02b）。
///
/// taint 对所有工具生效（同批外部+本地、跨步外部→区外读取、跨步区外读取
/// →外部请求）；审批仅对 run_command 消费 gate。消费点从旧 classify_risk
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
#[allow(clippy::too_many_arguments)] // 与 execute_tools_and_append 同口径的全链路参数集合
pub(super) async fn preflight_tool_calls(
    session_id: &str,
    tool_calls: &[crate::llm::ToolCall],
    mode: ExecutionMode,
    sink: &EventSink,
    approvals: &ApprovalRegistry,
    abort: &AbortSignal,
    workspace_root: &Path,
    ctx: &ToolContext,
) -> Vec<ToolVerdict> {
    let mut verdicts = Vec::with_capacity(tool_calls.len());
    // 同批“外部内容 + 本地读取”不能依赖执行顺序（工具默认并行）：
    // 批次里只要出现外部内容工具，本地读取/命令一律先拒。
    let batch_has_external = tool_calls
        .iter()
        .any(|tc| is_external_content_tool(&tc.function.name));
    // 同批激活不可信 Skill + run_command：激活尚未执行，不能靠 active 状态兜底，
    // 也不能依赖并行工具的完成顺序。
    let batch_untrusted_activation = tool_calls.iter().any(|tc| {
        tc.function.name == "activate_skill"
            && activation_target_untrusted(ctx, &tc.function.arguments)
    });

    for tc in tool_calls {
        let name = tc.function.name.as_str();

        // ── R-02b taint 预检（先于审批：会被拒的命令不弹确认卡）──
        if batch_has_external && is_local_access_tool(name) {
            verdicts.push(ToolVerdict::Denied {
                summary: "同一批工具调用同时包含外部资料与本地读取，已阻止本地读取".into(),
                reason_code: "taint_same_batch",
            });
            continue;
        }
        if ctx.taint().has_outside_read() && is_external_content_tool(name) {
            verdicts.push(ToolVerdict::Denied {
                summary: "本轮已读取工作区外内容，已阻止继续访问外部网络".into(),
                reason_code: "taint_outside_web",
            });
            continue;
        }
        if ctx.taint().has_external_content()
            && local_target_outside(name, &tc.function.arguments, workspace_root)
        {
            verdicts.push(ToolVerdict::Denied {
                summary: "本轮已读取外部资料，已阻止读取工作区外内容".into(),
                reason_code: "taint_external_local",
            });
            continue;
        }

        // 非 run_command 直放（审批仅执行类工具消费；未来执行类在此扩展）
        if name != "run_command" {
            verdicts.push(ToolVerdict::Run {
                outside_read: local_target_outside(name, &tc.function.arguments, workspace_root),
            });
            continue;
        }
        let Some(command) = extract_command(&tc.function.arguments) else {
            // 参数缺 command：放行，执行段自然报 Parse 错误
            verdicts.push(ToolVerdict::Run {
                outside_read: false,
            });
            continue;
        };
        // cwd：与 run_command 工具解析同源（缺省 = workspace_root；
        // 相对参数相对 workspace_root join）
        let cwd = resolve_cwd(&tc.function.arguments, workspace_root);
        let gate = crate::exec::gate::evaluate(&command, &cwd, workspace_root);

        // ── R-29：不可信 Skill 包内脚本/命令强制 Ask（Auto 也 Ask）──
        // 命中顺序：显式引用包目录 > 同批激活不可信 Skill > 本轮已激活兜底。
        let skill_dir_hit = ctx.skill_index.as_ref().is_some_and(|idx| {
            command_hits_untrusted_skill(&command, &cwd, &idx.untrusted_resource_dirs())
        });
        let skill_reason = if skill_dir_hit {
            Some("skill-package-script")
        } else if batch_untrusted_activation || ctx.active_untrusted_skill().is_some() {
            Some("skill-untrusted-active")
        } else {
            None
        };
        let effective_decision = if skill_reason.is_some()
            && gate.decision != crate::exec::gate::GateDecision::Blocked
        {
            crate::exec::gate::GateDecision::Ask
        } else {
            gate.decision
        };
        let gate_reason = skill_reason.unwrap_or(gate.reason);

        // 外部资料已摄入 → 区外命令不再执行（gate 的审计位是现成判据）。
        if ctx.taint().has_external_content() && gate.touched_external {
            verdicts.push(ToolVerdict::Denied {
                summary: "本轮已读取外部资料，已阻止执行工作区外命令".into(),
                reason_code: "taint_external_local",
            });
            continue;
        }

        let event_risk = match effective_decision {
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
            && effective_decision == crate::exec::gate::GateDecision::Auto
            && !is_full
        {
            sink.log_only(SessionEvent::tool_approval_request(
                session_id,
                &tc.id,
                &command_short,
                event_risk,
                mode,
                Some(gate_reason),
                ApprovalPrivilege::Restricted,
                None,
            ));
            sink.log_only(SessionEvent::tool_approval_result(
                session_id,
                &tc.id,
                ApprovalDecision::AutoApproved,
                event_risk,
                mode,
                Some(gate_reason),
                None,
                ApprovalPrivilege::Restricted,
            ));
            verdicts.push(ToolVerdict::Run {
                outside_read: gate.touched_external,
            });
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
            Some(gate_reason),
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
                    Some(gate_reason),
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
            Some(gate_reason),
            reject_reason,
            privilege,
        ));
        if approved {
            verdicts.push(ToolVerdict::Run {
                outside_read: gate.touched_external,
            });
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
#[allow(dead_code)] // 测试兼容包装；生产走 execute_tools_and_append_with_blocks
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
    execute_tools_and_append_with_blocks(
        session_id,
        messages,
        result,
        tools,
        ctx,
        sink,
        state,
        verdicts,
        abort,
        &[],
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_tools_and_append_with_blocks(
    session_id: &str,
    messages: &mut Vec<ChatMessage>,
    result: &StreamResult,
    tools: &ToolRegistry,
    ctx: &ToolContext,
    sink: &EventSink,
    state: &mut TurnState,
    verdicts: Option<&[ToolVerdict]>,
    abort: &AbortSignal,
    provider_blocks: &[crate::llm::ProviderBlock],
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
    )
    .with_provider_blocks(provider_blocks.to_vec());
    messages.push(assistant_msg);

    // 2. 批量记录 tool/call 入 log：LLM 一次返回的多个工具调用属于
    //    同一条 assistant 消息，先全部落账再逐个执行，保证派生顺序
    //    （chunk → tool/call × N → tool/result × N）与流式路径一致。
    //    （IPC 已在流式回调实时推送过，此处只写 log。）
    // primary 模式已在流式回调写入（含中止轮）；legacy 模式在这里批量写。
    if !sink.persists_tool_calls() {
        for tc in &result.tool_calls {
            sink.log_only(SessionEvent::tool_call_with_display(
                session_id,
                &tc.id,
                &tc.function.name,
                &tc.function.arguments,
                tools.display_for(&tc.function.name, &tc.function.arguments, ctx),
            ));
        }
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
                // R-02b：成功执行后才落 taint（失败/拒绝不染色）。
                // 区外读取来自预检的路径判定/gate 审计位；外部资料来自 provenance。
                if matches!(
                    verdicts.and_then(|v| v.get(idx)),
                    Some(ToolVerdict::Run { outside_read: true })
                ) {
                    ctx.taint().mark_outside_read();
                }
                if output
                    .provenance
                    .as_ref()
                    .is_some_and(|p| matches!(p.trust, crate::tools::ToolTrust::External))
                {
                    ctx.taint().mark_external_content();
                }
                // 工具产物里的图片走附件通路：附件引用随 tool/result 落账，
                // 跟轮 derive 重建出同一条带图 user 消息（消息数不变）。
                let image_attachments: Vec<crate::attachments::Attachment> = output
                    .images
                    .iter()
                    .map(|p| crate::attachments::reference_existing(std::path::Path::new(p)))
                    .collect();
                // 双写：IPC tool/result + log tool/result（presentation 用 summary）
                if image_attachments.is_empty() {
                    sink.emit(
                        SessionEvent::tool_result(
                            session_id,
                            &tc.id,
                            Some(&tc.function.name),
                            &output.summary,
                            true,
                        )
                        .with_provenance(output.provenance.clone()),
                    );
                } else {
                    sink.emit(
                        SessionEvent::tool_result_with_attachments(
                            session_id,
                            &tc.id,
                            Some(&tc.function.name),
                            &output.summary,
                            true,
                            image_attachments.clone(),
                        )
                        .with_provenance(output.provenance.clone()),
                    );
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
                // 失败不落 taint：没有成功摄入/读取，不把下一批工具误伤。
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

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ToolCall, ToolCallFunction};
    use crate::tools::{Tool, ToolError, ToolOutput, ToolProvenance, ToolRegistry};

    fn call(name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: format!("call_{name}"),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: name.into(),
                arguments: args.into(),
            },
        }
    }

    fn test_sink() -> EventSink {
        EventSink::new(Arc::new(Mutex::new(SessionLog::new())), |_| {})
    }

    fn test_sink_capturing() -> (EventSink, Arc<Mutex<Vec<SessionEvent>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        let sink = EventSink::new(Arc::new(Mutex::new(SessionLog::new())), move |event| {
            if let crate::events::IpcEvent::Session(event) = event {
                sink_seen.lock().unwrap().push(event);
            }
        });
        (sink, seen)
    }

    /// 预检可能进入审批等待：后台自动批准，返回判定与 IPC 事件供断言。
    async fn verdicts_with_approval(
        ctx: &ToolContext,
        calls: &[ToolCall],
        workspace: &Path,
    ) -> (Vec<ToolVerdict>, Arc<Mutex<Vec<SessionEvent>>>) {
        let approvals = Arc::new(ApprovalRegistry::new());
        let (sink, seen) = test_sink_capturing();
        let call_ids: Vec<String> = calls.iter().map(|c| c.id.clone()).collect();
        let resolver = {
            let approvals = Arc::clone(&approvals);
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                for call_id in call_ids {
                    approvals.resolve("s1", &call_id, UserDecision::Approved);
                }
            })
        };
        let verdicts = preflight_tool_calls(
            "s1",
            calls,
            ExecutionMode::Auto,
            &sink,
            &approvals,
            &AbortSignal::new(),
            workspace,
            ctx,
        )
        .await;
        let _ = resolver.await;
        (verdicts, seen)
    }

    fn untrusted_skill_index() -> (PathBuf, Arc<crate::skills_index::SkillIndex>) {
        let root = temp_dir("skill-root");
        let pkg = root.join("脚本技能");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("SKILL.md"),
            "---\nname: 脚本技能\ndescription: 含脚本\nsource: community\n---\n\n## 正文",
        )
        .unwrap();
        std::fs::write(pkg.join("run.ps1"), "Write-Host hi").unwrap();
        let index = crate::skills_index::SkillIndex::load(&root).unwrap();
        (pkg, Arc::new(index))
    }

    fn approval_rule(seen: &Arc<Mutex<Vec<SessionEvent>>>) -> Option<String> {
        seen.lock().unwrap().iter().find_map(|event| {
            if matches!(event, SessionEvent::ToolApprovalRequest { .. }) {
                serde_json::to_value(event)
                    .ok()
                    .and_then(|v| v.get("ruleId").and_then(|x| x.as_str()).map(str::to_string))
            } else {
                None
            }
        })
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("prts-taint-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn verdicts_for(
        ctx: &ToolContext,
        calls: &[ToolCall],
        workspace: &Path,
    ) -> Vec<ToolVerdict> {
        preflight_tool_calls(
            "s1",
            calls,
            ExecutionMode::Auto,
            &test_sink(),
            &ApprovalRegistry::new(),
            &AbortSignal::new(),
            workspace,
            ctx,
        )
        .await
    }

    #[tokio::test]
    async fn same_batch_external_and_local_read_is_denied() {
        let ws = temp_dir("batch");
        let ctx = ToolContext::new();
        let calls = vec![
            call("web_fetch", r#"{"url":"https://prts.wiki/a"}"#),
            call("read_file", r#"{"path":"a.txt"}"#),
        ];
        let v = verdicts_for(&ctx, &calls, &ws).await;
        assert!(matches!(v[0], ToolVerdict::Run { .. }));
        assert!(matches!(
            &v[1],
            ToolVerdict::Denied {
                reason_code: "taint_same_batch",
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn external_taint_blocks_outside_file_read_but_not_inside() {
        let ws = temp_dir("ext");
        let outside = temp_dir("outside");
        let ctx = ToolContext::new();
        ctx.taint().mark_external_content();

        let calls = vec![
            call(
                "read_file",
                &serde_json::json!({ "path": outside.join("secret.txt").to_string_lossy() })
                    .to_string(),
            ),
            call("read_file", r#"{"path":"a.txt"}"#),
        ];
        let v = verdicts_for(&ctx, &calls, &ws).await;
        assert!(matches!(
            &v[0],
            ToolVerdict::Denied {
                reason_code: "taint_external_local",
                ..
            }
        ));
        assert!(matches!(
            v[1],
            ToolVerdict::Run {
                outside_read: false
            }
        ));

        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn outside_read_blocks_later_external_tools() {
        let ws = temp_dir("out");
        let ctx = ToolContext::new();
        ctx.taint().mark_outside_read();
        let calls = vec![call("web_search", r#"{"query":"prts"}"#)];
        let v = verdicts_for(&ctx, &calls, &ws).await;
        assert!(matches!(
            &v[0],
            ToolVerdict::Denied {
                reason_code: "taint_outside_web",
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn external_taint_blocks_outside_run_command_by_gate_audit() {
        let ws = temp_dir("cmd");
        let outside = temp_dir("cmd-out");
        let ctx = ToolContext::new();
        ctx.taint().mark_external_content();
        let calls = vec![call(
            "run_command",
            &serde_json::json!({
                "command": "type secret.txt",
                "cwd": outside.to_string_lossy()
            })
            .to_string(),
        )];
        let v = verdicts_for(&ctx, &calls, &ws).await;
        assert!(matches!(
            &v[0],
            ToolVerdict::Denied {
                reason_code: "taint_external_local",
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(&outside);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn untrusted_skill_dir_command_asks_in_auto_mode() {
        let (pkg, index) = untrusted_skill_index();
        let ws = temp_dir("skill-ws");
        let mut ctx = ToolContext::new();
        ctx.skill_index = Some(index);
        let calls = vec![call(
            "run_command",
            &serde_json::json!({
                "command": "Get-Process",
                "cwd": pkg.to_string_lossy()
            })
            .to_string(),
        )];
        let (verdicts, seen) = verdicts_with_approval(&ctx, &calls, &ws).await;
        assert!(matches!(verdicts[0], ToolVerdict::Run { .. }));
        assert_eq!(
            approval_rule(&seen).as_deref(),
            Some("skill-package-script")
        );
        let _ = std::fs::remove_dir_all(pkg.parent().unwrap());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn same_batch_untrusted_activation_forces_ask() {
        let (pkg, index) = untrusted_skill_index();
        let ws = temp_dir("skill-batch-ws");
        let mut ctx = ToolContext::new();
        ctx.skill_index = Some(index);
        let calls = vec![
            call("activate_skill", r#"{"name":"脚本技能"}"#),
            call(
                "run_command",
                &serde_json::json!({"command": "Get-Process"}).to_string(),
            ),
        ];
        let (verdicts, seen) = verdicts_with_approval(&ctx, &calls, &ws).await;
        assert!(matches!(verdicts[0], ToolVerdict::Run { .. }));
        assert!(matches!(verdicts[1], ToolVerdict::Run { .. }));
        assert_eq!(
            approval_rule(&seen).as_deref(),
            Some("skill-untrusted-active")
        );
        let _ = std::fs::remove_dir_all(pkg.parent().unwrap());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn normal_command_not_forced_by_untrusted_skill_install() {
        let (_pkg, index) = untrusted_skill_index();
        let ws = temp_dir("skill-normal-ws");
        let mut ctx = ToolContext::new();
        ctx.skill_index = Some(index);
        let calls = vec![call(
            "run_command",
            &serde_json::json!({"command": "Get-Process"}).to_string(),
        )];
        let (sink, seen) = test_sink_capturing();
        let verdicts = preflight_tool_calls(
            "s1",
            &calls,
            ExecutionMode::Auto,
            &sink,
            &ApprovalRegistry::new(),
            &AbortSignal::new(),
            &ws,
            &ctx,
        )
        .await;
        assert!(matches!(verdicts[0], ToolVerdict::Run { .. }));
        assert!(
            approval_rule(&seen).is_none(),
            "普通命令不应被 Skill 信任模型拦截"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn activated_untrusted_skill_forces_later_command_ask() {
        let ws = temp_dir("skill-active-ws");
        let ctx = ToolContext::new();
        ctx.mark_active_untrusted_skill("脚本技能");
        let calls = vec![call(
            "run_command",
            &serde_json::json!({"command": "Get-Process"}).to_string(),
        )];
        let (verdicts, seen) = verdicts_with_approval(&ctx, &calls, &ws).await;
        assert!(matches!(verdicts[0], ToolVerdict::Run { .. }));
        assert_eq!(
            approval_rule(&seen).as_deref(),
            Some("skill-untrusted-active")
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn new_tool_context_resets_taint() {
        let first = ToolContext::new();
        first.taint().mark_external_content();
        first.taint().mark_outside_read();
        let next = ToolContext::new();
        assert!(!next.taint().has_external_content());
        assert!(!next.taint().has_outside_read());
    }

    /// 外部来源探针：执行成功应把本轮标记为“已摄入外部资料”。
    struct ExternalProbe;

    impl Tool for ExternalProbe {
        fn name(&self) -> &str {
            "external_probe"
        }
        fn description(&self) -> &str {
            "test"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }
        fn execute(&self, _ctx: &ToolContext, _params: &str) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("外部正文")
                .with_provenance(ToolProvenance::external("example.com/docs")))
        }
    }

    #[test]
    fn successful_external_provenance_marks_turn_taint() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(ExternalProbe)).unwrap();
        let ctx = ToolContext::new();
        let result = StreamResult {
            text: String::new(),
            tool_calls: vec![call("external_probe", "{}")],
            reasoning: None,
            usage: None,
        };
        let mut messages = vec![ChatMessage::user("测试")];
        let mut state = TurnState::new();
        let verdicts = vec![ToolVerdict::Run {
            outside_read: false,
        }];
        execute_tools_and_append(
            "s1",
            &mut messages,
            &result,
            &reg,
            &ctx,
            &test_sink(),
            &mut state,
            Some(&verdicts),
            &AbortSignal::new(),
        );
        assert!(ctx.taint().has_external_content());
        assert!(!ctx.taint().has_outside_read());
    }
}
