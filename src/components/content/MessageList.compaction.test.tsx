// @vitest-environment jsdom
// src/components/content/MessageList.compaction.test.tsx
// 模块 D：压缩点提示卡（本地条目，非 LLM 消息）
//   - 插在折叠轮与保留轮之间（retainedFrom 处），全列表恰一张
//   - 摘要默认收起，点击展开显示 checkpoint 正文
//   - 无 compaction / retainedFrom=0 → 不渲染卡片

import { describe, it, expect, beforeAll, afterAll, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { MessageList } from "./MessageList";
import type { CompactionMeta, SessionEvent } from "../../types";

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

function render(
  turns: SessionEvent[][],
  compaction?: CompactionMeta
) {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(
      React.createElement(MessageList, {
        turns,
        currentEvents: [],
        isFirstUse: false,
        compaction,
      })
    );
  });
  return {
    text: () => container.textContent ?? "",
    cards: () =>
      Array.from(
        container.querySelectorAll<HTMLElement>("[data-compaction-card]")
      ),
    summaries: () =>
      Array.from(
        container.querySelectorAll<HTMLElement>("[data-compaction-summary]")
      ),
  };
}

const compaction: CompactionMeta = {
  retainedFrom: 2,
  summary: "## Goal\n推演 1-7\n## Next Steps\n1. 验证配置",
  tokensBefore: 5000,
  at: 1,
};

function turn(label: string): SessionEvent[] {
  return [
    { type: "user/message", sessionId: "s1", content: `${label} 提问` },
    {
      type: "assistant/message",
      sessionId: "s1",
      content: `${label} 回答`,
      reasoningContent: null,
      usage: null,
    },
  ];
}

describe("MessageList · 压缩点提示卡", () => {
  it("插在折叠轮与保留轮之间；全列表恰一张；摘要默认收起", () => {
    const turns = [
      turn("折叠A"),
      turn("折叠B"),
      turn("保留C"),
      turn("保留D"),
    ];
    const { text, cards, summaries } = render(turns, compaction);

    expect(cards()).toHaveLength(1);
    // 折叠轮在卡片之前、保留轮在卡片之后
    const idxCard = text().indexOf("上下文已压缩 · 2 轮并入摘要");
    const idxFolded = text().indexOf("折叠B 回答");
    const idxKept = text().indexOf("保留C 提问");
    expect(idxFolded).toBeGreaterThan(-1);
    expect(idxCard).toBeGreaterThan(idxFolded);
    expect(idxKept).toBeGreaterThan(idxCard);
    // 折叠轮仍全量渲染（回看自由，无折叠无删除）
    expect(text()).toContain("折叠A 提问");
    // 摘要收起（不渲染正文）
    expect(summaries()).toHaveLength(0);
    expect(text()).not.toContain("推演 1-7");
  });

  it("点击头部展开摘要正文（checkpoint 文本可见）", () => {
    const turns = [turn("折叠A"), turn("保留B")];
    const cut = { ...compaction, retainedFrom: 1 };
    const { text, cards, summaries } = render(turns, cut);

    const header = cards()[0].querySelector("button")!;
    act(() => header.click());
    expect(summaries()).toHaveLength(1);
    expect(text()).toContain("## Goal");
    expect(text()).toContain("推演 1-7");
    // 再次点击收起
    act(() => header.click());
    expect(summaries()).toHaveLength(0);
  });

  it("无 compaction / retainedFrom=0 → 不渲染卡片", () => {
    const turns = [turn("A"), turn("B")];
    expect(render(turns, undefined).cards()).toHaveLength(0);
    expect(render(turns, { ...compaction, retainedFrom: 0 }).cards()).toHaveLength(
      0
    );
  });
});
