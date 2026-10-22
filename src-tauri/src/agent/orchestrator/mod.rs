//! 调度中枢 —— 事件循环与上下文管理
//!
//! 职责：接收用户输入和历史消息，执行调度循环（调 LLM → 有工具请求则执行 → 继续），
//! 通过回调推送事件流，最终产出完整文本。
//!
//! ## 循环终止条件（模块 5 · turn/step 模型，P0）
//!
//! 一次 `agent_process` 调用 = 一个 **turn**；turn 内每次 LLM 调用 + 其工具执行
//! = 一个 **step**。终止条件由 turn/end 的显式 reason 表达：
//!
//! - `completed`：LLM 返回的 tool_calls 为空（正常完成）
//! - `stagnation`：停滞检测触发（PRTS 独有资产，升格为一等终止原因）
//! - `error`：LLM 调用或工具执行出错
//! - `aborted` / `max-tokens`：预留词汇（随 stop 生成 / 截断处理落地）
//!
//! text 是否为空不参与终止判断——文本在 chat_stream() 过程中
//! 已通过回调实时推送，循环继续不影响前端展示。
//!
//! ## 会话日志（模块 1，P0）
//!
//! 后端 append-only session log 是事实来源：所有 LLM 可见事实（user 消息、
//! 流式 chunk、工具调用/结果、请求头快照）都写入 `SessionLog`，
//! LLM 历史由 `derive_messages()` 从 log 派生（替代 convert_history 的
//! 多轮穿透逻辑）。当前为双写观测期：同一事实同时推 IPC（AgentEvent，
//! 前端消费）与写 log；前端翻转为投影缓存（WP2）后 IPC 词汇同步切换。
//!
//! ## 调度循环与 RePlan 的关系
//!
//! 调度循环是硬编码的 Rust 基础设施：调 LLM → 执行工具 → 追加结果 → 重复。
//! RePlan 是 LLM 的行为策略，通过系统提示词指引 LLM 在对话中自行判断：
//! 简单问题直接回答，复杂问题先规划再执行、中间根据结果修正计划。
//! "计划"是对话历史中的文本，不是 Rust 数据结构。
//! 调度循环为 RePlan 提供了"多轮交互"的先决条件，但它本身不包含任何规划逻辑。

pub mod compaction;
pub mod context;
mod events;
mod llm_call;
pub mod memory;
mod memory_parse;
mod memory_prompt;
pub mod prompt;
mod prompt_registry;
mod retry;
pub mod title;
mod token_estimate;
mod tool_loop;
mod turn;
mod turn_loop;

use events::EventSink;
pub use retry::{is_retryable, RetryPolicy};
#[allow(unused_imports)] // 旧包装仅测试使用；生产导入 with_blocks
use tool_loop::{
    execute_tools_and_append, execute_tools_and_append_with_blocks, preflight_tool_calls,
};
use turn::TurnState;
use turn_loop::dispatch_turn_with_memory;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(test)]
use std::hash::Hasher;

use crate::approval::{ApprovalRegistry, UserDecision, APPROVAL_WAIT_TIMEOUT};
use crate::db::Database;
use crate::debug;
#[cfg(test)]
use crate::events::AgentEvent;
#[cfg(test)]
use crate::llm::TokenUsage;

use crate::events::{AgentPhase, FrontendMessage, IpcEvent};
use crate::exec_settings::ExecutionMode;
use crate::llm::{
    usage_stat, ChatMessage, ClientRegistry, LlmClient, LlmConfig, LlmError, ModelRegistry,
    ReasoningPolicy, StreamEvent, StreamResult, UsageStat,
};
use crate::session::{
    ApprovalDecision, ApprovalPrivilege, ApprovalRejectReason, ApprovalRisk, ChunkKind,
    SessionEvent, SessionLog, TurnEndReason,
};
use crate::session_store::CompactionMeta;
use crate::steer_queue::SteerQueue;
use crate::tools::{SkillEventMark, ToolContext, ToolError, ToolRegistry};
use crate::turn_registry::AbortSignal;

// ═══════════════════════════════════════════════════════
// 错误类型
// ═══════════════════════════════════════════════════════

/// 调度中枢的统一错误类型。
/// 包装 LLM 错误和工具错误，供上层（commands 层）处理。
#[derive(Debug)]
pub enum OrchestratorError {
    Llm(LlmError),
    Tool(ToolError),
}

impl From<LlmError> for OrchestratorError {
    fn from(e: LlmError) -> Self {
        Self::Llm(e)
    }
}

impl From<ToolError> for OrchestratorError {
    fn from(e: ToolError) -> Self {
        Self::Tool(e)
    }
}

impl std::fmt::Display for OrchestratorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Llm(e) => write!(f, "调度中枢 LLM 错误：{e}"),
            Self::Tool(e) => write!(f, "调度中枢工具错误：{e}"),
        }
    }
}

// ═══════════════════════════════════════════════════════
// TurnOutcome —— turn 的显式终止结果
// ═══════════════════════════════════════════════════════

/// 调度循环的正常终止结果（模块 5：终止原因显式化）。
///
/// `Err(OrchestratorError)` 对应 reason=error；此枚举承载
/// 非错误类终止，process() 据此写 turn/end 的 reason。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnOutcome {
    /// LLM 不再请求工具，正常完成
    Completed,
    /// 停滞检测触发，提前终止
    Stagnation,
    /// 用户中止（M3-B：abort_turn 命令置位 → 取消点收敛退出）。
    /// Aborted 出口在 process（清 approvals + turn/end(aborted) 双写），
    /// dispatch_turn 内不写 step/end 与 turn/end（与 error 路径一致）。
    /// 携带中止时已累计的本轮用量（统计诚实性：中止也是真实消耗）。
    Aborted(UsageStat),
}

// ═══════════════════════════════════════════════════════
// process —— 调度中枢唯一入口
// ═══════════════════════════════════════════════════════

/// 执行一次完整的 Agent 处理：会话日志 → 派生上下文 → 调度循环 → 事件推送。
///
/// # 参数
///
/// * `session_id` —— 前端生成的本次请求唯一标识，随所有事件回传
/// * `history` —— 前端传入的完整对话历史（含本轮 user 消息），用于播种日志
/// * `tools` —— 已注册的工具注册表引用
/// * `registry` —— LLM 客户端注册表（按 ApiType 路由适配器）
/// * `config` —— LLM 连接配置（provider/model/api_key 等）
/// * `model_registry` —— 模型元数据注册表（resolve_compat → reasoning 策略）
/// * `db` —— 数据库（Option 支持 Mock 测试场景）
/// * `emit` —— 事件推送回调。commands 层将 Tauri 的 `AppHandle::emit` 封装后传入
///
/// # 事件推送顺序
///
/// 一次完整的处理周期内，emit 被调用的典型顺序：
///
/// ```text
/// Phase(reasoning)                  ← 开始思考
/// ReasoningDelta (可能多次)          ← 推理链（同时写 log: assistant/chunk）
/// ReasoningDone                     ← 推理结束
/// Phase(executing) + ToolCall        ← 调工具（同时写 log: tool/call）
/// ToolResult                         ← 工具结果（同时写 log: tool/result）
/// Phase(generating) + TextDelta × N  ← 生成回答（同时写 log: assistant/chunk）
/// GenerationDone                     ← 本轮结束
/// ```
///
/// 不承诺线性推进。reasoning 和 executing/generating 可交替，
/// 取决于 LLM 何时决定思考、何时决定行动。
///
/// 会话日志侧（不可见，供回放/遥测）：
/// turn/start → 播种历史 → request/header（仅变化时追加）→
/// step/start × N … step/end × N → assistant/message → turn/end{reason}
#[allow(dead_code)] // 测试用无目录无记忆薄包装；生产走 process_with_memory
#[allow(clippy::too_many_arguments)]
pub async fn process(
    session_id: &str,
    history: Vec<FrontendMessage>,
    tools: &Arc<ToolRegistry>,
    registry: &ClientRegistry,
    config: &LlmConfig,
    model_registry: &ModelRegistry,
    db: Option<Arc<Database>>,
    mode: ExecutionMode,
    approvals: &ApprovalRegistry,
    abort: &AbortSignal,
    steer_queue: &SteerQueue,
    workspace_root: PathBuf,
    emit: impl Fn(IpcEvent) + Send + Sync + 'static,
) -> Result<(), OrchestratorError> {
    process_with_memory(
        session_id,
        history,
        tools,
        registry,
        config,
        model_registry,
        db,
        mode,
        approvals,
        abort,
        steer_queue,
        workspace_root,
        emit,
        context::assemble_system_prompt_with_memory("", ""), // 无目录 / 无记忆
        tools.list_defs(),
    )
    .await
}

/// 同 [`process`]，但**前缀由命令层解析后传入**（`prefix_snapshot.rs`）。
///
/// `system_prompt` 为对话冻结的完整前缀全（人格 + 界面 + 目录 + 记忆区），
/// `tool_defs` 与之同源（同一快照）。冻结后整个对话逐字节不变，故每 step
/// 重写 `messages[0]` 的内容恒定，`push_header` 去重不误判（前缀缓存不破）。
#[allow(clippy::too_many_arguments)]
pub async fn process_with_memory(
    session_id: &str,
    history: Vec<FrontendMessage>,
    tools: &Arc<ToolRegistry>,
    registry: &ClientRegistry,
    config: &LlmConfig,
    model_registry: &ModelRegistry,
    db: Option<Arc<Database>>,
    mode: ExecutionMode,
    approvals: &ApprovalRegistry,
    abort: &AbortSignal,
    steer_queue: &SteerQueue,
    workspace_root: PathBuf,
    emit: impl Fn(IpcEvent) + Send + Sync + 'static,
    system_prompt: String,
    tool_defs: Vec<crate::llm::ToolDef>,
) -> Result<(), OrchestratorError> {
    let log = Arc::new(Mutex::new(SessionLog::new()));
    process_with_log(
        session_id,
        HistorySeed::Frontend(history),
        log,
        tools,
        registry,
        config,
        model_registry,
        db,
        mode,
        approvals,
        abort,
        steer_queue,
        workspace_root,
        emit,
        system_prompt,
        tool_defs,
    )
    .await
}

/// 历史来源：前端 history（legacy）或后端官方 log（primary）。
pub enum HistorySeed {
    Frontend(Vec<FrontendMessage>),
    Log {
        turns: Vec<Vec<SessionEvent>>,
        user: Box<SessionEvent>,
        /// 会话级压缩记录（primary 必须按 retainedFrom + summary 投影）。
        compaction: Option<CompactionMeta>,
    },
}

/// 同 [`process_with_memory`]，但使用调用方传入的 log（primary 写盘需要）。
#[allow(clippy::too_many_arguments)]
pub async fn process_with_log(
    session_id: &str,
    history: HistorySeed,
    log: Arc<Mutex<SessionLog>>,
    tools: &Arc<ToolRegistry>,
    registry: &ClientRegistry,
    config: &LlmConfig,
    model_registry: &ModelRegistry,
    db: Option<Arc<Database>>,
    mode: ExecutionMode,
    approvals: &ApprovalRegistry,
    abort: &AbortSignal,
    steer_queue: &SteerQueue,
    workspace_root: PathBuf,
    emit: impl Fn(IpcEvent) + Send + Sync + 'static,
    system_prompt: String,
    tool_defs: Vec<crate::llm::ToolDef>,
) -> Result<(), OrchestratorError> {
    // ── 0. 按 ApiType 选择适配器 ──
    let llm = registry.select(&config.api_type).ok_or_else(|| {
        OrchestratorError::Llm(LlmError::Config(format!(
            "协议 {:?} 暂未支持",
            config.api_type
        )))
    })?;
    let llm: &dyn crate::llm::LlmClient = &**llm;

    // ── 1. 效果层 compat → reasoning 回传策略（缺陷 1 应用点）──
    let resolved =
        model_registry.get_resolved_compat(&config.provider, &config.model, config.api_type);
    let policy = resolved.policy;

    // ── 2. 会话日志：事实来源（模块 1）──
    // turn 边界 + 历史播种 + request/header 快照（仅变化时追加）
    // step/request/header 为结构性事实，只写 log 不推 IPC（前端投影不消费）
    //
    // M1 修复（Reminder 投递机制）：历史完全由前端决定（含块 user 消息由
    // 前端拼装后随请求到达并归档冻结），后端零注入——seed 只做原样播种。
    // Skill 目录索引：turn 级快照（每 turn 入口重建——Skill 生命周期绑定
    // 本轮对话：轮内激活、轮末随 ActiveContext 自动卸载，跨轮按需重新激活）。
    // **仅供激活工具与展示（保持活体）**；system 的 Catalog 用对话冻结目录文本。
    // 加载失败降级为空索引（激活工具恒未命中），不阻断本轮处理。
    let skill_index = Arc::new(
        match crate::skills_index::SkillIndex::load(&crate::skills_index::skills_dir()) {
            Ok(idx) => idx,
            Err(e) => {
                eprintln!("[orchestrator] Skill 索引加载失败（Skill 目录将为空）：{e}");
                crate::skills_index::SkillIndex::empty()
            }
        },
    );
    // 上下文管理修正（2026-08）：Active/Reference 动态层已移除，无状态容器。
    // 动态内容（skill 正文等）一律以工具结果消息进历史，不再进 system prompt。
    let persist_tool_calls = matches!(&history, HistorySeed::Log { .. });
    {
        let mut log = log.lock().unwrap();
        log.append(SessionEvent::turn_start(session_id));
        match history {
            HistorySeed::Frontend(history) => {
                log.seed_from_history(session_id, history);
                log.mark_turn_boundary();
            }
            HistorySeed::Log {
                turns,
                user,
                compaction,
            } => {
                let (kept, summary) = match compaction {
                    Some(c) if c.retained_from > 0 => {
                        let start = (c.retained_from as usize).min(turns.len());
                        (turns[start..].to_vec(), Some(c.summary))
                    }
                    _ => (turns, None),
                };
                if let Some(summary) = summary {
                    log.append(SessionEvent::user_message(
                        session_id,
                        &SessionLog::compaction_summary_message(&summary),
                    ));
                }
                for turn in kept {
                    log.seed_events(turn);
                }
                log.append(*user);
                // primary：当前轮从 user 消息开始（历史只用于派生，不写回官方 log）。
                log.mark_turn_boundary();
            }
        }
        log.push_header(session_id, &system_prompt, &tool_defs);
    }

    // ── 3. 派生 LLM 历史（替代 convert_history）→ 组装初始上下文 ──
    let derived = log.lock().unwrap().derive_messages(policy);
    let mut messages = context::build_context_with_system(&derived, system_prompt.clone());

    // ── 3.5 模型视觉能力 ──
    // 图像物化**不在这里做**：它必须发生在每次 LLM 请求前（见 dispatch 循环的
    // prepare_request_images）——工具返回的图是轮内追加的，只在轮起点处理会
    // 让它永远送不到模型手里（2026-10-06 修复：MAA 截图曾因此只到路径）。
    let supports_vision = model_registry
        .get_model(&config.provider, &config.model)
        .map(|m| m.supports_vision)
        .unwrap_or(false);

    // ── 4. 进入调度循环（一个 turn，零或多个 step）──
    // workspace_root：自动化模式地域性信任门的信任根。命令层装配时从
    // settings.workspace 段取一次注入（与 lib.rs 装配 RunCommandTool/Policy
    // 的来源一致——run_command 工具构造已持有同一 workspace_root）；经
    // dispatch_turn 显式注入 preflight。
    let sink = EventSink::new_with_persist(log.clone(), emit, persist_tool_calls);
    let outcome = dispatch_turn_with_memory(
        session_id,
        &sink,
        &mut messages,
        &skill_index,
        tools,
        &tool_defs,
        llm,
        config,
        db,
        policy,
        mode,
        approvals,
        abort,
        steer_queue,
        &workspace_root,
        &system_prompt,
        supports_vision,
    )
    .await;
    // ── M3-B 责任表⑦：Aborted 出口（dispatch 不写 turn/end，此处补）──
    // clear_session 兑底清 pending sender（防前端卡残留——preflight 中止出口
    // 已对本卡补 result，此处清的是尚未轮到/滞留的余卡）；turn/end(aborted)
    // sink.emit 双写（IPC 必须——前端归档唯一轮结束信号）。
    // 复用正常收尾通道（遥测照常），turn/end 已由上方补写。
    let outcome = match outcome {
        Ok(TurnOutcome::Aborted(stat)) => {
            approvals.clear_session(session_id);
            sink.emit(SessionEvent::turn_end_with_usage(
                session_id,
                TurnEndReason::Aborted,
                Some(stat),
            ));
            Ok(TurnOutcome::Completed)
        }
        other => other,
    };

    // ── 5. error 路径补写 turn/end（正常路径由 dispatch_turn 自写）──
    if outcome.is_err() {
        sink.log_only(SessionEvent::turn_end(session_id, TurnEndReason::Error));
    }

    // ── 6. 遥测：session log 投影（模块 8 P0，PRTS_DEBUG=1 时输出）──
    // log 是事实来源，调试开关只是过滤/采样器
    crate::observability::log_session(session_id, &log.lock().unwrap());

    outcome.map(|_| ())
}

// ═══════════════════════════════════════════════════════
// 调度循环（turn / step 模型）
// ═══════════════════════════════════════════════════════

/// 调度循环：turn 内反复执行 step（调 LLM → 判断是否需要工具 → 执行工具 → 继续），
/// 直到 LLM 不再请求工具或停滞检测触发。
///
/// 这是一个硬编码的 Rust 循环，不包含任何规划逻辑。
/// LLM 在循环中自行决定何时调工具、何时直接回答、
/// 是否在回答中复用之前的计划（RePlan 思考模式由系统提示词驱动）。
/// 调度循环的角色是"提供多轮交互的能力"，不是"替 LLM 规划"。
///
/// ## system prompt 静态（2026-08 修正）
///
/// system 由 assemble_system_prompt 静态组装（人格 + Skill 目录），轮内
/// 不再变化。push_header 自带去重，system 不变时不追加
/// （"模型到底看到了什么"可审计且不膨胀）。
///
/// ## 停滞检测（模块 5：升格为一等终止原因）
///
/// 每次工具执行后对 (工具名, args, result) 三元组取哈希指纹。
/// 同一指纹在最近 10 次执行中出现 ≥5 次 → 判定为停滞 → 返回
/// `TurnOutcome::Stagnation`（turn/end reason=stagnation，不再以
/// error 事件表达）。覆盖四种停滞模式：死循环、渔夫甩竿、乒乓球、报错重试。
/// ## 取消点（M3-B §3：中止通道消费面）
///
/// abort 在以下位置收敛：
/// - LLM 段：biased select（abort 分支 return Aborted，drop llm future → 断流）
/// - 审批等待（preflight 内三路 select，中止出口补本卡 result 后提前返回）
/// - 工具执行（runner 轮询 kill + 串行段剩余槽位补「未执行」型 result）
/// - 各段返回后的复查（中止优先于停滞/执行/step/end 闭合）
///
/// abort 出口**不写 step/end 与 turn/end**（与 error 路径现状一致；turn/end 由
/// process 的 Aborted 出口补写，log 容忍悬挂 step/start——前端不消费 step）。
#[allow(dead_code)] // 测试用无目录无记忆薄包装；生产走 dispatch_turn_with_memory
#[allow(clippy::too_many_arguments)]
async fn dispatch_turn(
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
) -> Result<TurnOutcome, OrchestratorError> {
    dispatch_turn_with_memory(
        session_id,
        sink,
        messages,
        skill_index,
        tools,
        tool_defs,
        llm,
        config,
        db,
        policy,
        mode,
        approvals,
        abort,
        steer_queue,
        workspace_root,
        &context::assemble_system_prompt_with_memory("", ""), // 无目录 / 无记忆
        false, // supports_vision（薄包装：无模型注册表，不物化）
    )
    .await
}

// ═══════════════════════════════════════════════════════
// Phase 辅助函数
// ═══════════════════════════════════════════════════════

/// 仅在 phase 确实变化时 emit Phase 事件。
///
/// 避免同一 phase 被重复推送（例如连续多个 TextDelta 不应
/// 每次都 emit Phase(generating)）。phase 是纯 UI 信号，不写 log。
fn switch_phase(
    state: &std::sync::Mutex<AgentPhase>,
    new_phase: AgentPhase,
    session_id: &str,
    sink: &EventSink,
) {
    let mut current = state.lock().unwrap();
    if *current != new_phase {
        sink.phase(session_id, new_phase.clone());
        *current = new_phase;
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::llm_call::{
        call_llm_with_retry, handle_stream_event, prepare_request_images, NO_VISION_NOTICE,
    };
    use super::*;
    use std::collections::VecDeque;

    /// 文本模型看不到图时，必须被明确告知「图没进去」——否则会当自己看过了。
    #[test]
    fn text_only_model_is_told_the_image_did_not_go_through() {
        let mut messages = vec![ChatMessage {
            role: crate::llm::MessageRole::Tool,
            content: "已截取当前画面（C:/x/latest.png）。".into(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
            provider_blocks: None,
            image_paths: Some(vec!["C:/x/shot.png".into()]),
            images: None,
        }];
        prepare_request_images(&mut messages, false);
        assert!(messages[0].content.contains(NO_VISION_NOTICE));
        assert!(messages[0].image_paths.is_none());
        // 每次请求前都会进这里：补过就不再补，免得逐轮堆积。
        prepare_request_images(&mut messages, false);
        assert_eq!(messages[0].content.matches(NO_VISION_NOTICE).count(), 1);
    }
    use std::future::Future;
    use std::pin::Pin;

    // ── 测试辅助：双写 sink（内存收集 emit + log）──

    #[allow(clippy::type_complexity)] // 测试 sink 的三元组返回；仅测试辅助，拆类型别名收益低
    fn test_sink() -> (EventSink, Arc<Mutex<Vec<IpcEvent>>>, Arc<Mutex<SessionLog>>) {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = Arc::new(Mutex::new(SessionLog::new()));
        let events_clone = events.clone();
        let emit = move |e: IpcEvent| {
            events_clone.lock().unwrap().push(e);
        };
        let sink = EventSink::new(log.clone(), emit);
        (sink, events, log)
    }

    /// 从 IPC 载荷中过滤持久事实（SessionEvent）。
    fn session_events(ipc: &[IpcEvent]) -> Vec<&SessionEvent> {
        ipc.iter()
            .filter_map(|e| match e {
                IpcEvent::Session(s) => Some(s),
                IpcEvent::Agent(_) => None,
            })
            .collect()
    }

    /// primary 最小闭环：后端 log 播种 → 处理 → 事件写官方 jsonl → 重新读出派生。
    /// 这条链就是 C-lite P2 的核心；不依赖前端 history。
    #[tokio::test]
    async fn primary_log_roundtrip_uses_backend_history_without_frontend_payload() {
        use crate::llm::mock::MockLlmClient;
        use crate::llm::ApiType;
        use crate::session_store::{AppendTurnReq, SessionStore};

        let dir = std::env::temp_dir().join(format!(
            "prts-primary-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = SessionStore::new(&dir);
        store
            .ensure_conversation("c1", Some("测试"), Some(1), None, None)
            .unwrap();

        let mock = Arc::new(MockLlmClient::simple_reply("答"));
        let mut client_registry = ClientRegistry::new();
        client_registry.register(ApiType::OpenAiCompletions, mock);
        let tools = Arc::new(ToolRegistry::new());
        let tool_defs = tools.list_defs();
        let log = Arc::new(Mutex::new(SessionLog::new()));
        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).unwrap();

        let seed = HistorySeed::Log {
            turns: Vec::new(),
            user: Box::new(SessionEvent::user_message("s1", "你好")),
            compaction: None,
        };
        process_with_log(
            "s1",
            seed,
            Arc::clone(&log),
            &tools,
            &client_registry,
            &LlmConfig::default(),
            &ModelRegistry::from_json(include_str!("../../../resources/models.json")).unwrap(),
            None,
            ExecutionMode::Ask,
            &ApprovalRegistry::new(),
            &AbortSignal::new(),
            &SteerQueue::new(),
            ws,
            |_| {},
            "system".into(),
            tool_defs.clone(),
        )
        .await
        .unwrap();

        let events = log.lock().unwrap().events().to_vec();
        let req = AppendTurnReq {
            conversation_id: "c1".into(),
            events_json: serde_json::to_string(&events).unwrap(),
            title: Some("测试".into()),
            created_at: None,
            model_name: None,
            context_length: None,
            context_tokens: None,
            last_notice_seq: None,
            usage_deltas: None,
        };
        store.append_turn(&req).unwrap();

        let turns = store.load_turns("c1").unwrap();
        assert_eq!(turns.len(), 1);
        let mut replay = SessionLog::new();
        replay.seed_events(turns.into_iter().flatten());
        let derived = replay.derive_messages(ReasoningPolicy::Preserve);
        assert!(derived.iter().any(|m| m.content == "你好"));
        assert!(derived.iter().any(|m| m.content == "答"));
        assert!(!derived.iter().any(|m| {
            m.role == crate::llm::MessageRole::Tool && m.content.contains("未执行")
        }));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── handle_stream_event（双写）──

    #[test]
    fn text_delta_emits_phase_generating_and_logs_chunk() {
        let (sink, events, log) = test_sink();
        let phase = std::sync::Mutex::new(AgentPhase::Reasoning);

        handle_stream_event(
            "s1",
            StreamEvent::TextDelta {
                content: "你好".into(),
            },
            &phase,
            &sink,
            &ToolRegistry::new(),
            &ToolContext::new(),
        );

        let emitted: Vec<_> = events.lock().unwrap().drain(..).collect();
        // 应该有 Phase(generating) + assistant/chunk(Text)
        assert_eq!(emitted.len(), 2);
        assert!(matches!(
            emitted[0],
            IpcEvent::Agent(AgentEvent::Phase { ref phase, .. }) if matches!(phase, AgentPhase::Generating)
        ));
        assert!(matches!(
            emitted[1],
            IpcEvent::Session(SessionEvent::AssistantChunk { kind: ChunkKind::Text, ref content, .. }) if content == "你好"
        ));

        // 双写：log 中应有 assistant/chunk(Text)
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert!(matches!(
            log.events()[0],
            SessionEvent::AssistantChunk { kind: ChunkKind::Text, ref content, .. } if content == "你好"
        ));
    }

    #[test]
    fn primary_tool_call_is_logged_immediately_for_abort_recovery() {
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = Arc::new(Mutex::new(SessionLog::new()));
        let events_clone = events.clone();
        let sink = EventSink::new_with_persist(
            log.clone(),
            move |e| {
                events_clone.lock().unwrap().push(e);
            },
            true,
        );
        let phase = std::sync::Mutex::new(AgentPhase::Reasoning);

        handle_stream_event(
            "s1",
            StreamEvent::ToolCall {
                id: "c1".into(),
                name: "echo".into(),
                arguments: "{}".into(),
            },
            &phase,
            &sink,
            &ToolRegistry::new(),
            &ToolContext::new(),
        );

        let log = log.lock().unwrap();
        assert!(
            matches!(log.events().last(), Some(SessionEvent::ToolCall { call_id, .. }) if call_id == "c1"),
            "primary 必须在流式阶段落 tool/call（preflight 中止才保得住卡片）"
        );
    }

    #[test]
    fn tool_call_emits_ipc_only_without_log() {
        let (sink, events, log) = test_sink();
        let phase = std::sync::Mutex::new(AgentPhase::Reasoning);

        handle_stream_event(
            "s1",
            StreamEvent::ToolCall {
                id: "c1".into(),
                name: "echo".into(),
                arguments: "{}".into(),
            },
            &phase,
            &sink,
            &ToolRegistry::new(),
            &ToolContext::new(),
        );

        let emitted: Vec<_> = events.lock().unwrap().drain(..).collect();
        // Phase(executing) + tool/call（IPC）
        assert_eq!(emitted.len(), 2);
        assert!(matches!(
            emitted[0],
            IpcEvent::Agent(AgentEvent::Phase { ref phase, .. }) if matches!(phase, AgentPhase::Executing)
        ));
        assert!(matches!(
            emitted[1],
            IpcEvent::Session(SessionEvent::ToolCall { ref call_id, ref tool_name, .. }) if call_id == "c1" && tool_name == "echo"
        ));

        // tool/call 入 log 由 execute_tools_and_append 批量负责：
        // 流式回调只推 IPC，log 此时为空（避免双记）
        assert_eq!(log.lock().unwrap().len(), 0);
    }

    /// 展示探针工具：tool/call 事件应携带 Registry 解析出的展示切片。
    struct DisplayProbeTool;

    impl crate::tools::Tool for DisplayProbeTool {
        fn name(&self) -> &str {
            "display_probe"
        }
        fn description(&self) -> &str {
            "展示探针"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }
        fn execute(
            &self,
            _ctx: &ToolContext,
            _params: &str,
        ) -> Result<crate::tools::ToolOutput, ToolError> {
            Ok(crate::tools::ToolOutput::text("ok"))
        }
        fn display(&self, _ctx: &ToolContext, _params: &str) -> crate::tools::ToolDisplay {
            crate::tools::ToolDisplay::info("探针对象")
        }
    }

    #[test]
    fn tool_call_ipc_carries_display_resolved_before_emit() {
        // “进行中即显示”数据链路：handle_stream_event 收到 ToolCall 时
        // 解析展示切片并随 tool/call IPC 一并下发。
        let (sink, events, _log) = test_sink();
        let phase = std::sync::Mutex::new(AgentPhase::Reasoning);
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(DisplayProbeTool)).unwrap();

        handle_stream_event(
            "s1",
            StreamEvent::ToolCall {
                id: "c9".into(),
                name: "display_probe".into(),
                arguments: "{}".into(),
            },
            &phase,
            &sink,
            &reg,
            &ToolContext::new(),
        );

        let emitted: Vec<_> = events.lock().unwrap().drain(..).collect();
        let display = emitted
            .iter()
            .find_map(|e| match e {
                IpcEvent::Session(SessionEvent::ToolCall { display, .. }) => Some(display.clone()),
                _ => None,
            })
            .expect("应发出 tool/call 事件");
        assert_eq!(display.text, "探针对象");
    }

    #[test]
    fn phase_not_repeated() {
        // 连续两个 TextDelta：Phase(generating) 只应 emit 一次
        let (sink, events, log) = test_sink();
        let phase = std::sync::Mutex::new(AgentPhase::Reasoning);

        handle_stream_event(
            "s1",
            StreamEvent::TextDelta {
                content: "第一段".into(),
            },
            &phase,
            &sink,
            &ToolRegistry::new(),
            &ToolContext::new(),
        );
        handle_stream_event(
            "s1",
            StreamEvent::TextDelta {
                content: "第二段".into(),
            },
            &phase,
            &sink,
            &ToolRegistry::new(),
            &ToolContext::new(),
        );

        let emitted: Vec<_> = events.lock().unwrap().drain(..).collect();
        // Phase(generating) + chunk + chunk = 3 个事件（Phase 不重复）
        assert_eq!(emitted.len(), 3);
        let phase_count = emitted
            .iter()
            .filter(|e| matches!(e, IpcEvent::Agent(AgentEvent::Phase { .. })))
            .count();
        assert_eq!(phase_count, 1);

        // 双写：两个 text chunk 都入 log
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 2);
    }

    // ── execute_tools_and_append ──

    /// 测试工具：返回文本 + 一张图片路径。
    struct ImageEchoTool;

    impl crate::tools::Tool for ImageEchoTool {
        fn name(&self) -> &str {
            "image_echo"
        }
        fn description(&self) -> &str {
            "返回一张图片（测试用）"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }
        fn execute(
            &self,
            _ctx: &ToolContext,
            _params: &str,
        ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
            let mut out = crate::tools::ToolOutput::text("图在这里");
            out.images = vec!["C:/tmp/shot.png".to_string()];
            Ok(out)
        }
        fn display(&self, _ctx: &ToolContext, _params: &str) -> crate::tools::ToolDisplay {
            crate::tools::ToolDisplay::default()
        }
    }

    /// 工具声明的图片要另起一条 user 消息，而不是塞进 tool 消息。
    #[test]
    fn tool_images_ride_along_as_a_user_message() {
        let mut messages: Vec<ChatMessage> = vec![ChatMessage::user("看看画面")];
        let tools = {
            let mut reg = ToolRegistry::new();
            reg.register(Box::new(ImageEchoTool)).unwrap();
            reg
        };
        let ctx = ToolContext::new();
        let (sink, _events, _log) = test_sink();
        let tool_calls = vec![crate::llm::ToolCall {
            id: "call_1".into(),
            call_type: "function".into(),
            function: crate::llm::ToolCallFunction {
                name: "image_echo".into(),
                arguments: "{}".into(),
            },
        }];
        let result = StreamResult {
            text: String::new(),
            tool_calls,
            reasoning: None,
            usage: None,
            finish_reason: None,
        };
        let mut state = TurnState::new();
        let abort = AbortSignal::new();
        let initial = messages.len();
        execute_tools_and_append(
            "s1",
            &mut messages,
            &result,
            &tools,
            &ctx,
            &sink,
            &mut state,
            None,
            &abort,
        );

        // assistant + tool_result + 带图的 user 消息
        assert_eq!(messages.len(), initial + 3);
        assert_eq!(messages[initial + 1].role, crate::llm::MessageRole::Tool);
        let with_image = &messages[initial + 2];
        assert_eq!(with_image.role, crate::llm::MessageRole::User);
        assert_eq!(
            with_image.image_paths,
            Some(vec!["C:/tmp/shot.png".to_string()]),
            "图片走消息附件通路"
        );
    }

    /// 请求前物化：只给「最后一条 assistant 之后」的消息像素；
    /// 历史图与不支持视觉的模型都不读字节。
    #[test]
    fn request_images_materialize_only_after_last_assistant() {
        let dir = std::env::temp_dir().join(format!("prts-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = dir.join("shot.png");
        image::RgbImage::new(2, 2).save(&img).unwrap();
        let path = img.to_string_lossy().into_owned();

        let mut messages = vec![
            ChatMessage::user_with_image_paths("历史图", vec![path.clone()]),
            ChatMessage::assistant("看过了"),
            ChatMessage::user_with_image_paths("当前图", vec![path.clone()]),
        ];
        prepare_request_images(&mut messages, true);
        assert!(messages[0].images.is_none(), "历史图不物化");
        assert!(messages[0].image_paths.is_none(), "历史图路径已清");
        assert!(
            messages[2].images.as_ref().is_some_and(|v| !v.is_empty()),
            "当前图物化为像素"
        );

        // 不支持视觉：谁都不物化
        let mut messages = vec![
            ChatMessage::assistant("看过了"),
            ChatMessage::user_with_image_paths("当前图", vec![path]),
        ];
        prepare_request_images(&mut messages, false);
        assert!(messages[1].images.is_none(), "无视觉能力不物化");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn appends_assistant_and_tool_messages() {
        let mut messages: Vec<ChatMessage> = vec![ChatMessage::user("测试")];
        let tools = {
            let mut reg = ToolRegistry::new();
            reg.register(Box::new(crate::tools::EchoTool)).unwrap();
            reg
        };
        let ctx = ToolContext::new();
        let (sink, events, log) = test_sink();

        let tool_calls = vec![crate::llm::ToolCall {
            id: "call_1".into(),
            call_type: "function".into(),
            function: crate::llm::ToolCallFunction {
                name: "echo".into(),
                arguments: r#"{"message":"hello"}"#.into(),
            },
        }];

        let result = StreamResult {
            text: String::new(),
            tool_calls: tool_calls.clone(),
            reasoning: Some("让我试试echo工具".into()),
            usage: None,
            finish_reason: None,
        };

        let mut state = TurnState::new();
        let abort = AbortSignal::new();
        let initial_len = messages.len();
        execute_tools_and_append(
            "s1",
            &mut messages,
            &result,
            &tools,
            &ctx,
            &sink,
            &mut state,
            None,
            &abort,
        );

        // 应该追加了两条消息：assistant（含 tool_calls + reasoning）+ tool_result
        assert_eq!(messages.len(), initial_len + 2);
        assert_eq!(
            messages[initial_len].role,
            crate::llm::MessageRole::Assistant
        );
        assert!(messages[initial_len].tool_calls.is_some());
        assert_eq!(
            messages[initial_len].reasoning_content.as_deref(),
            Some("让我试试echo工具")
        );
        assert_eq!(
            messages[initial_len + 1].role,
            crate::llm::MessageRole::Tool
        );
        assert_eq!(
            messages[initial_len + 1].tool_call_id,
            Some("call_1".into())
        );

        // 确认指纹已记录
        assert_eq!(state.fingerprint_window.len(), 1);

        // 确认 emit 推送了 tool/result（IPC 持久事实）
        let emitted: Vec<_> = events.lock().unwrap().drain(..).collect();
        assert_eq!(emitted.len(), 1);
        assert!(matches!(
            emitted[0],
            IpcEvent::Session(SessionEvent::ToolResult { ref tool_name, ok: true, .. })
                if tool_name.as_deref() == Some("echo")
        ));

        // 双写：log 中有 tool/call（execute 前无流式事件）与 tool/result
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 2);
        assert!(matches!(log.events()[0], SessionEvent::ToolCall { .. }));
        assert!(matches!(
            log.events()[1],
            SessionEvent::ToolResult { ref tool_name, ok: true, .. } if tool_name.as_deref() == Some("echo")
        ));
    }

    #[test]
    fn tool_failure_logs_ok_false() {
        let mut messages: Vec<ChatMessage> = vec![ChatMessage::user("测试")];
        let tools = {
            let mut reg = ToolRegistry::new();
            reg.register(Box::new(crate::tools::EchoTool)).unwrap();
            reg
        };
        let ctx = ToolContext::new();
        let (sink, events, log) = test_sink();

        let tool_calls = vec![crate::llm::ToolCall {
            id: "call_1".into(),
            call_type: "function".into(),
            function: crate::llm::ToolCallFunction {
                name: "nonexistent".into(),
                arguments: "{}".into(),
            },
        }];
        let result = StreamResult {
            text: String::new(),
            tool_calls,
            reasoning: None,
            usage: None,
            finish_reason: None,
        };

        let mut state = TurnState::new();
        let abort = AbortSignal::new();
        execute_tools_and_append(
            "s1",
            &mut messages,
            &result,
            &tools,
            &ctx,
            &sink,
            &mut state,
            None,
            &abort,
        );

        // 工具失败不中断：tool 消息携带错误文本（= ToolError::Display，
        // 无双前缀——双前缀修正（M3-A 子步 3）后 Err 分支 format!("{err}")）
        assert_eq!(messages.len(), 3);
        assert!(
            messages[2].content.contains("未注册"),
            "got: {}",
            messages[2].content
        );
        assert!(
            !messages[2].content.contains("工具执行失败：工具执行失败"),
            "不应双前缀: {}",
            messages[2].content
        );
        let _ = events;

        // log 中 tool/result 标记 ok=false
        let log = log.lock().unwrap();
        assert!(matches!(
            log.events()[1],
            SessionEvent::ToolResult { ok: false, .. }
        ));
    }

    #[test]
    fn parallel_tool_execution_preserves_call_order() {
        // 模块 3 P2：多个并发安全工具调用并行执行，
        // 结果与事件顺序按调用顺序确定（与线程调度无关）
        let mut messages: Vec<ChatMessage> = vec![ChatMessage::user("批量查询")];
        let tools = {
            let mut reg = ToolRegistry::new();
            reg.register(Box::new(crate::tools::EchoTool)).unwrap();
            reg
        };
        let ctx = ToolContext::new();
        let (sink, events, log) = test_sink();

        let tool_calls: Vec<crate::llm::ToolCall> = (0..3)
            .map(|i| crate::llm::ToolCall {
                id: format!("call_{i}"),
                call_type: "function".into(),
                function: crate::llm::ToolCallFunction {
                    name: "echo".into(),
                    arguments: format!(r#"{{"message":"第{i}个"}}"#),
                },
            })
            .collect();

        let result = StreamResult {
            text: String::new(),
            tool_calls,
            reasoning: None,
            usage: None,
            finish_reason: None,
        };

        let mut state = TurnState::new();
        let abort = AbortSignal::new();
        execute_tools_and_append(
            "s1",
            &mut messages,
            &result,
            &tools,
            &ctx,
            &sink,
            &mut state,
            None,
            &abort,
        );

        // 消息：user + assistant(3 调用) + tool × 3
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[2].content, "Echo: {\"message\":\"第0个\"}");
        assert_eq!(messages[4].content, "Echo: {\"message\":\"第2个\"}");

        // IPC tool/result 顺序 = 调用顺序
        let emitted = events.lock().unwrap();
        let results: Vec<&SessionEvent> = session_events(&emitted)
            .into_iter()
            .filter(|e| matches!(e, SessionEvent::ToolResult { .. }))
            .collect();
        assert_eq!(results.len(), 3);
        for (i, e) in results.iter().enumerate() {
            match e {
                SessionEvent::ToolResult {
                    call_id, ok: true, ..
                } => {
                    assert_eq!(
                        call_id,
                        &format!("call_{i}"),
                        "tool/result 顺序应与调用顺序一致"
                    );
                }
                _ => panic!("期望成功结果"),
            }
        }

        // log 顺序：tool/call × 3 → tool/result × 3（派生顺序保持）
        let log = log.lock().unwrap();
        let types: Vec<&str> = log.events().iter().map(event_type).collect();
        assert_eq!(
            types,
            vec![
                "tool/call",
                "tool/call",
                "tool/call",
                "tool/result",
                "tool/result",
                "tool/result"
            ],
            "并行执行后 log 顺序仍按调用顺序"
        );
    }

    // ── Reference 变更标记 / 缓存工具 / 静态注入测试 —— 已注释（2026-08：
    //     动态层移除、缓存工具取消注册，对应测试停用；未来复用时可恢复）──
    /*

    /// 测试用缓存工具：execute 返回携带 ReferenceLoad 指令的 ToolOutput
    /// （模拟模块 C 缓存工具的标记形态，本任务不实现缓存工具本身）。
    struct CacheStoreTestTool;

    impl crate::tools::Tool for CacheStoreTestTool {
        fn name(&self) -> &str {
            "cache_store_test"
        }

        fn description(&self) -> &str {
            "测试用：返回 Reference 载入指令"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }

        fn execute(
            &self,
            _ctx: &ToolContext,
            _params: &str,
        ) -> Result<crate::tools::ToolOutput, ToolError> {
            let mut output = crate::tools::ToolOutput::text("已载入缓存条目");
            output.context_ops = vec![crate::tools::ContextOp::ReferenceLoad {
                key: "enemy.unit_1001".into(),
                content: "源石虫".into(),
            }];
            Ok(output)
        }
    }

    #[test]
    fn context_ops_applied_to_active_context_by_orchestrator() {
        let mut messages: Vec<ChatMessage> = vec![ChatMessage::user("载入缓存")];
        let tools = {
            let mut reg = ToolRegistry::new();
            reg.register(Box::new(CacheStoreTestTool)).unwrap();
            reg
        };
        let ctx = ToolContext::new();
        let (sink, _, _) = test_sink();
        let mut active = ActiveContext::new();

        let tool_calls = vec![crate::llm::ToolCall {
            id: "call_cache".into(),
            call_type: "function".into(),
            function: crate::llm::ToolCallFunction {
                name: "cache_store_test".into(),
                arguments: "{}".into(),
            },
        }];
        let result = StreamResult {
            text: String::new(),
            tool_calls,
            reasoning: None,
            usage: None,
            finish_reason: None,
        };

        let mut state = TurnState::new();
        execute_tools_and_append(
            "s1",
            &mut messages,
            &mut active,
            &result,
            &tools,
            &ctx,
            &sink,
            &mut state,
        );

        // 工具返回的标记由中枢统一应用：ActiveContext 已含缓存条目
        assert_eq!(active.reference_len(), 1);
        let entries: Vec<_> = active.reference_entries().collect();
        assert_eq!(entries, vec![("enemy.unit_1001", "源石虫")]);
    }

    // ── 缓存工具：效果侧 + 落账侧（模块 C）──

    #[test]
    fn cache_tool_events_land_in_session_log() {
        use crate::cache::CacheStores;
        use crate::tools::CacheStoreTool;

        let mut messages: Vec<ChatMessage> = vec![ChatMessage::user("写入缓存")];
        let tools = {
            let mut reg = ToolRegistry::new();
            reg.register(Box::new(CacheStoreTool)).unwrap();
            reg
        };
        // 真实缓存工具需要通道：注入缓存分区 + 对话标识
        let cache_stores = Arc::new(Mutex::new(CacheStores::new()));
        let ctx = ToolContext::with_cache(Arc::clone(&cache_stores), "conv-1".into());
        let (sink, events, log) = test_sink();
        let mut active = ActiveContext::new();

        let tool_calls = vec![crate::llm::ToolCall {
            id: "call_cache_store".into(),
            call_type: "function".into(),
            function: crate::llm::ToolCallFunction {
                name: "cache_store".into(),
                arguments:
                    r#"{"key":"buff.总结","content":"同类取最高","bucket":"static","source_entry":"Buff机制基础"}"#
                        .into(),
            },
        }];
        let result = StreamResult {
            text: String::new(),
            tool_calls,
            reasoning: None,
            usage: None,
            finish_reason: None,
        };

        let mut state = TurnState::new();
        execute_tools_and_append(
            "s1",
            &mut messages,
            &mut active,
            &result,
            &tools,
            &ctx,
            &sink,
            &mut state,
        );

        // 落账侧：log 含 cache/store（工具不改 log，标记由中枢统一写）
        let log = log.lock().unwrap();
        let cache_events: Vec<&SessionEvent> = log
            .events()
            .iter()
            .filter(|e| matches!(e, SessionEvent::CacheStore { .. }))
            .collect();
        assert_eq!(cache_events.len(), 1);
        match cache_events[0] {
            SessionEvent::CacheStore { session_id, key, bucket, source_entry } => {
                assert_eq!(session_id, "s1");
                assert_eq!(key, "buff.总结");
                assert_eq!(*bucket, crate::session::CacheBucket::Static);
                assert_eq!(source_entry.as_deref(), Some("Buff机制基础"));
            }
            _ => unreachable!(),
        }
        // log 顺序：tool/call → tool/result → cache/store（与消息顺序一致）
        let types: Vec<&str> = log.events().iter().map(event_type).collect();
        assert_eq!(types, vec!["tool/call", "tool/result", "cache/store"]);

        // 效果侧：桶内容写入 + Reference 层当轮载入
        let stores = cache_stores.lock().unwrap();
        assert!(stores.get("conv-1").unwrap().load_static("buff.总结").is_some());
        assert_eq!(active.reference_len(), 1);
        let _ = events;
    }

    #[tokio::test]
    async fn process_injects_static_entries_only_for_own_conversation() {
        use crate::cache::{CacheEntry, CacheStores};
        use crate::llm::{ApiType, LlmConfig, ModelRegistry};
        use std::collections::VecDeque;

        // ScriptedClient（见 reference 测试）：快照每次 LLM 调用收到的 messages，
        // 队列只放一次最终回答（无工具调用）
        let queue: Arc<Mutex<VecDeque<Result<StreamResult, LlmError>>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        queue.lock().unwrap().push_back(Ok(StreamResult {
            text: "答".into(),
            tool_calls: vec![],
            reasoning: None,
            usage: None,
            finish_reason: None,
        }));
        let snapshots: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));
        let scripted = Arc::new(ScriptedClient::new(queue, snapshots.clone()));
        let mut client_registry = ClientRegistry::new();
        client_registry.register(ApiType::OpenAiCompletions, scripted);
        let model_registry = Arc::new(
            ModelRegistry::from_json(include_str!("../../resources/models.json")).unwrap(),
        );

        // 预写两个对话的静态分区：conv-a 应有、conv-b 不应泄漏
        let cache_stores = Arc::new(Mutex::new(CacheStores::new()));
        {
            let mut stores = cache_stores.lock().unwrap();
            stores.partition("conv-a").store_static(CacheEntry {
                key: "buff.总结".into(),
                content: "同类取最高".into(),
                source_entry: Some("Buff机制基础".into()),
            });
            stores.partition("conv-b").store_static(CacheEntry {
                key: "敌人.秘密".into(),
                content: "绝密内容".into(),
                source_entry: None,
            });
        }

        let tools = ToolRegistry::new(); // 无工具调用，空注册表即可
        let history = vec![FrontendMessage {
            role: "user".into(),
            content: "你好".into(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            attachments: Vec::new(),
        }];

        process(
            "s1",
            history,
            &tools,
            &client_registry,
            &LlmConfig::default(),
            &model_registry,
            None,
            "conv-a",
            Arc::clone(&cache_stores),
            Arc::new(Mutex::new(std::collections::HashMap::new())),
            |_| {}, // 本测试从 messages 快照断言，不消费 IPC
        )
        .await
        .unwrap();

        // 快照 = LLM 实际收到的 messages（重组后的 system prompt 在 [0]）
        let snapshots = snapshots.lock().unwrap();
        assert_eq!(snapshots.len(), 1, "应恰好一次 LLM 调用");
        assert_eq!(snapshots[0][0].role, crate::llm::MessageRole::System);
        assert!(
            snapshots[0][0].content.contains("同类取最高"),
            "本对话静态条目应注入 system prompt"
        );
        assert!(
            !snapshots[0][0].content.contains("绝密内容"),
            "另一对话的静态条目不应泄漏"
        );
    }
    */

    // ── TurnState ──

    #[test]
    fn turnstate_final_usage_is_last_call_snapshot() {
        // 缺陷 4 回归测试：跨调用求和被当作"上下文占用"显示
        // （第二轮上下文用量低于第一轮）→ 现在取最后一次调用快照
        let mut state = TurnState::new();
        state.record_call(
            Some(TokenUsage {
                input_tokens: 5000,
                context_input_tokens: None,
                output_tokens: 100,
                cache_hit_tokens: None,
                cache_miss_tokens: None,
            }),
            Some("第一轮推理".into()),
        );
        state.record_call(
            Some(TokenUsage {
                input_tokens: 3000,
                context_input_tokens: None,
                output_tokens: 200,
                cache_hit_tokens: None,
                cache_miss_tokens: None,
            }),
            Some("第二轮推理".into()),
        );
        let (reasoning, usage) = state.finish();
        assert_eq!(reasoning.as_deref(), Some("第二轮推理"));
        let u = usage.unwrap();
        // 最后一次调用快照（而非 8000/300 的求和）
        assert_eq!(u.input_tokens, 3000);
        assert_eq!(u.output_tokens, 200);
    }

    #[test]
    fn turnstate_total_cost_accumulates() {
        let mut state = TurnState::new();
        state.record_call(
            Some(TokenUsage {
                input_tokens: 100,
                context_input_tokens: None,
                output_tokens: 50,
                cache_hit_tokens: None,
                cache_miss_tokens: None,
            }),
            None,
        );
        state.record_call(
            Some(TokenUsage {
                input_tokens: 200,
                context_input_tokens: None,
                output_tokens: 80,
                cache_hit_tokens: None,
                cache_miss_tokens: None,
            }),
            None,
        );
        assert_eq!(state.total_input_tokens, 300);
        assert_eq!(state.total_output_tokens, 130);
    }

    #[test]
    fn turnstate_ignores_none_usage() {
        let mut state = TurnState::new();
        state.record_call(None, None);
        assert_eq!(state.total_input_tokens, 0);
        let (reasoning, usage) = state.finish();
        assert!(reasoning.is_none());
        assert!(usage.is_none());
    }

    #[test]
    fn turnstate_stagnation_detected() {
        let mut state = TurnState::new();
        // 同一个指纹重复 5 次（= STAGNATION_THRESHOLD）
        for _ in 0..5 {
            state.record_fingerprint("echo", "{}", "Echo: {}");
        }
        assert!(state.check_stagnation());
    }

    #[test]
    fn turnstate_no_stagnation_below_threshold() {
        let mut state = TurnState::new();
        for _ in 0..4 {
            state.record_fingerprint("echo", "{}", "Echo: {}");
        }
        assert!(!state.check_stagnation());
    }

    #[test]
    fn turnstate_window_evicts_oldest() {
        let mut state = TurnState::new();
        // 填满窗口（10 个不同指纹）
        for i in 0..10 {
            state.record_fingerprint("tool", &i.to_string(), "result");
        }
        assert_eq!(state.fingerprint_window.len(), 10);
        // 记录最早指纹，用于验证淘汰
        let evicted_fp = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash("tool", &mut h);
            std::hash::Hash::hash("0", &mut h);
            std::hash::Hash::hash("result", &mut h);
            h.finish()
        };
        // 第 11 个应该淘汰第 1 个
        state.record_fingerprint("tool", "10", "result");
        assert_eq!(state.fingerprint_window.len(), 10);
        // 新指纹在窗口中
        assert!(state.fingerprint_window.contains(&{
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash("tool", &mut h);
            std::hash::Hash::hash("10", &mut h);
            std::hash::Hash::hash("result", &mut h);
            h.finish()
        }));
        // 最早指纹已被淘汰
        assert!(!state.fingerprint_window.contains(&evicted_fp));
    }

    #[test]
    fn turnstate_different_args_different_fingerprint() {
        let mut state = TurnState::new();
        state.record_fingerprint("echo", r#"{"x":1}"#, "result");
        state.record_fingerprint("echo", r#"{"x":2}"#, "result");
        // 两个不同指纹，不应触发停滞
        assert!(!state.check_stagnation());
        assert_eq!(state.fingerprint_window.len(), 2);
    }

    // ── EventSink ──

    #[test]
    fn event_sink_emit_logs_then_pushes_ipc() {
        let (sink, events, log) = test_sink();
        // emit：持久事实 → 先入 log 再推 IPC
        sink.emit(SessionEvent::assistant_chunk("s1", ChunkKind::Text, "hi"));
        assert_eq!(log.lock().unwrap().len(), 1);
        assert_eq!(events.lock().unwrap().len(), 1);

        // log_only：只写 log，不推 IPC
        sink.log_only(SessionEvent::step_start("s1", 1, 0));
        assert_eq!(log.lock().unwrap().len(), 2);
        assert_eq!(events.lock().unwrap().len(), 1);

        // ipc_only：只推 IPC，不写 log
        sink.ipc_only(SessionEvent::tool_call("s1", "c1", "echo", "{}"));
        assert_eq!(log.lock().unwrap().len(), 2);
        assert_eq!(events.lock().unwrap().len(), 2);

        // phase：纯 UI 信号，只推 IPC
        sink.phase("s1", AgentPhase::Generating);
        assert_eq!(log.lock().unwrap().len(), 2);
        assert_eq!(events.lock().unwrap().len(), 3);
    }

    #[test]
    fn event_sink_snapshot_is_cloneable() {
        let (sink, _, log) = test_sink();
        sink.log_only(SessionEvent::turn_start("s1"));
        // SessionLog 可 Clone（派生/回放需要快照）
        let snap = log.lock().unwrap().clone();
        assert_eq!(snap.len(), 1);
        assert_eq!(sink.log.lock().unwrap().len(), 1);
    }

    // ── dispatch_turn 集成测试（Mock LLM 驱动）──

    /// 事件类型标签（log 结构断言用）
    fn event_type(e: &SessionEvent) -> &'static str {
        match e {
            SessionEvent::TurnStart { .. } => "turn/start",
            SessionEvent::TurnEnd { .. } => "turn/end",
            SessionEvent::StepStart { .. } => "step/start",
            SessionEvent::StepEnd { .. } => "step/end",
            SessionEvent::UserMessage { .. } => "user/message",
            SessionEvent::AssistantChunk { .. } => "assistant/chunk",
            SessionEvent::AssistantMessage { .. } => "assistant/message",
            SessionEvent::AssistantProviderBlocks { .. } => "assistant/provider-blocks",
            SessionEvent::ToolCall { .. } => "tool/call",
            SessionEvent::ToolResult { .. } => "tool/result",
            SessionEvent::RequestHeader { .. } => "request/header",
            SessionEvent::Error { .. } => "error",
            SessionEvent::CacheStore { .. } => "cache/store",
            SessionEvent::CacheLoad { .. } => "cache/load",
            SessionEvent::CacheRemove { .. } => "cache/remove",
            SessionEvent::SkillActivate { .. } => "skill/activate",
            SessionEvent::ToolApprovalRequest { .. } => "tool/approval-request",
            SessionEvent::ToolApprovalResult { .. } => "tool/approval-result",
            SessionEvent::Unknown => "unknown",
            // 卸载事件已移除（2026-08：卸载机制取消）
        }
    }

    #[tokio::test]
    async fn dispatch_turn_writes_coherent_log_and_messages() {
        use crate::llm::mock::{MockLlmClient, MockResponse};
        use crate::llm::{ApiType, ToolCall, ToolCallFunction};

        // 预设：第 1 次调用 → 推理 + echo 工具；第 2 次调用 → 最终回答
        let mock = Arc::new(MockLlmClient::new(vec![
            MockResponse {
                events: vec![
                    StreamEvent::ReasoningDelta {
                        content: "让我查一下".into(),
                    },
                    StreamEvent::ToolCall {
                        id: "call_1".into(),
                        name: "echo".into(),
                        arguments: r#"{"message":"hi"}"#.into(),
                    },
                ],
                result: StreamResult {
                    text: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call_1".into(),
                        call_type: "function".into(),
                        function: ToolCallFunction {
                            name: "echo".into(),
                            arguments: r#"{"message":"hi"}"#.into(),
                        },
                    }],
                    reasoning: Some("让我查一下".into()),
                    usage: Some(TokenUsage {
                        input_tokens: 1000,
                        context_input_tokens: None,
                        output_tokens: 50,
                        cache_hit_tokens: None,
                        cache_miss_tokens: None,
                    }),
                    finish_reason: None,
                },
            },
            MockResponse {
                events: vec![StreamEvent::TextDelta {
                    content: "查到了".into(),
                }],
                result: StreamResult {
                    text: "查到了".into(),
                    tool_calls: vec![],
                    reasoning: None,
                    usage: Some(TokenUsage {
                        input_tokens: 1500,
                        context_input_tokens: None,
                        output_tokens: 30,
                        cache_hit_tokens: None,
                        cache_miss_tokens: None,
                    }),
                    finish_reason: None,
                },
            },
        ]));

        let mut client_registry = ClientRegistry::new();
        client_registry.register(ApiType::OpenAiCompletions, mock);

        let mut tool_registry = ToolRegistry::new();
        tool_registry
            .register(Box::new(crate::tools::EchoTool))
            .unwrap();
        let tool_defs = tool_registry.list_defs();
        let tool_registry = Arc::new(tool_registry);
        // 会话日志（与 process() 相同的初始化序列）
        let log = Arc::new(Mutex::new(SessionLog::new()));
        {
            let mut log = log.lock().unwrap();
            log.append(SessionEvent::turn_start("s1"));
            log.seed_from_history(
                "s1",
                vec![FrontendMessage {
                    role: "user".into(),
                    content: "查一下源石虫".into(),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    attachments: Vec::new(),
                }],
            );
            log.push_header(
                "s1",
                &crate::orchestrator::context::assemble_system_prompt(""),
                &tool_defs,
            );
        }

        let derived = log
            .lock()
            .unwrap()
            .derive_messages(ReasoningPolicy::Preserve);
        let mut messages =
            crate::orchestrator::context::build_context(&derived, tool_defs.len(), "");

        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let sink = EventSink::new(log.clone(), move |e: IpcEvent| {
            events_clone.lock().unwrap().push(e);
        });

        let llm = &**client_registry.select(&ApiType::OpenAiCompletions).unwrap();
        let abort = AbortSignal::new(); // M3-B：本测试无中止（信号恒未置位，行为零变化）
        let steer_queue = SteerQueue::new(); // 插话队列空（默认零行为）前提
        let outcome = dispatch_turn(
            "s1",
            &sink,
            &mut messages,
            &Arc::new(crate::skills_index::SkillIndex::empty()),
            &tool_registry,
            &tool_defs,
            llm,
            &LlmConfig::default(),
            None,
            ReasoningPolicy::Preserve,
            ExecutionMode::Ask,
            &ApprovalRegistry::new(),
            &abort,
            &steer_queue,
            std::path::Path::new(&std::env::temp_dir()),
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);

        // ── log 结构：turn → step 边界、chunk、工具事件、终止原因 ──
        let log = log.lock().unwrap();
        let types: Vec<&str> = log.events().iter().map(event_type).collect();
        assert_eq!(
            types,
            vec![
                "turn/start",
                "user/message",
                "request/header",
                "step/start",
                "assistant/chunk", // 推理链
                "tool/call",
                "tool/result",
                "step/end", // step 1：LLM 调用 + 工具执行后闭合
                "step/start",
                "assistant/chunk",   // 最终回答
                "assistant/message", // 聚合事件
                "step/end",          // step 2：最终 step 闭合
                "turn/end",
            ],
            "log 事件序列不符合 turn/step 模型"
        );
        assert!(matches!(
            log.events().last().unwrap(),
            SessionEvent::TurnEnd {
                reason: TurnEndReason::Completed,
                ..
            }
        ));
        // step/end 记录各自调用快照（总成本账本）
        let usages: Vec<Option<TokenUsage>> = log
            .events()
            .iter()
            .filter_map(|e| match e {
                SessionEvent::StepEnd { usage, .. } => Some(*usage),
                _ => None,
            })
            .collect();
        assert_eq!(
            usages,
            vec![
                Some(TokenUsage {
                    input_tokens: 1000,
                    context_input_tokens: None,
                    output_tokens: 50,
                    cache_hit_tokens: None,
                    cache_miss_tokens: None,
                }),
                Some(TokenUsage {
                    input_tokens: 1500,
                    context_input_tokens: None,
                    output_tokens: 30,
                    cache_hit_tokens: None,
                    cache_miss_tokens: None,
                }),
            ]
        );

        // ── 派生一致性：messages[1..] == derive(log)（双写验证可重放）──
        let derived_final = log.derive_messages(ReasoningPolicy::Preserve);
        assert_eq!(
            derived_final.len(),
            4,
            "user + assistant(tools) + tool + assistant(final)"
        );
        assert_eq!(
            messages.len(),
            derived_final.len() + 1,
            "system 头 + 派生消息"
        );
        for (i, m) in derived_final.iter().enumerate() {
            let live = &messages[i + 1];
            assert_eq!(live.role, m.role, "消息 {i} role 不一致");
            assert_eq!(live.content, m.content, "消息 {i} content 不一致");
            assert_eq!(
                live.reasoning_content, m.reasoning_content,
                "消息 {i} reasoning 不一致"
            );
            assert_eq!(
                live.tool_calls.as_ref().map(|v| v.len()),
                m.tool_calls.as_ref().map(|v| v.len()),
                "消息 {i} tool_calls 不一致"
            );
        }
        // 具体断言：工具轮 assistant 消息携带推理与工具调用
        assert_eq!(
            derived_final[1].reasoning_content.as_deref(),
            Some("让我查一下")
        );
        assert!(derived_final[1].tool_calls.is_some());
        assert_eq!(derived_final[3].content, "查到了");

        // ── token 快照（缺陷 4）：assistant/message 携带最后一次调用快照 ──
        let emitted = events.lock().unwrap();
        let ipc_sessions = session_events(&emitted);
        let assistant_msg = ipc_sessions
            .iter()
            .find(|e| matches!(e, SessionEvent::AssistantMessage { .. }))
            .expect("应推送 assistant/message");
        match assistant_msg {
            SessionEvent::AssistantMessage { usage: Some(u), .. } => {
                assert_eq!(u.input_tokens, 1500, "应为最后一次调用快照（非跨调用求和）");
                assert_eq!(u.output_tokens, 30);
            }
            _ => panic!("assistant/message 应携带 token 快照"),
        }
        // 前端归档触发点：turn/end(completed) 推送到 IPC
        assert!(ipc_sessions.iter().any(|e| matches!(
            e,
            SessionEvent::TurnEnd {
                reason: TurnEndReason::Completed,
                ..
            }
        )));
        // 实时上下文水位：两个 step 的 step/end 都必须推 IPC（不能只写 log）
        let ipc_step_ends = ipc_sessions
            .iter()
            .filter(|e| matches!(e, SessionEvent::StepEnd { .. }))
            .count();
        assert_eq!(
            ipc_step_ends, 2,
            "step/end 必须推送 IPC，供前端实时更新上下文圆环"
        );
    }

    /// 工具步正文回填（2026-09 修复）：工具步的正文必须进入后续上下文。
    ///
    /// 修复前 execute_tools_and_append 以 String::new() 回填 assistant 消息，
    /// 模型在轮内看不见自己刚写过的文本——自指任务（如"输出两段、每段带标签"）
    /// 会因"我从没写过文本"的假象退化成工具调用死循环（实测 21 步）。
    #[tokio::test]
    async fn tool_step_text_is_carried_into_context() {
        use crate::llm::{ApiType, LlmError, ToolCall, ToolCallFunction};
        use std::collections::VecDeque;

        // 脚本：第一步 = 正文 + echo 工具；第二步 = 收尾文本。
        let queue: Arc<Mutex<VecDeque<Result<StreamResult, LlmError>>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        queue.lock().unwrap().push_back(Ok(StreamResult {
            text: "先看一下工作区。".into(),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "echo".into(),
                    arguments: r#"{"message":"hi"}"#.into(),
                },
            }],
            reasoning: None,
            usage: None,
            finish_reason: None,
        }));
        queue.lock().unwrap().push_back(Ok(StreamResult {
            text: "看完了。".into(),
            tool_calls: vec![],
            reasoning: None,
            usage: None,
            finish_reason: None,
        }));
        let snapshots: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));
        let scripted = Arc::new(ScriptedClient::new(queue, snapshots.clone()));

        let mut tool_registry = ToolRegistry::new();
        tool_registry
            .register(Box::new(crate::tools::EchoTool))
            .unwrap();
        let tools = Arc::new(tool_registry);

        let (sink, _events, _log, mut messages, tool_defs, client_registry, _tools2) =
            setup_dispatch_env(tools.clone(), scripted, vec![user_msg("测试")]);

        let abort = AbortSignal::new();
        let steer_queue = SteerQueue::new();
        let outcome = dispatch_turn(
            "s1",
            &sink,
            &mut messages,
            &Arc::new(crate::skills_index::SkillIndex::empty()),
            &tools,
            &tool_defs,
            &**client_registry.select(&ApiType::OpenAiCompletions).unwrap(),
            &LlmConfig::default(),
            None,
            ReasoningPolicy::Preserve,
            ExecutionMode::Ask,
            &ApprovalRegistry::new(),
            &abort,
            &steer_queue,
            std::path::Path::new(&std::env::temp_dir()),
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);

        // ① 第二次请求的快照里，第一步的 assistant 消息携带正文（修复前为空串）
        let snaps = snapshots.lock().unwrap();
        assert_eq!(snaps.len(), 2, "应恰好两次 LLM 调用");
        let with_tools = snaps[1]
            .iter()
            .find(|m| m.tool_calls.is_some())
            .expect("第二次请求应含带工具调用的 assistant 消息");
        assert_eq!(with_tools.role, crate::llm::MessageRole::Assistant);
        assert_eq!(
            with_tools.content, "先看一下工作区。",
            "工具步正文必须回填进上下文（修复前为 String::new()，模型轮内失忆）"
        );

        // ② 轮末 live 上下文同样携带正文（同一向量即后续所有请求的来源）
        assert_eq!(messages[2].content, "先看一下工作区。");
        assert!(messages[2].tool_calls.is_some());
        assert_eq!(messages.last().unwrap().content, "看完了。");
    }

    #[tokio::test]
    async fn dispatch_turn_stagnation_writes_reason() {
        use crate::llm::mock::{MockLlmClient, MockResponse};
        use crate::llm::{ApiType, ToolCall, ToolCallFunction};

        // 预设：5 次完全相同的工具调用（触发停滞检测）
        let mut responses = Vec::new();
        for _ in 0..5 {
            responses.push(MockResponse {
                events: vec![StreamEvent::ToolCall {
                    id: "call_x".into(),
                    name: "echo".into(),
                    arguments: r#"{"message":"hi"}"#.into(),
                }],
                result: StreamResult {
                    text: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "call_x".into(),
                        call_type: "function".into(),
                        function: ToolCallFunction {
                            name: "echo".into(),
                            arguments: r#"{"message":"hi"}"#.into(),
                        },
                    }],
                    reasoning: None,
                    usage: None,
                    finish_reason: None,
                },
            });
        }

        let mock = Arc::new(MockLlmClient::new(responses));
        let mut client_registry = ClientRegistry::new();
        client_registry.register(ApiType::OpenAiCompletions, mock);

        let mut tool_registry = ToolRegistry::new();
        tool_registry
            .register(Box::new(crate::tools::EchoTool))
            .unwrap();
        let tool_defs = tool_registry.list_defs();
        let tool_registry = Arc::new(tool_registry);
        let log = Arc::new(Mutex::new(SessionLog::new()));
        {
            let mut log = log.lock().unwrap();
            log.append(SessionEvent::turn_start("s1"));
            log.seed_from_history("s1", vec![]);
        }
        let derived = log
            .lock()
            .unwrap()
            .derive_messages(ReasoningPolicy::Preserve);
        let mut messages =
            crate::orchestrator::context::build_context(&derived, tool_defs.len(), "");

        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let sink = EventSink::new(log.clone(), move |e: IpcEvent| {
            events_clone.lock().unwrap().push(e);
        });

        let llm = &**client_registry.select(&ApiType::OpenAiCompletions).unwrap();
        let abort = AbortSignal::new(); // M3-B：本测试无中止（信号恒未置位，行为零变化）
        let steer_queue = SteerQueue::new(); // 插话队列空（默认零行为）前提
        let outcome = dispatch_turn(
            "s1",
            &sink,
            &mut messages,
            &Arc::new(crate::skills_index::SkillIndex::empty()),
            &tool_registry,
            &tool_defs,
            llm,
            &LlmConfig::default(),
            None,
            ReasoningPolicy::Preserve,
            ExecutionMode::Ask,
            &ApprovalRegistry::new(),
            &abort,
            &steer_queue,
            std::path::Path::new(&std::env::temp_dir()),
        )
        .await
        .unwrap();
        // 停滞升格为终止原因，而非错误
        assert_eq!(outcome, TurnOutcome::Stagnation);

        let log = log.lock().unwrap();
        assert!(
            matches!(
                log.events().last().unwrap(),
                SessionEvent::TurnEnd {
                    reason: TurnEndReason::Stagnation,
                    ..
                }
            ),
            "停滞应写 turn/end reason=stagnation"
        );

        // IPC 侧：只发 turn/end(stagnation)；不再发终止 error。
        let emitted = events.lock().unwrap();
        let ipc_sessions = session_events(&emitted);
        assert!(
            !ipc_sessions
                .iter()
                .any(|e| matches!(e, SessionEvent::Error { .. })),
            "停滞不应再发 error 事件"
        );
        assert!(
            ipc_sessions.iter().any(|e| matches!(
                e,
                SessionEvent::TurnEnd {
                    reason: TurnEndReason::Stagnation,
                    ..
                }
            )),
            "应推送 turn/end(stagnation) 供前端归档"
        );
    }

    // ── Reference 重组挂钩集成测试（ScriptedClient 驱动 dispatch_turn）──

    /// 测试用脚本客户端：`chat` 恒返回 Config 错误（调度循环只走 chat_stream，
    /// 此处仅满足 trait 契约）；`chat_stream` 从共享脚本队列按序 pop 结果，
    /// 并把收到的 messages 快照存入共享 Vec（验证"模型到底看到了什么"）。
    struct ScriptedClient {
        queue: Arc<Mutex<VecDeque<Result<StreamResult, LlmError>>>>,
        snapshots: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
        /// 每次请求前回调（入参 = 0 基请求序号；模拟「轮中到达」的旁路注入）。
        on_call: Option<Arc<dyn Fn(usize) + Send + Sync>>,
    }

    impl ScriptedClient {
        fn new(
            queue: Arc<Mutex<VecDeque<Result<StreamResult, LlmError>>>>,
            snapshots: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
        ) -> Self {
            Self {
                queue,
                snapshots,
                on_call: None,
            }
        }

        /// 带请求前回调的构造：供「轮中插话到达」时序测试使用。
        fn new_with_hook(
            queue: Arc<Mutex<VecDeque<Result<StreamResult, LlmError>>>>,
            snapshots: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
            on_call: Arc<dyn Fn(usize) + Send + Sync>,
        ) -> Self {
            Self {
                queue,
                snapshots,
                on_call: Some(on_call),
            }
        }
    }

    impl LlmClient for ScriptedClient {
        fn chat<'a>(
            &'a self,
            _config: &'a LlmConfig,
            _messages: &'a [ChatMessage],
            _tools: &'a [crate::llm::ToolDef],
        ) -> Pin<Box<dyn Future<Output = Result<crate::llm::LlmResponse, LlmError>> + Send + 'a>>
        {
            Box::pin(async move { Err(LlmError::Config("unused".into())) })
        }

        fn chat_stream<'a>(
            &'a self,
            _config: &'a LlmConfig,
            messages: &'a [ChatMessage],
            _tools: &'a [crate::llm::ToolDef],
            _on_event: Box<dyn Fn(StreamEvent) + Send + Sync>,
        ) -> Pin<Box<dyn Future<Output = Result<StreamResult, LlmError>> + Send + 'a>> {
            let snapshots = self.snapshots.clone();
            let queue = self.queue.clone();
            let on_call = self.on_call.clone();
            Box::pin(async move {
                // 快照先落账（含重组后的 system prompt），再从脚本队列取结果
                let call_index = {
                    let mut snaps = snapshots.lock().unwrap();
                    snaps.push(messages.to_vec());
                    snaps.len() - 1
                };
                if let Some(hook) = &on_call {
                    hook(call_index);
                }
                queue.lock().unwrap().pop_front().expect("脚本队列已空")
            })
        }
    }

    // ── Reference 重组 / Skill 激活 / 卸载测试 —— 已注释（2026-08：动态层移除、
    //     skill 改为消息注入、卸载机制取消；对应测试停用，未来复用时可恢复）──
    /*
    #[tokio::test]
    async fn reference_ops_reinject_prompt_and_push_header_on_change() {
        use crate::llm::{ApiType, ToolCall, ToolCallFunction};
        use std::collections::VecDeque;

        // 脚本：第一轮返回缓存工具调用；第二轮返回空 tool_calls + 最终文本
        let queue: Arc<Mutex<VecDeque<Result<StreamResult, LlmError>>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        queue.lock().unwrap().push_back(Ok(StreamResult {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_cache".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "cache_store_test".into(),
                    arguments: "{}".into(),
                },
            }],
            reasoning: None,
            usage: None,
            finish_reason: None,
        }));
        queue.lock().unwrap().push_back(Ok(StreamResult {
            text: "已载入缓存".into(),
            tool_calls: vec![],
            reasoning: None,
            usage: None,
            finish_reason: None,
        }));

        let snapshots: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));
        let scripted = Arc::new(ScriptedClient::new(queue, snapshots.clone()));
        let mut client_registry = ClientRegistry::new();
        client_registry.register(ApiType::OpenAiCompletions, scripted);

        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(CacheStoreTestTool)).unwrap();
        let tool_defs = tool_registry.list_defs();
        let mut active = ActiveContext::new();

        let log = Arc::new(Mutex::new(SessionLog::new()));
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let sink = EventSink::new(log.clone(), move |e: IpcEvent| {
            events_clone.lock().unwrap().push(e);
        });

        // 直接调 dispatch_turn：构造含 system 首条的 messages
        // （等价 process 的 build_context 产物；首条内容会被循环起点重组覆盖）
        let mut messages = vec![ChatMessage::system("初始 system（将被重组覆盖）")];

        let llm = &**client_registry.select(&ApiType::OpenAiCompletions).unwrap();
        let outcome = dispatch_turn(
            "s1",
            &sink,
            &mut messages,
            &mut active,
            &Arc::new(crate::skills_index::SkillIndex::empty()),
            &tool_registry,
            &tool_defs,
            llm,
            &LlmConfig::default(),
            None,
            "conv-test",
            Arc::new(Mutex::new(crate::cache::CacheStores::new())),
            ReasoningPolicy::Preserve,
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);

        // 第二轮快照的 messages[0] 已注入缓存条目（重组挂钩生效）
        let snapshots = snapshots.lock().unwrap();
        assert_eq!(snapshots.len(), 2, "应恰好两次 LLM 调用（第一轮工具 + 第二轮回答）");
        assert_eq!(snapshots[1][0].role, crate::llm::MessageRole::System);
        assert!(snapshots[1][0].content.contains("## 缓存条目：enemy.unit_1001"));
        assert!(snapshots[1][0].content.contains("源石虫"));
        // 第一轮快照不含注入内容（空 ActiveContext 等价形态）
        assert!(!snapshots[0][0].content.contains("缓存条目"));

        // session log 中 request/header 恰好两条：初始 + 变化后
        // （push_header 自带去重：第二轮 prompt 因缓存条目注入而变化才追加）
        let log = log.lock().unwrap();
        let headers = log
            .events()
            .iter()
            .filter(|e| matches!(e, SessionEvent::RequestHeader { .. }))
            .count();
        assert_eq!(headers, 2, "缓存条目注入后 system prompt 变化 → 追加第二条 request/header");
    }

    // ── Skill 激活集成测试（模块 B：ScriptedClient 驱动）──

    /// 创建唯一临时目录（不引入 tempfile crate，模式照 skills_index.rs）。
    fn skill_temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-orch-skill-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn skill_activation_injects_active_layer_and_logs_event() {
        use crate::llm::{ApiType, ToolCall, ToolCallFunction};
        use std::collections::VecDeque;

        // fixture 临时目录：一个有效 Skill（正文剥 frontmatter 后为 "## 推演正文"）
        let root = skill_temp_dir("activate");
        std::fs::write(
            root.join("tuiyan.md"),
            "---\nname: 推演分析\ndescription: 推演时的行为规则\n---\n\n## 推演正文",
        )
        .unwrap();
        let skill_index = Arc::new(crate::skills_index::SkillIndex::load(&root).unwrap());

        // 脚本：第一轮返回 activate_skill 工具调用；第二轮空 tool_calls 直接答
        let queue: Arc<Mutex<VecDeque<Result<StreamResult, LlmError>>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        queue.lock().unwrap().push_back(Ok(StreamResult {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_skill".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "activate_skill".into(),
                    arguments: r#"{"name":"推演分析"}"#.into(),
                },
            }],
            reasoning: None,
            usage: None,
            finish_reason: None,
        }));
        queue.lock().unwrap().push_back(Ok(StreamResult {
            text: "已激活，开始推演".into(),
            tool_calls: vec![],
            reasoning: None,
            usage: None,
            finish_reason: None,
        }));

        let snapshots: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));
        let scripted = Arc::new(ScriptedClient::new(queue, snapshots.clone()));
        let mut client_registry = ClientRegistry::new();
        client_registry.register(ApiType::OpenAiCompletions, scripted);

        let mut tool_registry = ToolRegistry::new();
        tool_registry
            .register(Box::new(crate::tools::ActivateSkillTool))
            .unwrap();
        let tool_defs = tool_registry.list_defs();
        let mut active = ActiveContext::new();

        let log = Arc::new(Mutex::new(SessionLog::new()));
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let events_clone = events.clone();
        let sink = EventSink::new(log.clone(), move |e: IpcEvent| {
            events_clone.lock().unwrap().push(e);
        });

        // 直接调 dispatch_turn：构造含 system 首条的 messages
        // （等价 process 的 build_context 产物；首条内容会被循环起点重组覆盖）
        let mut messages = vec![ChatMessage::system("初始 system（将被重组覆盖）")];

        let llm = &**client_registry.select(&ApiType::OpenAiCompletions).unwrap();
        let outcome = dispatch_turn(
            "s1",
            &sink,
            &mut messages,
            &mut active,
            &skill_index,
            &tool_registry,
            &tool_defs,
            llm,
            &LlmConfig::default(),
            None,
            "conv-test",
            Arc::new(Mutex::new(crate::cache::CacheStores::new())),
            ReasoningPolicy::Preserve,
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);

        // 第二轮快照：system prompt 已注入 Active 层（激活正文随重组挂钩生效）
        let snapshots = snapshots.lock().unwrap();
        assert_eq!(snapshots.len(), 2, "应恰好两次 LLM 调用（第一轮激活 + 第二轮回答）");
        assert_eq!(snapshots[1][0].role, crate::llm::MessageRole::System);
        assert!(snapshots[1][0].content.contains("## 已激活 Skill：推演分析"));
        assert!(snapshots[1][0].content.contains("## 推演正文"));
        // 第一轮快照不含激活注入（激活发生在第一轮工具执行之后）
        assert!(!snapshots[0][0].content.contains("已激活 Skill"));
        drop(snapshots);

        // session log 含 skill/activate 事件（激活动作可回放）
        let log = log.lock().unwrap();
        assert!(
            log.events().iter().any(|e| matches!(
                e,
                SessionEvent::SkillActivate { name, .. } if name == "推演分析"
            )),
            "log 应记录 skill/activate 事件"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn skill_state_unloaded_next_turn() {
        use crate::llm::{ApiType, LlmConfig, ModelRegistry, ToolCall, ToolCallFunction};
        use std::collections::VecDeque;

        // 两轮 process 共享脚本队列与快照：第一轮 = [激活「数值计算」, 空 tool_calls]；
        // 第二轮（新 turn）= [空 tool_calls 直接答]。真实 skills 目录含「数值计算」
        // （dev 路径随源码分发，见 skills_index.rs 的 skills_dir 测试前提）。
        let queue: Arc<Mutex<VecDeque<Result<StreamResult, LlmError>>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        queue.lock().unwrap().push_back(Ok(StreamResult {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_skill".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "activate_skill".into(),
                    arguments: r#"{"name":"数值计算"}"#.into(),
                },
            }],
            reasoning: None,
            usage: None,
            finish_reason: None,
        }));
        queue.lock().unwrap().push_back(Ok(StreamResult {
            text: "已激活".into(),
            tool_calls: vec![],
            reasoning: None,
            usage: None,
            finish_reason: None,
        }));
        // 第二轮（新 turn）：单次直接答（不重新激活）
        queue.lock().unwrap().push_back(Ok(StreamResult {
            text: "第二轮回答".into(),
            tool_calls: vec![],
            reasoning: None,
            usage: None,
            finish_reason: None,
        }));

        let snapshots: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));
        let scripted = Arc::new(ScriptedClient::new(queue, snapshots.clone()));
        let mut client_registry = ClientRegistry::new();
        client_registry.register(ApiType::OpenAiCompletions, scripted);
        let model_registry = Arc::new(
            ModelRegistry::from_json(include_str!("../../resources/models.json")).unwrap(),
        );

        let mut tool_registry = ToolRegistry::new();
        tool_registry
            .register(Box::new(crate::tools::ActivateSkillTool))
            .unwrap();
        let cache_stores = Arc::new(Mutex::new(crate::cache::CacheStores::new()));
        let last_unloads = Arc::new(Mutex::new(std::collections::HashMap::new()));

        // 第一轮：激活 + 回答（激活正文应进入第一轮第二次调用的 system prompt）
        process(
            "s1",
            vec![FrontendMessage {
                role: "user".into(),
                content: "计算伤害".into(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                attachments: Vec::new(),
            }],
            &tool_registry,
            &client_registry,
            &LlmConfig::default(),
            &model_registry,
            None,
            "conv-a",
            Arc::clone(&cache_stores),
            Arc::clone(&last_unloads),
            |_| {},
        )
        .await
        .unwrap();

        // 第二轮：全新 process（ActiveContext 每 turn 重建）——同一对话标识、
        // 不重新激活，验证 Skill 激活状态随 turn 自动卸载
        process(
            "s2",
            vec![FrontendMessage {
                role: "user".into(),
                content: "继续".into(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                attachments: Vec::new(),
            }],
            &tool_registry,
            &client_registry,
            &LlmConfig::default(),
            &model_registry,
            None,
            "conv-a",
            Arc::clone(&cache_stores),
            Arc::clone(&last_unloads),
            |_| {},
        )
        .await
        .unwrap();

        // 快照断言：第一轮第二次调用含 Skill 正文（激活生效）；
        // 第二轮（新 turn）system prompt 不再含正文（每轮重建自动卸载）；
        // Catalog 目录仍每轮注入；上轮卸载说明作为 system 消息注入第二轮
        // 历史（LLM 从时序理解“上轮激活已失效”），且消费后清空（无残留）
        let snapshots = snapshots.lock().unwrap();
        assert_eq!(snapshots.len(), 3, "第一轮两次调用 + 第二轮一次调用");
        assert!(snapshots[1][0].content.contains("## 已激活 Skill：数值计算"));
        assert!(
            snapshots[1][0].content.contains("## 工作流程"),
            "激活的正文应注入当轮 system prompt"
        );
        assert!(
            !snapshots[2][0].content.contains("## 工作流程"),
            "新 turn 的 ActiveContext 重建，上一轮激活的 Skill 应已卸载"
        );
        assert!(
            snapshots[2][0].content.contains("## Skill 目录"),
            "Catalog 层每轮照常注入"
        );
        assert_eq!(
            snapshots[2].len(),
            3,
            "system prompt + 卸载说明 + 本轮 user 消息"
        );
        assert_eq!(snapshots[2][1].role, crate::llm::MessageRole::System);
        assert!(
            snapshots[2][1].content.contains("已自动卸载：Skill「数值计算」"),
            "第二轮历史应含上轮卸载说明，got: {}",
            snapshots[2][1].content
        );
        drop(snapshots);
        // 摘要一次性消费：第二轮结束后共享状态应已清空
        assert!(last_unloads.lock().unwrap().is_empty(), "第二轮无激活，摘要应清空且不再写入");
    }
    */

    // ── 通用重试（缺陷 3；模块 5 P1 重试挂靠点）──

    #[test]
    fn is_retryable_judges_error_types() {
        use crate::llm::LlmError;
        // 超时恒可重试
        assert!(is_retryable(&LlmError::Timeout("连接超时".into())));
        // 429 限流 / 5xx 服务端可重试
        assert!(is_retryable(&LlmError::Api(
            "HTTP 429: rate limited".into()
        )));
        assert!(is_retryable(&LlmError::Api("HTTP 503: unavailable".into())));
        assert!(is_retryable(&LlmError::Api(
            "HTTP 500: internal error".into()
        )));
        // 客户端错误 / 配置 / 解析不重试
        assert!(!is_retryable(&LlmError::Api(
            "HTTP 401: unauthorized".into()
        )));
        assert!(!is_retryable(&LlmError::Api("HTTP 404: not found".into())));
        assert!(!is_retryable(&LlmError::Config("缺少 api_key".into())));
        assert!(!is_retryable(&LlmError::Parse("bad json".into())));
    }

    /// 测试用故障客户端：前 N 次 chat_stream 在**任何流事件前**返回 Timeout，
    /// 之后委托给真实 Mock。用于验证重试路径（请求级失败 → 重试）。
    struct FlakyClient {
        inner: Arc<crate::llm::mock::MockLlmClient>,
        failures_left: std::sync::atomic::AtomicUsize,
    }

    impl FlakyClient {
        fn new(inner: Arc<crate::llm::mock::MockLlmClient>, failures: usize) -> Self {
            Self {
                inner,
                failures_left: std::sync::atomic::AtomicUsize::new(failures),
            }
        }
    }

    impl LlmClient for FlakyClient {
        fn chat<'a>(
            &'a self,
            config: &'a LlmConfig,
            messages: &'a [ChatMessage],
            tools: &'a [crate::llm::ToolDef],
        ) -> Pin<Box<dyn Future<Output = Result<crate::llm::LlmResponse, LlmError>> + Send + 'a>>
        {
            Box::pin(async move { self.inner.chat(config, messages, tools).await })
        }

        fn chat_stream<'a>(
            &'a self,
            config: &'a LlmConfig,
            messages: &'a [ChatMessage],
            tools: &'a [crate::llm::ToolDef],
            on_event: Box<dyn Fn(StreamEvent) + Send + Sync>,
        ) -> Pin<Box<dyn Future<Output = Result<StreamResult, LlmError>> + Send + 'a>> {
            Box::pin(async move {
                let remaining = self.failures_left.load(std::sync::atomic::Ordering::SeqCst);
                if remaining > 0 {
                    self.failures_left
                        .store(remaining - 1, std::sync::atomic::Ordering::SeqCst);
                    return Err(LlmError::Timeout("模拟连接超时".into()));
                }
                self.inner
                    .chat_stream(config, messages, tools, on_event)
                    .await
            })
        }
    }

    /// 测试用"先出流后失败"客户端：emit 一个 TextDelta 后返回 Timeout。
    /// 用于验证"流已开始 → 不重试"的边界（重试会污染 log/IPC 回放）。
    struct EmitThenFailClient;

    impl LlmClient for EmitThenFailClient {
        fn chat<'a>(
            &'a self,
            _config: &'a LlmConfig,
            _messages: &'a [ChatMessage],
            _tools: &'a [crate::llm::ToolDef],
        ) -> Pin<Box<dyn Future<Output = Result<crate::llm::LlmResponse, LlmError>> + Send + 'a>>
        {
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
            _config: &'a LlmConfig,
            _messages: &'a [ChatMessage],
            _tools: &'a [crate::llm::ToolDef],
            on_event: Box<dyn Fn(StreamEvent) + Send + Sync>,
        ) -> Pin<Box<dyn Future<Output = Result<StreamResult, LlmError>> + Send + 'a>> {
            Box::pin(async move {
                on_event(StreamEvent::TextDelta {
                    content: "半截回答".into(),
                });
                Err(LlmError::Timeout("流中断".into()))
            })
        }
    }

    fn make_simple_mock() -> Arc<crate::llm::mock::MockLlmClient> {
        use crate::llm::mock::{MockLlmClient, MockResponse};
        Arc::new(MockLlmClient::new(vec![MockResponse {
            events: vec![StreamEvent::TextDelta {
                content: "重试成功".into(),
            }],
            result: StreamResult {
                text: "重试成功".into(),
                tool_calls: vec![],
                reasoning: None,
                usage: None,
                finish_reason: None,
            },
        }]))
    }

    #[tokio::test]
    async fn retry_after_request_level_timeout() {
        use crate::llm::ApiType;

        let mock = make_simple_mock();
        let flaky = Arc::new(FlakyClient::new(mock, 1)); // 第 1 次失败，重试后成功
        let mut client_registry = ClientRegistry::new();
        client_registry.register(ApiType::OpenAiCompletions, flaky);

        let (sink, events, _) = test_sink();
        let llm = &**client_registry.select(&ApiType::OpenAiCompletions).unwrap();

        let result = call_llm_with_retry(
            "s1",
            &[],
            &[],
            llm,
            &LlmConfig::default(),
            &sink,
            &Arc::new(ToolRegistry::new()),
            &Arc::new(ToolContext::new()),
            RetryPolicy {
                max_attempts: 2,
                base_delay_ms: 1,
            },
        )
        .await
        .unwrap();
        assert_eq!(result.text, "重试成功");
        // 重试不应产生重复事件（失败发生在任何流事件之前）
        let _ = events;
    }

    #[tokio::test]
    async fn retry_exhausted_returns_error() {
        use crate::llm::ApiType;

        let mock = make_simple_mock();
        let flaky = Arc::new(FlakyClient::new(mock, 2)); // 两次都失败（max_attempts=2）
        let mut client_registry = ClientRegistry::new();
        client_registry.register(ApiType::OpenAiCompletions, flaky);

        let (sink, _, _) = test_sink();
        let llm = &**client_registry.select(&ApiType::OpenAiCompletions).unwrap();

        let err = call_llm_with_retry(
            "s1",
            &[],
            &[],
            llm,
            &LlmConfig::default(),
            &sink,
            &Arc::new(ToolRegistry::new()),
            &Arc::new(ToolContext::new()),
            RetryPolicy {
                max_attempts: 2,
                base_delay_ms: 1,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, OrchestratorError::Llm(LlmError::Timeout(_))));
    }

    #[tokio::test]
    async fn no_retry_after_stream_started() {
        use crate::llm::ApiType;

        let mut client_registry = ClientRegistry::new();
        client_registry.register(ApiType::OpenAiCompletions, Arc::new(EmitThenFailClient));

        let (sink, events, log) = test_sink();
        let llm = &**client_registry.select(&ApiType::OpenAiCompletions).unwrap();

        let err = call_llm_with_retry(
            "s1",
            &[],
            &[],
            llm,
            &LlmConfig::default(),
            &sink,
            &Arc::new(ToolRegistry::new()),
            &Arc::new(ToolContext::new()),
            RetryPolicy {
                max_attempts: 3,
                base_delay_ms: 1,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, OrchestratorError::Llm(LlmError::Timeout(_))));

        // 流已开始：只 emit 了一次（无重试）——初始 phase + 切换 phase + 一条 chunk
        // （若发生重试，chunk 会重复出现）
        assert_eq!(events.lock().unwrap().len(), 3);
        assert_eq!(log.lock().unwrap().len(), 1);
    }

    // ═══════════════════════════════════════════════════════
    // M3-B 中止通道集成测试（§8 后端清单）
    // ═══════════════════════════════════════════════════════

    // ── dispatch 环境装配（与 dispatch_turn_writes_coherent_* 同构，供多用例复用）──

    #[allow(clippy::type_complexity)]
    fn setup_dispatch_env(
        tools: Arc<ToolRegistry>,
        client: Arc<dyn LlmClient>,
        history: Vec<FrontendMessage>,
    ) -> (
        EventSink,
        Arc<Mutex<Vec<IpcEvent>>>,
        Arc<Mutex<SessionLog>>,
        Vec<ChatMessage>,
        Vec<crate::llm::ToolDef>,
        Arc<ClientRegistry>,
        Arc<ToolRegistry>,
    ) {
        let mut client_registry = ClientRegistry::new();
        client_registry.register(crate::llm::ApiType::OpenAiCompletions, client);
        let client_registry = Arc::new(client_registry);
        let tool_defs = tools.list_defs();
        let (sink, events, log) = test_sink();
        {
            let mut log = log.lock().unwrap();
            log.append(SessionEvent::turn_start("s1"));
            log.seed_from_history("s1", history);
            log.push_header(
                "s1",
                &crate::orchestrator::context::assemble_system_prompt(""),
                &tool_defs,
            );
        }
        let derived = log
            .lock()
            .unwrap()
            .derive_messages(ReasoningPolicy::Preserve);
        let messages = crate::orchestrator::context::build_context(&derived, tool_defs.len(), "");
        (
            sink,
            events,
            log,
            messages,
            tool_defs,
            client_registry,
            tools,
        )
    }

    fn user_msg(content: &str) -> FrontendMessage {
        FrontendMessage {
            role: "user".into(),
            content: content.into(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            attachments: Vec::new(),
        }
    }

    fn mock_echo_response(text: &str) -> crate::llm::mock::MockResponse {
        use crate::llm::mock::MockResponse;
        MockResponse {
            events: vec![StreamEvent::TextDelta {
                content: text.into(),
            }],
            result: StreamResult {
                text: text.into(),
                tool_calls: vec![],
                reasoning: None,
                usage: None,
                finish_reason: None,
            },
        }
    }

    fn unique_ws_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-orch-abort-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn max_tokens_finish_reason_keeps_text_and_sets_turn_reason() {
        use crate::llm::mock::{MockLlmClient, MockResponse};
        use crate::llm::ApiType;

        let mock = Arc::new(MockLlmClient::new(vec![MockResponse {
            events: vec![StreamEvent::TextDelta {
                content: "半截回答".into(),
            }],
            result: StreamResult {
                text: "半截回答".into(),
                tool_calls: vec![],
                reasoning: None,
                usage: None,
                finish_reason: Some(crate::llm::FinishReason::MaxTokens),
            },
        }]));
        let tools = Arc::new(ToolRegistry::new());
        let (sink, _events, log, mut messages, tool_defs, client_registry, _tools) =
            setup_dispatch_env(tools.clone(), mock, vec![user_msg("问题")]);

        let outcome = dispatch_turn(
            "s1",
            &sink,
            &mut messages,
            &Arc::new(crate::skills_index::SkillIndex::empty()),
            &tools,
            &tool_defs,
            &**client_registry.select(&ApiType::OpenAiCompletions).unwrap(),
            &LlmConfig::default(),
            None,
            ReasoningPolicy::Preserve,
            ExecutionMode::Ask,
            &ApprovalRegistry::new(),
            &AbortSignal::new(),
            &SteerQueue::new(),
            std::path::Path::new(&std::env::temp_dir()),
        )
        .await
        .unwrap();

        assert_eq!(outcome, TurnOutcome::Completed);
        let log = log.lock().unwrap();
        assert!(
            log.events().iter().any(|e| matches!(
                e,
                SessionEvent::TurnEnd {
                    reason: TurnEndReason::MaxTokens,
                    ..
                }
            )),
            "finish_reason=length 应写 turn/end(max-tokens)"
        );
        assert!(
            log.events().iter().any(|e| matches!(
                e,
                SessionEvent::AssistantMessage { content, .. } if content == "半截回答"
            )),
            "截断轮仍应保留已生成的回答文本"
        );
    }

    /// 测试辅助工具：等待中止或释放信号后返回（并发安全 → 走并行组）。
    /// 返回固定文本（同参同果 → 同指纹，供停滞场景构造）。
    struct WaitOrReleaseTool {
        release: Arc<std::sync::atomic::AtomicBool>,
    }

    impl crate::tools::Tool for WaitOrReleaseTool {
        fn name(&self) -> &str {
            "wait_or_release"
        }
        fn description(&self) -> &str {
            "测试辅助：等待中止/释放"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }
        fn execute(
            &self,
            ctx: &ToolContext,
            _params: &str,
        ) -> Result<crate::tools::ToolOutput, ToolError> {
            loop {
                if self.release.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                if ctx.abort.as_ref().is_some_and(|a| a.is_aborted()) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(crate::tools::ToolOutput::text("wait_or_release: done"))
        }
    }

    /// 测试辅助工具：仅等待中止信号后返回（非并发安全 → 走串行段；
    /// 未置位时恒阻塞——保证「abort 置位必先于本工具返回」的确定性）。
    struct SerialWaitAbortTool;

    impl crate::tools::Tool for SerialWaitAbortTool {
        fn name(&self) -> &str {
            "serial_wait_abort"
        }
        fn description(&self) -> &str {
            "测试辅助：串行等待中止"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }
        fn is_concurrency_safe(&self) -> bool {
            false
        }
        fn execute(
            &self,
            ctx: &ToolContext,
            _params: &str,
        ) -> Result<crate::tools::ToolOutput, ToolError> {
            loop {
                if ctx.abort.as_ref().is_some_and(|a| a.is_aborted()) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(crate::tools::ToolOutput::text("serial_wait_abort: done"))
        }
    }

    /// 从 IPC 快照中收集（克隆后）指定类型的会话事件计数（观察端用）。
    fn ipc_has(events: &[IpcEvent], pred: impl Fn(&SessionEvent) -> bool) -> bool {
        events
            .iter()
            .filter_map(|e| match e {
                IpcEvent::Session(s) => Some(s),
                IpcEvent::Agent(_) => None,
            })
            .any(pred)
    }

    /// dispatch 中止用例的事件观察器：轮询 IPC 直到条件满足 → abort。
    /// 返回 true = 条件已见并已 abort；false = 超时（用例失败信号）。
    /// 参数按值持有（调用处 clone）——观察器常需 tokio::spawn（'static）。
    async fn abort_when<F>(events: Arc<Mutex<Vec<IpcEvent>>>, signal: AbortSignal, pred: F) -> bool
    where
        F: Fn(&[IpcEvent]) -> bool,
    {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            let snapshot: Vec<IpcEvent> = events.lock().unwrap().clone();
            if pred(&snapshot) {
                signal.abort();
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    // ── abort-in-LLM：Mock 流中途挂起 → abort → dispatch Aborted ──

    #[tokio::test]
    async fn dispatch_abort_in_hanging_llm_stream_returns_aborted_and_keeps_chunk() {
        use crate::llm::mock::MockLlmClient;

        // Mock：第 0 次调用推完 chunk 后挂起（模拟真实 LLM 长流被中止）
        let mock = Arc::new(MockLlmClient::new_with_hang(
            vec![{
                let mut r = mock_echo_response("半截回答");
                r.events.push(StreamEvent::TextDelta {
                    content: "（未完）".into(),
                });
                r
            }],
            0,
        ));
        let tools = Arc::new(ToolRegistry::new());
        let (sink, events, log, mut messages, tool_defs, client_registry, _tools) =
            setup_dispatch_env(Arc::clone(&tools), mock, vec![user_msg("测试中止")]);
        let llm = &**client_registry
            .select(&crate::llm::ApiType::OpenAiCompletions)
            .unwrap();
        let signal = AbortSignal::new();

        // 观察端：见 chunk → abort（挂起点必在 select 内，chunk 已双写入 log）
        let steer_queue = SteerQueue::new(); // 插话队列空（默认零行为）前提
        let observer = tokio::spawn(abort_when(Arc::clone(&events), signal.clone(), |evs| {
            ipc_has(evs, |s| matches!(s, SessionEvent::AssistantChunk { .. }))
        }));

        let outcome = dispatch_turn(
            "s1",
            &sink,
            &mut messages,
            &Arc::new(crate::skills_index::SkillIndex::empty()),
            &tools,
            &tool_defs,
            llm,
            &LlmConfig::default(),
            None,
            ReasoningPolicy::Preserve,
            ExecutionMode::Auto,
            &ApprovalRegistry::new(),
            &signal,
            &steer_queue,
            std::path::Path::new(&std::env::temp_dir()),
        )
        .await
        .unwrap();

        assert!(
            observer.await.unwrap(),
            "观察超时：未等到 chunk（Mock 挂起点未达）"
        );
        assert!(
            matches!(outcome, TurnOutcome::Aborted(_)),
            "LLM 段中止 → Aborted"
        );

        // 已产出 chunk 保留（log 双写）；无 step/end（中止出口不闭合边界）
        let log = log.lock().unwrap();
        assert!(
            log.events()
                .iter()
                .any(|e| matches!(e, SessionEvent::AssistantChunk { content, .. } if content.contains("半截回答"))),
            "已产出 chunk 应保留在 log"
        );
        assert!(
            !log.events()
                .iter()
                .any(|e| matches!(e, SessionEvent::StepEnd { .. })),
            "中止出口不写 step/end"
        );
        assert!(
            !log.events()
                .iter()
                .any(|e| matches!(e, SessionEvent::TurnEnd { .. })),
            "dispatch 不写 turn/end（process Aborted 出口补写）"
        );
    }

    // ── abort-in-approval：审批等待中中止 → 补 Rejected/Drop result + process 收尾 ──

    #[tokio::test]
    async fn process_abort_while_approval_pending_pairs_card_and_cleans_up() {
        use crate::llm::mock::{MockLlmClient, MockResponse};
        use crate::llm::{ApiType, ToolCall, ToolCallFunction};
        use crate::session::ApprovalDecision;
        use crate::session::ApprovalRejectReason;

        let ws = unique_ws_dir("approval");
        let mut tool_registry = ToolRegistry::new();
        tool_registry
            .register(Box::new(crate::tools::RunCommandTool::new(ws.clone())))
            .unwrap();
        let tool_registry = Arc::new(tool_registry);

        // Mock：一次 run_command 调用（审批卡挂起；无第二次预设——中止后不再调 LLM）
        let tool_call = ToolCall {
            id: "call_aprv".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "run_command".into(),
                arguments: r#"{"command":"echo hi"}"#.into(),
            },
        };
        let mock = Arc::new(MockLlmClient::new(vec![MockResponse {
            events: vec![StreamEvent::ToolCall {
                id: tool_call.id.clone(),
                name: tool_call.function.name.clone(),
                arguments: tool_call.function.arguments.clone(),
            }],
            result: StreamResult {
                text: String::new(),
                tool_calls: vec![tool_call],
                reasoning: None,
                usage: None,
                finish_reason: None,
            },
        }]));
        let mut client_registry = ClientRegistry::new();
        client_registry.register(ApiType::OpenAiCompletions, mock);
        let client_registry = Arc::new(client_registry);
        let model_registry = Arc::new(
            crate::llm::ModelRegistry::from_json(include_str!("../../../resources/models.json"))
                .unwrap(),
        );
        let approvals = Arc::new(ApprovalRegistry::new());
        let signal = Arc::new(AbortSignal::new());
        let steer_queue = Arc::new(SteerQueue::new());
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));

        // process 跑在子任务；主任务观察审批请求事件 → abort
        let task = {
            let events_clone = events.clone();
            let reg = Arc::clone(&client_registry);
            let tools = Arc::clone(&tool_registry);
            let model_registry = Arc::clone(&model_registry);
            let approvals = Arc::clone(&approvals);
            let signal = Arc::clone(&signal);
            let steer_queue = Arc::clone(&steer_queue);
            tokio::spawn(async move {
                crate::orchestrator::process(
                    "s1",
                    vec![user_msg("跑个命令")],
                    &tools,
                    &reg,
                    &LlmConfig::default(),
                    &model_registry,
                    None,
                    ExecutionMode::Ask,
                    &approvals,
                    &signal,
                    &steer_queue,
                    std::env::temp_dir().join("prts-orch-process-ws"),
                    move |e: IpcEvent| events_clone.lock().unwrap().push(e),
                )
                .await
            })
        };

        // 等审批请求卡出现 → abort（不 resolve，卡恒挂起）
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            let snapshot: Vec<IpcEvent> = events.lock().unwrap().clone();
            if ipc_has(&snapshot, |s| {
                matches!(s, SessionEvent::ToolApprovalRequest { .. })
            }) {
                signal.abort();
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "观察超时：未等到审批请求卡"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        // process 返回 Ok（中止绝不产生 Err）
        let inner = tokio::time::timeout(std::time::Duration::from_secs(8), task)
            .await
            .expect("process 未在超时内返回")
            .expect("process 任务 panic");
        assert!(inner.is_ok(), "process 应 Ok 返回（中止不产生 Err）");

        // ① 已 emit request 的卡补 Rejected/Drop（收前端卡保配对）
        let snapshot: Vec<IpcEvent> = events.lock().unwrap().clone();
        let paired = snapshot.iter().filter_map(|e| match e {
            IpcEvent::Session(SessionEvent::ToolApprovalResult {
                call_id,
                decision,
                reason,
                ..
            }) => Some((call_id.as_str(), *decision, *reason)),
            _ => None,
        });
        assert!(
            paired.clone().any(|(cid, d, r)| {
                cid == "call_aprv"
                    && d == ApprovalDecision::Rejected
                    && r == Some(ApprovalRejectReason::Drop)
            }),
            "应补 Rejected/Drop 收卡，got: {:?}",
            paired.collect::<Vec<_>>()
        );
        // ② turn/end(aborted) 双写 IPC（前端归档唯一轮结束信号）
        assert!(
            ipc_has(&snapshot, |s| {
                matches!(
                    s,
                    SessionEvent::TurnEnd {
                        reason: TurnEndReason::Aborted,
                        ..
                    }
                )
            }),
            "应推送 turn/end(aborted)"
        );
        // ③ 不产生 error 事件（中止 ≠ 错误）
        assert!(
            !ipc_has(&snapshot, |s| matches!(s, SessionEvent::Error { .. })),
            "中止不触发 error 事件"
        );
        // ④ process Aborted 出口 clear_session 兑底清 pending
        assert_eq!(
            approvals.pending_count(),
            0,
            "process 出口应清空 pending sender"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── abort-in-tool：长命令执行中被中止 → runner cancel → Killed → tool/result ok=false ──
    // 注意：本测试在 execute 内阻塞 OS 线程（runner 轮询），观察端（放行/中止）必须
    // 在另一 runtime worker 上跑——default current_thread 会饿死观察端，显式 multi_thread。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_abort_kills_running_command_and_writes_failed_result() {
        use crate::llm::mock::{MockLlmClient, MockResponse};
        use crate::llm::{ApiType, ToolCall, ToolCallFunction};
        use crate::session::ApprovalDecision;

        let ws = unique_ws_dir("tool-kill");
        let mut tools = ToolRegistry::new();
        tools
            .register(Box::new(crate::tools::RunCommandTool::new(ws.clone())))
            .unwrap();
        let tools = Arc::new(tools);

        // Ask 模式 + 门判定（Start-Sleep 非表内程序 → 询问），走真实审批→执行→被杀路径
        let tool_call = ToolCall {
            id: "call_rc".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "run_command".into(),
                arguments: r#"{"command":"Start-Sleep -Seconds 30"}"#.into(),
            },
        };
        let mock = Arc::new(MockLlmClient::new(vec![MockResponse {
            events: vec![StreamEvent::ToolCall {
                id: tool_call.id.clone(),
                name: tool_call.function.name.clone(),
                arguments: tool_call.function.arguments.clone(),
            }],
            result: StreamResult {
                text: String::new(),
                tool_calls: vec![tool_call],
                reasoning: None,
                usage: None,
                finish_reason: None,
            },
        }]));
        let (sink, events, log, mut messages, tool_defs, client_registry, _tools) =
            setup_dispatch_env(Arc::clone(&tools), mock, vec![user_msg("跑长命令")]);
        let llm = &**client_registry.select(&ApiType::OpenAiCompletions).unwrap();
        let signal = AbortSignal::new();
        let approvals = Arc::new(ApprovalRegistry::new());

        // 观察端：请求卡 → resolve 放行；result(Approved) 落账 → abort（命令已在跑）
        let observer = {
            let events = Arc::clone(&events);
            let approvals = Arc::clone(&approvals);
            let signal = signal.clone();
            tokio::spawn(async move {
                let mut resolved = false;
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
                loop {
                    let snapshot: Vec<IpcEvent> = events.lock().unwrap().clone();
                    if !resolved
                        && snapshot.iter().any(|e| {
                            matches!(
                                e,
                                IpcEvent::Session(SessionEvent::ToolApprovalRequest { .. })
                            )
                        })
                    {
                        // 重试至送达（register 与事件 emit 相邻，消除注册竞态）
                        if approvals.resolve(
                            "s1",
                            "call_rc",
                            crate::approval::UserDecision::Approved,
                        ) {
                            resolved = true;
                        }
                    }
                    if resolved
                        && ipc_has(&snapshot, |s| {
                            matches!(
                                s,
                                SessionEvent::ToolApprovalResult {
                                    decision: ApprovalDecision::Approved,
                                    ..
                                }
                            )
                        })
                    {
                        signal.abort();
                        return true;
                    }
                    if std::time::Instant::now() >= deadline {
                        return false;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
        };

        let steer_queue = SteerQueue::new(); // 插话队列空（默认零行为）前提
        let outcome = dispatch_turn(
            "s1",
            &sink,
            &mut messages,
            &Arc::new(crate::skills_index::SkillIndex::empty()),
            &tools,
            &tool_defs,
            llm,
            &LlmConfig::default(),
            None,
            ReasoningPolicy::Preserve,
            ExecutionMode::Ask,
            &approvals,
            &signal,
            &steer_queue,
            &ws,
        )
        .await
        .unwrap();
        assert!(
            observer.await.unwrap(),
            "观察超时：审批放行或命令启动未达预期"
        );
        assert!(matches!(outcome, TurnOutcome::Aborted(_)));

        // tool/result ok=false（runner cancel → Killed → 中性文案「命令已被终止」）
        let log = log.lock().unwrap();
        let results: Vec<&SessionEvent> = log
            .events()
            .iter()
            .filter(|e| matches!(e, SessionEvent::ToolResult { .. }))
            .collect();
        assert_eq!(results.len(), 1, "应有 1 条 tool/result");
        match results[0] {
            SessionEvent::ToolResult {
                ok: false, summary, ..
            } => {
                assert!(summary.contains("已被终止"), "got: {summary}");
            }
            other => panic!("期望被杀命令的失败 result，got {other:?}"),
        }
        assert!(
            !log.events()
                .iter()
                .any(|e| matches!(e, SessionEvent::StepEnd { .. })),
            "中止出口不写 step/end"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── abort 后串行段剩余槽位：补「处理已中止，工具未执行」型 result（保配对）──
    // multi_thread 原因同上：串行工具阻塞 worker 等 abort，观察端需另一 worker。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatch_abort_fills_unexecuted_serial_slots_with_aborted_result() {
        use crate::llm::mock::{MockLlmClient, MockResponse};
        use crate::llm::{ApiType, ToolCall, ToolCallFunction};

        // 两个串行槽位：[serial_wait_abort(执行中收 abort), serial_wait_abort(未轮到)]
        let make_call = |id: &str| ToolCall {
            id: id.into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "serial_wait_abort".into(),
                arguments: "{}".into(),
            },
        };
        let tool_calls = vec![make_call("call_ser0"), make_call("call_ser1")];
        let events_for_mock: Vec<StreamEvent> = tool_calls
            .iter()
            .map(|c| StreamEvent::ToolCall {
                id: c.id.clone(),
                name: c.function.name.clone(),
                arguments: c.function.arguments.clone(),
            })
            .collect();
        let mock = Arc::new(MockLlmClient::new(vec![MockResponse {
            events: events_for_mock,
            result: StreamResult {
                text: String::new(),
                tool_calls: tool_calls.clone(),
                reasoning: None,
                usage: None,
                finish_reason: None,
            },
        }]));

        let mut tools = ToolRegistry::new();
        tools.register(Box::new(SerialWaitAbortTool)).unwrap();
        let tools = Arc::new(tools);
        let (sink, events, log, mut messages, tool_defs, client_registry, _tools) =
            setup_dispatch_env(Arc::clone(&tools), mock, vec![user_msg("串行工具")]);
        let llm = &**client_registry.select(&ApiType::OpenAiCompletions).unwrap();
        let signal = AbortSignal::new();

        // 观察端：任一事件出现（执行已开始）后 abort——串行工具只在看到 abort 后返回，
        // 故「abort 置位先于槽位 0 收尾」确定性成立，槽位 1 必被跳过
        let steer_queue = SteerQueue::new(); // 插话队列空（默认零行为）前提
        let events_obs = Arc::clone(&events);
        let sig_observer = signal.clone();
        let observer = tokio::spawn(async move {
            // 稍候一拍，保证 dispatch 已进入执行（阻塞串行工具在等待 abort）
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            abort_when(events_obs, sig_observer, |_| true).await
        });

        let outcome = dispatch_turn(
            "s1",
            &sink,
            &mut messages,
            &Arc::new(crate::skills_index::SkillIndex::empty()),
            &tools,
            &tool_defs,
            llm,
            &LlmConfig::default(),
            None,
            ReasoningPolicy::Preserve,
            ExecutionMode::Auto,
            &ApprovalRegistry::new(),
            &signal,
            &steer_queue,
            std::path::Path::new(&std::env::temp_dir()),
        )
        .await
        .unwrap();
        assert!(observer.await.unwrap(), "观察器异常");
        assert!(matches!(outcome, TurnOutcome::Aborted(_)));

        // 两条 tool/result：槽 0 ok=true（正常收尾）、槽 1 ok=false + 未执行文案（保配对）
        let log = log.lock().unwrap();
        let results: Vec<&SessionEvent> = log
            .events()
            .iter()
            .filter(|e| matches!(e, SessionEvent::ToolResult { .. }))
            .collect();
        assert_eq!(results.len(), 2, "两槽位都应补 result（配对不变量）");
        match (&results[0], &results[1]) {
            (
                SessionEvent::ToolResult { ok: true, .. },
                SessionEvent::ToolResult {
                    ok: false,
                    summary,
                    call_id,
                    ..
                },
            ) => {
                assert_eq!(call_id, "call_ser1");
                assert!(summary.contains("处理已中止，工具未执行"), "got: {summary}");
            }
            other => panic!("槽位结果不符预期: {other:?}"),
        }
        assert!(
            !log.events()
                .iter()
                .any(|e| matches!(e, SessionEvent::StepEnd { .. })),
            "中止出口不写 step/end"
        );
    }

    // ── 停滞 vs 中止并发：中止优先（同收敛态下 abort 胜出）──
    // multi_thread 原因同上：并行组 join 阻塞 worker 等释放/中止，触发端需另一 worker。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stagnation_without_abort_terminates_with_stagnation() {
        use crate::llm::mock::{MockLlmClient, MockResponse};
        use crate::llm::{ApiType, ToolCall, ToolCallFunction};

        // 同一步内 5 个同参同果调用 → 指纹窗口 5× 相同 → 停滞可触发
        let tool_calls: Vec<ToolCall> = (0..5)
            .map(|i| ToolCall {
                id: format!("call_stag_{i}"),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "wait_or_release".into(),
                    arguments: "{}".into(),
                },
            })
            .collect();
        let events_for_mock: Vec<StreamEvent> = tool_calls
            .iter()
            .map(|c| StreamEvent::ToolCall {
                id: c.id.clone(),
                name: c.function.name.clone(),
                arguments: c.function.arguments.clone(),
            })
            .collect();
        let mock = Arc::new(MockLlmClient::new(vec![MockResponse {
            events: events_for_mock,
            result: StreamResult {
                text: String::new(),
                tool_calls: tool_calls.clone(),
                reasoning: None,
                usage: None,
                finish_reason: None,
            },
        }]));
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut tools = ToolRegistry::new();
        tools
            .register(Box::new(WaitOrReleaseTool {
                release: Arc::clone(&release),
            }))
            .unwrap();
        let tools = Arc::new(tools);
        let (sink, events, log, mut messages, tool_defs, client_registry, _tools) =
            setup_dispatch_env(Arc::clone(&tools), mock, vec![user_msg("停滞")]);
        let llm = &**client_registry.select(&ApiType::OpenAiCompletions).unwrap();
        let signal = AbortSignal::new();

        // 释放端：200ms 后放行（无 abort）——线程先阻塞于等待，释放才完成
        let steer_queue = SteerQueue::new(); // 插话队列空（默认零行为）前提
        let releaser = {
            let release = Arc::clone(&release);
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                release.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        };

        let outcome = dispatch_turn(
            "s1",
            &sink,
            &mut messages,
            &Arc::new(crate::skills_index::SkillIndex::empty()),
            &tools,
            &tool_defs,
            llm,
            &LlmConfig::default(),
            None,
            ReasoningPolicy::Preserve,
            ExecutionMode::Auto,
            &ApprovalRegistry::new(),
            &signal,
            &steer_queue,
            std::path::Path::new(&std::env::temp_dir()),
        )
        .await
        .unwrap();
        releaser.await.unwrap();
        assert_eq!(outcome, TurnOutcome::Stagnation, "无中止 → 停滞终止");

        let log = log.lock().unwrap();
        assert!(matches!(
            log.events().last().unwrap(),
            SessionEvent::TurnEnd {
                reason: TurnEndReason::Stagnation,
                ..
            }
        ));
        let _ = events;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn abort_wins_over_stagnation_when_concurrent() {
        use crate::llm::mock::{MockLlmClient, MockResponse};
        use crate::llm::{ApiType, ToolCall, ToolCallFunction};

        // 与 stagnation_without_abort 完全同构（同收敛态），仅中止替代释放
        let tool_calls: Vec<ToolCall> = (0..5)
            .map(|i| ToolCall {
                id: format!("call_stag_{i}"),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "wait_or_release".into(),
                    arguments: "{}".into(),
                },
            })
            .collect();
        let events_for_mock: Vec<StreamEvent> = tool_calls
            .iter()
            .map(|c| StreamEvent::ToolCall {
                id: c.id.clone(),
                name: c.function.name.clone(),
                arguments: c.function.arguments.clone(),
            })
            .collect();
        let mock = Arc::new(MockLlmClient::new(vec![MockResponse {
            events: events_for_mock,
            result: StreamResult {
                text: String::new(),
                tool_calls: tool_calls.clone(),
                reasoning: None,
                usage: None,
                finish_reason: None,
            },
        }]));
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut tools = ToolRegistry::new();
        tools
            .register(Box::new(WaitOrReleaseTool {
                release: Arc::clone(&release),
            }))
            .unwrap();
        let tools = Arc::new(tools);
        let (sink, _events, log, mut messages, tool_defs, client_registry, _tools) =
            setup_dispatch_env(Arc::clone(&tools), mock, vec![user_msg("停滞")]);
        let llm = &**client_registry.select(&ApiType::OpenAiCompletions).unwrap();
        let signal = AbortSignal::new();

        // 中止端：200ms 后 abort——线程只在中止置位后返回（释放标志恒 false），
        // 故「中止落在停滞判定前」确定性成立（5× 同指纹已入窗口，无中止必停滞）
        let steer_queue = SteerQueue::new(); // 插话队列空（默认零行为）前提
        let aborter = {
            let signal = signal.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                signal.abort();
            })
        };

        let outcome = dispatch_turn(
            "s1",
            &sink,
            &mut messages,
            &Arc::new(crate::skills_index::SkillIndex::empty()),
            &tools,
            &tool_defs,
            llm,
            &LlmConfig::default(),
            None,
            ReasoningPolicy::Preserve,
            ExecutionMode::Auto,
            &ApprovalRegistry::new(),
            &signal,
            &steer_queue,
            std::path::Path::new(&std::env::temp_dir()),
        )
        .await
        .unwrap();
        aborter.await.unwrap();
        assert!(
            matches!(outcome, TurnOutcome::Aborted(_)),
            "停滞与中止并发 → 中止优先"
        );
        // 中止出口不写 turn/end（process 补）；无 error 事件
        let log = log.lock().unwrap();
        assert!(!log
            .events()
            .iter()
            .any(|e| matches!(e, SessionEvent::TurnEnd { .. })));
        assert!(!log
            .events()
            .iter()
            .any(|e| matches!(e, SessionEvent::Error { .. })));
    }

    // ── process 级：Aborted 出口（clear_session + turn/end 双写 + Ok 返回）──

    #[tokio::test]
    async fn process_aborted_exit_emits_turn_end_and_clears_stale_approvals() {
        use crate::llm::mock::MockLlmClient;

        // Mock 挂起 + 预置一条滞留审批（process 出口应兑底清掉）
        let mock = Arc::new(MockLlmClient::new_with_hang(
            vec![mock_echo_response("半截回答")],
            0,
        ));
        let mut client_registry = ClientRegistry::new();
        client_registry.register(crate::llm::ApiType::OpenAiCompletions, mock);
        let client_registry = Arc::new(client_registry);
        let model_registry = Arc::new(
            crate::llm::ModelRegistry::from_json(include_str!("../../../resources/models.json"))
                .unwrap(),
        );
        let approvals = Arc::new(ApprovalRegistry::new());
        approvals.register("s1", "ghost_call");
        let signal = Arc::new(AbortSignal::new());
        let steer_queue = Arc::new(SteerQueue::new());
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let tools = Arc::new(ToolRegistry::new());

        let task = {
            let events_clone = events.clone();
            let reg = Arc::clone(&client_registry);
            let tools = Arc::clone(&tools);
            let model_registry = Arc::clone(&model_registry);
            let approvals = Arc::clone(&approvals);
            let signal = Arc::clone(&signal);
            let steer_queue = Arc::clone(&steer_queue);
            tokio::spawn(async move {
                crate::orchestrator::process(
                    "s1",
                    vec![user_msg("测试中止")],
                    &tools,
                    &reg,
                    &LlmConfig::default(),
                    &model_registry,
                    None,
                    ExecutionMode::Auto,
                    &approvals,
                    &signal,
                    &steer_queue,
                    std::env::temp_dir().join("prts-orch-process-ws"),
                    move |e: IpcEvent| events_clone.lock().unwrap().push(e),
                )
                .await
            })
        };

        // 见 chunk（挂起点已入 select）→ abort
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            let snapshot: Vec<IpcEvent> = events.lock().unwrap().clone();
            if ipc_has(&snapshot, |s| {
                matches!(s, SessionEvent::AssistantChunk { .. })
            }) {
                signal.abort();
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "观察超时：未等到 chunk"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let inner = tokio::time::timeout(std::time::Duration::from_secs(8), task)
            .await
            .expect("process 未在超时内返回")
            .expect("process 任务 panic");
        assert!(inner.is_ok(), "中止返回 Ok（绝不产生 Err）");

        // turn/end(aborted) 已双写 IPC；无 error；chunk 保留；滞留审批被清
        let snapshot: Vec<IpcEvent> = events.lock().unwrap().clone();
        assert!(ipc_has(&snapshot, |s| {
            matches!(
                s,
                SessionEvent::TurnEnd {
                    reason: TurnEndReason::Aborted,
                    ..
                }
            )
        }));
        assert!(!ipc_has(&snapshot, |s| matches!(
            s,
            SessionEvent::Error { .. }
        )));
        assert!(ipc_has(&snapshot, |s| {
            matches!(s, SessionEvent::AssistantChunk { content, .. } if content.contains("半截回答"))
        }));
        assert_eq!(
            approvals.pending_count(),
            0,
            "process 出口 clear_session 应兑底清滞留审批"
        );
    }

    // ═══════════════════════════════════════════════════════
    // M4 完整权限通道（permission=full）集成测试（批 2 定稿 a-e）
    // ═══════════════════════════════════════════════════════

    /// 驱动一次仅含单个 run_command tool_call 的 dispatch_turn，返回 IPC 快照。
    /// `auto_approve` = 观察端见到 request 即 resolve Approved（预期弹卡的用例）。
    #[allow(clippy::type_complexity)]
    async fn drive_rc_turn(
        mode: ExecutionMode,
        args_json: &str,
        auto_approve: bool,
    ) -> (Vec<IpcEvent>, Arc<ApprovalRegistry>, bool) {
        use crate::llm::mock::{MockLlmClient, MockResponse};
        use crate::llm::{ApiType, ToolCall, ToolCallFunction};
        let ws = unique_ws_dir("m4-full");
        let mut tools = ToolRegistry::new();
        tools
            .register(Box::new(crate::tools::RunCommandTool::new(ws.clone())))
            .unwrap();
        let tools = Arc::new(tools);
        let tool_call = ToolCall {
            id: "call_rc".into(),
            call_type: "function".into(),
            function: ToolCallFunction {
                name: "run_command".into(),
                arguments: args_json.into(),
            },
        };
        let mock = Arc::new(MockLlmClient::new(vec![MockResponse {
            events: vec![StreamEvent::ToolCall {
                id: tool_call.id.clone(),
                name: tool_call.function.name.clone(),
                arguments: tool_call.function.arguments.clone(),
            }],
            result: StreamResult {
                text: String::new(),
                tool_calls: vec![tool_call],
                reasoning: None,
                usage: None,
                finish_reason: None,
            },
        }]));
        let (sink, events, _log, mut messages, tool_defs, client_registry, _tools) =
            setup_dispatch_env(Arc::clone(&tools), mock, vec![user_msg("跑命令")]);
        let llm = &**client_registry.select(&ApiType::OpenAiCompletions).unwrap();
        let approvals = Arc::new(ApprovalRegistry::new());
        let signal = AbortSignal::new();

        // 观察端：request 出现 → 放行（resolve 重试至送达）
        let observer = if auto_approve {
            let events_obs = Arc::clone(&events);
            let approvals_obs = Arc::clone(&approvals);
            Some(tokio::spawn(async move {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    let snap: Vec<IpcEvent> = events_obs.lock().unwrap().clone();
                    if snap.iter().any(|e| {
                        matches!(
                            e,
                            IpcEvent::Session(SessionEvent::ToolApprovalRequest { .. })
                        )
                    }) && approvals_obs.resolve("s1", "call_rc", UserDecision::Approved)
                    {
                        return true;
                    }
                    if std::time::Instant::now() >= deadline {
                        return false;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            }))
        } else {
            None
        };

        let steer_queue = SteerQueue::new(); // 插话队列空（默认零行为）前提
        let skill_index = Arc::new(crate::skills_index::SkillIndex::empty());
        let cfg = LlmConfig::default();
        let fut = dispatch_turn(
            "s1",
            &sink,
            &mut messages,
            &skill_index,
            &tools,
            &tool_defs,
            llm,
            &cfg,
            None,
            ReasoningPolicy::Preserve,
            mode,
            &approvals,
            &signal,
            &steer_queue,
            &ws,
        );
        let _ = tokio::time::timeout(std::time::Duration::from_secs(20), fut)
            .await
            .expect("dispatch_turn 超时");
        let approved = match observer {
            Some(o) => o.await.expect("观察任务 panic"),
            None => false,
        };
        let snapshot: Vec<IpcEvent> = events.lock().unwrap().clone();
        let _ = std::fs::remove_dir_all(&ws);
        (snapshot, approvals, approved)
    }

    fn has_request_full(snapshot: &[IpcEvent], js_contains: &str) -> bool {
        ipc_has(snapshot, |s| {
            matches!(
                s,
                SessionEvent::ToolApprovalRequest {
                    privilege: ApprovalPrivilege::Full,
                    justification: Some(j),
                    ..
                } if j.contains(js_contains)
            )
        })
    }

    fn has_approved_full(snapshot: &[IpcEvent]) -> bool {
        ipc_has(snapshot, |s| {
            matches!(
                s,
                SessionEvent::ToolApprovalResult {
                    decision: ApprovalDecision::Approved,
                    privilege: ApprovalPrivilege::Full,
                    ..
                }
            )
        })
    }

    fn has_tool_result(snapshot: &[IpcEvent], want_ok: bool, summary_contains: &str) -> bool {
        ipc_has(snapshot, |s| {
            matches!(
                s,
                SessionEvent::ToolResult { ok, summary, .. }
                    if *ok == want_ok && summary.contains(summary_contains)
            )
        })
    }

    // a) Ask 模式 full：弹卡带 privilege=full + justification；批准后执行成功
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ask_full_requests_card_with_privilege_and_runs() {
        let (snapshot, _, approved) = drive_rc_turn(
            ExecutionMode::Ask,
            r#"{"command":"Write-Output full-run-ok","permission":"full","justification":"Need to write user-level cache for the dev tool"}"#,
            true,
        )
        .await;
        assert!(
            approved,
            "预期弹卡并批准（未等到 request 或 resolve 未送达）"
        );
        assert!(
            has_request_full(&snapshot, "user-level cache"),
            "request 应带 privilege=full + justification"
        );
        assert!(
            has_approved_full(&snapshot),
            "批准 result 应带 privilege=full"
        );
        assert!(
            has_tool_result(&snapshot, true, "full-run-ok"),
            "full 批准后应真实执行成功"
        );
    }

    // b) Auto 模式 full：不直放，仍弹卡询问
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auto_full_still_asks_card_not_auto_approved() {
        // echo 属 gate Auto 族——default 会直放；full 必须弹卡
        let (snapshot, _, approved) = drive_rc_turn(
            ExecutionMode::Auto,
            r#"{"command":"echo auto-full-ok","permission":"full","justification":"Probe a dev tool that needs full token"}"#,
            true,
        )
        .await;
        assert!(approved, "Auto 模式 full 也应弹卡询问（非直放）");
        assert!(
            has_request_full(&snapshot, "dev tool"),
            "request 应 privilege=full"
        );
        assert!(has_approved_full(&snapshot), "批准 result privilege=full");
        assert!(
            has_tool_result(&snapshot, true, "auto-full-ok"),
            "批准后应执行成功"
        );
    }

    // c) CRIT 恒拦任何档位：full 也不放行，无确认卡
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn crit_blocks_full_permission_too() {
        let (snapshot, _, _) = drive_rc_turn(
            ExecutionMode::Ask,
            r#"{"command":"del /s C:\\temp\\x","permission":"full","justification":"test crit"}"#,
            false,
        )
        .await;
        assert!(
            !ipc_has(&snapshot, |s| {
                matches!(s, SessionEvent::ToolApprovalRequest { .. })
            }),
            "CRIT+full 不应弹卡"
        );
        assert!(
            has_tool_result(&snapshot, false, "安全策略拦截"),
            "CRIT+full 应 Denied（tool/result 失败，文案含拦截）"
        );
    }

    // d) full 无 justification：Denied 参数错误，不弹卡
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_without_justification_denied_no_card() {
        let (snapshot, _, _) = drive_rc_turn(
            ExecutionMode::Ask,
            r#"{"command":"echo hi","permission":"full"}"#,
            false,
        )
        .await;
        assert!(
            !ipc_has(&snapshot, |s| {
                matches!(s, SessionEvent::ToolApprovalRequest { .. })
            }),
            "full 无 justification 不应弹卡"
        );
        assert!(
            has_tool_result(&snapshot, false, "justification"),
            "应 Denied 且文案含 justification"
        );
    }

    // e) default 零回归：Auto 模式低危直放（无 request IPC）
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn default_in_auto_still_auto_approves() {
        let (snapshot, _, _) = drive_rc_turn(
            ExecutionMode::Auto,
            r#"{"command":"echo default-auto-ok"}"#,
            false,
        )
        .await;
        assert!(
            !ipc_has(&snapshot, |s| {
                matches!(s, SessionEvent::ToolApprovalRequest { .. })
            }),
            "default + Auto 低危应直放（不弹卡）"
        );
        assert!(
            has_tool_result(&snapshot, true, "default-auto-ok"),
            "直放命令应执行成功"
        );
    }

    // ── 插话（steer）步边界领取集成测试（2026-09-25 语义修订）──

    /// 脚本化三连无工具回答 + 轮中两条插话（请求钩子按时序入队），
    /// 验证步边界领取的 FIFO 注入形成轮内多问答段。
    #[tokio::test]
    async fn steer_injection_forms_multi_segment_turn_with_single_turn_end() {
        use crate::llm::{ApiType, LlmError};
        use std::collections::VecDeque;

        // 三段无工具回答；usage.input 递增——最终 assistant/message 快照
        // 应为最后一次调用（覆盖式 = 最终上下文，非三段求和；累计口径核对点）。
        let queue: Arc<Mutex<VecDeque<Result<StreamResult, LlmError>>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        let mk = |text: &str, input: u32| -> Result<StreamResult, LlmError> {
            Ok(StreamResult {
                text: text.into(),
                tool_calls: vec![],
                reasoning: None,
                usage: Some(TokenUsage {
                    input_tokens: input,
                    context_input_tokens: None,
                    output_tokens: 10,
                    cache_hit_tokens: None,
                    cache_miss_tokens: None,
                }),
                finish_reason: None,
            })
        };
        queue.lock().unwrap().push_back(mk("第一段回答", 100));
        queue.lock().unwrap().push_back(mk("第二段回答", 200));
        queue.lock().unwrap().push_back(mk("第三段回答", 300));

        let snapshots: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));
        // 两条插话在轮中到达（模拟生成中提交）：第 1/2 次请求处理期间入队，
        // 分别在下一个步边界被领取注入（步边界领取时序 = 本测试核心）。
        let steer_queue = Arc::new(SteerQueue::new());
        let hook_queue = Arc::clone(&steer_queue);
        let ids: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let hook_ids = Arc::clone(&ids);
        let hook = Arc::new(move |idx: usize| {
            let id = match idx {
                0 => hook_queue.enqueue("s1", "插话一"),
                1 => hook_queue.enqueue("s1", "插话二"),
                _ => None,
            };
            if let Some(id) = id {
                hook_ids.lock().unwrap().push(id);
            }
        });
        let scripted = Arc::new(ScriptedClient::new_with_hook(
            queue,
            snapshots.clone(),
            hook,
        ));
        let tools = Arc::new(ToolRegistry::new());
        let (sink, _events, log, mut messages, tool_defs, client_registry, _tools2) =
            setup_dispatch_env(tools.clone(), scripted, vec![user_msg("原始问题")]);

        let abort = AbortSignal::new();
        let outcome = dispatch_turn(
            "s1",
            &sink,
            &mut messages,
            &Arc::new(crate::skills_index::SkillIndex::empty()),
            &tools,
            &tool_defs,
            &**client_registry.select(&ApiType::OpenAiCompletions).unwrap(),
            &LlmConfig::default(),
            None,
            ReasoningPolicy::Preserve,
            ExecutionMode::Ask,
            &ApprovalRegistry::new(),
            &abort,
            &steer_queue,
            std::path::Path::new(&std::env::temp_dir()),
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);
        assert_eq!(steer_queue.len("s1"), 0, "插话队列应被全部消费");
        let consumed_id = ids.lock().unwrap()[0].clone();
        assert!(
            !steer_queue.contains("s1", &consumed_id),
            "消费后条目不可再取消"
        );

        let log = log.lock().unwrap();
        // 注入的 user/message 恰两条、按 FIFO 顺序
        let injected_users: Vec<&str> = log
            .events()
            .iter()
            .filter_map(|e| match e {
                SessionEvent::UserMessage { content, .. }
                    if content == "插话一" || content == "插话二" =>
                {
                    Some(content.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(injected_users, vec!["插话一", "插话二"], "FIFO 注入顺序");
        assert_eq!(steer_queue.len("s1"), 0, "注入后队列清空（分区移除）");

        // turn/end 恰一次（队列空才收尾）
        let turn_ends = log
            .events()
            .iter()
            .filter(|e| matches!(e, SessionEvent::TurnEnd { .. }))
            .count();
        assert_eq!(turn_ends, 1, "turn/end 只在队列空时发一次");
        assert!(matches!(
            log.events().last().unwrap(),
            SessionEvent::TurnEnd {
                reason: TurnEndReason::Completed,
                ..
            }
        ));

        // 每段一次 assistant/message 聚合；顺序 = 回答 → 注入 → 回答 …
        let assists: Vec<&str> = log
            .events()
            .iter()
            .filter_map(|e| match e {
                SessionEvent::AssistantMessage { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(assists, vec!["第一段回答", "第二段回答", "第三段回答"]);

        // usage 覆盖式核对：最终 assistant/message usage = 最后一次调用快照
        // （input=300，非 100+200+300 求和）——轮归档/压缩 used 口径依赖此正确性。
        let last_assist_usage = log
            .events()
            .iter()
            .rev()
            .find_map(|e| match e {
                SessionEvent::AssistantMessage { usage: Some(u), .. } => Some(*u),
                _ => None,
            })
            .expect("应存在带 usage 的 assistant/message");
        assert_eq!(last_assist_usage.input_tokens, 300);

        // LLM 收到的上下文：第 2/3 次调用末尾各含对应插话 user（注入生效）
        let snapshots = snapshots.lock().unwrap();
        assert_eq!(snapshots.len(), 3, "应恰好三次 LLM 调用（三段回答）");
        assert_eq!(snapshots[1].last().unwrap().content, "插话一");
        assert_eq!(
            snapshots[1].last().unwrap().role,
            crate::llm::MessageRole::User
        );
        assert_eq!(snapshots[2].last().unwrap().content, "插话二");
        assert_eq!(
            messages.last().unwrap().content,
            "第三段回答",
            "messages 上下文以最后一段收尾"
        );
    }

    /// cancel 移除队首后：该条不被注入；剩余条目在首个步边界（首步请求前）
    /// 领取注入。
    #[tokio::test]
    async fn steer_cancel_prevents_injection_of_cancelled_entry() {
        use crate::llm::{ApiType, LlmError};
        use std::collections::VecDeque;

        let queue: Arc<Mutex<VecDeque<Result<StreamResult, LlmError>>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        let mk = |text: &str| -> Result<StreamResult, LlmError> {
            Ok(StreamResult {
                text: text.into(),
                tool_calls: vec![],
                reasoning: None,
                usage: None,
                finish_reason: None,
            })
        };
        queue.lock().unwrap().push_back(mk("段一"));

        let snapshots: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));
        let scripted = Arc::new(ScriptedClient::new(queue, snapshots.clone()));
        let tools = Arc::new(ToolRegistry::new());
        let (sink, _events, log, mut messages, tool_defs, client_registry, _tools2) =
            setup_dispatch_env(tools.clone(), scripted, vec![user_msg("原始问题")]);

        let steer_queue = SteerQueue::new();
        let id_cancel = steer_queue.enqueue("s1", "要被取消的插话").unwrap();
        steer_queue.enqueue("s1", "保留的插话").unwrap();
        assert!(steer_queue.cancel("s1", &id_cancel), "取消应命中");

        let abort = AbortSignal::new();
        let outcome = dispatch_turn(
            "s1",
            &sink,
            &mut messages,
            &Arc::new(crate::skills_index::SkillIndex::empty()),
            &tools,
            &tool_defs,
            &**client_registry.select(&ApiType::OpenAiCompletions).unwrap(),
            &LlmConfig::default(),
            None,
            ReasoningPolicy::Preserve,
            ExecutionMode::Ask,
            &ApprovalRegistry::new(),
            &abort,
            &steer_queue,
            std::path::Path::new(&std::env::temp_dir()),
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);

        let snapshots = snapshots.lock().unwrap();
        assert_eq!(
            snapshots.len(),
            1,
            "恰一次 LLM 调用（首步请求即领取剩余插话）"
        );
        assert_eq!(snapshots[0].last().unwrap().content, "保留的插话");
        drop(snapshots);

        let log = log.lock().unwrap();
        let injected: Vec<&str> = log
            .events()
            .iter()
            .filter_map(|e| match e {
                SessionEvent::UserMessage { content, .. } if content != "原始问题" => {
                    Some(content.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(injected, vec!["保留的插话"], "被取消条目不得注入");
        let turn_ends = log
            .events()
            .iter()
            .filter(|e| matches!(e, SessionEvent::TurnEnd { .. }))
            .count();
        assert_eq!(turn_ends, 1);
        assert_eq!(steer_queue.len("s1"), 0);
    }

    /// abort 置位：队列不被消费不注入（dispatch 侧不触碰队列）；中止后未
    /// 交付条目由命令层收尾置保留态、同对话下一轮 adopt 领取——尾部以
    /// 队列 API 直接演练该流转。
    #[tokio::test]
    async fn steer_queue_not_consumed_when_aborted_before_dispatch() {
        use crate::llm::{ApiType, LlmError};
        use std::collections::VecDeque;

        let queue: Arc<Mutex<VecDeque<Result<StreamResult, LlmError>>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        queue.lock().unwrap().push_back(Ok(StreamResult {
            text: "不应被调用".into(),
            tool_calls: vec![],
            reasoning: None,
            usage: None,
            finish_reason: None,
        }));
        let snapshots: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));
        let scripted = Arc::new(ScriptedClient::new(queue, snapshots.clone()));
        let tools = Arc::new(ToolRegistry::new());
        let (sink, _events, log, mut messages, tool_defs, client_registry, _tools2) =
            setup_dispatch_env(tools.clone(), scripted, vec![user_msg("原始问题")]);

        let steer_queue = SteerQueue::new();
        steer_queue.enqueue("s1", "中止前已入队").unwrap();

        let abort = AbortSignal::new();
        abort.abort(); // 置位于 dispatch 前：biased select 首轮即中止
        let outcome = dispatch_turn(
            "s1",
            &sink,
            &mut messages,
            &Arc::new(crate::skills_index::SkillIndex::empty()),
            &tools,
            &tool_defs,
            &**client_registry.select(&ApiType::OpenAiCompletions).unwrap(),
            &LlmConfig::default(),
            None,
            ReasoningPolicy::Preserve,
            ExecutionMode::Ask,
            &ApprovalRegistry::new(),
            &abort,
            &steer_queue,
            std::path::Path::new(&std::env::temp_dir()),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, TurnOutcome::Aborted(_)));

        // 队列原样保留（dispatch 未消费）；中止后由命令层收尾置保留态
        assert_eq!(steer_queue.len("s1"), 1, "中止路径不触碰队列");
        let log = log.lock().unwrap();
        assert!(
            !log
                .events()
                .iter()
                .any(|e| matches!(e, SessionEvent::UserMessage { content, .. } if content == "中止前已入队")),
            "中止不得注入"
        );
        drop(log);
        // 模拟命令层收尾（保留语义）：preserve 置保留态 → 同对话下一轮 adopt 领取
        assert!(steer_queue.preserve("s1", "conv-1"), "保留态标记");
        assert_eq!(steer_queue.adopt("conv-1", "s2"), 1, "下一轮领取合并");
        assert_eq!(steer_queue.len("s2"), 1, "保留条目落在新轮分区");
        assert_eq!(steer_queue.take_next("s2").unwrap().text, "中止前已入队");
        let snapshots = snapshots.lock().unwrap();
        assert_eq!(snapshots.len(), 0, "中止优先：LLM 不应被调用");
    }

    /// 工具步之间到达的插话在下一个步边界领取：不再等回答段收尾。
    ///
    /// 时序：请求 0 收到工具调用（插话在请求 0 处理期间入队）→ 工具执行 →
    /// 请求 1 发出前领取注入（旧语义要等到请求 1 收尾后的 Completed 出口
    /// 才领取，需多消耗一次请求）。断言请求 1 上下文中已含插话。
    #[tokio::test]
    async fn steer_arriving_during_tool_step_lands_at_next_step_boundary() {
        use crate::llm::{ApiType, LlmError, ToolCall, ToolCallFunction};
        use std::collections::VecDeque;

        let queue: Arc<Mutex<VecDeque<Result<StreamResult, LlmError>>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        queue.lock().unwrap().push_back(Ok(StreamResult {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "call_echo".into(),
                call_type: "function".into(),
                function: ToolCallFunction {
                    name: "echo".into(),
                    arguments: "{\"message\":\"工具执行\"}".into(),
                },
            }],
            reasoning: None,
            usage: None,
            finish_reason: None,
        }));
        queue.lock().unwrap().push_back(Ok(StreamResult {
            text: "工具步后的收尾回答".into(),
            tool_calls: vec![],
            reasoning: None,
            usage: None,
            finish_reason: None,
        }));
        // 第三组仅作旧语义对照留存（新语义不应消费——snapshots.len 断言）
        queue.lock().unwrap().push_back(Ok(StreamResult {
            text: "不应被调用".into(),
            tool_calls: vec![],
            reasoning: None,
            usage: None,
            finish_reason: None,
        }));

        let snapshots: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));
        // 插话在请求 0 处理期间（工具步中）到达
        let steer_queue = Arc::new(SteerQueue::new());
        let hook_queue = Arc::clone(&steer_queue);
        let hook = Arc::new(move |idx: usize| {
            if idx == 0 {
                hook_queue.enqueue("s1", "工具步中插话").unwrap();
            }
        });
        let scripted = Arc::new(ScriptedClient::new_with_hook(
            queue,
            snapshots.clone(),
            hook,
        ));
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(crate::tools::EchoTool)).unwrap();
        let tools = Arc::new(tools);
        let (sink, _events, log, mut messages, tool_defs, client_registry, _tools2) =
            setup_dispatch_env(tools.clone(), scripted, vec![user_msg("原始问题")]);

        let abort = AbortSignal::new();
        let outcome = dispatch_turn(
            "s1",
            &sink,
            &mut messages,
            &Arc::new(crate::skills_index::SkillIndex::empty()),
            &tools,
            &tool_defs,
            &**client_registry.select(&ApiType::OpenAiCompletions).unwrap(),
            &LlmConfig::default(),
            None,
            ReasoningPolicy::Preserve,
            ExecutionMode::Ask,
            &ApprovalRegistry::new(),
            &abort,
            &steer_queue,
            std::path::Path::new(&std::env::temp_dir()),
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);
        assert_eq!(steer_queue.len("s1"), 0, "插话已被领取");

        let snapshots = snapshots.lock().unwrap();
        assert_eq!(
            snapshots.len(),
            2,
            "两次调用：工具步 + 领取后的收尾步（旧语义需三次）"
        );
        assert!(
            !snapshots[0].iter().any(|m| m.content == "工具步中插话"),
            "请求 0（工具步）不应含插话"
        );
        assert_eq!(
            snapshots[1].last().unwrap().content,
            "工具步中插话",
            "请求 1 发出前领取注入（步边界）"
        );
        assert_eq!(
            snapshots[1].last().unwrap().role,
            crate::llm::MessageRole::User
        );
        drop(snapshots);

        let log = log.lock().unwrap();
        let turn_ends = log
            .events()
            .iter()
            .filter(|e| matches!(e, SessionEvent::TurnEnd { .. }))
            .count();
        assert_eq!(turn_ends, 1, "turn/end 只在队列空时发一次");
    }

    /// 中止保留 → 下一轮领取：preserve/adopt 流转后，新轮首步请求即含
    /// 保留插话（轮次边界领取）。
    #[tokio::test]
    async fn retained_steer_is_claimed_by_next_turn_first_request() {
        use crate::llm::{ApiType, LlmError};
        use std::collections::VecDeque;

        let queue: Arc<Mutex<VecDeque<Result<StreamResult, LlmError>>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        queue.lock().unwrap().push_back(Ok(StreamResult {
            text: "接续回答".into(),
            tool_calls: vec![],
            reasoning: None,
            usage: None,
            finish_reason: None,
        }));

        let snapshots: Arc<Mutex<Vec<Vec<ChatMessage>>>> = Arc::new(Mutex::new(Vec::new()));
        let scripted = Arc::new(ScriptedClient::new(queue, snapshots.clone()));
        let tools = Arc::new(ToolRegistry::new());
        let (sink, _events, _log, mut messages, tool_defs, client_registry, _tools2) =
            setup_dispatch_env(tools.clone(), scripted, vec![user_msg("新问题")]);

        // 上一轮（已中止）留下的未交付插话：preserve → adopt（命令层流转）
        let steer_queue = SteerQueue::new();
        steer_queue.enqueue("s-old", "上一轮未交付").unwrap();
        assert!(steer_queue.preserve("s-old", "conv-1"));
        assert_eq!(steer_queue.adopt("conv-1", "s-new"), 1);

        let abort = AbortSignal::new();
        let outcome = dispatch_turn(
            "s-new",
            &sink,
            &mut messages,
            &Arc::new(crate::skills_index::SkillIndex::empty()),
            &tools,
            &tool_defs,
            &**client_registry.select(&ApiType::OpenAiCompletions).unwrap(),
            &LlmConfig::default(),
            None,
            ReasoningPolicy::Preserve,
            ExecutionMode::Ask,
            &ApprovalRegistry::new(),
            &abort,
            &steer_queue,
            std::path::Path::new(&std::env::temp_dir()),
        )
        .await
        .unwrap();
        assert_eq!(outcome, TurnOutcome::Completed);
        assert_eq!(steer_queue.len("s-new"), 0, "保留条目已被领取");

        let snapshots = snapshots.lock().unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(
            snapshots[0].last().unwrap().content,
            "上一轮未交付",
            "首步请求即领取保留插话（轮次边界领取）"
        );
    }
}
