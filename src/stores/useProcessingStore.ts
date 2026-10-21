// useProcessingStore · Agent 请求处理生命周期（架构升级 · 模块 1）
//
// 管理 processing 快照字典与事件写入；发送、收尾、事件解析分别由
// processingSend / processingFinalize / processingEvents 承担。
// 被 useAgentEvents Hook 驱动。
// uiState 不在此管理——它是 deriveUIState() 从原始事实计算的派生值。
//
// 投影缓存语义：processing[sessionId].currentEvents 是本轮
// SessionEvent 的实时投影（user/message 由前端合成预置为第一条），
// turn/end 到达时整轮归档进 Conversation.turns。

import { create } from "zustand";
import type {
  SessionEvent,
  TokenUsage,
  TurnEndReason,
  Mood,
  MoodIntensity,
  UsageStat,
  Attachment,
} from "../types";
import { useConversationStore } from "./useConversationStore";
import { configureChunkBuffer } from "./processingChunks";
import { appendCurrentEventImpl } from "./processingEvents";
import { sendProcessingMessage } from "./processingSend";
import { finishProcessingImpl, failProcessingImpl } from "./processingFinalize";

export { flushPending } from "./processingChunks";
export { titleExpectedFor } from "./processingTitle";
export { stashTitleUsage } from "./processingTitleUsage";
export {
  getModelInfo,
  setModelInfo,
  setModelThinking,
} from "./processingModelInfo";

// ── 类型 ──

/**
 * 单个 sendMessage 的处理中快照。
 * key = sessionId（前端生成的本次请求唯一标识，非 conversationId）。
 * 仅在 Agent 处理期间存在于 processing 字典中，处理完成后删除。
 */
export interface ProcessingState {
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

export interface ProcessingStore {
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
   * step/end：把本次 LLM 调用的 provider usage 写入对应对话的实时上下文。
   * 只更新内存，不落盘；快照不存在（已收尾/已删除）时忽略。
   */
  updateLiveContextFromStep: (sessionId: string, usage: TokenUsage) => void;

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
   * 2. 上下文 Token 用量：从最后一个 assistant/message 的 usage 快照读取（覆盖式）；
   *    liveContextTokens 随 appendTurn 清空，圆环回退到已提交值。
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

// ── 流式节流缓冲（模块 F · 批 2-A+B 设计一） ──
//
// assistant/chunk 是 token 级高频事件：每个 delta 一个事件 → 每个事件一次
// Zustand set → MessageList 全列表重建 → Markdown 全量转换。这里按既有实现的
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
//   模块级 Map 无此问题（与既有实现的独立 manager 同理）
// - turn/end 强 flush 原子化在 finishProcessing 内部，useAgentEvents 零改动

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

// ═══════════════════════════════════════════════════
// Store
// ═══════════════════════════════════════════════════

export const useProcessingStore = create<ProcessingStore>((set, get) => ({
  processing: {},

  sendMessage: async (input: string, attachments: Attachment[] = []) =>
    sendProcessingMessage(get, set, input, attachments),

  appendCurrentEvent: (event) =>
    appendCurrentEventImpl(get, set, event),

  updateLiveContextFromStep: (sessionId, usage) => {
    const snap = get().processing[sessionId];
    if (!snap) return;
    useConversationStore
      .getState()
      .updateLiveContextTokens(snap.conversationId, usage);
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

  finishProcessing: (sessionId: string, reason?: TurnEndReason, turnUsage?: UsageStat | null) =>
    finishProcessingImpl(get, set, sessionId, reason, turnUsage),

  failProcessing: (sessionId: string, errorMsg: string) =>
    failProcessingImpl(get, set, sessionId, errorMsg),

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

// chunk 缓冲层通过 sink 回写快照，避免 processingChunks ↔ store 循环依赖。
configureChunkBuffer({
  appendMany(sessionId, events) {
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
  },
  appendOne(event) {
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
  },
});
