// @vitest-environment jsdom
// ChatErrorBoundary：渲染抛错时显示可读兜底，不让整个对话区白屏。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { ChatErrorBoundary } from "./ChatErrorBoundary";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;
let errorSpy: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  errorSpy.mockRestore();
});

function Boom(): React.ReactNode {
  throw new Error("boom");
}

describe("ChatErrorBoundary", () => {
  it("子组件渲染抛错 → 显示兜底与重新加载按钮", () => {
    act(() => {
      root.render(
        React.createElement(
          ChatErrorBoundary,
          null,
          React.createElement(Boom)
        )
      );
    });

    expect(container.textContent).toContain("对话界面加载失败");
    const button = container.querySelector("button");
    expect(button?.textContent).toBe("重新加载");
  });

  it("正常子组件原样渲染", () => {
    act(() => {
      root.render(
        React.createElement(
          ChatErrorBoundary,
          null,
          React.createElement("div", null, "正常内容")
        )
      );
    });

    expect(container.textContent).toContain("正常内容");
    expect(container.textContent).not.toContain("对话界面加载失败");
  });
});
