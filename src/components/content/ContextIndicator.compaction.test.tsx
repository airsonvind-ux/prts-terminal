// @vitest-environment jsdom
// src/components/content/ContextIndicator.compaction.test.tsx
// 模块 D：圆环交互（点击开压缩菜单 / 压缩中取消）+ 水位口径
//
// 覆盖：
//   1. 空闲点击圆环 → 「压缩较早对话？」菜单；点「压缩」→ compact_conversation
//   2. 压缩中：圆环进入旋转加载态（spinning class）；点击 → 「取消压缩？」
//      确认 → abort_turn（ctxdecay 通道）
//   3. 口径：进度用 inputTokens（output 不再计入水位）

import { describe, it, expect, beforeAll, afterAll, afterEach, beforeEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { ContextIndicator } from "./ContextIndicator";
import { useConversationStore } from "../../stores/useConversationStore";
import { useCompactionStore, compactionSessionId } from "../../stores/useCompactionStore";
import { setModelInfo } from "../../stores/useProcessingStore";
import { useToastStore } from "../../stores/useToastStore";
import type { SessionEvent } from "../../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

const originalRO = (globalThis as { ResizeObserver?: unknown }).ResizeObserver;
beforeAll(() => {
  (globalThis as { ResizeObserver?: unknown }).ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
});
afterAll(() => {
  if (originalRO === undefined) {
    delete (globalThis as { ResizeObserver?: unknown }).ResizeObserver;
  } else {
    (globalThis as { ResizeObserver?: unknown }).ResizeObserver = originalRO;
  }
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
  useToastStore.setState({ toasts: [] });
  setModelInfo(null);
  __invokeMock.mockClear();
}

beforeEach(resetAll);

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
});

function renderIndicator() {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(React.createElement(ContextIndicator));
  });
  return {
    ring: () =>
      container.querySelector<HTMLElement>('[role="button"]'),
    menu: () => document.body.querySelector<HTMLElement>('[role="menu"]'),
    menuButtons: () =>
      Array.from(
        document.body.querySelectorAll<HTMLElement>('[role="menu"] button')
      ).map((b) => b.textContent ?? ""),
    bodyText: () => document.body.textContent ?? "",
  };
}

function seedConversation(contextTokens?: {
  inputTokens: number;
  outputTokens: number;
}, turns?: SessionEvent[][]): void {
  const conv: Record<string, unknown> = {
    id: "c1",
    title: "会话",
    turns: turns ?? [
      [{ type: "user/message", sessionId: "s1", content: "你好" }],
    ],
    createdAt: 1,
  };
  if (contextTokens) conv.contextTokens = contextTokens;
  useConversationStore.setState({
    conversations: [conv as never],
    activeId: "c1",
    pendingTitle: null,
  });
}

/** 长会话：总估远超 20K 保留预算（可折叠） */
function longTurns(): SessionEvent[][] {
  const turns: SessionEvent[][] = [];
  for (let i = 0; i < 400; i++) {
    turns.push([
      {
        type: "user/message",
        sessionId: "s1",
        content: "问题讨论内容甲".repeat(40),
      },
    ]);
  }
  return turns;
}

function clickRing(ui: ReturnType<typeof renderIndicator>): void {
  const ring = ui.ring()!;
  act(() => ring.click());
}

describe("ContextIndicator · 压缩入口", () => {
  it("空闲点击圆环 → 单档压缩菜单；点「压缩」发起 compact_conversation", () => {
    seedConversation(
      { inputTokens: 100, outputTokens: 20 },
      longTurns()
    );
    __invokeMock.mockImplementationOnce((cmd: string) =>
      cmd === "compact_conversation"
        ? Promise.resolve({
            summary: "## Goal\n摘要",
            degraded: false,
            tokensBefore: 100,
          })
        : Promise.resolve(undefined)
    );
    const ui = renderIndicator();

    // 菜单默认关闭
    expect(ui.menu()).toBeNull();
    clickRing(ui);

    // 菜单 = 单一动作项（无标题/说明/二次确认）：点击即执行压缩
    const body = ui.bodyText();
    expect(body).not.toContain("压缩较早对话？");
    expect(ui.menuButtons()).toEqual(["压缩"]);

    // 点击压缩 → runCompaction 经 store 编排（compact_conversation 被调用）
    const startBtn = document.body.querySelector<HTMLElement>(
      '[role="menu"] button'
    )!;
    act(() => startBtn.click());

    const calls = (globalThis as { __invokeMock: { mock: { calls: unknown[][] } } })
      .__invokeMock.mock.calls;
    expect(calls.some((c) => c[0] === "compact_conversation")).toBe(true);
  });

  it("压缩中：弧点旋转加载；点击 → 单一动作项「取消压缩」→ abort_turn 通道", () => {
    seedConversation();
    useCompactionStore.setState({ compactingConversationId: "c1" });
    const ui = renderIndicator();

    // 旋转弧点：渲染 track + arcDot 两个 circle（进度环暂隐）；
    // arcDot = 短弧双值 dasharray（短弧 + 整周空隙）；旋转动画由 CSS
    // keyframes 驱动（弧点自身 rotate 0→360°，transform-origin 圆心）
    const circles = container.querySelectorAll("svg circle");
    expect(circles.length).toBe(2);
    expect(circles[0].getAttribute("stroke-dasharray")).toBeNull(); // track
    expect(circles[1].getAttribute("stroke-dasharray")).toMatch(/\s/); // 短弧双值

    clickRing(ui);
    // 无确认卡：仅一个动作项「取消压缩」
    const body = ui.bodyText();
    expect(body).not.toContain("取消压缩？");
    expect(ui.menuButtons()).toEqual(["取消压缩"]);

    const cancelBtn = document.body.querySelector<HTMLElement>(
      '[role="menu"] button'
    )!;
    act(() => cancelBtn.click());

    const calls = (globalThis as { __invokeMock: { mock: { calls: unknown[][] } } })
      .__invokeMock.mock.calls;
    const abortCall = calls.find((c) => c[0] === "abort_turn");
    expect(abortCall).toBeDefined();
    expect(
      (abortCall![1] as { sessionId: string }).sessionId
    ).toBe(compactionSessionId("c1"));
  });

  it("无活跃对话 → 圆环不可交互（无菜单）", () => {
    const ui = renderIndicator();
    const ring = ui.ring();
    // 无对话：tabIndex -1（不进入焦点序）；点击无菜单
    expect(ring?.getAttribute("tabindex")).toBe("-1");
    clickRing(ui);
    expect(ui.menu()).toBeNull();
  });
});

describe("ContextIndicator · 水位口径", () => {
  it("进度使用 inputTokens（output 不计入水位，悬停面板不含成本行）", () => {
    seedConversation({ inputTokens: 100, outputTokens: 900 });
    setModelInfo({ name: "DeepSeek", contextLength: 200 });
    renderIndicator();
    // 悬停浮层（React onMouseEnter 由 mouseover 合成）断言口径：
    // used = input（100/200 = 50%）；output 既不进水位也不显示成本行
    const ring = container.querySelector<HTMLElement>('[role="button"]')!;
    act(() => {
      ring.dispatchEvent(new MouseEvent("mouseover", { bubbles: true }));
    });
    const body = document.body.textContent ?? "";
    expect(body).toContain("上下文 100 / 200 (50%)");
    expect(body).not.toContain("out");
  });
});
