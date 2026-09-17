// @vitest-environment jsdom
// src/components/content/MessageList.steer.test.tsx
// 插话多段展示（设计定稿）：轮内多问答段按消息边界切分——
// - 轮内多条 user 各成气泡；assistant 段独立闭合（各段文本互不吞并）
// - 实时流（currentEvents）中途注入：前段已闭合、末段 streaming
// - pending 气泡：渲染于列表底部（data-steer-pending），点击触发取消回调
// 单段轮零回归由既有 MessageList.*.test 覆盖（本文件只补多段/pending 行为）。

import { describe, it, expect, vi, beforeAll, afterAll, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { MessageList } from "./MessageList";
import type { SessionEvent } from "../../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

// jsdom 无 ResizeObserver / scrollIntoView：stub（仿 MessageList.aborted.test）
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

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
});

interface PendingLike {
  id: string;
  text: string;
}

function render(
  turns: SessionEvent[][],
  currentEvents: SessionEvent[] = [],
  pending?: PendingLike[],
  onCancelPending?: (id: string) => void
) {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(
      React.createElement(MessageList, {
        turns,
        currentEvents,
        isFirstUse: false,
        ...(pending ? { pending } : {}),
        ...(onCancelPending ? { onCancelPending } : {}),
      })
    );
  });
  return {
    text: () => container.textContent ?? "",
    bubbles: () =>
      Array.from(
        container.querySelectorAll<HTMLElement>("[data-message-area]")
      ).map((el) => (el.textContent ?? "").trim()),
    pendingButtons: () =>
      Array.from(
        container.querySelectorAll<HTMLElement>("[data-steer-pending]")
      ).map((el) => el),
    pendingTexts: () =>
      Array.from(
        container.querySelectorAll<HTMLElement>("[data-steer-pending]")
      ).map((el) => (el.textContent ?? "").trim()),
  };
}

function sid(): string {
  return "s1";
}

function user(content: string): SessionEvent {
  return { type: "user/message", sessionId: sid(), content };
}

function assistantSegment(content: string): SessionEvent[] {
  return [
    { type: "assistant/chunk", sessionId: sid(), kind: "text", content },
    {
      type: "assistant/message",
      sessionId: sid(),
      content,
      reasoningContent: null,
      usage: null,
    },
  ];
}

describe("MessageList · 轮内多问答段（归档 turns）", () => {
  it("两条 user 各成气泡、各 assistant 段独立闭合，文本互不吞并", () => {
    const turn: SessionEvent[] = [
      user("第一问"),
      ...assistantSegment("回答A"),
      user("插话一"),
      ...assistantSegment("回答B"),
      { type: "turn/end", sessionId: sid(), reason: "completed" },
    ];
    const { bubbles } = render([turn]);
    expect(bubbles()).toEqual(["第一问", "回答A", "插话一", "回答B"]);
  });

  it("轮内无 assistant 内容的中止段（半截后注入前 abort）：user 各自成气泡 + 残段", () => {
    const turn: SessionEvent[] = [
      user("原始问题"),
      { type: "assistant/chunk", sessionId: sid(), kind: "text", content: "半截" },
      user("插话（未注入即中止）"),
      { type: "assistant/chunk", sessionId: sid(), kind: "text", content: "残段" },
      { type: "turn/aborted", sessionId: sid() },
      { type: "turn/end", sessionId: sid(), reason: "aborted" },
    ];
    const bubbles = render([turn]).bubbles();
    expect(bubbles[0]).toBe("原始问题");
    expect(bubbles[1]).toBe("半截");
    expect(bubbles[2]).toBe("插话（未注入即中止）");
    expect(bubbles[3]).toBe("残段");
    expect(render([turn]).text()).toContain("该轮已中止");
  });
});

describe("MessageList · 实时流（currentEvents）多段", () => {
  it("流式中途注入：前段已闭合、末段 streaming（唯一文本气泡尾不重复旧段）", () => {
    // 段1 已完成（assistant/message 权威）→ 注入 user2 → 段2 流式 chunks（无聚合）
    const current: SessionEvent[] = [
      user("原始问题"),
      ...assistantSegment("第一段完成"),
      user("插话已注入"),
      { type: "assistant/chunk", sessionId: sid(), kind: "text", content: "第二段" },
      { type: "assistant/chunk", sessionId: sid(), kind: "text", content: "流式中" },
    ];
    const bubbles = render([], current).bubbles();
    expect(bubbles).toEqual([
      "原始问题",
      "第一段完成",
      "插话已注入",
      "第二段流式中",
    ]);
  });
});

describe("MessageList · pending 待注入气泡", () => {
  it("渲染于列表底部（纯样式差异态）；无 pending 不渲染", () => {
    const current: SessionEvent[] = [
      user("原始问题"),
      { type: "assistant/chunk", sessionId: sid(), kind: "text", content: "正在流" },
    ];
    const pending = [{ id: "0", text: "待注入插话" }];
    const noPending = render([], current);
    expect(noPending.pendingButtons()).toHaveLength(0);

    const withPending = render([], current, pending);
    const buttons = withPending.pendingButtons();
    expect(buttons).toHaveLength(1);
    expect(buttons[0].textContent).toBe("待注入插话");
    expect(buttons[0].getAttribute("title")).toBe("点击取消");
    // 位于消息气泡之后（列表末尾）
    expect(withPending.text()).toContain("待注入插话");
  });

  it("多条 pending FIFO 顺序渲染；点击 → 携带条目 id 的取消回调", () => {
    const onCancel = vi.fn<(id: string) => void>();
    const pending = [
      { id: "0", text: "插话甲" },
      { id: "1", text: "插话乙" },
    ];
    const { pendingButtons, pendingTexts } = render([], [], pending, onCancel);
    const buttons = pendingButtons();
    expect(pendingTexts()).toEqual(["插话甲", "插话乙"]);
    act(() => {
      buttons[1].dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(onCancel).toHaveBeenCalledWith("1");
  });
});
