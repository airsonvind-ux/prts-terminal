// PRTS 终端 · Agent 事件与 IPC 事件合约

import type { Attachment } from "./chat";
import type { Mood, MoodIntensity } from "../types";
import type { ExecutionMode } from "./settings";
import type { TokenUsage, UsageStat } from "./llm";

// ── Agent 事件（前后端 IPC 合约，架构升级 · 模块 1） ──

/**
 * 后端推送的结构化事件（Discriminated Union）。
 *
 * 事件拆两平面（与 Rust 侧 session.rs SessionEvent + events.rs AgentEvent 对齐）：
 *   - 持久事实（SessionEvent）：turn/start|end、step/start|end、user/message、
 *     assistant/chunk、assistant/message、tool/call、tool/result、request/header、error
 *   - 瞬时 UI 信号（仅 phase）
 *
 * 所有事件均携带 sessionId —— 对应 invoke agent_process 时前端传入的会话标识。
 * 事件路由不依赖全局状态：useAgentEvents 读 event.sessionId 直接写入
 * processing[event.sessionId]，与当前活跃对话完全解耦。
 *
 * 旧词汇（reasoning_delta/reasoning_done/text_delta/generation_done/tool_call/
 * tool_result）已吸收进 assistant/chunk 等事件：推理与文本统一为
 * assistant/chunk（kind 区分），轮次结束由 turn/end（显式 reason）表达，
 * 最终归档由 assistant/message（含 content + reasoning + usage 快照）承载。
 */
export type AgentEvent =
  | SessionEvent
  | PhaseEvent
  | ConversationTitleEvent
  | ConversationSnapshotStaleEvent
  | MemoryUsageEvent;

/** 持久事实（后端 session log 的事件镜像，前端投影缓存消费） */
export type SessionEvent =
  | TurnStartEvent
  | TurnEndEvent
  | StepStartEvent
  | StepEndEvent
  | UserMessageEvent
  | AssistantChunkEvent
  | AssistantMessageEvent
  | ToolCallEvent
  | ToolResultEvent
  | ToolApprovalRequestEvent
  | ToolApprovalResultEvent
  | MoodEvent
  | RequestHeaderEvent
  | SessionErrorEvent
  | CacheStoreEvent
  | CacheLoadEvent
  | CacheRemoveEvent
  // M3-B：轮中止标记（前端私有事件，非后端事件；mood/event 同款先例——
  // derive/展示层不派生内容，仅 MessageList 轮尾展示 + 存储透传）
  | TurnAbortedEvent
  // R-07：轮截断标记（前端私有事件，同 aborted 模式）
  | TurnTruncatedEvent;

/**
 * 轮中止标记（M3-B §4.1 前端私有事件）。
 *
 * 非后端事件：后端词汇表不对称（mood/event 先例）。由 useProcessingStore
 * 在 finishProcessing(sessionId, "aborted") 归档前合成追加到 currentEvents 末尾，
 * 随轮落库（append_turn events_json 原样写盘 → 重启可见）。
 *
 * 消费方约定：deriveMessages / deriveTextSegments / isMetaEvent 全部忽略它
 * （不进 LLM 历史、不参与展示派生）；MessageList appendTurnItems 渲染正常
 * 内容后，检测到本标记则追加轮尾标记条（“该轮已中止”，灰/斜体小字）。
 */
export interface TurnAbortedEvent {
  type: "turn/aborted";
  sessionId: string;
}

/**
 * 轮截断标记（R-07 前端私有事件）。
 *
 * 由 finishProcessing(sessionId, "max-tokens") 归档前合成；primary 模式下
 * hydration 从 turn/end(max-tokens) 重建。消息列表在轮尾展示
 * “回答未完整（达到长度上限）”，不进入模型历史。
 */
export interface TurnTruncatedEvent {
  type: "turn/truncated";
  sessionId: string;
}

/** 增量片段类型 */
export type ChunkKind = "text" | "reasoning";

/** turn 终止原因（与 Rust TurnEndReason 对齐） */
export type TurnEndReason =
  | "completed"
  | "error"
  | "aborted"
  | "stagnation"
  | "max-tokens";

/** turn 开始：一次 agent_process 处理周期 */
export interface TurnStartEvent {
  type: "turn/start";
  sessionId: string;
}

/** turn 结束：显式终止原因（completed 到达时前端归档本轮） */
export interface TurnEndEvent {
  type: "turn/end";
  sessionId: string;
  reason: TurnEndReason;
  /** 本轮累计用量（统计增量；后端计算，前端随 append_turn 提交） */
  usage?: UsageStat | null;
}

/** step 开始：一次 LLM 请求 + 其调用的工具。estimatedTokens 为请求前估算（双锚兜底） */
export interface StepStartEvent {
  type: "step/start";
  sessionId: string;
  step: number;
  estimatedTokens: number;
}

/** step 结束：本次调用的 token 用量（最后一次即最终上下文状态；前端用于实时圆环） */
export interface StepEndEvent {
  type: "step/end";
  sessionId: string;
  step: number;
  usage: TokenUsage | null;
  elapsedMs: number;
}

/** 用户消息 */
export interface UserMessageEvent {
  type: "user/message";
  sessionId: string;
  content: string;
  /** 附件引用（有附件时携带；无附件/旧数据缺省） */
  attachments?: Attachment[];
}

/** 流式增量片段（token 级回放；推理与文本分道） */
export interface AssistantChunkEvent {
  type: "assistant/chunk";
  sessionId: string;
  kind: ChunkKind;
  /** 展示文本（情绪标签已剥离；流式半标签期间可为空串）。 */
  content: string;
  /** 模型原始文本（含情绪标签）；历史派生用，旧事件缺省。 */
  rawContent?: string;
}

/** 最终 assistant 消息聚合（归档依据：content + reasoning + usage 快照） */
export interface AssistantMessageEvent {
  type: "assistant/message";
  sessionId: string;
  /** 展示文本（情绪标签已剥离）。 */
  content: string;
  /** 模型原始文本（含情绪标签）；历史派生用，旧事件缺省。 */
  rawContent?: string;
  reasoningContent: string | null;
  /** 最后一次 LLM 调用的 token 快照（缺陷 4 口径：最终上下文状态） */
  usage: TokenUsage | null;
}

/** 工具调用请求（callId 用于与 tool/result 配对） */
export interface ToolCallEvent {
  type: "tool/call";
  sessionId: string;
  callId: string;
  toolName: string;
  /** 工具参数（JSON string），匹配 LLM function calling 的 arguments 格式 */
  params: string;
  /**
   * 展示切片（后端在发出本事件前解析）：卡片短语后紧随 text + link。
   * 旧归档/无信息工具无此字段 → 按缺省处理（不附加任何信息）。
   */
  display?: ToolDisplay;
}

/** 展示切片链接类型：file（open_path 打开）/ url（系统浏览器打开） */
export type ToolDisplayLinkKind = "file" | "url";

/** 展示切片链接（后端解析后的绝对路径 / URL；前端不拼路径） */
export interface ToolDisplayLink {
  kind: ToolDisplayLinkKind;
  /** file = 绝对路径；url = 完整 URL */
  target: string;
  /** 显示标签（文件名 / URL） */
  label: string;
}

/** 工具调用展示切片（文本信息 + 可选链接 + 可选完整原文） */
export interface ToolDisplay {
  text?: string;
  link?: ToolDisplayLink;
  /**
   * 完整原文（后端仅在展示被截断时给出）。截断是视觉约束，悬停时用它显示全文，
   * 保证用户始终有看到完整内容的渠道。
   */
  full?: string;
}

/** 情绪事件（批 2-D 情绪卡片）：标签匹配时由前端投影层产出，记录情绪及强度档位 */
export interface MoodEvent {
  type: "mood/event";
  sessionId: string;
  mood: Exclude<Mood, "平静">;
  intensity: MoodIntensity;
}

/** 工具结果来源分类：external 表示外部资料，不表达内容真假或敌意 */
export type ToolTrust = "authoritative" | "derived" | "external";

/** 工具结果来源（R-02a；旧归档缺省） */
export interface ToolProvenance {
  trust: ToolTrust;
  /** 来源标识：URL 的 host+path、数据源名等 */
  source: string;
  version?: string;
}

/** 工具执行结果。toolName 为 null 表示历史播种的旧数据 */
export interface ToolResultEvent {
  type: "tool/result";
  sessionId: string;
  callId: string;
  toolName: string | null;
  /** 结果摘要，前端直接展示 */
  summary: string;
  /** 是否执行成功（失败内容同样回传 LLM，由模型决定下一步） */
  ok: boolean;
  /** 结果来源；旧归档/未声明工具无此字段 */
  provenance?: ToolProvenance;
}

/** 工具审批请求（M3-A §5.1 / M4）：run_command 询问用户确认时后端推送 */
export interface ToolApprovalRequestEvent {
  type: "tool/approval-request";
  sessionId: string;
  callId: string;
  /** 完整命令（≤512 字符，后端已截断） */
  command: string;
  /** 危险度：crit 不产生 request（仅 low/high 可达；类型保留三档对齐后端） */
  risk: "low" | "high" | "crit";
  mode: ExecutionMode;
  /** 出卡族 reason 码（地域性信任门）：确认卡类别短语映射键。
   *  null = 无门信息（前端不渲染类别行）；crit 不产生 request 故不会出现 crit-* */
  ruleId: string | null;
  /** 请求身份（M4）：restricted=默认受限 / full=完整权限（应用自身令牌） */
  privilege: "restricted" | "full";
  /** full 档的一句话理由（确认卡展示；restricted 档为 null） */
  justification: string | null;
}

/** 工具审批结果（M3-A §5.1 / M4）：询问路径 emit（IPC）；auto-approved/blocked 仅 log 不推 IPC */
export interface ToolApprovalResultEvent {
  type: "tool/approval-result";
  sessionId: string;
  callId: string;
  decision: "approved" | "rejected" | "auto-approved" | "blocked";
  risk: "low" | "high" | "crit";
  mode: ExecutionMode;
  ruleId: string | null;
  reason: "user" | "timeout" | "drop" | null;
  /** 请求身份（M4；与 request 配对） */
  privilege: "restricted" | "full";
}

/** 请求头快照（渲染后的 system prompt + 工具 schema；前端不消费） */
export interface RequestHeaderEvent {
  type: "request/header";
  sessionId: string;
  systemPrompt: string;
  toolDefs: unknown[];
}

/** 错误事件。recoverable=true 时 Agent 继续处理，false 时处理中断 */
export interface SessionErrorEvent {
  type: "error";
  sessionId: string;
  message: string;
  recoverable: boolean;
}

/**
 * 缓存桶判别（模块 7：静态桶常驻提醒 / 动态桶会话级）。
 * 缓存区与 log 分层：CacheStore 独立于 log，cache_* 操作入 log 可回放。
 */
export type CacheBucket = "static" | "dynamic";

/** 缓存区写操作（第三轮 cache_store 工具接线后推送） */
export interface CacheStoreEvent {
  type: "cache/store";
  sessionId: string;
  key: string;
  bucket: CacheBucket;
  /** 静态桶的来源知识条目名（None = 动态桶） */
  sourceEntry: string | null;
}

/** 缓存区读取操作（注入动作入 log） */
export interface CacheLoadEvent {
  type: "cache/load";
  sessionId: string;
  key: string;
  bucket: CacheBucket;
  hit: boolean;
}

/** 缓存区移除操作 */
export interface CacheRemoveEvent {
  type: "cache/remove";
  sessionId: string;
  key: string;
  bucket: CacheBucket;
  existed: boolean;
}

/**
 * 模式切换：Agent 处理流程的当前模式转换（瞬时 UI 信号，不入 log）。
 *
 * phase 不承诺线性推进。reasoning 和 executing 可能反复交替
 *（LLM 思考 → 调工具 → 基于结果再思考 → 再调工具），
 * 直到信息足够后进入 generating 输出最终回答。
 *
 * 映射关系：
 *   reasoning  → UI 状态保持 processing，消息区渲染思维链面板
 *   executing  → UI 状态保持 processing，消息区渲染工具卡片
 *   generating → agentPhase 变为 streaming，deriveUIState 返回 "streaming"
 */
export interface PhaseEvent {
  type: "phase";
  sessionId: string;
  /** reasoning = 思考中 | executing = 调用工具中 | generating = 生成最终回答 */
  phase: "reasoning" | "executing" | "generating";
}

/**
 * 会话标题更新事件（后端生成标题后推送；非 session log 事实）。
 *
 * 与 Rust `events.rs::AgentEvent::ConversationTitle`（serde
 * `type="conversation/title"`, camelCase）对齐。携带 conversationId 而非
 * sessionId——turn 快照在轮末删除，sessionId 无法路由；标题可能在轮末之后到达。
 */
export interface ConversationTitleEvent {
  type: "conversation/title";
  conversationId: string;
  title: string;
  /** 本次标题生成用量（未归档时前端携带至下次 append_turn） */
  usage?: UsageStat | null;
}

/**
 * 旧对话冻结前缀与当前版本不一致（R-08；非 session log 事实）。
 * 不改变本轮前缀，只让用户知道这个对话仍基于创建时的工具/技能快照。
 */
export interface ConversationSnapshotStaleEvent {
  type: "conversation/snapshot-stale";
  conversationId: string;
  /** 快照创建时的应用版本（旧快照可能为空串） */
  appVersion: string;
}

/**
 * 记忆管线用量事件（后端内部累加后推送；前端仅镜像进投影用于展示，不回写）。
 * 与 conversation/title 同属旁路信号平面。
 */
export interface MemoryUsageEvent {
  type: "memory/usage";
  conversationId: string;
  usage: UsageStat;
}
