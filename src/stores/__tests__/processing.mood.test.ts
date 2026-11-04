// src/stores/__tests__/processing.mood.test.ts
// 情绪标签 · 处理链路测试（MOOD v2）
//
// v2 语义变了：整行（`【mood:偏向】心声` 到行尾）是一个块，心声不进正文，
// 所以「剥离后正文」在多数场景里是空串——情绪卡自己承载那句心声。
// 覆盖：
//   1. 跨 flush 边界：半标签扣留、闭合后 mood/event 落位、正文为空
//   2. 无标签轮次逐字节一致；reasoning chunk 不进扫描器
//   3. 段边界（tool/call、轮内注入）后可再消费；每段最多一个
//   4. 归档：chunk 与 assistant/message 均已剥离，mood/event 防重
//   5. finish/fail 的 pending 生命周期
//   6. 未知偏向：产事件、定色相，但不驱动动作
//   7. 扫描器按 session 隔离
//   8. 强度档位与非法强度回退
//   9. mood/event 携带 tint/text，旧字段兼容

import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { useProcessingStore, flushPending } from "../useProcessingStore";
import { useConversationStore } from "../useConversationStore";
import { useUIStore } from "../useUIStore";
import { resetScanners } from "../../moodTag";
import type { SessionEvent } from "../../types";

// ── 辅助 ──

function resetAllStores(): void {
  useProcessingStore.setState({ processing: {} });
  useConversationStore.setState({ conversations: [], activeId: null });
  useUIStore.setState({
    errorMsg: null,
    overlay: null,
  });
  resetScanners();
  __invokeMock.mockClear();
}

/** 发送一条消息并返回 sessionId（快照预置 user/message + mood=平静 + moodIntensity=0） */
async function startTurn(input: string): Promise<string> {
  await useProcessingStore.getState().sendMessage(input);
  const processing = useProcessingStore.getState().processing;
  const sids = Object.keys(processing);
  expect(sids).toHaveLength(1);
  return sids[0];
}

function textChunk(
  sessionId: string,
  content: string,
  rawContent?: string
): SessionEvent {
  return {
    type: "assistant/chunk",
    sessionId,
    kind: "text",
    content,
    ...(rawContent ? { rawContent } : {}),
  };
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

/** 取当前快照的 mood */
function moodOf(sessionId: string): string {
  return useProcessingStore.getState().processing[sessionId].mood;
}

/** 取当前快照的 moodIntensity */
function intensityOf(sessionId: string): number {
  return useProcessingStore.getState().processing[sessionId].moodIntensity;
}

beforeEach(resetAllStores);
afterEach(() => vi.useRealTimers());

// ═══════════════════════════════════════════════
// 场景 1：跨 flush 边界标签
// ═══════════════════════════════════════════════

describe("场景 1：跨 flush 边界标签", () => {
  it("半标签扣留不落位；闭合且换行后 mood/event 落位，正文为空", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:高"));
    flushPending(sid);
    expect(currentEventsOf(sid).map((e) => e.type)).toEqual(["user/message"]);

    // 闭合 + 换行 → 整行成块，心声「好的。」不进正文
    store.appendCurrentEvent(textChunk(sid, "兴】好的。\n"));
    flushPending(sid);

    const events = currentEventsOf(sid);
    expect(events.map((e) => e.type)).toEqual(["user/message", "mood/event"]);
    expect(events[1]).toMatchObject({
      type: "mood/event",
      sessionId: sid,
      mood: "高兴",
      tint: "高兴",
      text: "好的。",
      intensity: 0,
    });
    expect(moodOf(sid)).toBe("高兴");

    useProcessingStore.getState().finishProcessing(sid);
  });

  it("逐 token 切开的中文括号标签：块在换行处落位，之后是正文", async () => {
    const sid = await startTurn("测试");
    const store = useProcessingStore.getState();

    for (const part of ["【", "m", "ood", ":", "困惑", "】", "\n", "正文"]) {
      store.appendCurrentEvent(textChunk(sid, part));
    }
    flushPending(sid);

    const events = currentEventsOf(sid);
    expect(events.map((e) => e.type)).toEqual([
      "user/message",
      "mood/event",
      "assistant/chunk",
    ]);
    expect(events[1]).toMatchObject({
      type: "mood/event",
      sessionId: sid,
      mood: "困惑",
      tint: "困惑",
    });
    expect(events[1]).not.toHaveProperty("text");
    // 正文从下一行开始，原样落位
    expect(events[2]).toEqual(textChunk(sid, "正文"));
    expect(moodOf(sid)).toBe("困惑");

    useProcessingStore.getState().finishProcessing(sid);
  });
});

// ═══════════════════════════════════════════════
// 场景 2：无标签轮次逐字节一致
// ═══════════════════════════════════════════════

describe("场景 2：无标签轮次逐字节一致", () => {
  it("剥离器零副作用：无标签 chunk 原样入列，mood 保持平静", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "关卡 1-7 是主线第一章，"));
    store.appendCurrentEvent(textChunk(sid, "消耗 6 点理智。"));
    flushPending(sid);

    const events = currentEventsOf(sid);
    expect(events).toHaveLength(3); // user + 2 chunks
    expect(events[1]).toEqual(textChunk(sid, "关卡 1-7 是主线第一章，"));
    expect(events[2]).toEqual(textChunk(sid, "消耗 6 点理智。"));
    expect(moodOf(sid)).toBe("平静");
    expect(intensityOf(sid)).toBe(0);

    useProcessingStore.getState().finishProcessing(sid);
  });

  it("reasoning chunk 不进扫描器：标签形态文本原样缓冲", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(reasoningChunk(sid, "【mood:高"));
    store.appendCurrentEvent(textChunk(sid, "兴】答案。"));
    flushPending(sid);

    const events = currentEventsOf(sid);
    expect(events[1]).toEqual(reasoningChunk(sid, "【mood:高"));
    // 行中标签（前面没有换行、也不是段首）不识别
    expect(events[2]).toEqual(textChunk(sid, "兴】答案。"));
    expect(moodOf(sid)).toBe("平静");

    useProcessingStore.getState().finishProcessing(sid);
  });
});

// ═══════════════════════════════════════════════
// 场景 3：轮内段边界（工具调用与注入）
// ═══════════════════════════════════════════════

describe("场景 3：轮内段边界（工具调用与注入）", () => {
  it("tool/call re-arm 后段首标签被消费", async () => {
    const sid = await startTurn("查询");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "我先查一下数据库。"));
    store.appendCurrentEvent(toolCall(sid, "c1"));
    store.appendCurrentEvent(textChunk(sid, "【mood:困惑】查到了，让我想想。\n"));
    flushPending(sid);

    const events = currentEventsOf(sid);
    expect(events.map((e) => e.type)).toEqual([
      "user/message",
      "assistant/chunk",
      "tool/call",
      "mood/event",
    ]);
    expect(events[1]).toEqual(textChunk(sid, "我先查一下数据库。"));
    expect(events[3]).toMatchObject({
      type: "mood/event",
      sessionId: sid,
      mood: "困惑",
      text: "查到了，让我想想。",
    });
    expect(moodOf(sid)).toBe("困惑");

    useProcessingStore.getState().finishProcessing(sid);
  });

  it("跨段二次标签可再消费：两段两情绪各产一条事件", async () => {
    const sid = await startTurn("查询");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:高兴】第一个。\n"));
    store.appendCurrentEvent(toolCall(sid, "c1"));
    store.appendCurrentEvent(textChunk(sid, "【mood:沮丧】第二个。\n"));
    flushPending(sid);

    const events = currentEventsOf(sid);
    expect(events.map((e) => e.type)).toEqual([
      "user/message",
      "mood/event",
      "tool/call",
      "mood/event",
    ]);
    const moodEvents = events.filter((e) => e.type === "mood/event");
    expect(moodEvents.map((e) => (e as { mood?: string }).mood)).toEqual([
      "高兴",
      "沮丧",
    ]);
    expect(moodOf(sid)).toBe("沮丧");

    useProcessingStore.getState().finishProcessing(sid);
  });

  it("同段第二个标签不处理（每段最多一个）", async () => {
    const sid = await startTurn("查询");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:高兴】第一段。\n"));
    store.appendCurrentEvent(textChunk(sid, "【mood:沮丧】同段第二行。\n"));
    flushPending(sid);

    const moodEvents = currentEventsOf(sid).filter(
      (e) => e.type === "mood/event"
    );
    expect(moodEvents).toHaveLength(1);
    expect(moodOf(sid)).toBe("高兴");

    useProcessingStore.getState().finishProcessing(sid);
  });

  it("轮内注入也是段边界：注入后新回答的标签可再消费", async () => {
    const sid = await startTurn("查询");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:高兴】先答一段。\n"));
    flushPending(sid);
    store.appendCurrentEvent({
      type: "user/message",
      sessionId: sid,
      content: "追问一句",
    });
    store.appendCurrentEvent(textChunk(sid, "【mood:困惑】补答一段。\n"));
    flushPending(sid);

    const moodEvents = currentEventsOf(sid).filter(
      (e) => e.type === "mood/event"
    );
    expect(moodEvents.map((e) => (e as { mood?: string }).mood)).toEqual([
      "高兴",
      "困惑",
    ]);
    expect(moodOf(sid)).toBe("困惑");

    useProcessingStore.getState().finishProcessing(sid);
  });
});

// ═══════════════════════════════════════════════
// 场景 4：turn/end 归档
// ═══════════════════════════════════════════════

describe("场景 4：turn/end 归档", () => {
  it("归档 content 已剥离；chunk 与 assistant/message 都不重复插事件", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(
      textChunk(sid, "【mood:高兴】答案在此。\n正文在此。")
    );
    store.appendCurrentEvent({
      type: "assistant/message",
      sessionId: sid,
      content: "【mood:高兴】答案在此。\n正文在此。",
      reasoningContent: null,
      usage: null,
    });
    expect(moodOf(sid)).toBe("高兴");

    store.finishProcessing(sid);

    const conv = useConversationStore.getState().conversations[0];
    const turn = conv.turns[0];
    expect(turn.map((e) => e.type)).toEqual([
      "user/message",
      "mood/event",
      "assistant/chunk",
      "assistant/message",
      "turn/end",
    ]);
    expect(turn.filter((e) => e.type === "mood/event")).toHaveLength(1);
    expect((turn[2] as { content: string }).content).toBe("正文在此。");
    expect((turn[3] as { content: string }).content).toBe("正文在此。");

    expect(useProcessingStore.getState().processing).toEqual({});
  });

  it("finishProcessing 未闭合 pending 放行：半标签最终以 chunk 落位归档", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:高"));
    store.finishProcessing(sid);
    const conv = useConversationStore.getState().conversations[0];
    const chunks = conv.turns[0].filter((e) => e.type === "assistant/chunk");
    expect(chunks).toHaveLength(1);
    expect((chunks[0] as { content: string }).content).toBe("【mood:高");
    expect(useProcessingStore.getState().processing).toEqual({});
  });
});

// ═══════════════════════════════════════════════
// 场景 5：failProcessing 清理
// ═══════════════════════════════════════════════

describe("场景 5：failProcessing 清理", () => {
  it("未闭合 pending 随缓冲丢弃：错误轮只归档 [user, error]", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:高"));
    store.failProcessing(sid, "查询失败：连接超时");

    const conv = useConversationStore.getState().conversations[0];
    expect(conv.turns[0].map((e) => e.type)).toEqual(["user/message", "error"]);
    expect(useProcessingStore.getState().processing).toEqual({});

    const sid2 = await startTurn("下一轮");
    store.appendCurrentEvent(textChunk(sid2, "【mood:沮丧】好的。\n"));
    flushPending(sid2);
    expect(useProcessingStore.getState().processing[sid2].mood).toBe("沮丧");
    useProcessingStore.getState().finishProcessing(sid2);
  });
});

// ═══════════════════════════════════════════════
// 场景 6：未知偏向
// ═══════════════════════════════════════════════

describe("场景 6：未知偏向", () => {
  it("自由偏向产事件、保留原词做色相，但不驱动动作", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:愤怒】提示\n"));
    flushPending(sid);

    const events = currentEventsOf(sid);
    expect(moodOf(sid)).toBe("平静"); // 未知词不触发动作
    expect(events.find((e) => e.type === "mood/event")).toMatchObject({
      type: "mood/event",
      sessionId: sid,
      tint: "愤怒",
      text: "提示",
      intensity: 0,
    });
    expect(
      (events.find((e) => e.type === "mood/event") as { mood?: string }).mood
    ).toBeUndefined();

    store.finishProcessing(sid);
  });
});

// ═══════════════════════════════════════════════
// 场景 7：扫描器按 session 隔离
// ═══════════════════════════════════════════════

describe("场景 7：扫描器按 session 隔离", () => {
  it("两个并发轮次互不串扰：各自的标签各自消费", () => {
    const store = useProcessingStore.getState();

    const sidA = "mood-session-a";
    const sidB = "mood-session-b";
    useProcessingStore.setState({
      processing: {
        [sidA]: {
          conversationId: "conv-a",
          currentEvents: [{ type: "user/message", sessionId: sidA, content: "A" }],
          agentPhase: "streaming",
          mood: "平静",
          moodIntensity: 0,
        },
        [sidB]: {
          conversationId: "conv-b",
          currentEvents: [{ type: "user/message", sessionId: sidB, content: "B" }],
          agentPhase: "streaming",
          mood: "平静",
          moodIntensity: 0,
        },
      },
    });

    store.appendCurrentEvent(textChunk(sidA, "【mood:高兴】A 的答案。\n"));
    store.appendCurrentEvent(textChunk(sidB, "【mood:困惑】B 的答案。\n"));
    flushPending(sidA);
    flushPending(sidB);

    expect(moodOf(sidA)).toBe("高兴");
    expect(moodOf(sidB)).toBe("困惑");
    expect(currentEventsOf(sidA)[1]).toMatchObject({
      type: "mood/event",
      sessionId: sidA,
      mood: "高兴",
      text: "A 的答案。",
    });
    expect(currentEventsOf(sidB)[1]).toMatchObject({
      type: "mood/event",
      sessionId: sidB,
      mood: "困惑",
      text: "B 的答案。",
    });

    useProcessingStore.getState().finishProcessing(sidA);
    useProcessingStore.getState().finishProcessing(sidB);
  });
});

// ═══════════════════════════════════════════════
// 场景 8：情绪强度解析
// ═══════════════════════════════════════════════

describe("场景 8：情绪强度解析", () => {
  it("标签带强度：mood + moodIntensity 同时置位，事件携带强度", async () => {
    const sid = await startTurn("测试");

    useProcessingStore
      .getState()
      .appendCurrentEvent(textChunk(sid, "【mood:高兴:2】内容。\n"));
    flushPending(sid);

    const snap = useProcessingStore.getState().processing[sid];
    expect(snap.mood).toBe("高兴");
    expect(snap.moodIntensity).toBe(2);
    expect(currentEventsOf(sid)[1]).toMatchObject({
      type: "mood/event",
      sessionId: sid,
      mood: "高兴",
      intensity: 2,
      text: "内容。",
    });

    useProcessingStore.getState().finishProcessing(sid);
  });

  it("省略强度 → 0（基础档）", async () => {
    const sid = await startTurn("测试");

    useProcessingStore
      .getState()
      .appendCurrentEvent(textChunk(sid, "【mood:困惑】内容。\n"));
    flushPending(sid);

    const snap = useProcessingStore.getState().processing[sid];
    expect(snap.mood).toBe("困惑");
    expect(snap.moodIntensity).toBe(0);

    useProcessingStore.getState().finishProcessing(sid);
  });

  it.each(["【mood:高兴:0】", "【mood:高兴:4】", "【mood:高兴:x】"])(
    "非法强度 %s → intensity 回退 0（mood 仍生效）",
    async (tag) => {
      const sid = await startTurn("测试");

      useProcessingStore
        .getState()
        .appendCurrentEvent(textChunk(sid, tag + "内容。\n"));
      flushPending(sid);

      const snap = useProcessingStore.getState().processing[sid];
      expect(snap.mood).toBe("高兴");
      expect(snap.moodIntensity).toBe(0);

      useProcessingStore.getState().finishProcessing(sid);
    }
  );
});

// ═══════════════════════════════════════════════
// 场景 9：mood/event 情绪事件
// ═══════════════════════════════════════════════

describe("场景 9：mood/event 情绪事件", () => {
  it("流式匹配：mood/event 立即落位，不产生正文 chunk", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:沮丧:3】好的，我明白了。\n"));

    const events = currentEventsOf(sid);
    expect(events.map((e) => e.type)).toEqual(["user/message", "mood/event"]);
    expect(events[1]).toMatchObject({
      type: "mood/event",
      sessionId: sid,
      mood: "沮丧",
      intensity: 3,
      text: "好的，我明白了。",
    });

    flushPending(sid);
    expect(currentEventsOf(sid).map((e) => e.type)).toEqual([
      "user/message",
      "mood/event",
    ]);

    useProcessingStore.getState().finishProcessing(sid);
  });

  it("assistant/message 补插：流式未消费（聚合意外带标签）→ 补插 mood/event", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    // 流式阶段：无标签文本（扫描器未消费）
    store.appendCurrentEvent(textChunk(sid, "我查一下。"));
    // 聚合意外带标签（异常路径）
    store.appendCurrentEvent({
      type: "assistant/message",
      sessionId: sid,
      content: "【mood:沮丧】我查一下。\n正文",
      reasoningContent: null,
      usage: null,
    });
    flushPending(sid);

    const events = currentEventsOf(sid);
    expect(events.map((e) => e.type)).toEqual([
      "user/message",
      "assistant/chunk",
      "mood/event",
      "assistant/message",
    ]);
    expect(events[2]).toMatchObject({
      type: "mood/event",
      sessionId: sid,
      mood: "沮丧",
      tint: "沮丧",
      text: "我查一下。",
      intensity: 0,
    });
    expect(useProcessingStore.getState().processing[sid].mood).toBe("沮丧");

    useProcessingStore.getState().finishProcessing(sid);
  });
});
