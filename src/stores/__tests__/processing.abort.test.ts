// src/stores/__tests__/processing.abort.test.ts
// M3-B §4 aborted 归档语义：finishProcessing(sessionId, reason) 合成轮中止标记
//
// 覆盖（§8 前端归档项）：
//   1. reason === "aborted" → 归档轮含半截 chunk + 轮尾 turn/aborted 标记
//   2. processing 快照清空；usage null → append_turn contextTokens 不覆盖
//   3. 其余 reason / 缺省 reason（旧调用兼容）→ 无标记（回归）
//   4. getProcessingSessionId 命中/未命中/边界
//   5. 派生层约定：deriveMessages 忽略标记（半截成完整 assistant）、
//      deriveTextSegments 显示半截、isMetaEvent 不含标记
//   6. markAborting：中止反馈态（乐观置位/撤回、随快照消失）

import { describe, it, expect, beforeEach } from "vitest";
import { useProcessingStore, markAborting } from "../useProcessingStore";
import { useConversationStore } from "../useConversationStore";
import {
  deriveMessages,
  deriveTextSegments,
  isMetaEvent,
} from "../../projection";
import type { SessionEvent } from "../../types";

function resetAllStores(): void {
  useProcessingStore.setState({ processing: {} });
  useConversationStore.setState({ conversations: [], activeId: null });
  __invokeMock.mockClear();
}

function chunk(sid: string, content: string): SessionEvent {
  return { type: "assistant/chunk", sessionId: sid, kind: "text", content };
}

function getSingle(): { sid: string } {
  const sids = Object.keys(useProcessingStore.getState().processing);
  expect(sids).toHaveLength(1);
  return { sid: sids[0] };
}

beforeEach(resetAllStores);

// ═══════════════════════════════════════════════════
// aborted 归档
// ═══════════════════════════════════════════════════

describe("finishProcessing(sid, 'aborted') 归档语义", () => {
  it("轮含半截 chunk + 轮尾 turn/aborted 标记；processing 清空", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("中止测试");

    const { sid } = getSingle();
    store.appendCurrentEvent(chunk(sid, "半截回答"));
    // turn/end(aborted) 到达 → 归档（finishProcessing 内部强 flush chunk）
    useProcessingStore.getState().finishProcessing(sid, "aborted");

    // 快照已删除（deriveUIState → ready）
    expect(useProcessingStore.getState().processing).toEqual({});

    // 归档轮事件：user + 半截 chunk + 轮尾标记（标记为最后一条）
    const conv = useConversationStore.getState().conversations[0];
    expect(conv.turns).toHaveLength(1);
    const turn = conv.turns[0];
    expect(turn[0].type).toBe("user/message");
    expect(turn[1]).toEqual(chunk(sid, "半截回答"));
    const last = turn[turn.length - 1];
    expect(last).toEqual({ type: "turn/aborted", sessionId: sid });

    // 持久化：append_turn events_json 原样含标记（存储透传 → 重启可见）
    const appendCalls = __invokeMock.mock.calls.filter(
      (c) => c[0] === "append_turn"
    );
    expect(appendCalls).toHaveLength(1);
    const archived = JSON.parse(appendCalls[0][1].eventsJson) as SessionEvent[];
    expect(archived[archived.length - 1]).toEqual({
      type: "turn/aborted",
      sessionId: sid,
    });
    // contextTokens 缺省（usage null → 不覆盖旧值；新对话 meta 为 null）
    expect(appendCalls[0][1].contextTokens).toBeNull();
  });

  it("max-tokens 轮：保留半截回答 + 轮尾 turn/truncated 标记", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("截断测试");

    const { sid } = getSingle();
    store.appendCurrentEvent(chunk(sid, "半截回答"));
    useProcessingStore.getState().finishProcessing(sid, "max-tokens");

    const conv = useConversationStore.getState().conversations[0];
    expect(conv.turns).toHaveLength(1);
    const turn = conv.turns[0];
    expect(turn[1]).toEqual(chunk(sid, "半截回答"));
    expect(turn[turn.length - 1]).toEqual({
      type: "turn/truncated",
      sessionId: sid,
    });

    const appendCalls = __invokeMock.mock.calls.filter(
      (c) => c[0] === "append_turn"
    );
    expect(appendCalls).toHaveLength(1);
    const archived = JSON.parse(appendCalls[0][1].eventsJson) as SessionEvent[];
    expect(archived[archived.length - 1]).toEqual({
      type: "turn/truncated",
      sessionId: sid,
    });
  });

  it("有历史 contextTokens 的对话被中止轮归档后不覆盖", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("首轮");
    const { sid } = getSingle();
    const convId = useConversationStore.getState().activeId!;
    // 第一轮正常完成（assistant/message 携带 usage）
    store.appendCurrentEvent({
      type: "assistant/message",
      sessionId: sid,
      content: "完成回答",
      reasoningContent: null,
      usage: { inputTokens: 100, outputTokens: 10 },
    });
    store.finishProcessing(sid, "completed");
    expect(
      useConversationStore.getState().conversations[0].contextTokens
    ).toEqual({ inputTokens: 100, outputTokens: 10 });

    // 第二轮被中止：无 assistant/message → usage null → contextTokens 不变
    const store2 = useProcessingStore.getState();
    await store2.sendMessage("第二轮");
    const { sid: sid2 } = getSingle();
    store2.appendCurrentEvent(chunk(sid2, "半截"));
    useProcessingStore.getState().finishProcessing(sid2, "aborted");

    const conv = useConversationStore
      .getState()
      .conversations.find((c) => c.id === convId)!;
    expect(conv.contextTokens).toEqual({
      inputTokens: 100,
      outputTokens: 10,
    });
    // aborted 轮含标记
    expect(conv.turns[1][conv.turns[1].length - 1]).toEqual({
      type: "turn/aborted",
      sessionId: sid2,
    });
    // 第二次 append_turn 的 contextTokens 携带旧对话现值（不覆盖语义不变）
    const appendCalls = __invokeMock.mock.calls.filter(
      (c) => c[0] === "append_turn"
    );
    expect(appendCalls).toHaveLength(2);
    expect(appendCalls[1][1].contextTokens).toEqual({
      inputTokens: 100,
      outputTokens: 10,
    });
  });

  it("非 aborted reason（completed）与缺省 reason：不产标记（回归）", async () => {
    // completed
    const store = useProcessingStore.getState();
    await store.sendMessage("完成");
    const { sid } = getSingle();
    store.appendCurrentEvent({
      type: "assistant/message",
      sessionId: sid,
      content: "答",
      reasoningContent: null,
      usage: null,
    });
    store.finishProcessing(sid, "completed");
    let turn = useConversationStore.getState().conversations[0].turns[0];
    expect(turn.some((e) => e.type === "turn/aborted")).toBe(false);

    // 缺省 reason（旧调用路径兼容）
    const store2 = useProcessingStore.getState();
    await store2.sendMessage("无参");
    const { sid: sid2 } = getSingle();
    useProcessingStore.getState().finishProcessing(sid2);
    turn = useConversationStore.getState().conversations[0].turns[0];
    expect(turn.some((e) => e.type === "turn/aborted")).toBe(false);
  });

  it("快照已清理（failProcessing 先到）时幂等：不产孤儿轮", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("错误轮");
    const { sid } = getSingle();
    store.failProcessing(sid, "boom");
    useProcessingStore.getState().finishProcessing(sid, "aborted");
    expect(useConversationStore.getState().conversations[0].turns).toHaveLength(
      1
    );
    expect(
      useConversationStore.getState().conversations[0].turns[0].some(
        (e) => e.type === "turn/aborted"
      )
    ).toBe(false);
  });
});

// ═══════════════════════════════════════════════════
// getProcessingSessionId（中止按钮取参）
// ═══════════════════════════════════════════════════

describe("getProcessingSessionId", () => {
  it("命中返回 sessionId；他对话/无快照返回 undefined；归档后消失", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("A");
    const { sid } = getSingle();
    const convId = useConversationStore.getState().activeId!;

    expect(store.getProcessingSessionId(convId)).toBe(sid);
    expect(store.getProcessingSessionId("no-such-conv")).toBeUndefined();

    // 归档（中止）后快照删除 → undefined（按钮自动 disabled 的依据）
    useProcessingStore.getState().finishProcessing(sid, "aborted");
    expect(useProcessingStore.getState().getProcessingSessionId(convId)).toBeUndefined();
  });
});

// ═══════════════════════════════════════════════════
// markAborting（中止反馈：正在中止态）
// ═══════════════════════════════════════════════════

describe("markAborting", () => {
  it("置位/撤回落在快照上；轮收尾后随快照消失", async () => {
    await useProcessingStore.getState().sendMessage("中止反馈");
    const { sid } = getSingle();

    markAborting(sid, true);
    expect(useProcessingStore.getState().processing[sid].aborting).toBe(true);

    markAborting(sid, false);
    expect(useProcessingStore.getState().processing[sid].aborting).toBe(false);

    // 再置位后收尾：状态随快照一起消失，无需显式清理
    markAborting(sid, true);
    useProcessingStore.getState().finishProcessing(sid, "aborted");
    expect(useProcessingStore.getState().processing[sid]).toBeUndefined();
  });

  it("没有对应快照时无副作用（不新建条目）", () => {
    markAborting("no-such-session", true);
    expect(
      useProcessingStore.getState().processing["no-such-session"]
    ).toBeUndefined();
  });
});

// ═══════════════════════════════════════════════════
// 派生层约定（标记不可见但无害）
// ═══════════════════════════════════════════════════

describe("aborted 轮派生约定", () => {
  it("deriveMessages 忽略标记、半截 chunk 冲刷成完整 assistant；含标记轮不进 LLM 历史", () => {
    const sid = "s-abort";
    const turn: SessionEvent[] = [
      { type: "user/message", sessionId: sid, content: "问" },
      chunk(sid, "半截回答"),
      { type: "turn/aborted", sessionId: sid },
    ];
    const msgs = deriveMessages([turn]);
    expect(msgs).toEqual([
      { role: "user", content: "问" },
      { role: "assistant", content: "半截回答" },
    ]);
  });

  it("deriveTextSegments 显示半截（正常段）；turn/aborted 不参与分段", () => {
    const sid = "s-abort";
    const turn: SessionEvent[] = [
      chunk(sid, "半截回答"),
      { type: "turn/aborted", sessionId: sid },
    ];
    expect(deriveTextSegments(turn)).toEqual([
      { content: "半截回答", transition: false },
    ]);
  });

  it("isMetaEvent 不含 turn/aborted（不进事件卡片流）", () => {
    expect(
      isMetaEvent({ type: "turn/aborted", sessionId: "x" } as SessionEvent)
    ).toBe(false);
  });
});
