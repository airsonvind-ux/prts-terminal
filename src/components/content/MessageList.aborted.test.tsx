// @vitest-environment jsdom
// src/components/content/MessageList.aborted.test.tsx
// M3-B §4.5：归档中止轮 → 正常内容照常渲染（半截 chunk）+ 轮尾标记条
// 「该轮已中止」（role aborted；不得走 error 分支——error 轮吞其余事件）

import { describe, it, expect, beforeAll, afterAll, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { MessageList } from "./MessageList";
import type { SessionEvent } from "../../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

// jsdom 无 ResizeObserver / scrollIntoView：stub 最小实现（组件 effect 依赖，
// 仿 Markdown.test.ts 组件级端到端用例）
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

function render(turns: SessionEvent[][], currentEvents: SessionEvent[] = []) {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(
      React.createElement(MessageList, {
        turns,
        currentEvents,
        showWelcome: false,
      })
    );
  });
  return {
    text: () => container.textContent ?? "",
    container,
    messageAreas: () =>
      Array.from(container.querySelectorAll<HTMLElement>("[data-message-area]")),
  };
}

function abortedTurn(sid: string): SessionEvent[] {
  return [
    { type: "user/message", sessionId: sid, content: "生成中途中止" },
    {
      type: "assistant/chunk",
      sessionId: sid,
      kind: "text",
      content: "半截回答",
    },
    { type: "turn/aborted", sessionId: sid },
  ];
}

describe("MessageList · aborted 轮展示", () => {
  it("归档中止轮：半截 chunk 成气泡 + 轮尾「该轮已中止」标记条", () => {
    const { text, messageAreas } = render([abortedTurn("s1")]);

    expect(text()).toContain("半截回答");
    expect(text()).toContain("该轮已中止");

    // 标记条是独立展示项（轮尾），不是与内容拼在同一气泡
    const areas = messageAreas();
    const last = areas[areas.length - 1];
    expect(last.textContent).toBe("该轮已中止");
  });

  it("正常归档轮（无标记）不渲染中止条", () => {
    const sid = "s2";
    const turn: SessionEvent[] = [
      { type: "user/message", sessionId: sid, content: "正常轮" },
      { type: "assistant/message", sessionId: sid, content: "完整回答", reasoningContent: null, usage: null },
    ];
    const { text } = render([turn]);
    expect(text()).toContain("完整回答");
    expect(text()).not.toContain("该轮已中止");
  });

  it("中止轮不得走 error 分支：内容照常渲染（error 轮会吞掉其余事件）", () => {
    // 对照：error 轮只渲染错误气泡
    const errText = render([
      [
        { type: "user/message", sessionId: "e", content: "x" },
        {
          type: "assistant/chunk",
          sessionId: "e",
          kind: "text",
          content: "不该出现",
        },
        { type: "error", sessionId: "e", message: "出错啦", recoverable: false },
      ],
    ]).text();
    expect(errText).toContain("出错啦");
    expect(errText).not.toContain("不该出现");

    // 中止轮：半截 + 标记都在（未被 error 分支吞掉）
    const abortedText = render([abortedTurn("s3")]).text();
    expect(abortedText).toContain("半截回答");
    expect(abortedText).toContain("该轮已中止");
  });

  it("首部错误轮在后续正常轮后折叠为启动失败记录", () => {
    const errorTurn: SessionEvent[] = [
      { type: "user/message", sessionId: "e1", content: "第一问" },
      {
        type: "error",
        sessionId: "e1",
        message: "HTTP 401",
        recoverable: false,
      },
    ];
    const normalTurn: SessionEvent[] = [
      { type: "user/message", sessionId: "s2", content: "第二问" },
      {
        type: "assistant/message",
        sessionId: "s2",
        content: "正常回答",
        reasoningContent: null,
        usage: null,
      },
    ];
    const view = render([errorTurn, normalTurn]);
    expect(view.text()).toContain("启动阶段失败 1 次 · 已恢复");
    expect(view.text()).not.toContain("第一问");
    expect(view.text()).not.toContain("HTTP 401");
    expect(view.text()).toContain("第二问");
    expect(view.container.querySelector("[data-preflight-card]")).not.toBeNull();
  });

  it("只有错误轮、尚无正常轮时不折叠", () => {
    const errorTurn: SessionEvent[] = [
      { type: "user/message", sessionId: "e1", content: "第一问" },
      {
        type: "error",
        sessionId: "e1",
        message: "HTTP 401",
        recoverable: false,
      },
    ];
    const view = render([errorTurn]);
    expect(view.text()).toContain("第一问");
    expect(view.text()).toContain("HTTP 401");
    expect(view.container.querySelector("[data-preflight-card]")).toBeNull();
  });
});
