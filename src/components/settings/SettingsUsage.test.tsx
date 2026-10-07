// @vitest-environment jsdom
// src/components/settings/SettingsUsage.test.tsx
// "用量"子页：空态 + 总览聚合 + 单对话 scope（后端数据经投影缓存读取）。

import { describe, it, expect, beforeEach, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { SettingsUsage } from "./SettingsUsage";
import { useConversationStore } from "../../stores/useConversationStore";
import type { CategoryUsage, Conversation, ConversationUsage } from "../../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

function cat(input: number, hit: number): CategoryUsage {
  return { inputTokens: input, outputTokens: 0, cacheHitTokens: hit, cachePromptTokens: input };
}

function usageOf(session: number, compaction: number): ConversationUsage {
  return {
    session: cat(session, 0),
    compaction: cat(compaction, 0),
    memory: cat(0, 0),
    title: cat(0, 0),
  };
}

function conv(id: string, title: string, u?: ConversationUsage): Conversation {
  return { id, title, turns: [], createdAt: 1, usage: u };
}

function render(): void {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(React.createElement(SettingsUsage));
  });
}

beforeEach(() => {
  useConversationStore.setState({ conversations: [], activeId: null, pendingTitle: null });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("SettingsUsage", () => {
  it("无任何用量 → 空态", () => {
    useConversationStore.setState({ conversations: [conv("a", "甲")] });
    render();
    expect(container.textContent).toContain("暂无用量记录");
  });

  it("总览：聚合四类 + 显示总量与类别", () => {
    useConversationStore.setState({
      conversations: [
        conv("a", "甲", usageOf(1000, 500)),
        conv("b", "乙", usageOf(3000, 0)),
      ],
    });
    render();
    expect(container.textContent).toContain("全部对话");
    // 合计 4500 → 4.5K
    expect(container.textContent).toContain("4.5K");
    expect(container.textContent).toContain("对话");
    expect(container.textContent).toContain("压缩");
    expect(container.textContent).toContain("记忆");
    expect(container.textContent).toContain("标题");
  });
});
