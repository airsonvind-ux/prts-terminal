// @vitest-environment jsdom
// src/__tests__/titleGeneration.event.test.tsx
// 会话自动标题 · 事件通道测试（useAgentEvents 消费 conversation/title）
//
// 覆盖：
//   - 事件到达 → 内存标题刷新（展示层从 store 读）
//   - 首轮已归档 → 带 CAS 守卫的 update_conversation_meta 补丁
//   - 首轮未归档（处理中）→ 仅内存（后续 append_turn 携带）
//   - 应用前复检：用户已改名 → 丢弃生成标题

import { describe, it, expect, beforeEach, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { useAgentEvents } from "../hooks/useAgentEvents";
import { useProcessingStore } from "../stores/useProcessingStore";
import { useConversationStore } from "../stores/useConversationStore";
import { useUIStore } from "../stores/useUIStore";
import { useToastStore } from "../stores/useToastStore";
import type { ConversationTitleEvent, SessionEvent } from "../types";

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
  useConversationStore.setState({
    conversations: [],
    activeId: null,
    pendingTitle: null,
  });
  useUIStore.setState({
    errorMsg: null,
    overlay: null,
    restoring: false,
  });
  useToastStore.setState({ toasts: [] });
  __invokeMock.mockClear();
  __invokeMock.mockImplementation(
    (globalThis as unknown as { __defaultInvokeImpl: (cmd: string) => unknown })
      .__defaultInvokeImpl
  );
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

function pushTitle(e: ConversationTitleEvent): void {
  act(() => {
    (
      globalThis as { __simulateAgentEvent: (e: unknown) => void }
    ).__simulateAgentEvent(e as unknown as { type: string; sessionId: string });
  });
}

function titlePatchCalls(): Array<Record<string, unknown>> {
  return __invokeMock.mock.calls
    .filter((c) => c[0] === "update_conversation_meta")
    .map((c) => c[1] as Record<string, unknown>);
}

function titleOf(id: string): string | undefined {
  return useConversationStore
    .getState()
    .conversations.find((c) => c.id === id)?.title;
}

const USER_EVENT: SessionEvent = {
  type: "user/message",
  sessionId: "s1",
  content: "查源石虫",
};

describe("useAgentEvents · conversation/title", () => {
  it("首轮已归档 → 刷新标题 + 带 CAS 守卫的落盘补丁", async () => {
    const id = useConversationStore.getState().createConversation("查源石虫");
    useConversationStore.getState().appendTurn(id, [USER_EVENT]);
    __invokeMock.mockClear();

    pushTitle({ type: "conversation/title", conversationId: id, title: "源石虫数值" });
    await flush();

    expect(titleOf(id)).toBe("源石虫数值");
    // 真标题落地标记（引导的"对话标题"那条靠它把门：占位标题不算数）
    expect(
      useConversationStore.getState().conversations.find((c) => c.id === id)
        ?.titleGenerated
    ).toBe(true);
    const patches = titlePatchCalls();
    expect(patches).toHaveLength(1);
    expect(patches[0]).toEqual({
      conversationId: id,
      title: "源石虫数值",
      expectedTitle: "查源石虫",
      usageDeltas: null,
    });
  });

  it("首轮未归档（处理中）→ 仅内存刷新，不发补丁", async () => {
    const id = useConversationStore.getState().createConversation("查根");
    // 模拟处理中快照（含首条 user 事件）
    useProcessingStore.setState({
      processing: {
        s1: {
          conversationId: id,
          currentEvents: [
            { type: "user/message", sessionId: "s1", content: "查根" },
          ],
          agentPhase: "streaming",
          mood: "平静",
          moodIntensity: 0,
        },
      },
    });
    __invokeMock.mockClear();

    pushTitle({ type: "conversation/title", conversationId: id, title: "根信息查询" });
    await flush();

    expect(titleOf(id)).toBe("根信息查询");
    expect(titlePatchCalls()).toHaveLength(0);
  });

  it("应用前复检：用户已改名 → 丢弃生成标题（不改内存、不落盘）", async () => {
    const id = useConversationStore.getState().createConversation("查源石虫");
    useConversationStore.getState().appendTurn(id, [USER_EVENT]);
    useConversationStore.getState().renameConversation(id, "我改的标题");
    __invokeMock.mockClear();

    pushTitle({ type: "conversation/title", conversationId: id, title: "源石虫数值" });
    await flush();

    expect(titleOf(id)).toBe("我改的标题");
    expect(titlePatchCalls()).toHaveLength(0);
  });

  it("对话不存在 → 忽略（防御）", async () => {
    pushTitle({ type: "conversation/title", conversationId: "ghost", title: "x" });
    await flush();
    expect(titlePatchCalls()).toHaveLength(0);
  });
});
