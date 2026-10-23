// 后端事件写入 processing 快照：流式 chunk 缓冲、情绪标签扫描与段边界处理。

import type { Mood, MoodIntensity, SessionEvent } from "../types";
import {
  scanTextChunk,
  stripTagFromText,
  rearmScanner,
  segmentConsumed,
  releasePending,
} from "../moodTag";
import {
  appendImmediate,
  clearChunkBuffer,
  pushChunk,
  pushTextChunk,
} from "./processingChunks";
import { accumulateTitleText } from "./processingTitle";
import type { ProcessingStore } from "./useProcessingStore";

type SetStore = (
  partial:
    | Partial<ProcessingStore>
    | ((state: ProcessingStore) => Partial<ProcessingStore>)
) => void;

/** 更新指定 session 快照的 mood 与强度（标签匹配成功时调用；快照已清理则忽略） */
export function setProcessingMood(
  set: SetStore,
  sessionId: string,
  mood: Mood,
  intensity: MoodIntensity
): void {
  set((state) => {
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

export function appendCurrentEventImpl(
  get: () => ProcessingStore,
  set: SetStore,
  event: SessionEvent
): void {
  const snap = get().processing[event.sessionId];
  if (!snap) return; // 快照已被清理（finishProcessing 或 failProcessing 已执行），忽略

  if (event.type === "assistant/reset") {
    // 重试前撤回半成品：丢弃尚未 flush 的 chunk 和扫描器扣留文本；
    // 已经落进 currentEvents 的旧 chunk 由投影层按本事件截断。
    clearChunkBuffer(event.sessionId);
    releasePending(event.sessionId);
    set((state) => {
      const current = state.processing[event.sessionId];
      if (!current) return state;
      return {
        processing: {
          ...state.processing,
          [event.sessionId]: { ...current, mood: "平静", moodIntensity: 0 },
        },
      };
    });
    appendImmediate(event);
    return;
  }

  if (event.type === "assistant/chunk") {
    // 高频增量：进入 33ms 批量缓冲（窗口内累积，flush 时一次性追加）。
    // text / reasoning 两种 kind 均缓冲——EventBadge 的推理累积与
    // deriveTextSegments 的文本累积都只读 currentEvents，序列不变。
    if (event.kind === "text") {
      // 情绪标签扫描（批 2-D）：展示层剥离标签并更新 mood；
      // rawContent 保留模型原文，供下一轮历史使用（模型能看到自己写过的标签）。
      const rawContent = event.content;
      const scan = scanTextChunk(event.sessionId, rawContent);
      if (scan.mood) {
        // 情绪事件先落位（时序：前段文本 → 情绪卡片 → 本段剥离文本）
        appendImmediate({
          type: "mood/event",
          sessionId: event.sessionId,
          mood: scan.mood,
          intensity: scan.intensity,
        });
        setProcessingMood(set, event.sessionId, scan.mood, scan.intensity);
      }
      if (scan.text.length === 0) return;
      event =
        scan.rawText !== scan.text
          ? { ...event, content: scan.text, rawContent: scan.rawText }
          : { ...event, content: scan.text };
    }
    // reasoning chunk 不进扫描器，原样缓冲。
    pushChunk(event);
    // 会话自动标题：累积首答可见文本（仅 text kind；达阈值即触发）
    if (event.kind === "text") {
      accumulateTitleText(snap.conversationId, event.content, snap.currentEvents);
    }
    return;
  }

  if (event.type === "assistant/message") {
    // 一次性剥离（防止归档瞬间标签复活）：assistant/message 是后端原始聚合，
    // 流式期间标签已在 chunk 层剥离，此处用同一套剥离保证归档 turns 干净。
    const rawContent = event.content;
    const { text, mood, intensity } = stripTagFromText(rawContent);
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
      setProcessingMood(set, event.sessionId, mood, intensity);
    }
    if (text !== rawContent) {
      event = { ...event, content: text, rawContent };
    }
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
}
