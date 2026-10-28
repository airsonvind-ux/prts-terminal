// src/stores/__tests__/processing.mood.test.ts
// 情绪标签 · 处理链路测试（批 2-D 动画映射 · 情绪通道）
//
// 复用既有 fake-timer / 直接调 store actions 的测试约定（processing.throttle.test.ts）。
// 覆盖：
//   1. 跨 33ms flush 边界标签：pending 扣留不落位，闭合后 mood 正确 + currentEvents 干净
//   2. 无标签轮次逐字节一致（剥离器零副作用，mood 保持平静）
//   3. reasoning chunk 不进扫描器（标签形态文本原样缓冲，不启动 pending）
//   4. 工具型轮次：tool/call 段边界 re-arm 后段首标签被消费
//   5. 段内二次标签忽略；跨段可再消费（轨迹语义，一轮多情绪）
//   6. turn/end 归档：chunk 与 assistant/message 均已剥离 + mood 置位
//   7. finishProcessing 未闭合 pending 放行（半标签最终可见）
//   8. failProcessing 清理：pending 随缓冲丢弃，错误轮只归档 [user, error]
//   9. 非法枚举不破坏归档（标签剥离，mood 保持平静）
//  10. 扫描器状态按 session 隔离（每轮一个 sessionId，互不串扰）
// 新增覆盖（批 2-D 强度档位 + mood/event 事件）：
//  11. 强度解析：intensity 档位置位；非法强度（:0、:4、:x）回退 0；省略 → 0
//  12. mood/event 事件：流式匹配先于剥离文本落位、携带 mood/intensity、
//      归档 turns 含 mood/event、assistant/message 防重（流式已消费不重复插）

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
  resetScanners(); // 清空情绪扫描器模块级瞬态（与 chunkBuffers 并列的生命周期）
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
  it("pending 扣留不落位；闭合后 mood 正确 + currentEvents 干净", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:高"));

    // 手动 flush（模拟 33ms 窗口到期）——扣留的半标签不应落位
    flushPending(sid);
    expect(currentEventsOf(sid)).toHaveLength(1); // 只有 user/message

    store.appendCurrentEvent(textChunk(sid, "兴】好的。"));
    flushPending(sid);

    const events = currentEventsOf(sid);
    expect(events.map((e) => e.type)).toEqual([
      "user/message",
      "mood/event",
      "assistant/chunk",
    ]);
    // 时序：前段文本 → 情绪事件 → 本段剥离文本
    expect(events[1]).toEqual({
      type: "mood/event",
      sessionId: sid,
      mood: "高兴",
      intensity: 0,
    });
    expect(events[2]).toEqual(
      textChunk(sid, "好的。", "【mood:高兴】好的。")
    ); // 展示已剥离，rawContent 保留原文
    expect(moodOf(sid)).toBe("高兴");
    expect(intensityOf(sid)).toBe(0);

    useProcessingStore.getState().finishProcessing(sid);
  });

  it("逐 token 切开的中文括号标签仍产 mood/event", async () => {
    const sid = await startTurn("测试");
    const store = useProcessingStore.getState();

    for (const part of ["【", "m", "ood", ":", "困惑", "】\n\n", "正文"]) {
      store.appendCurrentEvent(textChunk(sid, part));
    }
    flushPending(sid);

    const events = currentEventsOf(sid);
    expect(events.map((e) => e.type)).toEqual([
      "user/message",
      "mood/event",
      "assistant/chunk",
      "assistant/chunk",
    ]);
    expect(events[1]).toEqual({
      type: "mood/event",
      sessionId: sid,
      mood: "困惑",
      intensity: 0,
    });
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

  it("reasoning chunk 不进扫描器：标签形态文本原样缓冲，不启动 pending", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(reasoningChunk(sid, "【mood:高"));
    // 若 reasoning 进了扫描器，此文本 chunk 会闭合 pending 并剥离；实际不会
    store.appendCurrentEvent(textChunk(sid, "兴】答案。"));
    flushPending(sid);

    const events = currentEventsOf(sid);
    expect(events[1]).toEqual(reasoningChunk(sid, "【mood:高"));
    expect(events[2]).toEqual(textChunk(sid, "兴】答案。")); // 原样（非段首标签不剥离）
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
    store.appendCurrentEvent(toolCall(sid, "c1")); // 段边界 re-arm
    store.appendCurrentEvent(textChunk(sid, "【mood:困惑】查到了，让我想想。"));
    flushPending(sid);

    const events = currentEventsOf(sid);
    expect(events.map((e) => e.type)).toEqual([
      "user/message",
      "assistant/chunk",
      "tool/call",
      "mood/event",
      "assistant/chunk",
    ]);
    expect(events[1]).toEqual(textChunk(sid, "我先查一下数据库。"));
    expect(events[3]).toEqual({
      type: "mood/event",
      sessionId: sid,
      mood: "困惑",
      intensity: 0,
    });
    expect(events[4]).toEqual(
      textChunk(sid, "查到了，让我想想。", "【mood:困惑】查到了，让我想想。")
    );
    expect(moodOf(sid)).toBe("困惑");

    useProcessingStore.getState().finishProcessing(sid);
  });

  it("跨段二次标签可再消费（轨迹）：两段两情绪各产一条事件", async () => {
    const sid = await startTurn("查询");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:高兴】第一个。"));
    store.appendCurrentEvent(toolCall(sid, "c1"));
    store.appendCurrentEvent(textChunk(sid, "【mood:沮丧】第二个。"));
    flushPending(sid);

    const events = currentEventsOf(sid);
    expect(events.map((e) => e.type)).toEqual([
      "user/message",
      "mood/event",
      "assistant/chunk",
      "tool/call",
      "mood/event",
      "assistant/chunk",
    ]);
    // 段边界可再消费：两条事件成轨迹（顺序＝时间）
    const moodEvents = events.filter((e) => e.type === "mood/event") as Array<{
      mood: string;
    }>;
    expect(moodEvents.map((e) => e.mood)).toEqual(["高兴", "沮丧"]);
    expect(events[5]).toEqual(
      textChunk(sid, "第二个。", "【mood:沮丧】第二个。")
    );
    expect(moodOf(sid)).toBe("沮丧"); // 落点＝最后一段

    useProcessingStore.getState().finishProcessing(sid);
  });

  it("轨迹全链路：两段两情绪 + 聚合防重（本段已消费不重复插）", async () => {
    const sid = await startTurn("查询");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:高兴:1】我先查一下。"));
    flushPending(sid);
    store.appendCurrentEvent(toolCall(sid, "c1"));
    store.appendCurrentEvent(textChunk(sid, "【mood:困惑:2】查到了，结论如下。"));
    flushPending(sid);
    // 段完成聚合（后端原始聚合，携带标签）→ 本段已消费，应只剥离不补插
    store.appendCurrentEvent({
      type: "assistant/message",
      sessionId: sid,
      content: "【mood:困惑:2】查到了，结论如下。",
      reasoningContent: null,
      usage: null,
    });

    const events = currentEventsOf(sid);
    const moodEvents = events.filter((e) => e.type === "mood/event") as Array<{
      mood: string;
      intensity: number;
    }>;
    expect(moodEvents).toHaveLength(2); // 防重：不重复插
    expect(moodEvents.map((e) => [e.mood, e.intensity])).toEqual([
      ["高兴", 1],
      ["困惑", 2],
    ]);
    expect(moodOf(sid)).toBe("困惑"); // 落点＝最后一段
    expect(intensityOf(sid)).toBe(2);

    store.finishProcessing(sid);
    const turn = useConversationStore.getState().conversations[0].turns[0];
    expect(turn.filter((e) => e.type === "mood/event")).toHaveLength(2);
  });

  it("轮内注入也是段边界：注入后新回答的标签可再消费", async () => {
    const sid = await startTurn("查询");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:高兴】先答一段。"));
    flushPending(sid);
    // 轮内注入（插话）：user/message 到达 → 段边界（重置段首与消费位）
    store.appendCurrentEvent({
      type: "user/message",
      sessionId: sid,
      content: "追问一句",
    });
    store.appendCurrentEvent(textChunk(sid, "【mood:困惑】补答一段。"));
    flushPending(sid);

    const events = currentEventsOf(sid);
    const moodEvents = events.filter((e) => e.type === "mood/event") as Array<{
      mood: string;
    }>;
    expect(moodEvents.map((e) => e.mood)).toEqual(["高兴", "困惑"]);
    expect(moodOf(sid)).toBe("困惑");

    useProcessingStore.getState().finishProcessing(sid);
  });
});

// ═══════════════════════════════════════════════
// 场景 4：turn/end 归档
// ═══════════════════════════════════════════════

describe("场景 4：turn/end 归档", () => {
  it("归档 content 已剥离 + mood 置位；快照删除后 mood 随快照清除", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:高兴】答案在此。"));
    store.appendCurrentEvent({
      type: "assistant/message",
      sessionId: sid,
      content: "【mood:高兴】答案在此。",
      reasoningContent: null,
      usage: null,
    });
    expect(moodOf(sid)).toBe("高兴"); // chunk 匹配时已置位

    store.finishProcessing(sid);

    const conv = useConversationStore.getState().conversations[0];
    expect(conv.turns).toHaveLength(1);
    const turn = conv.turns[0];
    // 时序：user → mood/event → chunk → assistant/message → turn/end
    expect(turn.map((e) => e.type)).toEqual([
      "user/message",
      "mood/event",
      "assistant/chunk",
      "assistant/message",
      "turn/end",
    ]);
    // chunk 剥离
    expect(turn[2].type).toBe("assistant/chunk");
    expect((turn[2] as { content: string }).content).toBe("答案在此。");
    // assistant/message 剥离（防止归档瞬间标签复活）
    const msg = turn[turn.length - 2];
    expect(msg.type).toBe("assistant/message");
    expect((msg as { content: string }).content).toBe("答案在此。");

    // 快照已删除；mood 随快照自动清除
    expect(useProcessingStore.getState().processing).toEqual({});
  });

  it("finishProcessing 未闭合 pending 放行：半标签最终以 chunk 落位归档", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:高"));
    store.finishProcessing(sid); // 未手动 flush（真实路径：turn/end → finishProcessing）
    const conv = useConversationStore.getState().conversations[0];
    const turn = conv.turns[0];
    const chunks = turn.filter((e) => e.type === "assistant/chunk");
    expect(chunks).toHaveLength(1);
    expect((chunks[0] as { content: string }).content).toBe("【mood:高");
    expect(useProcessingStore.getState().processing).toEqual({});
  });
});

// ═══════════════════════════════════════════════
// 场景 5：failProcessing 清理
// ═══════════════════════════════════════════════

describe("场景 5：failProcessing 清理", () => {
  it("未闭合 pending 随缓冲丢弃：错误轮只归档 [user, error]，mood 随快照清除", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:高"));
    store.failProcessing(sid, "查询失败：连接超时");

    const conv = useConversationStore.getState().conversations[0];
    expect(conv.turns).toHaveLength(1);
    expect(conv.turns[0].map((e) => e.type)).toEqual(["user/message", "error"]);

    expect(useProcessingStore.getState().processing).toEqual({});
    // 扫描器状态已清空：新一轮可重新匹配
    const sid2 = await startTurn("下一轮");
    store.appendCurrentEvent(textChunk(sid2, "【mood:沮丧】好的。"));
    flushPending(sid2);
    expect(useProcessingStore.getState().processing[sid2].mood).toBe("沮丧");
    useProcessingStore.getState().finishProcessing(sid2);
  });
});

// ═══════════════════════════════════════════════
// 场景 6：非法枚举不破坏归档
// ═══════════════════════════════════════════════

describe("场景 6：非法枚举不破坏归档", () => {
  it("非法标签剥离后归档，mood 保持平静", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:愤怒】提示"));
    flushPending(sid);
    expect(moodOf(sid)).toBe("平静"); // 非法值不更新 mood
    expect(intensityOf(sid)).toBe(0);

    store.finishProcessing(sid);
    const conv = useConversationStore.getState().conversations[0];
    const turn = conv.turns[0];
    const chunk = turn[1];
    expect(chunk.type).toBe("assistant/chunk");
    expect((chunk as { content: string }).content).toBe("提示"); // 只剥离，不更新
    expect(useProcessingStore.getState().processing).toEqual({});
  });
});

// ═══════════════════════════════════════════════
// 场景 7：扫描器按 session 隔离
// ═══════════════════════════════════════════════

describe("场景 7：扫描器按 session 隔离", () => {
  it("两个并发轮次互不串扰：各自的标签各自消费", () => {
    const store = useProcessingStore.getState();

    // 直接构造两个快照（绕过 sendMessage async，配合并发场景更稳）
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

    store.appendCurrentEvent(textChunk(sidA, "【mood:高兴】A 的答案。"));
    store.appendCurrentEvent(textChunk(sidB, "【mood:困惑】B 的答案。"));
    flushPending(sidA);
    flushPending(sidB);

    expect(moodOf(sidA)).toBe("高兴");
    expect(moodOf(sidB)).toBe("困惑");
    expect(currentEventsOf(sidA)[1]).toEqual({
      type: "mood/event",
      sessionId: sidA,
      mood: "高兴",
      intensity: 0,
    });
    expect(currentEventsOf(sidA)[2]).toEqual(
      textChunk(sidA, "A 的答案。", "【mood:高兴】A 的答案。")
    );
    expect(currentEventsOf(sidB)[1]).toEqual({
      type: "mood/event",
      sessionId: sidB,
      mood: "困惑",
      intensity: 0,
    });
    expect(currentEventsOf(sidB)[2]).toEqual(
      textChunk(sidB, "B 的答案。", "【mood:困惑】B 的答案。")
    );

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

    useProcessingStore.getState().appendCurrentEvent(
      textChunk(sid, "【mood:高兴:2】内容。")
    );
    flushPending(sid);

    const snap = useProcessingStore.getState().processing[sid];
    expect(snap.mood).toBe("高兴");
    expect(snap.moodIntensity).toBe(2);
    expect(currentEventsOf(sid)[1]).toEqual({
      type: "mood/event",
      sessionId: sid,
      mood: "高兴",
      intensity: 2,
    });

    useProcessingStore.getState().finishProcessing(sid);
  });

  it("省略强度 → 0（基础档）", async () => {
    const sid = await startTurn("测试");

    useProcessingStore.getState().appendCurrentEvent(
      textChunk(sid, "【mood:困惑】内容。")
    );
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

      useProcessingStore.getState().appendCurrentEvent(
        textChunk(sid, tag + "内容。")
      );
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
  it("流式匹配：mood/event 先于剥离文本落位（appendImmediate 路径）", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sid, "【mood:沮丧:3】好的，我明白了。"));

    // mood/event 立即落位；剥离文本仍在 33ms 缓冲中
    const events = currentEventsOf(sid);
    expect(events.map((e) => e.type)).toEqual(["user/message", "mood/event"]);
    expect(events[1]).toEqual({
      type: "mood/event",
      sessionId: sid,
      mood: "沮丧",
      intensity: 3,
    });

    flushPending(sid);
    const flushed = currentEventsOf(sid);
    expect(flushed.map((e) => e.type)).toEqual([
      "user/message",
      "mood/event",
      "assistant/chunk",
    ]);
    expect(flushed[2]).toEqual(
      textChunk(sid, "好的，我明白了。", "【mood:沮丧:3】好的，我明白了。")
    );

    useProcessingStore.getState().finishProcessing(sid);
  });

  it("归档 turns 含 mood/event；assistant/message 防重（流式已消费不重复插）", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    // 流式 chunk 已消费标签（产出 mood/event）
    store.appendCurrentEvent(textChunk(sid, "【mood:高兴:2】答案在此。"));
    // 聚合再次带同一标签（后端原始聚合）→ 应只剥离不重复插事件
    store.appendCurrentEvent({
      type: "assistant/message",
      sessionId: sid,
      content: "【mood:高兴:2】答案在此。",
      reasoningContent: null,
      usage: null,
    });
    store.finishProcessing(sid);

    const turn = useConversationStore.getState().conversations[0].turns[0];
    expect(turn.map((e) => e.type)).toEqual([
      "user/message",
      "mood/event",
      "assistant/chunk",
      "assistant/message",
      "turn/end",
    ]);
    expect(turn.filter((e) => e.type === "mood/event")).toHaveLength(1); // 防重
    expect(turn[1]).toEqual({
      type: "mood/event",
      sessionId: sid,
      mood: "高兴",
      intensity: 2,
    });
    // 归档内容均已剥离
    expect((turn[2] as { content: string }).content).toBe("答案在此。");
    expect((turn[3] as { content: string }).content).toBe("答案在此。");
  });

  it("assistant/message 补插：流式未消费（聚合意外带标签）→ 补插 mood/event", async () => {
    const sid = await startTurn("测试");

    const store = useProcessingStore.getState();
    // 流式阶段：无标签文本（扫描器未消费）
    store.appendCurrentEvent(textChunk(sid, "我查一下。"));
    // 聚合意外带标签（异常路径：流式未匹配，如标签此前出现在段中）
    store.appendCurrentEvent({
      type: "assistant/message",
      sessionId: sid,
      content: "【mood:沮丧】我查一下。",
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
    expect(events[2]).toEqual({
      type: "mood/event",
      sessionId: sid,
      mood: "沮丧",
      intensity: 0,
    });
    expect(useProcessingStore.getState().processing[sid].mood).toBe("沮丧");
    expect(useProcessingStore.getState().processing[sid].moodIntensity).toBe(0);

    useProcessingStore.getState().finishProcessing(sid);
  });
});
