// 轮末收尾：成功/中止归档与失败归档。
//
// 由 useProcessingStore 的 finishProcessing / failProcessing 委托执行。

import { invoke } from "@tauri-apps/api/core";
import type {
  SessionEvent,
  TokenUsage,
  TurnEndReason,
  UsageDelta,
  UsageStat,
} from "../types";
import { stripReminderBlock } from "../projection";
import { useConversationStore } from "./useConversationStore";
import { useUIStore } from "./useUIStore";
import { releasePending } from "../moodTag";
import {
  clearChunkBuffer,
  flushPending,
  pushTextChunk,
} from "./processingChunks";
import {
  clearTitleGenState,
  hasTitleGen,
  maybeTriggerTitleFallback,
} from "./processingTitle";
import { takeTitleUsage } from "./processingTitleUsage";
import type { ProcessingStore } from "./useProcessingStore";

type SetStore = (
  partial:
    | Partial<ProcessingStore>
    | ((state: ProcessingStore) => Partial<ProcessingStore>)
) => void;

export function finishProcessingImpl(
  get: () => ProcessingStore,
  set: SetStore,
  sessionId: string,
  reason?: TurnEndReason,
  turnUsage?: UsageStat | null
): void {
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
  // 随轮 events_json 原样写盘 → 重启可见）。R-07 的截断标记同构。
  // R-33：stagnation 轮同样保留完整 currentEvents，只补轮尾标记；不能走
  // failProcessing 的错误轮路径，否则半截内容、工具记录和 usage 都会丢。
  const abortedMarker: SessionEvent = { type: "turn/aborted", sessionId };
  const truncatedMarker: SessionEvent = { type: "turn/truncated", sessionId };
  const stagnatedMarker: SessionEvent = { type: "turn/stagnated", sessionId };
  const archiveEvents: SessionEvent[] =
    reason === "aborted"
      ? [...snap.currentEvents, abortedMarker]
      : reason === "max-tokens"
        ? [...snap.currentEvents, truncatedMarker]
        : reason === "stagnation"
          ? [...snap.currentEvents, stagnatedMarker]
          : snap.currentEvents;

  const convStore = useConversationStore.getState();
  // 本次轮是否挂着自动标题现场（失败轮/中止轮会清掉；下一轮正常发送再挂上）。
  const hadTitleGen = hasTitleGen(snap.conversationId);
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
  const pendingTitle = takeTitleUsage(snap.conversationId);
  if (pendingTitle) {
    usageDeltas.push(pendingTitle);
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

  // 会话自动标题：本次轮挂有标题现场（含报错后第一次正常轮）→ 轮末兜底；
  // 标题事件可能晚于轮末到达，故现场清理不影响事件通道（titleExpected 保留）。
  if (hadTitleGen) {
    if (reason === undefined || reason === "completed") {
      maybeTriggerTitleFallback(snap.conversationId, snap.currentEvents);
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
}

export function failProcessingImpl(
  get: () => ProcessingStore,
  set: SetStore,
  sessionId: string,
  errorMsg: string
): void {
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
}
