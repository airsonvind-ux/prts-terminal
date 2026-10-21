//! 会话日志（session log）—— 后端事实来源与 LLM 历史派生
//!
//! 架构升级提案 · 模块 1：
//! 后端 append-only session log 成为对话事实来源，前端退为 log 的投影缓存。
//! LLM 历史由 `derive_messages()` 从 log 派生，替代 convert_history 的
//! 多轮穿透逻辑（reasoning / tool_calls / tool_call_id 的透传不再需要——
//! 每步事件已在 log 里，派生时按需重建）。
//!
//! 事件拆两平面：
//! - `SessionEvent`：持久事实（本模块）。仅变化时追加。
//! - `AgentEvent`：IPC 层 UI 信号（仅 phase 等瞬时态，见 events.rs）。
//!
//! 本模块不依赖 Tauri；SessionLog 保持每轮内存态（当轮镜像，不跨轮）。
//! 跨轮对话持久化已由 session_store.rs 承担（前端归档轮视图存档，见
//! 后端-软件工程 §十二）——本 log 的“持久化 seam 留 Phase 4”表述作废。

use serde::Serialize;

use crate::events::FrontendMessage;
use crate::llm::{ChatMessage, ReasoningPolicy, TokenUsage, ToolCall, ToolDef, UsageStat};

// ═══════════════════════════════════════════════════════
// 事件词汇表
// ═══════════════════════════════════════════════════════

/// 增量片段类型（assistant/chunk 的判别字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChunkKind {
    /// 最终回答文本增量
    Text,
    /// 推理链增量
    Reasoning,
}

/// turn 终止原因（模块 5：终止原因显式化）。
/// P0 落地：completed / error / stagnation 由调度循环直接产生；
/// aborted（用户中止）随 stop 生成（P2）落地；max_tokens 为预留词汇。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TurnEndReason {
    /// 正常完成：LLM 不再请求工具
    Completed,
    /// 处理出错终止
    Error,
    /// 用户中止（stop 生成，Phase 后置预留）
    #[allow(dead_code)] // 预留词汇：随取消（stop 生成，P2）落地
    Aborted,
    /// 停滞检测触发（PRTS 独有资产，升格为一等终止原因）
    Stagnation,
    /// 上下文超限（max_tokens 截断，预留）
    #[allow(dead_code)] // 预留词汇：随 max-tokens 截断处理落地
    MaxTokens,
}

/// 工具审批请求的危险度（M3-A §5.1）。
/// crit 恒拦不产生审批请求，但其判定结果事件（blocked）需要档位字段，故含三档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalRisk {
    /// 低危（自动化模式自动放行，不产生询问）
    Low,
    /// 高危（自动化/询问模式都需用户确认）
    High,
    /// 极危（恒拦，不产生请求；仅出现在 blocked 判定结果）
    Crit,
}

/// 命令执行身份面（M4 §：privilege 与 ApprovalRisk 正交——risk 是 mode 面
/// 分类，privilege 是本命令请求的进程身份）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalPrivilege {
    /// 默认：受限身份执行（restricted token + Job）
    Restricted,
    /// 完整权限：应用自身令牌执行（仍带 Job 回收），需用户确认
    Full,
}

/// 工具审批/策略判定结果（M3-A §5.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalDecision {
    /// 用户确认执行
    Approved,
    /// 用户拒绝 / 审批等待超时 / 通道 drop（原因见 reason）
    Rejected,
    /// 自动化模式低危直放（log_only，无 IPC）
    AutoApproved,
    /// CRIT 恒拦（策略拦截，不产生 request）
    Blocked,
}

/// 拒绝原因（decision = Rejected 时携带）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalRejectReason {
    /// 用户点击拒绝
    User,
    /// 审批等待超时（300s）
    Timeout,
    /// 等待通道 drop（会话结束/清理）
    Drop,
}

/// 缓存桶判别（模块 7：静态桶常驻提醒 / 动态桶会话级）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
#[allow(dead_code)] // 第三轮 cache_* 工具接线启用
pub enum CacheBucket {
    /// 静态桶：LLM 读知识条目后的自写总结（常驻，压缩才卸）
    #[serde(rename = "static")]
    Static,
    /// 动态桶：LLM 工作中产生的重要数据（会话级，每轮可重载）
    #[serde(rename = "dynamic")]
    Dynamic,
}

/// 持久事实事件（Discriminated Union）。
///
/// serde 输出 `{ "type": "turn/start", ... }` 形式，与 IPC 合约一致。
/// 每个事件携带 session_id 用于路由；`seq` 不落字段——由
/// `SessionLog` 中的位置隐式给出（索引即 seq，单调连续）。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SessionEvent {
    /// turn 开始：一次 agent_process 处理周期
    #[serde(rename = "turn/start", rename_all = "camelCase")]
    TurnStart { session_id: String },

    /// turn 结束：记录显式终止原因
    #[serde(rename = "turn/end", rename_all = "camelCase")]
    TurnEnd {
        session_id: String,
        reason: TurnEndReason,
        /// 本轮累计用量（统计增量；无则 None）。前端归档时随 append_turn 提交。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<UsageStat>,
    },

    /// step 开始：一次 LLM 请求 + 其调用的工具。
    /// `estimated_tokens` 为请求前估算（模块 8 双锚的 estimate 兜底锚，
    /// 与 step/end 的 provider usage 基线锚对齐）。
    #[serde(rename = "step/start", rename_all = "camelCase")]
    StepStart {
        session_id: String,
        step: u32,
        estimated_tokens: u64,
    },

    /// step 结束：记录本次调用的 token 用量（最后一次调用即最终上下文状态）
    #[serde(rename = "step/end", rename_all = "camelCase")]
    StepEnd {
        session_id: String,
        step: u32,
        usage: Option<TokenUsage>,
        elapsed_ms: u64,
    },

    /// 用户消息（含历史播种与当前输入）
    #[serde(rename = "user/message", rename_all = "camelCase")]
    UserMessage {
        session_id: String,
        content: String,
        /// 附件引用（历史播种携带；空 = 无附件）。
        /// skip_serializing_if 保持无附件事件的既有序列化字节不变。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::attachments::Attachment>,
    },

    /// 流式增量片段（token 级回放保真；reasoning 与 text 分道）
    #[serde(rename = "assistant/chunk", rename_all = "camelCase")]
    AssistantChunk {
        session_id: String,
        kind: ChunkKind,
        content: String,
    },

    /// 最终 assistant 消息聚合（turn 结束时追加，供回放/遥测与前端归档；
    /// 派生时忽略，派生只认 chunk 序列，避免双计数）。
    /// `usage` 为最后一次 LLM 调用的 token 快照（最终上下文状态，缺陷 4 口径）。
    #[serde(rename = "assistant/message", rename_all = "camelCase")]
    AssistantMessage {
        session_id: String,
        content: String,
        reasoning_content: Option<String>,
        usage: Option<TokenUsage>,
    },

    /// 工具调用请求（arguments 为模型原始 JSON 字符串）
    /// `display` 为展示切片（文件引用 + 文本信息）：orchestrator 在发出本事件前
    /// 经 Registry::display_for 解析；空则序列化时省略（旧归档无此字段）。
    #[serde(rename = "tool/call", rename_all = "camelCase")]
    ToolCall {
        session_id: String,
        call_id: String,
        tool_name: String,
        params: String,
        #[serde(default, skip_serializing_if = "crate::tools::ToolDisplay::is_empty")]
        display: crate::tools::ToolDisplay,
    },

    /// 工具执行结果。`tool_name` 为 None 表示历史播种的旧数据（前端不携带）。
    /// `ok` 区分成功与失败（失败内容同样回传 LLM，由模型决定下一步）。
    #[serde(rename = "tool/result", rename_all = "camelCase")]
    ToolResult {
        session_id: String,
        call_id: String,
        tool_name: Option<String>,
        summary: String,
        ok: bool,
        /// 工具产物的图片附件引用（如 MAA 截图）。派生时在工具结果之后
        /// 补一条带图 user 消息——与轮内内存形态同源，故跟轮可重建。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::attachments::Attachment>,
    },

    /// 请求头快照：渲染后的 system prompt + 工具 schema。
    /// 仅变化时追加（SessionLog::push_header 负责去重），
    /// 保证"模型到底看到了什么"可审计、可重放。
    #[serde(rename = "request/header", rename_all = "camelCase")]
    RequestHeader {
        session_id: String,
        system_prompt: String,
        tool_defs: Vec<ToolDef>,
    },

    /// 缓存区写操作（模块 7 P0：缓存区与 log 分层——CacheStore 独立于 log，
    /// log 只记录 `cache_*` 操作，可回放）。`bucket` 区分静态/动态桶，
    /// `source_entry` 为静态桶的来源知识条目名（None = 动态桶）。
    #[allow(dead_code)] // 第三轮 cache_store 工具接线启用
    #[serde(rename = "cache/store", rename_all = "camelCase")]
    CacheStore {
        session_id: String,
        key: String,
        bucket: CacheBucket,
        source_entry: Option<String>,
    },

    /// 缓存区读取操作（注入动作入 log）。
    #[allow(dead_code)] // 第三轮 cache_retrieve 工具接线启用
    #[serde(rename = "cache/load", rename_all = "camelCase")]
    CacheLoad {
        session_id: String,
        key: String,
        bucket: CacheBucket,
        hit: bool,
    },

    /// 缓存区移除操作。
    #[allow(dead_code)] // 第三轮 cache_remove / reset_workspace 工具接线启用
    #[serde(rename = "cache/remove", rename_all = "camelCase")]
    CacheRemove {
        session_id: String,
        key: String,
        bucket: CacheBucket,
        existed: bool,
    },

    /// Skill 激活事件（模块 B）：激活动作入 log 可回放，仿 cache/* 事件模式。
    /// 激活状态本身随 ActiveContext 每 turn 重建（天然卸载），log 只记录
    /// 事实（何时激活了哪个 Skill），不承载状态。
    #[serde(rename = "skill/activate", rename_all = "camelCase")]
    SkillActivate {
        session_id: String,
        name: String,
    },

    /// 工具审批请求（M3-A §5.1 / M4）：run_command 询问用户确认时推送。
    /// auto 直放仅 log_only（无 IPC）；crit 拦截不产生 request。
    /// `rule_id` = 判定 reason 码（M3-A 地域性信任门的出卡族码：
    /// delete-always-ask / region-external / unanalyzable-syntax 等，
    /// 供前端确认卡渲染"为什么问"类别短语）。crit 不产生 request 故恒 None。
    /// `privilege` = 命令请求的身份面（M4：restricted 默认 / full 完整权限）。
    /// `justification` = full 档必填的一句话理由（前端确认卡展示）。
    /// derive_messages 不消费（结构事件）——LLM 经 tool/result 文案感知。
    #[serde(rename = "tool/approval-request", rename_all = "camelCase")]
    ToolApprovalRequest {
        session_id: String,
        call_id: String,
        /// 完整命令（≤512 字符；preflight 构造时截断）
        command: String,
        risk: ApprovalRisk,
        mode: crate::exec_settings::ExecutionMode,
        /// 出卡族 reason 码（地域性信任门；None = 无门信息，前端不显示类别）
        rule_id: Option<String>,
        /// 请求身份：restricted（默认）/ full（完整权限）
        privilege: ApprovalPrivilege,
        /// full 档的一句话理由（前端展示；restricted 档为 None）
        justification: Option<String>,
    },

    /// 工具审批/策略判定结果（M3-A §5.1 / M4）。
    /// approved/rejected 由用户决策或超时产生；auto-approved = 自动化低危
    /// 直放（log_only）；blocked = CRIT 恒拦（log_only，无 request）。
    /// `privilege` = 请求身份面（与 request 配对）。
    #[serde(rename = "tool/approval-result", rename_all = "camelCase")]
    ToolApprovalResult {
        session_id: String,
        call_id: String,
        decision: ApprovalDecision,
        risk: ApprovalRisk,
        mode: crate::exec_settings::ExecutionMode,
        rule_id: Option<String>,
        reason: Option<ApprovalRejectReason>,
        privilege: ApprovalPrivilege,
    },

    // —— SkillUnload / CacheUnload 已注释（2026-08：卸载机制移除，
    //     无生产者；未来记忆系统复用时可恢复）──
    // /// Skill 卸载事件（模块 B）：轮末自动卸载入 log，与 activate 对称。
    // /// 卸载是系统自动行为（ActiveContext 每 turn 重建），此处记录事实，
    // /// derive_messages 渲染为 LLM 可见的卸载说明——让 LLM 从历史时序
    // /// 理解"上一轮激活已随本轮结束失效"，而非误以为状态持续。
    // #[serde(rename = "skill/unload", rename_all = "camelCase")]
    // SkillUnload {
    //     session_id: String,
    //     name: String,
    // },
    //
    // /// 动态缓存卸载事件（模块 C 补强）：动态桶条目的载入状态轮末自动
    // /// 卸载入 log（桶内容跨轮保留，需要时重新载入）。静态桶条目常驻、
    // /// 每轮自动恢复在场，不写卸载。
    // #[serde(rename = "cache/unload", rename_all = "camelCase")]
    // CacheUnload {
    //     session_id: String,
    //     key: String,
    // },

    /// 错误事件（可恢复错误也入 log，与前端 error 事件对应）。
    /// 当前调度循环以 turn/end reason=error 表达错误，本变体为
    /// 可恢复错误（recoverable=true，继续处理）预留。
    #[allow(dead_code)] // 预留：可恢复错误路径落地时启用
    #[serde(rename = "error", rename_all = "camelCase")]
    Error {
        session_id: String,
        message: String,
        recoverable: bool,
    },
}

// ─── 构造器 ──────────────────────────────────────────

impl SessionEvent {
    pub fn turn_start(session_id: &str) -> Self {
        Self::TurnStart { session_id: session_id.to_string() }
    }
    pub fn turn_end(session_id: &str, reason: TurnEndReason) -> Self {
        Self::TurnEnd { session_id: session_id.to_string(), reason, usage: None }
    }

    /// turn 结束（携带本轮累计用量；用量统计用）。
    pub fn turn_end_with_usage(
        session_id: &str,
        reason: TurnEndReason,
        usage: Option<UsageStat>,
    ) -> Self {
        Self::TurnEnd { session_id: session_id.to_string(), reason, usage }
    }
    pub fn step_start(session_id: &str, step: u32, estimated_tokens: u64) -> Self {
        Self::StepStart {
            session_id: session_id.to_string(),
            step,
            estimated_tokens,
        }
    }
    pub fn step_end(session_id: &str, step: u32, usage: Option<TokenUsage>, elapsed_ms: u64) -> Self {
        Self::StepEnd { session_id: session_id.to_string(), step, usage, elapsed_ms }
    }
    pub fn user_message(session_id: &str, content: &str) -> Self {
        Self::UserMessage {
            session_id: session_id.to_string(),
            content: content.to_string(),
            attachments: Vec::new(),
        }
    }

    /// 带附件的用户消息（历史播种）。
    pub fn user_message_with_attachments(
        session_id: &str,
        content: &str,
        attachments: Vec<crate::attachments::Attachment>,
    ) -> Self {
        Self::UserMessage {
            session_id: session_id.to_string(),
            content: content.to_string(),
            attachments,
        }
    }
    pub fn assistant_chunk(session_id: &str, kind: ChunkKind, content: &str) -> Self {
        Self::AssistantChunk { session_id: session_id.to_string(), kind, content: content.to_string() }
    }
    pub fn assistant_message(
        session_id: &str,
        content: &str,
        reasoning_content: Option<String>,
        usage: Option<TokenUsage>,
    ) -> Self {
        Self::AssistantMessage {
            session_id: session_id.to_string(),
            content: content.to_string(),
            reasoning_content,
            usage,
        }
    }
    pub fn tool_call(session_id: &str, call_id: &str, tool_name: &str, params: &str) -> Self {
        Self::ToolCall {
            session_id: session_id.to_string(),
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            params: params.to_string(),
            display: crate::tools::ToolDisplay::default(),
        }
    }
    /// 带展示切片的 tool/call（orchestrator 发出 IPC / 写 log 时使用）。
    pub fn tool_call_with_display(
        session_id: &str,
        call_id: &str,
        tool_name: &str,
        params: &str,
        display: crate::tools::ToolDisplay,
    ) -> Self {
        Self::ToolCall {
            session_id: session_id.to_string(),
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            params: params.to_string(),
            display,
        }
    }
    pub fn tool_result(session_id: &str, call_id: &str, tool_name: Option<&str>, summary: &str, ok: bool) -> Self {
        Self::ToolResult {
            session_id: session_id.to_string(),
            call_id: call_id.to_string(),
            tool_name: tool_name.map(|s| s.to_string()),
            summary: summary.to_string(),
            ok,
            attachments: Vec::new(),
        }
    }

    /// 带图片附件的工具结果（工具产物走附件通路）。
    pub fn tool_result_with_attachments(
        session_id: &str,
        call_id: &str,
        tool_name: Option<&str>,
        summary: &str,
        ok: bool,
        attachments: Vec<crate::attachments::Attachment>,
    ) -> Self {
        Self::ToolResult {
            session_id: session_id.to_string(),
            call_id: call_id.to_string(),
            tool_name: tool_name.map(|s| s.to_string()),
            summary: summary.to_string(),
            ok,
            attachments,
        }
    }
    pub fn request_header(session_id: &str, system_prompt: &str, tool_defs: Vec<ToolDef>) -> Self {
        Self::RequestHeader {
            session_id: session_id.to_string(),
            system_prompt: system_prompt.to_string(),
            tool_defs,
        }
    }
    pub fn error(session_id: &str, message: &str, recoverable: bool) -> Self {
        Self::Error {
            session_id: session_id.to_string(),
            message: message.to_string(),
            recoverable,
        }
    }

    #[allow(dead_code)] // 第三轮 cache_store 工具接线启用
    pub fn cache_store(
        session_id: &str,
        key: &str,
        bucket: CacheBucket,
        source_entry: Option<&str>,
    ) -> Self {
        Self::CacheStore {
            session_id: session_id.to_string(),
            key: key.to_string(),
            bucket,
            source_entry: source_entry.map(|s| s.to_string()),
        }
    }

    #[allow(dead_code)] // 第三轮 cache_retrieve 工具接线启用
    pub fn cache_load(session_id: &str, key: &str, bucket: CacheBucket, hit: bool) -> Self {
        Self::CacheLoad {
            session_id: session_id.to_string(),
            key: key.to_string(),
            bucket,
            hit,
        }
    }

    #[allow(dead_code)] // 第三轮 cache_remove / reset_workspace 工具接线启用
    pub fn cache_remove(session_id: &str, key: &str, bucket: CacheBucket, existed: bool) -> Self {
        Self::CacheRemove {
            session_id: session_id.to_string(),
            key: key.to_string(),
            bucket,
            existed,
        }
    }

    pub fn skill_activate(session_id: &str, name: &str) -> Self {
        Self::SkillActivate {
            session_id: session_id.to_string(),
            name: name.to_string(),
        }
    }

    /// 工具审批请求（构造器；command 调用方负责截断到 512 字符）。
    /// `rule_id` = 判定 reason 码（地域性信任门出卡族；无门信息传 None）。
    /// `privilege` = 请求身份面（restricted 默认 / full 完整权限）。
    /// `justification` = full 档必填的一句话理由（restricted 档传 None）。
    pub fn tool_approval_request(
        session_id: &str,
        call_id: &str,
        command: &str,
        risk: ApprovalRisk,
        mode: crate::exec_settings::ExecutionMode,
        rule_id: Option<&str>,
        privilege: ApprovalPrivilege,
        justification: Option<&str>,
    ) -> Self {
        Self::ToolApprovalRequest {
            session_id: session_id.to_string(),
            call_id: call_id.to_string(),
            command: command.to_string(),
            risk,
            mode,
            rule_id: rule_id.map(|s| s.to_string()),
            privilege,
            justification: justification.map(|s| s.to_string()),
        }
    }

    /// 工具审批/策略判定结果（构造器）。
    pub fn tool_approval_result(
        session_id: &str,
        call_id: &str,
        decision: ApprovalDecision,
        risk: ApprovalRisk,
        mode: crate::exec_settings::ExecutionMode,
        rule_id: Option<&str>,
        reason: Option<ApprovalRejectReason>,
        privilege: ApprovalPrivilege,
    ) -> Self {
        Self::ToolApprovalResult {
            session_id: session_id.to_string(),
            call_id: call_id.to_string(),
            decision,
            risk,
            mode,
            rule_id: rule_id.map(|s| s.to_string()),
            reason,
            privilege,
        }
    }

    // —— skill_unload / cache_unload 构造已注释（2026-08：卸载机制移除）──
    // pub fn skill_unload(session_id: &str, name: &str) -> Self {
    //     Self::SkillUnload {
    //         session_id: session_id.to_string(),
    //         name: name.to_string(),
    //     }
    // }
    //
    // pub fn cache_unload(session_id: &str, key: &str) -> Self {
    //     Self::CacheUnload {
    //         session_id: session_id.to_string(),
    //         key: key.to_string(),
    //     }
    // }
}

// ═══════════════════════════════════════════════════════
// SessionLog —— append-only 事件日志
// ═══════════════════════════════════════════════════════

/// 单次会话（一次 agent_process 处理周期）的 append-only 事件日志。
///
/// 不变量：
/// - 事件只能追加，不能删除或修改（`events` 字段私有）。
/// - `len()` 即下一个 seq；索引即 seq（单调连续，与上游 `seq = log.length` 对齐）。
/// - LLM 可见事实必有对应事件（"model-visible means logged"）。
#[derive(Debug, Default, Clone)]
pub struct SessionLog {
    events: Vec<SessionEvent>,
    /// 最近一次 request/header 快照，用于"仅变化时追加"去重。
    last_header: Option<(String, Vec<ToolDef>)>,
}

impl SessionLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一个事件。seq 由追加位置隐式给出。
    pub fn append(&mut self, event: SessionEvent) {
        self.events.push(event);
    }

    /// 已追加事件数 = 下一个事件将获得的 seq。
    #[allow(dead_code)] // 遥测/回放（WP4）与测试使用
    pub fn len(&self) -> usize {
        self.events.len()
    }

    #[allow(dead_code)] // 遥测/回放（WP4）与测试使用
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// 只读访问全部事件（seq = 索引）。
    #[allow(dead_code)] // 遥测/回放（WP4）与测试使用
    pub fn events(&self) -> &[SessionEvent] {
        &self.events
    }

    /// 追加 request/header 快照——仅当 system prompt 或工具 schema 变化时
    /// 才真正追加（cache-safe，对齐上游 `request/header` 的 reason=change 语义）。
    pub fn push_header(&mut self, session_id: &str, system_prompt: &str, tool_defs: &[ToolDef]) {
        let changed = match &self.last_header {
            Some((prev_prompt, prev_defs)) => {
                prev_prompt != system_prompt || !tool_defs_eq(prev_defs, tool_defs)
            }
            None => true,
        };
        if changed {
            self.append(SessionEvent::request_header(session_id, system_prompt, tool_defs.to_vec()));
            self.last_header = Some((system_prompt.to_string(), tool_defs.to_vec()));
        }
    }

    /// 从前端回传的历史消息播种日志。
    ///
    /// 播种规则（语义等价于旧 convert_history，但以事件形式入 log）：
    /// - role="error" 跳过（前端 UI 元素，不进 LLM 上下文，也不入 log）
    /// - role="user" → user/message
    /// - role="assistant" → reasoning_content → assistant/chunk(Reasoning)，
    ///   content → assistant/chunk(Text)，tool_calls → tool/call × N
    /// - role="tool" → tool/result（call_id 缺失时防御性跳过）
    /// - 未知 role 按 user 处理（防御性）
    pub fn seed_from_history(&mut self, session_id: &str, history: Vec<FrontendMessage>) {
        for m in history {
            match m.role.as_str() {
                "error" => {
                    // 前端错误提示仅用于 UI 渲染，跳过（与旧 convert_history 一致）
                }
                "user" => {
                    if m.attachments.is_empty() {
                        self.append(SessionEvent::user_message(session_id, &m.content));
                    } else {
                        self.append(SessionEvent::user_message_with_attachments(
                            session_id,
                            &m.content,
                            m.attachments,
                        ));
                    }
                }
                "assistant" => {
                    if let Some(ref reasoning) = m.reasoning_content {
                        if !reasoning.is_empty() {
                            self.append(SessionEvent::assistant_chunk(
                                session_id,
                                ChunkKind::Reasoning,
                                reasoning,
                            ));
                        }
                    }
                    if !m.content.is_empty() {
                        self.append(SessionEvent::assistant_chunk(
                            session_id,
                            ChunkKind::Text,
                            &m.content,
                        ));
                    }
                    if let Some(ref calls) = m.tool_calls {
                        for tc in calls {
                            self.append(SessionEvent::tool_call(
                                session_id,
                                &tc.id,
                                &tc.function.name,
                                &tc.function.arguments,
                            ));
                        }
                    }
                }
                "tool" => {
                    // 历史播种的 tool 结果不携带 tool_name（前端消息无此字段）
                    if let Some(ref call_id) = m.tool_call_id {
                        self.append(SessionEvent::tool_result(
                            session_id,
                            call_id,
                            None,
                            &m.content,
                            true,
                        ));
                    }
                }
                // 未知 role 按 user 处理（防御性，正常不应出现）
                _ => {
                    self.append(SessionEvent::user_message(session_id, &m.content));
                }
            }
        }
    }

    /// 工具返回图片时那条 user 消息的文本。轮内（orchestrator 追加）与
    /// 跟轮（derive 重建）必须同源，否则那条消息会多发一次字节变化。
    pub fn tool_image_text(tool_name: Option<&str>) -> String {
        match tool_name {
            Some(n) if !n.is_empty() => format!("（工具 {n} 返回的图片）"),
            _ => "（工具返回的图片）".to_string(),
        }
    }

    /// 由工具结果的图片附件派生一条带图 user 消息（无图片附件 → None）。
    pub fn tool_image_message(
        tool_name: Option<&str>,
        attachments: &[crate::attachments::Attachment],
    ) -> Option<ChatMessage> {
        let image_paths: Vec<String> = attachments
            .iter()
            .filter(|a| a.is_image())
            .map(|a| a.path.clone())
            .collect();
        if image_paths.is_empty() {
            return None;
        }
        Some(ChatMessage::user_with_image_paths(
            Self::tool_image_text(tool_name),
            image_paths,
        ))
    }

    /// 由 user/message 事件派生 LLM 消息：图片附件 → image_paths（请求前物化）；
    /// 所有附件追加 `[附件：名，路径：落点]` 文本标记——模型据此知道有附件、
    /// 并能按落点自己读它（无视觉能力时，这行也是它知道有图的唯一线索）。
    ///
    /// 给的是**会话落点**（快照副本），非用户原始路径：原文件被移动或删除后标记仍可读，
    /// 且不把用户的目录结构带进请求。
    fn derive_user_message(content: &str, attachments: &[crate::attachments::Attachment]) -> ChatMessage {
        if attachments.is_empty() {
            return ChatMessage::user(content);
        }
        let image_paths: Vec<String> = attachments
            .iter()
            .filter(|a| a.is_image())
            .map(|a| a.path.clone())
            .collect();
        // 标记带落点路径（图片也带，无分支）：模型拿到位置就能自己读文件。
        let marks: Vec<String> = attachments
            .iter()
            .map(|a| format!("[附件：{}，路径：{}]", a.name, a.path))
            .collect();
        let text = if content.is_empty() {
            marks.join("\n")
        } else {
            format!("{}\n\n{}", content, marks.join("\n"))
        };
        if image_paths.is_empty() {
            ChatMessage::user(text)
        } else {
            ChatMessage::user_with_image_paths(text, image_paths)
        }
    }

    /// 从 log 派生 LLM 可见的历史消息（替代 convert_history 的多轮穿透）。
    ///
    /// 派生规则：
    /// - user/message → user 消息
    /// - assistant/chunk 按 kind 累积到 text / reasoning 缓冲
    /// - tool/call 开始收集工具调用（同轮多次调用合并进同一条 assistant 消息）
    /// - tool/result → 先冲刷挂起的 assistant 消息（含工具调用与累积文本），
    ///   再追加 tool 消息
    /// - 冲刷时机：tool/result、下一条 user/message、派生结束
    /// - assistant/message 不参与派生（与 chunk 序列同源，避免双计数）
    ///
    /// `policy` 是 reasoning 回传策略（缺陷 1 应用点）：
    /// - Preserve：推理链始终保留在 assistant 消息中
    /// - RequireToolCall：仅携带工具调用的 assistant 消息保留推理链
    pub fn derive_messages(&self, policy: ReasoningPolicy) -> Vec<ChatMessage> {
        let mut out: Vec<ChatMessage> = Vec::new();
        let mut text_buf = String::new();
        let mut reasoning_buf = String::new();
        let mut pending_tool_calls: Vec<ToolCall> = Vec::new();

        for event in &self.events {
            match event {
                SessionEvent::UserMessage { content, attachments, .. } => {
                    flush_assistant(
                        &mut out,
                        &mut text_buf,
                        &mut reasoning_buf,
                        &mut pending_tool_calls,
                        policy,
                    );
                    out.push(Self::derive_user_message(content, attachments));
                }
                SessionEvent::AssistantChunk { kind, content, .. } => match kind {
                    ChunkKind::Text => text_buf.push_str(content),
                    ChunkKind::Reasoning => reasoning_buf.push_str(content),
                },
                SessionEvent::ToolCall { call_id, tool_name, params, .. } => {
                    pending_tool_calls.push(ToolCall {
                        id: call_id.clone(),
                        call_type: "function".into(),
                        function: crate::llm::ToolCallFunction {
                            name: tool_name.clone(),
                            arguments: params.clone(),
                        },
                    });
                }
                SessionEvent::ToolResult { call_id, summary, tool_name, attachments, .. } => {
                    flush_assistant(
                        &mut out,
                        &mut text_buf,
                        &mut reasoning_buf,
                        &mut pending_tool_calls,
                        policy,
                    );
                    out.push(ChatMessage::tool_result(call_id.clone(), summary.clone()));
                    // 工具图的像素只在当前轮送达，消息本身必须重建。
                    if let Some(msg) = Self::tool_image_message(tool_name.as_deref(), attachments) {
                        out.push(msg);
                    }
                }
                // —— SkillUnload / CacheUnload 派生已注释（2026-08：卸载机制移除）──
                // 不参与派生的结构事件（turn/step 边界、request/header、
                // assistant/message 聚合、error、skill/activate、cache/*）
                _ => {}
            }
        }

        flush_assistant(
            &mut out,
            &mut text_buf,
            &mut reasoning_buf,
            &mut pending_tool_calls,
            policy,
        );
        out
    }
}

// —— flush_unloads 已注释（2026-08：卸载机制移除）──
// fn flush_unloads(out: &mut Vec<ChatMessage>, buf: &mut Vec<String>) {
//     if buf.is_empty() {
//         return;
//     }
//     let items = std::mem::take(buf).join("、");
//     out.push(ChatMessage::system(format!(
//         "（系统）上一轮结束，已自动卸载：{items}"
//     )));
// }

/// 工具定义列表是否等价（比较序列化形式，serde_json::Value 支持 PartialEq）。
fn tool_defs_eq(a: &[ToolDef], b: &[ToolDef]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b)
        .all(|(x, y)| serde_json::to_value(x).ok() == serde_json::to_value(y).ok())
}

/// 冲刷挂起的 assistant 消息。
///
/// 三条规则：
/// 1. 空内容 + 无工具调用 + 无推理 → 不产出（防御性，避免空消息进上下文）
/// 2. RequireToolCall 策略下，无工具调用的 assistant 消息丢弃推理链
/// 3. 工具调用归属：chunk 先于 tool/call 到达 → 文本/推理挂到带工具调用的消息上
fn flush_assistant(
    out: &mut Vec<ChatMessage>,
    text_buf: &mut String,
    reasoning_buf: &mut String,
    pending_tool_calls: &mut Vec<ToolCall>,
    policy: ReasoningPolicy,
) {
    let has_content = !text_buf.is_empty() || !reasoning_buf.is_empty();
    if !has_content && pending_tool_calls.is_empty() {
        return;
    }

    // 缺陷 1（RequireToolCall 死代码）：此处获得应用点——
    // 无工具调用的 assistant 消息在 RequireToolCall 策略下不携带推理链。
    // 推理缓冲为空时同样不附载（避免 Some("") 污染上下文）。
    let reasoning = if reasoning_buf.is_empty()
        || (pending_tool_calls.is_empty() && policy == ReasoningPolicy::RequireToolCall)
    {
        None
    } else {
        Some(reasoning_buf.clone())
    };

    let mut msg = ChatMessage {
        role: crate::llm::MessageRole::Assistant,
        content: std::mem::take(text_buf),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: reasoning,
        image_paths: None,
        images: None,
    };
    if !pending_tool_calls.is_empty() {
        msg.tool_calls = Some(std::mem::take(pending_tool_calls));
    }
    out.push(msg);
    reasoning_buf.clear();
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: &str, content: &str) -> FrontendMessage {
        FrontendMessage {
            role: role.into(),
            content: content.into(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            attachments: Vec::new(),
        }
    }

    fn make_tool_call(id: &str, name: &str, args: &str) -> crate::llm::ToolCall {
        crate::llm::ToolCall {
            id: id.into(),
            call_type: "function".into(),
            function: crate::llm::ToolCallFunction {
                name: name.into(),
                arguments: args.into(),
            },
        }
    }

    // ── 词汇表：序列化 ──

    #[test]
    fn session_event_serializes_camelcase_with_type_tags() {
        let json = serde_json::to_string(&SessionEvent::turn_start("s1")).unwrap();
        assert!(json.contains("\"type\":\"turn/start\""), "got {json}");
        assert!(json.contains("\"sessionId\":\"s1\""), "got {json}");

        let json = serde_json::to_string(&SessionEvent::tool_call("s1", "c1", "echo", "{}")).unwrap();
        assert!(json.contains("\"type\":\"tool/call\""), "got {json}");
        assert!(json.contains("\"callId\":\"c1\""), "got {json}");
        assert!(json.contains("\"toolName\":\"echo\""), "got {json}");
        // 空展示切片：序列化时省略 display 字段（旧数据兼容口径）
        assert!(!json.contains("display"), "got {json}");

        let json = serde_json::to_string(&SessionEvent::turn_end("s1", TurnEndReason::Stagnation)).unwrap();
        assert!(json.contains("\"type\":\"turn/end\""), "got {json}");
        assert!(json.contains("\"reason\":\"stagnation\""), "got {json}");

        let json = serde_json::to_string(&SessionEvent::assistant_chunk("s1", ChunkKind::Reasoning, "想")).unwrap();
        assert!(json.contains("\"type\":\"assistant/chunk\""), "got {json}");
        assert!(json.contains("\"kind\":\"reasoning\""), "got {json}");
    }

    #[test]
    fn tool_call_with_display_serializes_and_empty_is_omitted() {
        // 带文件链接展示切片：display 字段出现（kind 小写）
        let ev = SessionEvent::tool_call_with_display(
            "s1",
            "c1",
            "read_file",
            "{}",
            crate::tools::ToolDisplay::link(crate::tools::DisplayLink::file(
                "C:/ws/a.md",
                "a.md",
            )),
        );
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"display\""), "got {json}");
        assert!(json.contains("\"kind\":\"file\""), "got {json}");
        assert!(json.contains("\"target\":\"C:/ws/a.md\""), "got {json}");
        // 文本型展示：仅 text 字段
        let ev2 = SessionEvent::tool_call_with_display(
            "s1",
            "c2",
            "query_enemy",
            "{}",
            crate::tools::ToolDisplay::info("源石虫"),
        );
        let json2 = serde_json::to_string(&ev2).unwrap();
        assert!(json2.contains("\"display\":{\"text\":\"源石虫\"}"), "got {json2}");
    }

    #[test]
    fn turn_end_reason_vocabulary() {
        for (reason, expected) in [
            (TurnEndReason::Completed, "completed"),
            (TurnEndReason::Error, "error"),
            (TurnEndReason::Aborted, "aborted"),
            (TurnEndReason::Stagnation, "stagnation"),
            (TurnEndReason::MaxTokens, "max-tokens"),
        ] {
            let json = serde_json::to_string(&reason).unwrap();
            assert_eq!(json, format!("\"{expected}\""), "reason {reason:?} 序列化不符合词汇表");
        }
    }

    // ── 播种 + 派生：等价性（替代旧 convert_history 测试）──

    #[test]
    fn cache_events_serialize_with_type_tags() {
        // 缓存区与 log 分层（模块 7 P0）：cache_* 操作入 log 可回放
        let json = serde_json::to_string(&SessionEvent::cache_store(
            "s1",
            "buff.总结",
            CacheBucket::Static,
            Some("Buff机制基础"),
        ))
        .unwrap();
        assert!(json.contains("\"type\":\"cache/store\""), "got {json}");
        assert!(json.contains("\"bucket\":\"static\""), "got {json}");
        assert!(json.contains("\"sourceEntry\":\"Buff机制基础\""), "got {json}");

        let json = serde_json::to_string(&SessionEvent::cache_load(
            "s1", "k", CacheBucket::Dynamic, true,
        ))
        .unwrap();
        assert!(json.contains("\"type\":\"cache/load\""), "got {json}");
        assert!(json.contains("\"hit\":true"), "got {json}");
        assert!(json.contains("\"bucket\":\"dynamic\""), "got {json}");

        let json = serde_json::to_string(&SessionEvent::cache_remove(
            "s1", "k", CacheBucket::Dynamic, false,
        ))
        .unwrap();
        assert!(json.contains("\"type\":\"cache/remove\""), "got {json}");
        assert!(json.contains("\"existed\":false"), "got {json}");
    }

    #[test]
    fn skill_activate_serializes_with_type_tag_and_name() {
        // Skill 激活动作入 log 可回放：type 为 skill/activate、携带 name 字段
        let json = serde_json::to_string(&SessionEvent::skill_activate("s1", "推演分析")).unwrap();
        assert!(json.contains("\"type\":\"skill/activate\""), "got {json}");
        assert!(json.contains("\"name\":\"推演分析\""), "got {json}");
        assert!(json.contains("\"sessionId\":\"s1\""), "got {json}");
    }

    // —— 卸载相关测试已注释（2026-08：卸载机制移除）──
    // #[test]
    // fn unload_events_serialize_with_type_tags() {
    //     // 轮末卸载事实入 log 可回放：与 activate/cache/* 同风格
    //     let json = serde_json::to_string(&SessionEvent::skill_unload("s1", "伤害计算")).unwrap();
    //     assert!(json.contains("\"type\":\"skill/unload\""), "got {json}");
    //     assert!(json.contains("\"name\":\"伤害计算\""), "got {json}");
    //
    //     let json = serde_json::to_string(&SessionEvent::cache_unload("s1", "地图")).unwrap();
    //     assert!(json.contains("\"type\":\"cache/unload\""), "got {json}");
    //     assert!(json.contains("\"key\":\"地图\""), "got {json}");
    // }
    //
    // #[test]
    // fn derive_renders_unloads_as_system_message_before_next_user() {
    //     // 轮末卸载事件渲染为 LLM 可见的 system 消息：合并一条、位于
    //     // 上一轮 assistant 回复之后、下一轮 user 消息之前——LLM 从时序
    //     // 理解"激活/载入已随上一轮结束失效"
    //     let mut log = SessionLog::new();
    //     log.append(SessionEvent::user_message("s1", "算总伤"));
    //     log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Text, "计算结果如下"));
    //     log.append(SessionEvent::skill_unload("s1", "数值计算"));
    //     log.append(SessionEvent::cache_unload("s1", "地图"));
    //     log.append(SessionEvent::user_message("s1", "换个问题"));
    //
    //     let derived = log.derive_messages(ReasoningPolicy::Preserve);
    //     assert_eq!(derived.len(), 4, "user/assistant/系统说明/user，got: {derived:?}");
    //     assert_eq!(derived[0].role, crate::llm::MessageRole::User);
    //     assert_eq!(derived[1].role, crate::llm::MessageRole::Assistant);
    //     assert_eq!(derived[1].content, "计算结果如下");
    //     assert_eq!(derived[2].role, crate::llm::MessageRole::System);
    //     assert_eq!(
    //         derived[2].content,
    //         "（系统）上一轮结束，已自动卸载：Skill「数值计算」、动态缓存条目「地图」"
    //     );
    //     assert_eq!(derived[3].role, crate::llm::MessageRole::User);
    //     assert_eq!(derived[3].content, "换个问题");
    // }
    //
    // #[test]
    // fn derive_renders_only_skill_unload_when_no_cache() {
    //     // 只激活了 Skill、未载入动态缓存：卸载说明只含 Skill 项
    //     let mut log = SessionLog::new();
    //     log.append(SessionEvent::user_message("s1", "算总伤"));
    //     log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Text, "结果"));
    //     log.append(SessionEvent::skill_unload("s1", "数值计算"));
    //     log.append(SessionEvent::user_message("s1", "继续"));
    //
    //     let derived = log.derive_messages(ReasoningPolicy::Preserve);
    //     assert_eq!(
    //         derived[2].content,
    //         "（系统）上一轮结束，已自动卸载：Skill「数值计算」"
    //     );
    // }

    #[test]
    fn seed_and_derive_roundtrip_simple() {
        let history = vec![make_msg("user", "你好"), make_msg("assistant", "你好博士")];
        let mut log = SessionLog::new();
        log.seed_from_history("s1", history);
        let derived = log.derive_messages(ReasoningPolicy::Preserve);

        assert_eq!(derived.len(), 2);
        assert_eq!(derived[0].role, crate::llm::MessageRole::User);
        assert_eq!(derived[0].content, "你好");
        assert_eq!(derived[1].role, crate::llm::MessageRole::Assistant);
        assert_eq!(derived[1].content, "你好博士");
        assert!(derived[1].tool_calls.is_none());
    }

    /// 附件标记带落点路径：模型据此能自己读文件（形状即契约，改动需同步文档）。
    #[test]
    fn user_attachment_mark_carries_stored_path() {
        let att = crate::attachments::Attachment {
            path: "C:/data/session-files/c1/note_7.txt".into(),
            name: "note.txt".into(),
            mime_type: "text/plain".into(),
            size: 4,
        };
        let msg = SessionLog::derive_user_message("看这个", &[att]);
        assert!(
            msg.content.contains("[附件：note.txt，路径：C:/data/session-files/c1/note_7.txt]"),
            "got: {}",
            msg.content
        );
        assert!(msg.content.contains("看这个"), "原文应保留：{}", msg.content);
    }

    /// 工具结果的图片附件 → 派生时补一条带图 user 消息（跟轮可重建）。
    #[test]
    fn tool_result_attachments_derive_image_message() {
        use crate::attachments::Attachment;
        let mut log = SessionLog::new();
        log.append(SessionEvent::user_message("s1", "截个图看看"));
        log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Text, "好"));
        log.append(SessionEvent::tool_call(
            "s1",
            "call_1",
            "run_maa_task",
            "{}",
        ));
        log.append(SessionEvent::tool_result_with_attachments(
            "s1",
            "call_1",
            Some("run_maa_task"),
            "已截取当前画面：C:/x/latest.png",
            true,
            vec![Attachment {
                path: "C:/x/screenshot_1.png".into(),
                name: "screenshot_1.png".into(),
                mime_type: "image/png".into(),
                size: 10,
            }],
        ));

        let derived = log.derive_messages(ReasoningPolicy::Preserve);
        // [user, assistant(工具调用), 工具结果, 带图 user]
        assert_eq!(derived.len(), 4);
        assert_eq!(derived[2].role, crate::llm::MessageRole::Tool);
        assert_eq!(derived[3].role, crate::llm::MessageRole::User);
        assert_eq!(
            derived[3].image_paths,
            Some(vec!["C:/x/screenshot_1.png".to_string()]),
            "附图引用走跟轮重建"
        );
        assert_eq!(derived[3].content, "（工具 run_maa_task 返回的图片）");
    }

    #[test]
    fn seed_filters_error_messages() {
        let history = vec![
            make_msg("user", "你好"),
            FrontendMessage {
                role: "error".into(),
                content: "请求超时".into(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                attachments: Vec::new(),
            },
            make_msg("assistant", "你好博士"),
        ];
        let mut log = SessionLog::new();
        log.seed_from_history("s1", history);
        let derived = log.derive_messages(ReasoningPolicy::Preserve);
        assert_eq!(derived.len(), 2);
        // error 不产生任何事件
        assert_eq!(log.events().len(), 2);
    }

    #[test]
    fn seed_unknown_role_defaults_to_user() {
        let history = vec![make_msg("unknown", "test")];
        let mut log = SessionLog::new();
        log.seed_from_history("s1", history);
        let derived = log.derive_messages(ReasoningPolicy::Preserve);
        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0].role, crate::llm::MessageRole::User);
    }

    #[test]
    fn seed_empty_history() {
        let mut log = SessionLog::new();
        log.seed_from_history("s1", vec![]);
        assert!(log.derive_messages(ReasoningPolicy::Preserve).is_empty());
    }

    #[test]
    fn seed_passes_through_reasoning_and_tool_calls() {
        let tc = make_tool_call("call_1", "echo", "{}");
        let history = vec![
            make_msg("user", "你好"),
            FrontendMessage {
                role: "assistant".into(),
                content: String::new(),
                reasoning_content: Some("需要查数据".into()),
                tool_calls: Some(vec![tc]),
                tool_call_id: None,
                attachments: Vec::new(),
            },
            FrontendMessage {
                role: "tool".into(),
                content: "查询结果".into(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_1".into()),
                attachments: Vec::new(),
            },
        ];
        let mut log = SessionLog::new();
        log.seed_from_history("s1", history);
        let derived = log.derive_messages(ReasoningPolicy::Preserve);

        assert_eq!(derived.len(), 3);
        assert_eq!(derived[1].role, crate::llm::MessageRole::Assistant);
        assert_eq!(derived[1].reasoning_content.as_deref(), Some("需要查数据"));
        assert!(derived[1].tool_calls.is_some());
        assert_eq!(derived[2].role, crate::llm::MessageRole::Tool);
        assert_eq!(derived[2].tool_call_id.as_deref(), Some("call_1"));
    }

    // ── 派生：chunk 累积与冲刷 ──

    #[test]
    fn derive_accumulates_chunks_into_single_message() {
        let mut log = SessionLog::new();
        log.append(SessionEvent::user_message("s1", "你好"));
        log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Text, "第一段"));
        log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Text, "第二段"));
        log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Reasoning, "让我想想"));
        log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Text, "第三段"));

        let derived = log.derive_messages(ReasoningPolicy::Preserve);
        assert_eq!(derived.len(), 2);
        assert_eq!(derived[1].content, "第一段第二段第三段");
        assert_eq!(derived[1].reasoning_content.as_deref(), Some("让我想想"));
    }

    #[test]
    fn derive_merges_tool_round_into_one_assistant_message() {
        let mut log = SessionLog::new();
        log.append(SessionEvent::user_message("s1", "查敌人"));
        log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Reasoning, "需要工具"));
        log.append(SessionEvent::tool_call("s1", "c1", "query_enemy", r#"{"x":1}"#));
        log.append(SessionEvent::tool_call("s1", "c2", "query_enemy", r#"{"x":2}"#));
        log.append(SessionEvent::tool_result("s1", "c1", Some("query_enemy"), "结果1", true));
        log.append(SessionEvent::tool_result("s1", "c2", Some("query_enemy"), "结果2", true));
        log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Text, "最终回答"));

        let derived = log.derive_messages(ReasoningPolicy::Preserve);
        // user + assistant(带2个工具调用) + tool × 2 + assistant(最终)
        assert_eq!(derived.len(), 5);
        assert_eq!(derived[1].role, crate::llm::MessageRole::Assistant);
        let calls = derived[1].tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "c1");
        assert_eq!(calls[1].id, "c2");
        assert_eq!(derived[1].reasoning_content.as_deref(), Some("需要工具"));
        assert_eq!(derived[2].role, crate::llm::MessageRole::Tool);
        assert_eq!(derived[2].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(derived[3].role, crate::llm::MessageRole::Tool);
        assert_eq!(derived[4].content, "最终回答");
    }

    #[test]
    fn derive_skips_empty_assistant() {
        let mut log = SessionLog::new();
        log.append(SessionEvent::user_message("s1", "你好"));
        // 空 chunk 不应产生空 assistant 消息
        log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Text, ""));
        let derived = log.derive_messages(ReasoningPolicy::Preserve);
        assert_eq!(derived.len(), 1);
    }

    // ── 缺陷 1：RequireToolCall 应用点 ──

    #[test]
    fn require_tool_call_policy_drops_reasoning_without_tools() {
        let mut log = SessionLog::new();
        log.append(SessionEvent::user_message("s1", "你好"));
        log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Reasoning, "思考过程"));
        log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Text, "回答"));

        let derived = log.derive_messages(ReasoningPolicy::RequireToolCall);
        assert_eq!(derived[1].content, "回答");
        assert!(derived[1].reasoning_content.is_none(), "无工具轮次应丢弃推理链");
    }

    #[test]
    fn require_tool_call_policy_keeps_reasoning_with_tools() {
        let mut log = SessionLog::new();
        log.append(SessionEvent::user_message("s1", "查数据"));
        log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Reasoning, "需要工具"));
        log.append(SessionEvent::tool_call("s1", "c1", "echo", "{}"));
        log.append(SessionEvent::tool_result("s1", "c1", Some("echo"), "结果", true));

        let derived = log.derive_messages(ReasoningPolicy::RequireToolCall);
        assert_eq!(derived[1].role, crate::llm::MessageRole::Assistant);
        assert!(derived[1].tool_calls.is_some());
        assert_eq!(derived[1].reasoning_content.as_deref(), Some("需要工具"));
    }

    #[test]
    fn preserve_policy_keeps_reasoning_always() {
        let mut log = SessionLog::new();
        log.append(SessionEvent::user_message("s1", "你好"));
        log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Reasoning, "思考过程"));
        log.append(SessionEvent::assistant_chunk("s1", ChunkKind::Text, "回答"));

        let derived = log.derive_messages(ReasoningPolicy::Preserve);
        assert_eq!(derived[1].reasoning_content.as_deref(), Some("思考过程"));
    }

    // ── request/header：仅变化时追加 ──

    #[test]
    fn push_header_dedups_identical_snapshots() {
        let mut log = SessionLog::new();
        let defs = vec![ToolDef::new("echo", "回显", serde_json::json!({"type": "object"}))];
        log.push_header("s1", "system prompt", &defs);
        log.push_header("s1", "system prompt", &defs);
        assert_eq!(log.events().len(), 1);

        // system prompt 变化 → 追加
        log.push_header("s1", "changed prompt", &defs);
        assert_eq!(log.events().len(), 2);

        // 工具 schema 变化 → 追加
        let other_defs = vec![ToolDef::new("echo", "改了描述", serde_json::json!({"type": "object"}))];
        log.push_header("s1", "changed prompt", &other_defs);
        assert_eq!(log.events().len(), 3);
    }

    // ── log 不变量 ──

    #[test]
    fn len_is_next_seq() {
        let mut log = SessionLog::new();
        assert_eq!(log.len(), 0);
        log.append(SessionEvent::turn_start("s1"));
        assert_eq!(log.len(), 1);
        log.append(SessionEvent::user_message("s1", "你好"));
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn log_is_append_only() {
        // events 字段私有且无修改接口——编译期保证；
        // 这里验证顺序保持与 seq 连续性
        let mut log = SessionLog::new();
        log.append(SessionEvent::turn_start("s1"));
        log.append(SessionEvent::user_message("s1", "你好"));
        log.append(SessionEvent::turn_end("s1", TurnEndReason::Completed));
        let events = log.events();
        assert!(matches!(events[0], SessionEvent::TurnStart { .. }));
        assert!(matches!(events[1], SessionEvent::UserMessage { .. }));
        assert!(matches!(events[2], SessionEvent::TurnEnd { .. }));
    }
}
