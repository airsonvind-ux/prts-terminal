// src/stores/__tests__/processing.throttle.test.ts
// 流式节流缓冲测试（模块 F · 批 2-A+B 设计一）
//
// 覆盖：
//   1. assistant/chunk 走 33ms 缓冲：append 后未 flush 不入列
//   2. flushPending 落位且事件顺序保持
//   3. 非 chunk 事件前置 flush：缓冲 chunk 必须先于后续事件落位（顺序不变）
//   4. turn/end 强 flush：finishProcessing 前未 flush 的 chunk 完整归档
//   5. failProcessing 清理缓冲：未 flush chunk 丢弃，错误轮只归档 [user, error]
//   6. 33ms 窗口合并：窗口内多事件一次 flush（fake timers）
//   7. flushPending 幂等：无缓冲/空缓冲调用无副作用

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  useProcessingStore,
  flushPending,
} from "../useProcessingStore";
import { useConversationStore } from "../useConversationStore";
import { useUIStore } from "../useUIStore";
import type { SessionEvent } from "../../types";

// ── 辅助 ──

function resetAllStores(): void {
  useProcessingStore.setState({ processing: {} });
  useConversationStore.setState({ conversations: [], activeId: null });
  useUIStore.setState({
    hiddenMode: null,
    errorMsg: null,
    overlay: null,
  });
  __invokeMock.mockClear();
}

/** 发送一条消息并返回 sessionId（快照预置 user/message） */
async function startTurn(input: string): Promise<string> {
  await useProcessingStore.getState().sendMessage(input);
  const processing = useProcessingStore.getState().processing;
  const sids = Object.keys(processing);
  expect(sids).toHaveLength(1);
  return sids[0];
}

function textChunk(sessionId: string, content: string): SessionEvent {
  return { type: "assistant/chunk", sessionId, kind: "text", content };
}

function reasoningChunk(sessionId: string, content: string): SessionEvent {
  return { type: "assistant/chunk", sessionId, kind: "reasoning", content };
}

function toolCall(sessionId: string, callId: string): SessionEvent {
  return {
    type: "tool/call",
    sessionId,
    callId,
    toolName: "echo",
    params: "{}",
  };
}

/** 取当前快照的 currentEvents */
function currentEventsOf(sessionId: string): SessionEvent[] {
  return useProcessingStore.getState().processing[sessionId].currentEvents;
}

beforeEach(resetAllStores);
afterEach(() => vi.useRealTimers());

// ═══════════════════════════════════════════════
// 场景 1：chunk 走缓冲，未 flush 不入列
// ═══════════════════════════════════════════════

describe("节流缓冲 · chunk 未 flush 不入列", () => {
  it("appendCurrentEvent(assistant/chunk) 后 currentEvents 不立即增加", async () => {
    const sid = await startTurn("查询 1-7");

    useProcessingStore.getState().appendCurrentEvent(textChunk(sid, "关卡 1-7"));
    useProcessingStore.getState().appendCurrentEvent(textChunk(sid, "是主线第一章。"));

    // 缓冲中：currentEvents 仍只有合成的 user/message
    expect(currentEventsOf(sid)).toHaveLength(1);

    // 清理（flush 落位 + 归档），避免 33ms 真实 timer 残留影响后续用例
    flushPending(sid);
    useProcessingStore.getState().finishProcessing(sid);
  });

  it("flushPending 后事件全部落位且顺序保持", async () => {
    const sid = await startTurn("查询 1-7");

    useProcessingStore.getState().appendCurrentEvent(textChunk(sid, "关卡 1-7"));
    useProcessingStore.getState().appendCurrentEvent(reasoningChunk(sid, "思考中"));
    useProcessingStore.getState().appendCurrentEvent(textChunk(sid, "是主线第一章。"));

    flushPending(sid);

    const events = currentEventsOf(sid);
    expect(events).toHaveLength(4); // user/message + 3 chunks
    expect(events[1]).toEqual(textChunk(sid, "关卡 1-7"));
    expect(events[2]).toEqual(reasoningChunk(sid, "思考中"));
    expect(events[3]).toEqual(textChunk(sid, "是主线第一章。"));

    useProcessingStore.getState().finishProcessing(sid);
  });

  it("flushPending 幂等：无缓冲/空缓冲调用无副作用", async () => {
    const sid = await startTurn("查询 1-7");

    expect(() => flushPending(sid)).not.toThrow();
    expect(() => flushPending("不存在的-session")).not.toThrow();
    expect(currentEventsOf(sid)).toHaveLength(1);

    useProcessingStore.getState().finishProcessing(sid);
  });
});

// ═══════════════════════════════════════════════
// 场景 2：非 chunk 事件前置 flush，顺序保持
// ═══════════════════════════════════════════════

describe("节流缓冲 · 非 chunk 事件前置 flush", () => {
  it("缓冲 chunk + tool/call：tool/call 立即处理，chunk 必须先于其落位", async () => {
    const sid = await startTurn("查询 1-7");

    useProcessingStore.getState().appendCurrentEvent(textChunk(sid, "我先查一下"));
    useProcessingStore.getState().appendCurrentEvent(textChunk(sid, "数据"));
    // 不手动 flush，直接推 tool/call —— 应触发前置 flush
    useProcessingStore.getState().appendCurrentEvent(toolCall(sid, "call_1"));

    const events = currentEventsOf(sid);
    expect(events.map((e) => e.type)).toEqual([
      "user/message",
      "assistant/chunk",
      "assistant/chunk",
      "tool/call",
    ]);
    // chunk 顺序保持（缓冲 FIFO）
    expect(events[1]).toEqual(textChunk(sid, "我先查一下"));
    expect(events[2]).toEqual(textChunk(sid, "数据"));

    useProcessingStore.getState().finishProcessing(sid);
  });

  it("assistant/message 同样触发前置 flush（chunk 全落位后才追加聚合）", async () => {
    const sid = await startTurn("查询 1-7");

    useProcessingStore.getState().appendCurrentEvent(textChunk(sid, "答案一"));
    useProcessingStore.getState().appendCurrentEvent(textChunk(sid, "答案二"));
    useProcessingStore.getState().appendCurrentEvent({
      type: "assistant/message",
      sessionId: sid,
      content: "答案一答案二",
      reasoningContent: null,
      usage: null,
    });

    const events = currentEventsOf(sid);
    expect(events).toHaveLength(4); // user + 2 chunks + assistant/message
    expect(events[events.length - 1].type).toBe("assistant/message");

    useProcessingStore.getState().finishProcessing(sid);
  });
});

// ═══════════════════════════════════════════════
// 场景 3：turn/end 强 flush
// ═══════════════════════════════════════════════

describe("节流缓冲 · turn/end 强 flush", () => {
  it("finishProcessing 前未手动 flush，缓冲 chunk 仍完整归档", async () => {
    const sid = await startTurn("查询 1-7");

    useProcessingStore.getState().appendCurrentEvent(textChunk(sid, "关卡 1-7"));
    useProcessingStore.getState().appendCurrentEvent(textChunk(sid, "是主线第一章，"));
    useProcessingStore.getState().appendCurrentEvent(textChunk(sid, "消耗 6 点理智。"));

    // 不 flush，直接归档（真实后端路径：turn/end → finishProcessing）
    useProcessingStore.getState().finishProcessing(sid);

    const conv = useConversationStore.getState().conversations[0];
    expect(conv.turns).toHaveLength(1);
    const turn = conv.turns[0];
    expect(turn.filter((e) => e.type === "assistant/chunk")).toHaveLength(3);
    // 归档后缓冲应已被清空（flush 幂等验证）
    flushPending(sid);
    expect(useConversationStore.getState().conversations[0].turns).toHaveLength(1);
  });
});

// ═══════════════════════════════════════════════
// 场景 4：failProcessing 清理缓冲
// ═══════════════════════════════════════════════

describe("节流缓冲 · failProcessing 清理", () => {
  it("未 flush chunk 在失败路径被丢弃，错误轮只归档 [user, error]", async () => {
    const sid = await startTurn("查询 1-7");

    useProcessingStore.getState().appendCurrentEvent(textChunk(sid, "半截输出"));
    useProcessingStore.getState().failProcessing(sid, "查询失败：连接超时");

    // 错误轮归档：无 chunk
    const conv = useConversationStore.getState().conversations[0];
    expect(conv.turns).toHaveLength(1);
    expect(conv.turns[0].map((e) => e.type)).toEqual([
      "user/message",
      "error",
    ]);

    // 缓冲已清理：对已归档 session 的 flushPending 无副作用、不再写
    expect(() => flushPending(sid)).not.toThrow();
    expect(useConversationStore.getState().conversations[0].turns).toHaveLength(1);
  });
});

// ═══════════════════════════════════════════════
// 场景 5：33ms 窗口合并（fake timers）
// ═══════════════════════════════════════════════

describe("节流缓冲 · 33ms 窗口合并", () => {
  it("窗口内多事件累积，一次 flush 全部落位；后续事件重新成批", () => {
    vi.useFakeTimers();
    const store = useProcessingStore.getState();

    // 直接构造快照（绕过 sendMessage 的 async，配合 fake timers 更稳）
    const sid = "throttle-session-1";
    useProcessingStore.setState({
      processing: {
        [sid]: {
          conversationId: "conv-throttle",
          currentEvents: [
            { type: "user/message", sessionId: sid, content: "查询" },
          ],
          agentPhase: "streaming",
          mood: "平静",
          moodIntensity: 0,
        },
      },
    });

    // 第一批 chunk：进入缓冲，尚未 flush
    store.appendCurrentEvent(textChunk(sid, "A"));
    store.appendCurrentEvent(textChunk(sid, "B"));
    expect(currentEventsOf(sid)).toHaveLength(1); // 只有 user/message

    // 推进 33ms → 窗口到期 → 批量落位
    vi.advanceTimersByTime(33);
    expect(currentEventsOf(sid).map((e) => e.type)).toEqual([
      "user/message",
      "assistant/chunk",
      "assistant/chunk",
    ]);

    // 第二批：新窗口重新计时
    store.appendCurrentEvent(textChunk(sid, "C"));
    store.appendCurrentEvent(textChunk(sid, "D"));
    expect(currentEventsOf(sid)).toHaveLength(3); // C/D 仍在缓冲
    vi.advanceTimersByTime(33);
    expect(currentEventsOf(sid)).toHaveLength(5);

    // 清理：强 flush + 归档（该快照的 conversationId 在 conversation store 中不存在，
    // appendTurn 不会命中任何对话——只验证缓冲清理路径）
    useProcessingStore.getState().finishProcessing(sid);
    expect(() => flushPending(sid)).not.toThrow();
  });

  it("距上次 flush ≥ 33ms 时新 chunk 立即 flush（不排队）", () => {
    vi.useFakeTimers();
    const store = useProcessingStore.getState();
    const sid = "throttle-session-2";
    useProcessingStore.setState({
      processing: {
        [sid]: {
          conversationId: "conv-throttle-2",
          currentEvents: [
            { type: "user/message", sessionId: sid, content: "查询" },
          ],
          agentPhase: "streaming",
          mood: "平静",
          moodIntensity: 0,
        },
      },
    });

    store.appendCurrentEvent(textChunk(sid, "A"));
    vi.advanceTimersByTime(33); // 第一批落位
    expect(currentEventsOf(sid)).toHaveLength(2);

    // 推进到距上次 flush ≥ 33ms 后：新 chunk 立即 flush
    vi.advanceTimersByTime(40);
    store.appendCurrentEvent(textChunk(sid, "B"));
    expect(currentEventsOf(sid)).toHaveLength(3); // 立即落位，无需再等窗口

    useProcessingStore.getState().finishProcessing(sid);
  });
});
