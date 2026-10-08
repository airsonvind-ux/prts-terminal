// @vitest-environment jsdom
// src/__tests__/agentEvents.reason.test.tsx
// M3-B §4.3：useAgentEvents turn/end 把 reason 传给 finishProcessing
//
// 端到端（React 层 + store）：挂载使用 useAgentEvents 的宿主 → 模拟后端事件流
// （半截 chunk → turn/end(aborted)）→ 归档轮含轮中止标记 + 快照清空；
// 对照组 turn/end(completed) → 无标记（reason 传递只触发 aborted 分支）。

import { describe, it, expect, beforeEach, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { useAgentEvents } from "../hooks/useAgentEvents";
import { useProcessingStore } from "../stores/useProcessingStore";
import { useConversationStore } from "../stores/useConversationStore";
import { useUIStore } from "../stores/useUIStore";
import { useToastStore } from "../stores/useToastStore";
import type { SessionEvent } from "../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

// ── 宿主组件：仅注册 useAgentEvents（事件监听副作用） ──
function EventHost() {
  useAgentEvents();
  return React.createElement("div", null, "host");
}

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

function mountHost(): void {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(React.createElement(EventHost));
  });
}

function resetAll(): void {
  useProcessingStore.setState({ processing: {} });
  useConversationStore.setState({ conversations: [], activeId: null });
  useUIStore.setState({
    errorMsg: null,
    overlay: null,
    restoring: false,
  });
  useToastStore.setState({ toasts: [] });
  __invokeMock.mockClear();
}

async function flush(): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
}

beforeEach(() => {
  resetAll();
  mountHost();
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  resetAll();
});

/** 模拟后端推送一条持久事实事件 */
function pushEvent(e: SessionEvent): void {
  act(() => {
    (globalThis as { __simulateAgentEvent: (e: unknown) => void }).__simulateAgentEvent(e as unknown as { type: string; sessionId: string });
  });
}

describe("useAgentEvents · turn/end reason 传递", () => {
  it("reason=aborted → 归档轮含轮中止标记 + 半截 chunk + 快照清空", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("中止测试");
    const sids = Object.keys(useProcessingStore.getState().processing);
    expect(sids).toHaveLength(1);
    const sid = sids[0];

    // 后端事件流：半截 chunk → turn/end(aborted)
    pushEvent({
      type: "assistant/chunk",
      sessionId: sid,
      kind: "text",
      content: "半截回答",
    });
    await flush(); // chunk 缓冲 flush 落位
    pushEvent({ type: "turn/end", sessionId: sid, reason: "aborted" });
    await flush();

    // 快照清空（按钮/输入自动回 ready）
    expect(useProcessingStore.getState().processing).toEqual({});

    // 归档轮：半截 chunk 保留 + 轮尾标记
    const conv = useConversationStore.getState().conversations[0];
    expect(conv.turns).toHaveLength(1);
    const turn = conv.turns[0];
    const texts = turn.filter(
      (e) => e.type === "assistant/chunk"
    ) as Array<{ content: string }>;
    expect(texts[0].content).toBe("半截回答");
    expect(turn[turn.length - 1]).toEqual({
      type: "turn/aborted",
      sessionId: sid,
    });
  });

  it("reason=completed → 无标记（reason 传递不改变正常轮行为）", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("正常完成");
    const sid = Object.keys(useProcessingStore.getState().processing)[0];

    pushEvent({
      type: "assistant/message",
      sessionId: sid,
      content: "完整回答",
      reasoningContent: null,
      usage: { inputTokens: 10, outputTokens: 5 },
    });
    pushEvent({ type: "turn/end", sessionId: sid, reason: "completed" });
    await flush();

    const turn = useConversationStore.getState().conversations[0].turns[0];
    expect(turn.some((e) => e.type === "turn/aborted")).toBe(false);
    expect(useProcessingStore.getState().processing).toEqual({});
  });
});
