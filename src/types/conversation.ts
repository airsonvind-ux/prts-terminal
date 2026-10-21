// PRTS 终端 · IPC 请求、对话持久化与记忆页类型

import type { ChatMessage } from "./chat";
import type { SessionEvent } from "./events";
import type { ConversationUsage, TokenUsage, UsageStat } from "./llm";

// ── IPC 请求契约 ──

/**
 * agent_process invoke 的请求体。
 * Rust 侧根据此结构接收参数，适配层负责在 LLM API 调用前组装消息。
 */
export interface AgentRequest {
  /** 用户原始输入（纯文本） */
  input: string;
  /** 完整对话历史（含本次 user 消息），用于播种后端 session log */
  history: ChatMessage[];
  /** 本次调用的唯一会话标识，随所有事件回传以路由事件 */
  sessionId: string;
  /**
   * 对话标识（模块 C：后端缓存区按此分区，跨轮持久）。
   * 可选：与后端 serde default 对齐——旧后端不读此字段，
   * 后端缺失时退化为 sessionId 兜底。
   */
  conversationId?: string;
}

// ── 对话数据（投影缓存） ──

// ── 对话持久化（Phase 4）──

/**
 * 压缩记录（模块 D · 上下文衰减）——会话级元数据扩展。
 *
 * 语义：压缩 = 一次性的对话起点前移。turns 全量保留（回看资产，永不回灌
 * LLM）；`retainedFrom` = 压缩后保留段第一轮在 turns 的下标，派生层从该处
 * 截断并前置摘要消息；`summary` 为 LLM checkpoint 正文（仅存最新一份，
 * 重压缩覆盖）；`tokensBefore` 为折叠区压缩前估算（信息用）。
 */
export interface CompactionMeta {
  /** 保留段第一轮在 turns 的下标 */
  retainedFrom: number;
  /** 结构化 checkpoint 摘要正文（不含定界块——壳由派生层合成） */
  summary: string;
  /** 折叠区压缩前估算 token */
  tokensBefore: number | null;
  /** 压缩发生时间（unix 毫秒） */
  at: number;
}

/** compact_conversation 成功载荷（与后端 CompactionOutcome serde 对齐） */
export interface CompactOutcome {
  summary: string;
  /** true = 输入超窗规则降级产出（无 LLM 调用） */
  degraded: boolean;
  tokensBefore: number | null;
  /** 本次摘要 LLM 调用用量（降级产出时 null） */
  usage: UsageStat | null;
}

/**
 * 会话持久化元数据（后端 index.json 条目 = list_conversations 载荷）。
 * 与后端 ConversationMeta serde（camelCase）对齐；可缺省字段以 null 表达，
 * 恢复（hydrate）时转 undefined 适配 Conversation。
 * updatedAt 供前端恢复 hook 做导航决策（选最近活跃会话为 activeId）。
 */
export interface ConversationMeta {
  id: string;
  title: string;
  /** 创建时间（unix 毫秒） */
  createdAt: number;
  /** 最近一次轮提交/元数据更新时间（unix 毫秒） */
  updatedAt: number;
  modelName: string | null;
  contextLength: number | null;
  contextTokens: TokenUsage | null;
  lastNoticeSeq: number | null;
  /** 压缩记录（模块 D；null = 未压缩过；旧存档缺字段兼容） */
  compaction: CompactionMeta | null;
  /** 用量统计（Phase 5；null / 缺省 = 从未记录） */
  usage?: ConversationUsage | null;
}

/**
 * get_conversation 载荷：meta + 轮行原文数组（每行 = 该轮 SessionEvent 数组
 * JSON，与前端归档序列化逐字节一致）。恢复时逐行 JSON.parse 重建 turns。
 */
export interface ConversationRecord {
  meta: ConversationMeta;
  turns: string[];
}

/**
 * 提醒数据（后端 get_reminder_data 返回）。
 * - `dims`：环境维度覆写式快照（value 空串的维度后端不下发）；stamp = 最近变更序号。
 * - `events`：记忆事件（有界，容量 200）；`seq` = 已分配的最大序号。
 */
export interface ReminderDim {
  key: string;
  title: string;
  /** 类别：`"value"`（值维度，入首轮快照）／`"catalog"`（目录维度，不入）。 */
  kind: "value" | "catalog";
  value: string;
  note: string;
  stamp: number;
}

/** 记忆事件 */
export interface ReminderEvent {
  seq: number;
  title: string;
  detail: string;
  ts: string;
}

/** get_reminder_data 载荷 */
export interface ReminderData {
  seq: number;
  dims: ReminderDim[];
  events: ReminderEvent[];
}

// ── 记忆页（设置页「记忆」子页 · Phase 4 任务二）──

/** 成长页元数据（overview） */
export interface MemoryGrowthMeta {
  name: string;
  brief: string;
  tags: string[];
  updatedAt: number;
  isProfile: boolean;
}

/** 「近期」条目（与提示词渲染同源） */
export interface MemoryRecentItem {
  date: string;
  title: string;
  summary: string;
}

/** memory_overview 载荷 */
export interface MemoryOverview {
  growth: MemoryGrowthMeta[];
  recent: MemoryRecentItem[];
  previous: string;
  experienceCount: number;
}

/** 成长页详情（memory_get growth） */
export interface MemoryGrowthPage {
  name: string;
  brief: string;
  body: string;
  tags: string[];
  source: string;
  createdAt: number;
  updatedAt: number;
  isProfile: boolean;
}

/** 经历条目（list / get）。无正文层：状态句＋标签＋来源。 */
export interface MemoryExperienceItem {
  id: string;
  date: string;
  title: string;
  summary: string;
  tags: string[];
  absorbed: boolean;
  /** 未收纳且重写达封存线（列表小标用） */
  sealed: boolean;
  /** 来源：对话 id ＋ 轮次范围 */
  conversationId: string;
  turnFrom: number;
  turnTo: number;
}

/** memory_get 返回（kind 判别） */
export interface MemoryDetail {
  kind: "growth" | "experience";
  page?: MemoryGrowthPage | null;
  entry?: MemoryExperienceItem | null;
}

/** memory_set_longterm 返回 */
export interface MemorySetLongtermResult {
  version: number;
  chars: number;
  /** 是否有实际改动（false＝原文逐字相同：未写盘、未推进版本、未推提醒） */
  changed: boolean;
}

/** memory_tidy 返回 */
export interface MemoryTidyResult {
  status: "created" | "rewritten" | "compressed" | "no_delta" | "cooldown" | "failed";
  message: string;
  created: number;
  rewritten: number;
  compressed: boolean;
}

/**
 * 一个完整的对话记录（前端 = 后端 session log 的投影缓存）。
 *
 * `turns` 每轮一组 SessionEvent 序列（user/message 由前端合成置于轮首），
 * 消息气泡与事件卡片均由投影函数（deriveMessages / buildDisplayItems）从
 * turns 派生，不再单独维护 messages 数组。
 */
export interface Conversation {
  id: string;
  title: string;
  /**
   * 真标题（后端生成 / 用户改名）是否已落地。
   *
   * `createConversation` 时 title 是"首条消息截断"出来的占位标题 → false；
   * 收到 conversation/title 事件或用户改名后 → true。历史数据（水合）没有这个
   * 字段 = undefined，视为已落地（它们的标题本来就来自盘上）。
   *
   * 用途：引导的"对话标题"那条要等真标题落地才出——否则会先框住占位标题，
   * 等真标题到了再框一次，用户看到闪烁。
   */
  titleGenerated?: boolean;
  /** 每轮的 SessionEvent 序列（投影缓存——后端 log 的事件镜像） */
  turns: SessionEvent[][];
  createdAt: number;
  /** 创建对话时使用的模型名（ContextIndicator 展示） */
  modelName?: string;
  /** 创建对话时模型的上下文窗口大小（Token 数） */
  contextLength?: number;
  /** 最近一轮 assistant/message 的上下文 Token 用量快照（覆盖式更新） */
  contextTokens?: TokenUsage;
  /**
   * 当前轮实时上下文用量（仅内存，不落盘）。
   * step/end 收到 provider usage 时写入；turn/end、失败、压缩时清空，
   * 圆环回退到 contextTokens。
   */
  liveContextTokens?: TokenUsage;
  /**
   * 压缩记录（模块 D；undefined = 未压缩过）。压缩后 contextTokens 被置空
   * （进度环归零）——下一次真实轮 usage 自然覆盖。
   */
  compaction?: CompactionMeta;
  /**
   * 环境通知已告知游标（M1 Reminder）：上次拼装进本对话历史的通知最大 seq。
   * sendMessage 时经 advanceNoticeSeq action 推进（拼装即防重复；invoke
   * 失败回滚）。游标为会话元数据的一部分——对话持久化落地时随会话落盘，
   * 接口不变（读写收敛于 ConversationStore action）。缺省 = 0：下轮全量告知。
   * 压缩成功时经 applyCompaction 复位（批 4：下一条消息重发三值快照）。
   */
  lastNoticeSeq?: number;
  /** 用量统计（Phase 5）：按四类累计，随轮 / 压缩 / 记忆 / 标题更新。 */
  usage?: ConversationUsage;
}
