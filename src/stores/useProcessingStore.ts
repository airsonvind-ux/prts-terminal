// useProcessingStore · Agent 请求处理生命周期（架构升级 · 模块 1）
//
// 管理 processing 快照字典 + sendMessage 完整生命周期。
// 被 useAgentEvents Hook 驱动。
// 依赖 useConversationStore（轮次归档）和 useUIStore（错误记录）。
// uiState 不在此管理——它是 deriveUIState() 从原始事实计算的派生值。
//
// 投影缓存语义：processing[sessionId].currentEvents 是本轮
// SessionEvent 的实时投影（user/message 由前端合成预置为第一条），
// turn/end 到达时整轮归档进 Conversation.turns。

import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import type {
  AssistantChunkEvent,
  ChatMessage,
  SessionEvent,
  TokenUsage,
  TurnEndReason,
  UserMessageEvent,
  AgentRequest,
  ReminderData,
  Mood,
  MoodIntensity,
  UsageDelta,
  UsageStat,
  Attachment,
} from "../types";
import {
  scanTextChunk,
  stripTagFromText,
  rearmScanner,
  releasePending,
  segmentConsumed,
} from "../moodTag";
import { generateId } from "../utils";
import {
  deriveActiveHistory,
  buildReminderBlock,
  stripReminderBlock,
  defaultTitleFromInput,
  willNextTurnOverflow,
  type ReminderLine,
} from "../projection";
import { useUIStore } from "./useUIStore";
import { useConversationStore } from "./useConversationStore";
import { useCompactionStore } from "./useCompactionStore";
import { showToast } from "./useToastStore";

/** 首轮未归档时暂存的标题用量（随下一次 appendTurn 提交）。 */
const pendingTitleUsage = new Map<string, UsageDelta>();

/** 暂存标题用量（供 useAgentEvents 在会话未归档时调用）。 */
export function stashTitleUsage(conversationId: string, delta: UsageDelta): void {
  pendingTitleUsage.set(conversationId, delta);
}

// ── 全局模型信息缓存 ──
//
// 模块级 ref：SettingsProvider 在用户选模型时写入，sendMessage 创建对话时读取。
// 不在任何 store 中——模型信息本质是全局配置的快照，不需要响应式订阅。
// 组件通过 getModelInfo() / setModelInfo() 读写，避免直接操作 ref 内部结构。

interface ModelInfoCache {
  name: string;
  contextLength: number;
  /** 当前模型是否支持图像输入（未知/未收录 → undefined，按不支持处理） */
  supportsVision?: boolean;
  /**
   * 生效思考档的展示文案（如 `中（默认）`；模型不支持推理 → null/undefined）。
   * **算好的值，不是存储偏好**——由写入点（App 启动 / 模型页选中 / 改档位）
   * 用 `thinking.ts` 派生后写入（悬停信息读的是快照，不能在渲染时现算）。
   */
  thinking?: string | null;
}

const modelInfoRef: { current: ModelInfoCache | null } = { current: null };

/** SettingsProvider 在用户选定模型时调用，写入缓存 */
export function setModelInfo(info: ModelInfoCache | null): void {
  modelInfoRef.current = info;
}

/** 读取缓存的模型信息 */
export function getModelInfo(): ModelInfoCache | null {
  return modelInfoRef.current;
}

/**
 * 只更新缓存的「生效思考档」文案（模型没变、档位变了时用；名字/窗口不动）。
 * 缓存里还没模型信息（未配置）→ 无操作。
 */
export function setModelThinking(label: string | null): void {
  if (modelInfoRef.current) modelInfoRef.current.thinking = label;
}

// ── 类型 ──

/**
 * 单个 sendMessage 的处理中快照。
 * key = sessionId（前端生成的本次请求唯一标识，非 conversationId）。
 * 仅在 Agent 处理期间存在于 processing 字典中，处理完成后删除。
 */
interface ProcessingState {
  /** 反向映射：该 session 属于哪个 Conversation。用于归档时找到正确的对话 */
  conversationId: string;
  /**
   * 本轮全部事件的时序列表（投影缓存进行态）。
   * 第一条由 sendMessage 合成 user/message，其后为后端推送的持久事实。
   * turn/end 到达时整轮归档到 Conversation.turns。
   */
  currentEvents: SessionEvent[];
  /**
   * Agent 当前认知模式。由 onPhase 根据后端推送的 Phase 事件更新。
   * "reasoning"  → LLM 思考中（思维链），UI 渲染思维链面板
   * "processing" → 调工具或等工具返回，UI 渲染工具卡片
   * "streaming"  → 生成最终回答，UI 打字机输出。此模式下 deriveUIState 返回 "streaming"
   */
  agentPhase: "reasoning" | "processing" | "streaming";
  /**
   * 中止已发出、轮未收尾（2026-10-08）：按钮与输入框据此给出「正在中止」反馈。
   * 乐观置位（点击即置），后端报告没有活动轮时撤回；随快照删除自动清除。
   */
  aborting?: boolean;
  /**
   * 情绪标签状态（批 2-D · 情绪通道）。
   * sendMessage 建快照默认 "平静"（省略）；标签匹配成功时 set mood。
   * 快照删除（finishProcessing / failProcessing）时随快照自动清空，无显式清理动作。
   */
  mood: Mood;
  /** 情绪强度档位（0 = 基础；1~3 = 加深），与 mood 同生命周期 */
  moodIntensity: MoodIntensity;
}

interface ProcessingStore {
  processing: Record<string, ProcessingState>;

  /**
   * 发送消息。一次调用 = 一个独立的处理生命周期。
   *
   * 1. 并发保护：同一对话已有处理中请求 → 忽略本次
   * 2. 生成 sessionId，创建 processing[本次sessionId] 快照
   *    （currentEvents 预置合成 user/message 事件）
   * 3. 新建对话（title 从输入派生）或复用活跃对话
   * 4. invoke("agent_process") 触发后端，不等结果（后续事件由 useAgentEvents Hook 异步接收）
   *
   * sessionId 的作用：同时作为 processing 字典的 key、传给后端的标识、
   * 以及后端回传事件的路由 key。不是 conversationId——后者标识对话，
   * 但一个对话可以有多轮 sendMessage，每轮需要独立的处理快照。
   */
  sendMessage: (input: string, attachments?: Attachment[]) => Promise<void>;

  /**
   * 追加事件到 processing[event.sessionId].currentEvents。
   * 使用 event.sessionId 做 key（不是 conversationId），因为后端事件只携带 sessionId。
   */
  appendCurrentEvent: (event: SessionEvent) => void;

  /**
   * 更新 Agent 认知模式。收到 Phase 事件时调用。
   *
   * PhaseEvent.phase（后端推送的值） → ProcessingState.agentPhase（内部值）：
   *   "reasoning"  → "reasoning"    LLM 正在思考，UI 显示思维链
   *   "executing"  → "processing"   LLM 正在调工具，UI 显示工具卡片
   *   "generating" → "streaming"    LLM 正在生成最终文本，UI 打字机输出
   *
   * reasoning 和 executing 可反复交替（LLM 思考→调工具→再思考→再调工具），
   * onPhase 每次收到事件只覆盖当前 agentPhase 值，不维护历史。
   * 此交替过程全部在 processing 内部——UI 状态始终为 "processing"，不触发状态转移。
   */
  onPhase: (event: {
    sessionId: string;
    phase: "reasoning" | "executing" | "generating";
  }) => void;

  /**
   * 归档处理结果。turn/end 事件到达时由 useAgentEvents Hook 调用。
   *
   * 1. currentEvents（含合成 user/message 与后端持久事实）→ Conversation.turns 整轮归档
   * 2. 上下文 Token 用量：从最后一个 assistant/message 的 usage 快照读取（覆盖式）
   * 3. 从 processing 字典删除该条目
   *
   * 快照不存在（failProcessing 已清理，如停滞路径 error 先到）时幂等返回。
   * 删除后 processing 字典变化 → deriveUIState 自动返回 "ready"。
   *
   * M3-B：
   * - `reason === "aborted"` → 归档前在 currentEvents 末尾合成前端私有
   *   turn/aborted 标记（消息列表轮尾展示 + 随轮落库持久化）；
   *   其余逻辑（usage 扫描/快照删除/contextTokens 不覆盖）与普通轮一致——
   *   aborted 轮无 assistant/message → usage null → 不覆盖对话 contextTokens。
   */
  finishProcessing: (sessionId: string, reason?: TurnEndReason, turnUsage?: UsageStat | null) => void;

  /**
   * 处理失败。两种触发路径：
   * - invoke 本身抛异常（后端不可达等） → sendMessage 的 .catch 调用
   * - 后端推送 error 事件（recoverable=false） → useAgentEvents Hook 调用
   *
   * 1. 从 processing 字典删除该 session 的快照
   * 2. 错误轮归档进对话时间线（[user/message, error] 一组）
   * 3. 若错误所属对话为当前活跃对话 → 写入 UIStore.errorMsg
   *    → deriveUIState 读到非 null → 返回 "error"
   */
  failProcessing: (sessionId: string, errorMsg: string) => void;

  /**
   * 查询指定对话是否有处理中快照。
   * 供 switchConversation 协调使用：存在 → 从 agentPhase 恢复 UI 状态。
   * 遍历 processing 字典的 O(n) 操作，n = 并发处理中的对话数（正常 ≤3）。
   */
  getProcessingForConversation: (conversationId: string) => ProcessingState | undefined;

  /**
   * 按对话取处理中快照的 sessionId（M3-B 中止按钮取参用）。
   * undefined = 该对话无处理中快照（边界：快照在点击瞬间已消失 → 无操作）。
   * 遍历 processing 字典的 O(n) 操作（同 getProcessingForConversation）。
   */
  getProcessingSessionId: (conversationId: string) => string | undefined;
}

// ── 工具函数 ──

/**
 * busy 口径选择器（M3-B：全局锁——任一对话有处理中快照即 busy，
 * 与后端 `active_count() > 0` 同口径；设置闸/中止按钮可用态消费）。
 * 纯函数：传入 store state（或 state.processing 所在的任何对象）→ 布尔。
 * 组件订阅用 `useProcessingStore(isProcessingBusy)`（返回原始布尔，稳定）。
 */
export function isProcessingBusy(state: { processing: Record<string, ProcessingState> }): boolean {
  return Object.keys(state.processing).length > 0;
}

/**
 * 「有待审批」口径（设置入口收窄用）：任一处理中快照里存在**未配对**的
 * `tool/approval-request`（有 request 无 result）即为有待审批。
 *
 * 与 EventBadge 的配对规则同源（callId 精确配对）。设置遮罩真正会盖住的只有
 * 审批确认卡，而不是“任何对话在跑”——后者让长对话里只能干等。
 */
export function hasPendingApproval(state: {
  processing: Record<string, ProcessingState>;
}): boolean {
  for (const snap of Object.values(state.processing)) {
    const decided = new Set(
      snap.currentEvents
        .filter((e) => e.type === "tool/approval-result")
        .map((e) => e.callId)
    );
    const pending = snap.currentEvents.some(
      (e) => e.type === "tool/approval-request" && !decided.has(e.callId)
    );
    if (pending) return true;
  }
  return false;
}

/** 事件处理器内即时查 busy（getState 快照，避免为一次性读订阅重渲染） */
export function isAnyProcessing(): boolean {
  return isProcessingBusy(useProcessingStore.getState());
}

/**
 * 检查 processing 字典中是否已有属于指定 conversation 的条目。
 * sendMessage 的并发保护：同一对话在处理中时拒绝新请求。
 * 需要遍历是因为 sessionId（字典 key）与 conversationId（快照内部字段）不同。
 */
function hasProcessingFor(
  processing: Record<string, ProcessingState>,
  conversationId: string
): boolean {
  return Object.values(processing).some(
    (snap) => snap.conversationId === conversationId
  );
}

// ── 流式节流缓冲（模块 F · 批 2-A+B 设计一） ──
//
// assistant/chunk 是 token 级高频事件：每个 delta 一个事件 → 每个事件一次
// Zustand set → MessageList 全列表重建 → Markdown 全量转换。这里按 Hanako
// 参考（30FPS 节流，use-stream-buffer.ts 的 FLUSH_INTERVAL 语义）引入 33ms
// 批量缓冲：窗口内 chunk 累积在纯 JS Map 中（不触发 React 渲染），flush 时
// 一次性追加进 currentEvents。
//
// 语义：
// - 只缓冲 assistant/chunk（text + reasoning 两种 kind 都缓冲）
// - tool/call、tool/result、assistant/message、error(可恢复) 等立即处理，
//   且先 flush 该 session 的缓冲再追加本事件（两次同步 set，React 自动批处理；
//   缓冲 chunk 必须排在后续事件之前，事件顺序不变）
// - turn/end 到达（finishProcessing）必须强 flush：保证 chunk 全落位后归档
// - failProcessing 丢弃并清理缓冲（错误轮只归档 [user/message, error]）
//
// 位置选在 store 内部而非 hook 层：
// - 缓冲属于「事件 → 投影缓存」的写入策略，归 store 职责（测试直接调
//   store actions 模拟后端事件的架构约定不变）
// - StrictMode 双挂载下 hook 层缓冲会在 cleanup 时丢未 flush 事件，store
//   模块级 Map 无此问题（与 Hanako 独立 manager 同理）
// - turn/end 强 flush 原子化在 finishProcessing 内部，useAgentEvents 零改动

const PENDING_FLUSH_MS = 33;

interface ChunkBuffer {
  /** 窗口内累积的 assistant/chunk 事件（FIFO） */
  events: SessionEvent[];
  /** 上次 flush 的时间戳（Date.now()，用于计算距 33ms 窗口的剩余时间） */
  lastFlush: number;
  /** 补足窗口剩余时间的定时器；null = 无挂起定时器 */
  timer: ReturnType<typeof setTimeout> | null;
}

/** sessionId → chunk 缓冲（模块级，不进入 Zustand 状态） */
const chunkBuffers = new Map<string, ChunkBuffer>();

/**
 * 把指定 session 的缓冲 chunk 一次性追加进 currentEvents（强 flush）。
 *
 * 导出的目的：测试可直接显式 flush 验证落位与顺序（processing.throttle.test.ts），
 * 生产路径由 scheduleFlush（33ms 窗口）与 finishProcessing（turn/end 强 flush）驱动。
 *
 * 缓冲为空或无缓冲 → 无操作（幂等）。
 * 快照已被清理（failProcessing 先执行）→ 丢弃缓冲，不追加。
 */
export function flushPending(sessionId: string): void {
  const buf = chunkBuffers.get(sessionId);
  if (!buf || buf.events.length === 0) return;

  const events = buf.events;
  if (buf.timer) clearTimeout(buf.timer);
  buf.timer = null;
  buf.events = [];
  // 保留 buffer 并推进 lastFlush：节流窗口跨批次持续（Hanako 语义——
  // 「距上次 flush ≥ 33ms → 立即 flush」以真实的上次 flush 时间为基准）。
  // 若在此处删除 buffer，下次 append 会重建 lastFlush=now，窗口重置为 0，
  // 永远等满 33ms，与设计不符。
  buf.lastFlush = Date.now();

  useProcessingStore.setState((state) => {
    const snap = state.processing[sessionId];
    if (!snap) return state; // 快照已被清理，丢弃缓冲
    return {
      processing: {
        ...state.processing,
        [sessionId]: {
          ...snap,
          currentEvents: [...snap.currentEvents, ...events],
        },
      },
    };
  });
}

/**
 * 调度 33ms 节流 flush（Hanako scheduleFlush 语义）：
 * 距上次 flush ≥ 33ms → 立即 flush；否则按剩余时间安排 setTimeout 补足。
 */
function scheduleFlush(sessionId: string): void {
  const buf = chunkBuffers.get(sessionId);
  if (!buf) return;

  const now = Date.now();
  if (now - buf.lastFlush >= PENDING_FLUSH_MS) {
    flushPending(sessionId);
  } else if (!buf.timer) {
    buf.timer = setTimeout(() => {
      const b = chunkBuffers.get(sessionId);
      if (b) b.timer = null;
      flushPending(sessionId);
    }, PENDING_FLUSH_MS - (now - buf.lastFlush));
  }
}

/**
 * 立即追加非 chunk 事件；先 flush 该 session 的缓冲（chunk 先落位），
 * 再追加本事件——两次同步 set 在 React 批处理下合并为一次渲染，
 * 事件顺序不变，且无需额外等待 timer。
 */
function appendImmediate(event: SessionEvent): void {
  flushPending(event.sessionId);

  useProcessingStore.setState((state) => {
    const snap = state.processing[event.sessionId];
    if (!snap) return state; // 快照已被清理，忽略
    return {
      processing: {
        ...state.processing,
        [event.sessionId]: {
          ...snap,
          currentEvents: [...snap.currentEvents, event],
        },
      },
    };
  });
}

/** 清理指定 session 的缓冲与定时器（failProcessing 路径，丢弃未 flush chunk） */
function clearChunkBuffer(sessionId: string): void {
  const pending = chunkBuffers.get(sessionId);
  if (!pending) return;
  if (pending.timer) clearTimeout(pending.timer);
  chunkBuffers.delete(sessionId);
}

/** 把一条 assistant/chunk 事件推入该 session 的 33ms 节流缓冲 */
function pushChunk(event: AssistantChunkEvent): void {
  let buf = chunkBuffers.get(event.sessionId);
  if (!buf) {
    buf = { events: [], lastFlush: Date.now(), timer: null };
    chunkBuffers.set(event.sessionId, buf);
  }
  buf.events.push(event);
  scheduleFlush(event.sessionId);
}

/** 把一段文本以 assistant/chunk(text) 合成事件推入缓冲（扫描器放行的扣留文本） */
function pushTextChunk(sessionId: string, content: string): void {
  if (content.length === 0) return;
  pushChunk({ type: "assistant/chunk", sessionId, kind: "text", content });
}

/**
 * 标记「中止已发出、轮未收尾」（2026-10-08）。
 *
 * 乐观：点击中止即置位，界面当场给出反馈；后端报告没有活动轮（`abort_turn`
 * 返回 false）时撤回。快照随轮收尾被删除，这个状态无需另行清理。
 */
export function markAborting(sessionId: string, on: boolean): void {
  useProcessingStore.setState((state) => {
    const snap = state.processing[sessionId];
    if (!snap) return state;
    return {
      processing: {
        ...state.processing,
        [sessionId]: { ...snap, aborting: on },
      },
    };
  });
}

/** 更新指定 session 快照的 mood 与强度（标签匹配成功时调用；快照已被清理则忽略） */
function setMood(
  sessionId: string,
  mood: Mood,
  intensity: MoodIntensity
): void {
  useProcessingStore.setState((state) => {
    const snap = state.processing[sessionId];
    if (!snap) return state;
    return {
      processing: {
        ...state.processing,
        [sessionId]: { ...snap, mood, moodIntensity: intensity },
      },
    };
  });
}

/** 从输入派生对话标题（首条 user 消息截断）——规则收口在 projection。 */

// ── 会话自动标题 · 触发层（前端编排） ──
//
// 职责：素材累积（首答可见文本）+ 阈值触发（500 字）+ 轮末兜底 + 幂等 +
// 默认标题哨兵 + 生成前预检。LLM 生成与写盘在后端（generate_conversation_title
// 命令）；本层只负责“何时发起”。
//
// 幂等标志放模块级 Map（仿 chunkBuffers：跨快照删除存活）。key=conversationId
// ——自动标题仅针对“新对话首轮”，注册于 sendMessage 建对话处，轮末清理。

/** 首答可见文本触发阈值（字符数） */
const TITLE_TRIGGER_CHARS = 500;
/** 尝试次数上限（阈值一次 + 轮末兜底一次） */
const TITLE_MAX_ATTEMPTS = 2;
/** 首答文本累积上限（仅防无界；实际传后端时再截 500） */
const TITLE_ASSISTANT_CAP = 2000;

interface TitleGenState {
  /** 默认标题哨兵（首问 trim + 前 12 字符；创建时快照） */
  defaultTitle: string;
  /** 首答可见文本累积（text kind；已剥离情绪标签后的可见文本） */
  text: string;
  /** 阈值是否已触发（幂等） */
  thresholdFired: boolean;
  /** 已发起尝试次数（上限 TITLE_MAX_ATTEMPTS） */
  attempts: number;
}

const titleGenState = new Map<string, TitleGenState>();

/** 注册自动标题现场（仅新对话首轮；sendMessage 调用）。 */
function registerTitleGen(conversationId: string, defaultTitle: string): void {
  titleGenState.set(conversationId, {
    defaultTitle,
    text: "",
    thresholdFired: false,
    attempts: 0,
  });
}

/** 清理自动标题现场（轮末 / 错误轮）。 */
function clearTitleGenState(conversationId: string): void {
  titleGenState.delete(conversationId);
}

/** 按 conversationId 找当前处理快照（标题素材取首条 user 事件）。 */
function findProcessingSnapshot(conversationId: string): ProcessingState | undefined {
  return Object.values(useProcessingStore.getState().processing).find(
    (s) => s.conversationId === conversationId
  );
}

/**
 * 收集生成素材（首问剥离 [prts_reminder] + 首答可见文本）。
 * 素材在触发时刻从当前处理快照取（首条 user 事件为前端合成的含块版本）。
 */
function collectTitleMaterial(conversationId: string): {
  userText: string;
  assistantText: string;
  expectedTitle: string;
} | null {
  const st = titleGenState.get(conversationId);
  if (!st) return null;
  const snap = findProcessingSnapshot(conversationId);
  if (!snap) return null;
  const userEv = snap.currentEvents.find(
    (ev): ev is UserMessageEvent => ev.type === "user/message"
  );
  if (!userEv) return null;
  return {
    userText: stripReminderBlock(userEv.content),
    assistantText: st.text.slice(0, 500),
    expectedTitle: st.defaultTitle,
  };
}

/**
 * 发起一次标题生成（fire-and-forget；结果经 conversation/title 事件回推）。
 *
 * 预检（写盘侧另有 CAS 兜底）：当前标题 !== 默认哨兵（用户已改名）→ 跳过。
 */
function triggerTitleGeneration(
  conversationId: string,
  source: "threshold" | "fallback"
): void {
  const st = titleGenState.get(conversationId);
  if (!st) return;
  if (st.attempts >= TITLE_MAX_ATTEMPTS) return;
  if (source === "threshold") {
    if (st.thresholdFired) return;
    st.thresholdFired = true;
  }
  const conv = useConversationStore
    .getState()
    .conversations.find((c) => c.id === conversationId);
  if (!conv) return;
  if (conv.title !== st.defaultTitle) return; // 已被改/已生成 → 跳过
  const material = collectTitleMaterial(conversationId);
  if (!material || !material.assistantText.trim()) return;
  st.attempts += 1;
  invoke("generate_conversation_title", {
    conversationId,
    userText: material.userText,
    assistantText: material.assistantText,
    expectedTitle: material.expectedTitle,
  }).catch(() => {});
}

/** 累积首答可见文本（appendCurrentEvent 的 text chunk 路径调用）。 */
function accumulateTitleText(conversationId: string, content: string): void {
  const st = titleGenState.get(conversationId);
  if (!st || st.thresholdFired) return;
  if (st.text.length < TITLE_ASSISTANT_CAP) {
    st.text += content;
  }
  if (st.text.length >= TITLE_TRIGGER_CHARS) {
    triggerTitleGeneration(conversationId, "threshold");
  }
}

/**
 * 轮末兜底（仅首轮正常完成时；finishProcessing 调用）。
 * 条件：尚未成功获得标题（标题仍为默认）+ 尝试次数未达上限。
 */
function maybeTriggerTitleFallback(conversationId: string): void {
  const st = titleGenState.get(conversationId);
  if (!st) return;
  if (st.attempts >= TITLE_MAX_ATTEMPTS) return;
  const conv = useConversationStore
    .getState()
    .conversations.find((c) => c.id === conversationId);
  if (!conv || conv.title !== st.defaultTitle) return;
  triggerTitleGeneration(conversationId, "fallback");
}

/** 合成 user/message 事件（前端输入即事实，后端不镜像回传） */
function makeUserEvent(
  sessionId: string,
  content: string,
  attachments?: Attachment[]
): UserMessageEvent {
  return attachments && attachments.length > 0
    ? { type: "user/message", sessionId, content, attachments }
    : { type: "user/message", sessionId, content };
}

// ═══════════════════════════════════════════════════
// Store
// ═══════════════════════════════════════════════════

export const useProcessingStore = create<ProcessingStore>((set, get) => ({
  processing: {},

  sendMessage: async (input: string, attachments: Attachment[] = []) => {
    const convStore = useConversationStore.getState();
    const { processing } = get();
    const activeId = convStore.activeId;

    if (activeId !== null && hasProcessingFor(processing, activeId)) return;

    // ── 模块 D：压缩中禁发（同会话）——压缩进行中 turns 处于状态转换窗口，
    // 新消息若随旧 turns 发送将与压缩结果错位（双向互斥的前端半边；后端
    // busy 闸兜底）。 ──
    if (
      activeId !== null &&
      useCompactionStore.getState().compactingConversationId === activeId
    )
      return;

    // ── Reminder：拉取提醒数据（后端 get_reminder_data：快照＝值 + 有界事件）──
    // 新对话（lastNoticeSeq 为空）：值维度快照——仅 kind === "value" 的
    //   维度 value，不含事件行；游标＝payload.seq。
    // 进行中对话：增量——stamp>游标 的维度（note）＋ seq>游标 的事件；
    //   维度行在上、事件行在下；游标＝payload.seq。
    // 含块版本随轮归档冻结；invoke 失败仅进行中对话回滚（新对话不回滚为 0，
    // 避免重放）。拉取失败 → 静默跳过本轮（游标不动 → 下轮重试）。
    let noticeBlock = "";
    let noticeAdvanceTo = 0;
    let prevNoticeSeq = 0;
    let noticeIsNewConversation = false;
    try {
      const data = (await invoke<ReminderData>("get_reminder_data")) ?? {
        seq: 0,
        dims: [],
        events: [],
      };
      // await 后以最新对话状态为准（拉取期间状态可能被推进）
      const st = useConversationStore.getState();
      const conv = st.activeId !== null
        ? st.conversations.find((c) => c.id === st.activeId)
        : undefined;
      const cursor = conv?.lastNoticeSeq;
      const lines: ReminderLine[] = [];
      if (cursor === undefined || cursor === null) {
        // 新对话：仅值维度快照（kind === "value"）
        noticeIsNewConversation = true;
        for (const d of data.dims) {
          if (d.kind === "value" && d.value !== "") {
            lines.push({ title: d.title, detail: d.value });
          }
        }
      } else {
        prevNoticeSeq = cursor;
        // 维度行（stamp > 游标）在上
        for (const d of data.dims) {
          if (d.stamp > cursor) lines.push({ title: d.title, detail: d.note });
        }
        // 事件行（seq > 游标）在下
        for (const e of data.events) {
          if (e.seq > cursor) lines.push({ title: e.title, detail: e.detail });
        }
      }
      if (lines.length > 0) noticeBlock = buildReminderBlock(lines);
      noticeAdvanceTo = data.seq;
    } catch {
      // 提醒拉取失败：本轮不带通知，游标未动 → 下轮重试
    }
    const displayContent = noticeBlock
      ? `${noticeBlock}\n\n${input}`
      : input;

    // 准备传给 LLM 的完整对话历史（含本次 user 消息——含块版本随 history
    // 发送并归档冻结，是 LLM 实际所见）。投影派生：从已归档轮次的
    // SessionEvent 序列重建 LLM 可见历史，error 事件天然被过滤；user 消息
    // 保留块（deriveMessages 不剥离——块是冻结历史的一部分）。
    const st = useConversationStore.getState();
    const isNewNow = st.activeId === null;
    const conv = isNewNow
      ? undefined
      : st.conversations.find((c) => c.id === st.activeId);
    const priorMessages: ChatMessage[] = isNewNow
      ? []
      : deriveActiveHistory(conv?.turns ?? [], conv?.compaction);
    const lastUser: ChatMessage = { role: "user", content: displayContent };
    if (attachments.length > 0) lastUser.attachments = attachments;
    const history: ChatMessage[] = [...priorMessages, lastUser];

    // ── 模块 D：上下文窗口硬拦（发送前预估）──
    // 水位 = 最近 usage 的 inputTokens（provider 权威口径）；下一轮预估 =
    // user 文本（含 reminder 块）+ output 预留。无窗口信息（未知模型）→ 不拦。
    // 拦截为提示而非自动压缩（共识：手动触发）。
    const usedInput = conv?.contextTokens?.inputTokens ?? 0;
    const windowSize = modelInfoRef.current?.contextLength ?? 0;
    if (
      !isNewNow &&
      conv &&
      willNextTurnOverflow(usedInput, displayContent, windowSize)
    ) {
      showToast("上下文接近窗口上限，请先压缩后再继续", {
        kind: "warning",
        dedupeKey: "ctx-window-full",
      });
      return;
    }

    // 创建或更新对话，同时获取 conversationId
    let conversationId: string;
    if (isNewNow) {
      const info = modelInfoRef.current;
      const dTitle = defaultTitleFromInput(input);
      conversationId = useConversationStore
        .getState()
        .createConversation(
          dTitle,
          info?.name,
          info?.contextLength,
        );
      // 自动标题现场注册（仅新对话首轮）：哨兵 + 素材累积起点
      registerTitleGen(conversationId, dTitle);
    } else {
      conversationId = st.activeId!;
    }

    // 游标推进：游标＝payload.seq（快照与增量同口径；含本次未拼入的事件，防下轮补放）。
    if (noticeAdvanceTo > prevNoticeSeq) {
      useConversationStore
        .getState()
        .advanceNoticeSeq(conversationId, noticeAdvanceTo);
    }

    // await/创建期间可能已有本对话的处理中快照（快速连发）→ 重查并发保护
    // （双拉同时完成的理论窗口仍存在，UI 双击主场景被此覆盖）
    if (hasProcessingFor(get().processing, conversationId)) return;

    const sessionId = generateId();
    // 函数式 set：合并最新 state.processing（await 期间其他对话可能已新增条目）
    set((state) => ({
      processing: {
        ...state.processing,
        [sessionId]: {
          conversationId,
          // user/message 预置为第一条（含块版本；投影缓存进行态，归档时随轮落库）
          currentEvents: [makeUserEvent(sessionId, displayContent, attachments)],
          agentPhase: "processing",
          // 情绪默认平静（省略）；标签匹配成功时 set mood
          mood: "平静",
          moodIntensity: 0,
        },
      },
    }));
    // processing 字典新增了活跃对话的条目 → deriveUIState 自动返回 "processing"

    // 缓存区按对话分区（模块 C）：每轮随请求携带 conversationId，
    // 后端据此定位本对话的静态/动态桶（跨轮持久）。
    // sessionId 每轮重新生成，不能作为分区键；旧后端不读此字段。
    const request: AgentRequest = { input, history, sessionId, conversationId };
    invoke("agent_process", { request }).catch((err) => {
      // invoke 本身的异常（后端进程崩溃、网络不可达等）：后端未跑 →
      // 回滚通知游标（下轮重拉重拼，防丢失）。错误轮归档剥离块
      // （failProcessing 内）——错误轮不对 LLM 可见，块不应残留历史。
      // Agent 处理过程中的错误通过 error 事件到达，不经过此 catch。
      // 回滚语义：仅**进行中**对话回滚游标（防丢失）；新对话基线推进
      // 不回滚为 0——否则重试时会把历史通知全量重放。
      if (!noticeIsNewConversation && noticeAdvanceTo > prevNoticeSeq) {
        useConversationStore
          .getState()
          .advanceNoticeSeq(conversationId, prevNoticeSeq);
      }
      get().failProcessing(sessionId, String(err));
    });
  },

  appendCurrentEvent: (event) => {
    const snap = get().processing[event.sessionId];
    if (!snap) return; // 快照已被清理（finishProcessing 或 failProcessing 已执行），忽略

    if (event.type === "assistant/chunk") {
      // 高频增量：进入 33ms 批量缓冲（窗口内累积，flush 时一次性追加）。
      // text / reasoning 两种 kind 均缓冲——EventBadge 的推理累积与
      // deriveTextSegments 的文本累积都只读 currentEvents，序列不变。
      if (event.kind === "text") {
        // 情绪标签扫描（批 2-D）：进缓冲前剥离标签并更新 mood。
        // pending 扣留的文本（扫描返回空串）不进缓冲——用户不可见半标签。
        const scan = scanTextChunk(event.sessionId, event.content);
        if (scan.mood) {
          // 情绪事件先落位（时序：前段文本 → 情绪卡片 → 本段剥离文本）
          appendImmediate({
            type: "mood/event",
            sessionId: event.sessionId,
            mood: scan.mood,
            intensity: scan.intensity,
          });
          setMood(event.sessionId, scan.mood, scan.intensity);
        }
        if (scan.text.length === 0) return;
        event = { ...event, content: scan.text };
      }
      // reasoning chunk 不进扫描器，原样缓冲。
      pushChunk(event);
      // 会话自动标题：累积首答可见文本（仅 text kind；达阈值即触发）
      if (event.kind === "text") {
        accumulateTitleText(snap.conversationId, event.content);
      }
      return;
    }

    if (event.type === "assistant/message") {
      // 一次性剥离（防止归档瞬间标签复活）：assistant/message 是后端原始聚合，
      // 流式期间标签已在 chunk 层剥离，此处用同一套剥离保证归档 turns 干净。
      const { text, mood, intensity } = stripTagFromText(event.content);
      if (mood) {
        // 防重：本段（聚合所属的末段）已消费（流式已产 mood/event）→ 只同步 mood；
        // 未消费（异常路径：流式未匹配但聚合带标签）→ 补插事件，保证归档完整。
        if (!segmentConsumed(event.sessionId)) {
          appendImmediate({
            type: "mood/event",
            sessionId: event.sessionId,
            mood,
            intensity,
          });
        }
        setMood(event.sessionId, mood, intensity);
      }
      if (text !== event.content) event = { ...event, content: text };
      appendImmediate(event);
      return;
    }

    if (event.type === "tool/call" || event.type === "user/message") {
      // 段边界 re-arm（轨迹语义）：tool/call 与轮内注入都是“一段新回复”的边界——
      // 重置段首与消费位，后续段可再尝试匹配（每段最多一个）。
      // 未闭合 pending 的扣留文本在此放行，作为边界事件之前的 chunk 落位
      // （事件顺序不变：pending 文本 → 边界事件）。
      const held = rearmScanner(event.sessionId);
      if (held) pushTextChunk(event.sessionId, held);
    }

    // 其他事件（tool/result、error 等）立即处理；
    // 若该 session 有未 flush 的 chunk 缓冲，同一 set 内先追加缓冲再追加本事件
    // （缓冲 chunk 必须排在后续事件之前，事件顺序不变）。
    appendImmediate(event);
  },

  onPhase: (event) => {
    const { processing } = get();
    const snap = processing[event.sessionId];
    if (!snap) return;

    // PhaseEvent.phase → ProcessingState.agentPhase 的映射
    // "executing" 映射到 "processing" 而非保留原名，因为从快照视角
    // "调工具"和"等工具返回"是不可区分的——两者都属于工具调用阶段
    const phaseMap: Record<string, ProcessingState["agentPhase"]> = {
      reasoning: "reasoning",
      executing: "processing",
      generating: "streaming",
    };

    // 未知 phase 值（后端未来扩展或适配层 bug）时保持当前值不变。
    // 不认识的 phase 不应改变前端状态——后端信号是输入而非指令。
    // deriveUIState 继续按当前 agentPhase 推导 UI 状态。
    const mapped = phaseMap[event.phase] ?? snap.agentPhase;
    set((state) => ({
      processing: {
        ...state.processing,
        [event.sessionId]: { ...snap, agentPhase: mapped },
      },
    }));
    // agentPhase 变化 → deriveUIState 自动返回对应 UI 状态
    // reasoning/processing → "processing"，streaming → "streaming"
  },

  finishProcessing: (sessionId: string, reason?: TurnEndReason, turnUsage?: UsageStat | null) => {
    // 未闭合 pending 放行（批 2-D）：不可见的半标签最终以 chunk 落位进归档
    // （finishProcessing/failProcessing 时未闭合 pending → 放行）。
    const held = releasePending(sessionId);
    if (held) pushTextChunk(sessionId, held);

    // 强 flush：turn/end 到达时缓冲中的 chunk 必须全部落位后再归档，
    // 保证整轮事件（含最后一个 chunk）完整进入 Conversation.turns。
    flushPending(sessionId);
    // 归档后清理空 buffer 条目（events 已清空，从 Map 移除防残留）
    clearChunkBuffer(sessionId);

    const { processing } = get();
    const snap = processing[sessionId];
    if (!snap) return; // failProcessing 已清理（如停滞路径 error 先到）→ 幂等

    // M3-B §4.2：aborted 轮归档前在 currentEvents 末尾合成轮中止标记
    // （前端私有事件——derive/展示层忽略、MessageList 轮尾展示；持久化
    // 随轮 events_json 原样写盘 → 重启可见）。其余 reason 路径完全不变。
    const abortedMarker: SessionEvent = { type: "turn/aborted", sessionId };
    const archiveEvents: SessionEvent[] =
      reason === "aborted"
        ? [...snap.currentEvents, abortedMarker]
        : snap.currentEvents;

    const convStore = useConversationStore.getState();
    // 首轮判定（归档前）：该对话尚无已归档轮 → 本次为首个 turn。
    const convBefore = convStore.conversations.find(
      (c) => c.id === snap.conversationId
    );
    const isFirstTurn = convBefore ? convBefore.turns.length === 0 : false;
    // 上下文 Token 用量（覆盖式）：最后一个 assistant/message 的 usage 快照。
    // 职责分工（§7.8）：内存由 updateContextTokens 管、持久由 append_turn 的
    // metaSnapshot（usage 参数）管——先扫出本轮 usage，随轮提交保证落盘。
    // aborted 轮无 assistant/message → usage null → append_turn usage 缺省
    // （contextTokens 不覆盖，见 finishProcessing doc）。
    let usage: TokenUsage | null = null;
    for (let i = snap.currentEvents.length - 1; i >= 0; i--) {
      const e = snap.currentEvents[i];
      if (e.type === "assistant/message" && e.usage) {
        usage = e.usage;
        break;
      }
    }

    // 用量增量：本轮 session（来自 turn/end）+ 首轮未归档时暂存的标题用量
    const usageDeltas: UsageDelta[] = [];
    if (turnUsage) usageDeltas.push({ category: "session", ...turnUsage });
    const pendingTitle = pendingTitleUsage.get(snap.conversationId);
    if (pendingTitle) {
      usageDeltas.push(pendingTitle);
      pendingTitleUsage.delete(snap.conversationId);
    }

    // 整轮归档：currentEvents = [user/message, ...后端持久事实]；
    // 持久提交（append_turn）携带本轮 usage 与用量增量
    convStore.appendTurn(
      snap.conversationId,
      archiveEvents,
      usage ?? undefined,
      usageDeltas.length > 0 ? usageDeltas : undefined
    );

    // 内存同步（纯内存 action，不再发 IPC）
    if (usage) {
      convStore.updateContextTokens(snap.conversationId, usage);
    }

    // 会话自动标题：首轮轮末兜底（仅正常完成——阈值已触发但未成功时也兜底）
    // + 现场清理（标题可能在轮末之后到达，由事件处理按 turns 重算哨兵）。
    if (isFirstTurn) {
      if (reason === undefined || reason === "completed") {
        maybeTriggerTitleFallback(snap.conversationId);
      }
      clearTitleGenState(snap.conversationId);
    }

    // 不可变删除：Zustand 要求返回新对象，不能直接修改 state.processing
    set((state) => {
      const next = { ...state.processing };
      delete next[sessionId];
      return { processing: next };
    });
    // processing 字典删除了该条目 → deriveUIState 自动返回 "ready"

    // 经历管线触发（Phase 4 任务二）：轮末 fire-and-forget。
    // 后端自判增量阈值（不足 6 轮 no-op）；失败/整轮报废后端静默（不拦用户）。
    void invoke("finalize_memory", { conversationId: snap.conversationId }).catch(
      () => {}
    );
  },

  failProcessing: (sessionId: string, errorMsg: string) => {
    // 未闭合 pending 放行（扫描器状态释放）；随后随缓冲一并丢弃——
    // 错误轮只归档 [user/message, error]，chunk（含放行的 pending）不落库。
    releasePending(sessionId);
    // 清理未 flush 的 chunk 缓冲（错误轮只归档 [user/message, error]，chunk 丢弃）
    clearChunkBuffer(sessionId);

    const snap = get().processing[sessionId];
    if (!snap) return;

    set((state) => {
      const next = { ...state.processing };
      delete next[sessionId];
      return { processing: next };
    });

    // 错误轮归档进对话时间线：[user/message, error] 一组。
    // 轮次序列本身就是时间线——错误自然属于它发生的那一轮，
    // 后续 user 消息追加在其后，无需手动清除。
    // M1 Reminder：归档 user 消息剥离 [prts_reminder] 块——错误轮（invoke
    // 失败路径）后端未跑、块未生效，不应残留历史（游标已在 sendMessage
    // catch 回滚 → 下轮重拼；纯输入进错误轮避免派生链出现重复块）。
    const convStore = useConversationStore.getState();
    // currentEvents[0] 恒为前端合成的 user/message（快照预置不变量），
    // 类型层收窄后剥离块（错误轮归档纯输入）
    const firstEvent = snap.currentEvents[0] as SessionEvent & {
      type: "user/message";
    };
    convStore.appendTurn(snap.conversationId, [
      {
        ...firstEvent,
        content: stripReminderBlock(firstEvent.content),
      },
      {
        type: "error",
        sessionId,
        message: errorMsg,
        recoverable: false,
      },
    ]);

    // 会话自动标题：错误轮不兜底（素材不足/用户未得到回答）；若阈值已触发、
    // 生成在途，返回后仍可经 CAS 落地（标题是会话级信息，与轮完成态无关）。
    clearTitleGenState(snap.conversationId);

    // 只有错误发生在活跃对话时才触发即时展示——切走了不该看到别人的错误
    if (snap.conversationId === convStore.activeId) {
      useUIStore.getState().setError(errorMsg);
    }
  },

  getProcessingForConversation: (conversationId) =>
    Object.values(get().processing).find(
      (snap) => snap.conversationId === conversationId
    ),

  getProcessingSessionId: (conversationId) => {
    const entry = Object.entries(get().processing).find(
      ([, snap]) => snap.conversationId === conversationId
    );
    return entry ? entry[0] : undefined;
  },
}));
