// @vitest-environment jsdom
// src/__tests__/agentEvents.steer.test.tsx
// 插话注入事件流（useAgentEvents → store 链路）：
// - user/message 事件（dispatch Completed 出口注入的轮内下一条 user）进入
//   processing 快照投影（currentEvents 多段数据源）+ pending 队首转正移除
// - turn/end 轮收尾防御清理 pending（后端队列空才发；此处验证兜底）
// - 中止（turn/end aborted）时 pending 已由中止侧清空（清空幂等）

import { describe, it, expect, beforeEach, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { useAgentEvents } from "../hooks/useAgentEvents";
import { useProcessingStore } from "../stores/useProcessingStore";
import { useConversationStore } from "../stores/useConversationStore";
import { useUIStore } from "../stores/useUIStore";
import { useToastStore } from "../stores/useToastStore";
import { useSteerStore } from "../stores/useSteerStore";
import type { SessionEvent } from "../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

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
    hiddenMode: null,
    errorMsg: null,
    overlay: null,
    restoring: false,
  });
  useToastStore.setState({ toasts: [] });
  useSteerStore.setState({ pendingBySession: {} });
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

function pushEvent(e: SessionEvent): void {
  act(() => {
    (globalThis as { __simulateAgentEvent: (e: unknown) => void }).__simulateAgentEvent(
      e as unknown as { type: string; sessionId: string }
    );
  });
}

describe("useAgentEvents · 插话注入事件流", () => {
  it("user/message 注入事件 → 进入投影缓存 + FIFO 队首 pending 转正", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("原始问题");
    const sid = Object.keys(useProcessingStore.getState().processing)[0];

    // 模拟前端插话提交成功登记的两条 pending
    useSteerStore.getState().addPending(sid, { id: "0", text: "插话一" });
    useSteerStore.getState().addPending(sid, { id: "1", text: "插话二" });

    // 后端注入第一条（轮内 user/message 事件）
    pushEvent({ type: "user/message", sessionId: sid, content: "插话一" });
    await flush();
    let snap = useProcessingStore.getState().processing[sid];
    // 注入事件已进投影（第 2 条 user 段数据源）
    expect(snap.currentEvents.filter((e) => e.type === "user/message").length).toBe(2);
    expect(snap.currentEvents[snap.currentEvents.length - 1]).toMatchObject({
      type: "user/message",
      content: "插话一",
    });
    // 队首 pending 转正移除（第二条仍在）
    expect(useSteerStore.getState().pendingBySession[sid]).toEqual([
      { id: "1", text: "插话二" },
    ]);

    // 注入第二条
    pushEvent({ type: "user/message", sessionId: sid, content: "插话二" });
    await flush();
    expect(useSteerStore.getState().pendingBySession[sid]).toBeUndefined();
    // 轮收尾前注入未触发 turn/end → 快照仍在（同轮 streaming 连续）
    expect(useProcessingStore.getState().processing[sid]).toBeDefined();

    // turn/end：轮结束（快照清空 + pending 防御清理幂等）
    pushEvent({ type: "turn/end", sessionId: sid, reason: "completed" });
    await flush();
    expect(useProcessingStore.getState().processing).toEqual({});
    expect(useSteerStore.getState().pendingBySession).toEqual({});
    snap = undefined as unknown as typeof snap;
  });

  it("中止轮：pending 已被中止侧清空；turn/end(aborted) 兜底清理幂等", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("生成中中止");
    const sid = Object.keys(useProcessingStore.getState().processing)[0];
    // 中止侧（ChatView handleStop）已乐观清 pending —— 此处残留防御场景：
    // 若仍有残留，turn/end 到达时兜底清除
    useSteerStore.getState().addPending(sid, { id: "0", text: "残留插话" });
    pushEvent({ type: "turn/end", sessionId: sid, reason: "aborted" });
    await flush();
    expect(useProcessingStore.getState().processing).toEqual({});
    expect(useSteerStore.getState().pendingBySession).toEqual({});
  });

  it("注入内容与队首不匹配（协议错位）→ pending 保留至轮收尾清除（防御）", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("原始问题");
    const sid = Object.keys(useProcessingStore.getState().processing)[0];
    useSteerStore.getState().addPending(sid, { id: "0", text: "插话一" });
    pushEvent({ type: "user/message", sessionId: sid, content: "异常内容" });
    await flush();
    // 不匹配：pending 未动（不破坏 FIFO 视觉），事件仍进投影（异常内容按普通
    // user 段展示——不会重复气泡，因 pending 仍在且与事件内容不同）
    expect(useSteerStore.getState().pendingBySession[sid]).toEqual([
      { id: "0", text: "插话一" },
    ]);
    pushEvent({ type: "turn/end", sessionId: sid, reason: "completed" });
    await flush();
    expect(useSteerStore.getState().pendingBySession).toEqual({});
  });
});
