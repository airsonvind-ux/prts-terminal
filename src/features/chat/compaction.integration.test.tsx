// @vitest-environment jsdom
// src/components/content/compaction.integration.test.tsx
// 模块 D 压缩点提示卡 · 真实链路集成：runCompaction（store 编排 + mock 后端）
// → applyCompaction 写入 conversationStore → 以 store 实际状态渲染 MessageList
// → 断言压缩点提示卡出现在边界处。
//
// 目的：捕捉「组件直传 compaction 可用，但经真实压缩流后数据丢失/错位」类的
// 链路断裂（此前组件测试只覆盖直传 props，未覆盖 applyCompaction 产物）。

import { describe, it, expect, beforeAll, afterAll, afterEach, beforeEach, vi } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { MessageList } from "./MessageList";
import { useConversationStore } from "../../stores/useConversationStore";
import {
  useCompactionStore,
  scrollToCompactionCard,
} from "../../stores/useCompactionStore";
import { useProcessingStore, setModelInfo } from "../../stores/useProcessingStore";
import { useToastStore } from "../../stores/useToastStore";
import type { SessionEvent } from "../../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

const originalRO = (globalThis as { ResizeObserver?: unknown }).ResizeObserver;
const originalScrollIntoView = Element.prototype.scrollIntoView;

beforeAll(() => {
  (globalThis as { ResizeObserver?: unknown }).ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
  Element.prototype.scrollIntoView = (() => {}) as typeof Element.prototype.scrollIntoView;
});

afterAll(() => {
  if (originalRO === undefined) {
    delete (globalThis as { ResizeObserver?: unknown }).ResizeObserver;
  } else {
    (globalThis as { ResizeObserver?: unknown }).ResizeObserver = originalRO;
  }
  Element.prototype.scrollIntoView = originalScrollIntoView;
});

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

function resetAll(): void {
  useConversationStore.setState({
    conversations: [],
    activeId: null,
    pendingTitle: null,
  });
  useCompactionStore.setState({ compactingConversationId: null });
  useProcessingStore.setState({ processing: {} });
  useToastStore.setState({ toasts: [] });
  setModelInfo(null);
  __invokeMock.mockClear();
}

beforeEach(resetAll);

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
});

// ── 辅助 ──

/** 长会话：400 轮 × ~190 tok ≈ 76K tok，远超 20K 保留预算（必有折叠区） */
function longTurns(): SessionEvent[][] {
  const turns: SessionEvent[][] = [];
  for (let i = 0; i < 400; i++) {
    turns.push([
      {
        type: "user/message",
        sessionId: "s1",
        content: `第 ${i} 题`.repeat(3) + "问题讨论内容甲".repeat(30),
      },
    ]);
  }
  return turns;
}

function renderList(conversationId: string) {
  const conv = useConversationStore
    .getState()
    .conversations.find((c) => c.id === conversationId)!;
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(
      React.createElement(MessageList, {
        turns: conv.turns,
        currentEvents: [],
        showWelcome: false,
        compaction: conv.compaction,
      })
    );
  });
  return {
    text: () => container.textContent ?? "",
    cards: () =>
      Array.from(
        container.querySelectorAll<HTMLElement>("[data-compaction-card]")
      ),
  };
}

describe("压缩提示卡 · 真实压缩流集成", () => {
  it("runCompaction 成功后以 store 状态渲染：卡片出现在边界处且可展开", async () => {
    // 1. store 预置长会话（活跃）
    useConversationStore.setState({
      conversations: [
        {
          id: "c1",
          title: "长对话",
          turns: longTurns(),
          createdAt: 1,
        } as never,
      ],
      activeId: "c1",
      pendingTitle: null,
    });

    // 2. mock 后端压缩成功
    __invokeMock.mockImplementationOnce((cmd: string) =>
      cmd === "compact_conversation"
        ? Promise.resolve({
            summary: "## Goal\n真实链路摘要",
            degraded: false,
            tokensBefore: 40_000,
          })
        : Promise.resolve(undefined)
    );

    // 3. 走真实编排
    await useCompactionStore.getState().runCompaction("c1");

    // 4. store 状态核对：compaction 落记录 + contextTokens 清空
    const conv = useConversationStore
      .getState()
      .conversations.find((c) => c.id === "c1")!;
    expect(conv.compaction).toBeDefined();
    expect(conv.compaction!.retainedFrom).toBeGreaterThan(0);
    expect(conv.compaction!.retainedFrom).toBeLessThan(400);

    // 5. 渲染 MessageList（store 实际状态）→ 卡片存在且位置在边界
    const { text, cards } = renderList("c1");
    expect(cards()).toHaveLength(1);
    expect(text()).toContain("上下文已压缩 · ");
    expect(text()).toContain(`${conv.compaction!.retainedFrom} 轮并入摘要`);

    // 6. 展开可见摘要正文
    const header = cards()[0].querySelector("button")!;
    act(() => header.click());
    expect(text()).toContain("真实链路摘要");
  });
});

describe("scrollToCompactionCard（可点 toast 动作）", () => {
  it("会话活跃且卡存在 → 滚动到分界卡（scrollIntoView 被调）", async () => {
    useConversationStore.setState({
      conversations: [
        {
          id: "c1",
          title: "长对话",
          turns: longTurns(),
          createdAt: 1,
          compaction: {
            retainedFrom: 300,
            summary: "## Goal\n摘要",
            tokensBefore: 40_000,
            at: 1,
          },
        } as never,
      ],
      activeId: "c1",
      pendingTitle: null,
    });
    renderList("c1");
    const spy = vi
      .spyOn(Element.prototype, "scrollIntoView")
      .mockImplementation(() => {});
    try {
      expect(scrollToCompactionCard("c1")).toBe(true);
      expect(spy).toHaveBeenCalled();
    } finally {
      spy.mockRestore();
    }
  });

  it("会话已切走 / 卡不存在 → 空操作返回 false（不滚动）", () => {
    useConversationStore.setState({
      conversations: [
        {
          id: "c1",
          title: "长对话",
          turns: longTurns(),
          createdAt: 1,
        } as never,
      ],
      activeId: "c1",
      pendingTitle: null,
    });
    // 未压缩 → 无卡；渲染后 helper 应为 false（且不抛）
    renderList("c1");
    expect(scrollToCompactionCard("c1")).toBe(false);
    // 会话不活跃（切走）→ false
    useConversationStore.setState({ activeId: null });
    expect(scrollToCompactionCard("c1")).toBe(false);
  });
});
