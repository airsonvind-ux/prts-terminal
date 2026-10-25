// @vitest-environment jsdom
// 圆环内嵌供应商标记：图标来自当前对话 provider，明度来自思考档。

import { describe, it, expect, beforeEach, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { ContextIndicator } from "./ContextIndicator";
import { useConversationStore } from "../../stores/useConversationStore";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  useConversationStore.setState({
    conversations: [
      {
        id: "c1",
        title: "测试",
        turns: [],
        createdAt: 1,
        modelName: "DeepSeek V4.1",
        contextLength: 1_000_000,
        provider: "deepseek",
        model: "deepseek-v4-flash",
        thinkingLevel: "high",
      },
    ],
    activeId: "c1",
    pendingTitle: null,
    draftConfig: null,
  });
  __invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "get_local_model_info") {
      return Promise.resolve({
        displayName: "DeepSeek V4.1",
        contextLength: 1_000_000,
        supportsVision: false,
        supportsReasoning: true,
        thinkingLevels: ["off", "low", "high", "max"],
        defaultThinkingLevel: "high",
      });
    }
    return Promise.resolve(undefined);
  });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

async function flush(): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
}

describe("ContextIndicator · 供应商标记", () => {
  it("供应商图标在圆环内，high 档明度 0.8", async () => {
    act(() => root.render(React.createElement(ContextIndicator)));
    await flush();

    const svgs = container.querySelectorAll("svg");
    expect(svgs).toHaveLength(2);
    const providerIcon = svgs[1] as SVGSVGElement;
    expect(providerIcon.style.opacity).toBe("0.8");
    expect(providerIcon.querySelector("path")).toBeTruthy();
  });
});
