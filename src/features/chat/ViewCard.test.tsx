// @vitest-environment jsdom
// 视图卡渲染测试
//
// 钉住四件事：html 进沙箱 iframe（sandbox 只有 allow-scripts）、高度 clamp
// 到 40–160、doc 用 session_file_abs_path 换成绝对路径后交给 open_path、
// 文件不在时显示占位而不是空白。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import type { ToolResultView } from "../../types";
import { ViewCard, viewHeightPx, viewUrl } from "./ViewCard";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const invokeMock = (globalThis as { __invokeMock?: ReturnType<typeof vi.fn> }).__invokeMock!;

const mounted: Array<{ container: HTMLDivElement; root: ReturnType<typeof createRoot> }> = [];

function render(view: ToolResultView, conversationId = "c1") {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(React.createElement(ViewCard, { view, conversationId }));
  });
  mounted.push({ container, root });
  return container;
}

async function flush() {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
}

afterEach(() => {
  for (const m of mounted) {
    try {
      act(() => m.root.unmount());
    } catch {
      // 卸载失败不阻塞清理
    }
    m.container.remove();
  }
  mounted.length = 0;
});

beforeEach(() => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(() => Promise.resolve("C:/data/session-files/c1/views/a.pdf"));
});

describe("ViewCard · html", () => {
  it("进沙箱 iframe，只有 allow-scripts，src 指向会话目录", () => {
    const container = render({ kind: "html", title: "地图", path: "views/v1.html" });
    const frame = container.querySelector<HTMLIFrameElement>("[data-view-frame]")!;
    expect(frame.getAttribute("sandbox")).toBe("allow-scripts");
    expect(frame.getAttribute("src")).toBe(viewUrl("c1", "views/v1.html"));
    expect(frame.getAttribute("src")).toContain("/c1/views/v1.html");
    // 沙箱文档不联网、不同源，靠协议响应头给策略
    expect(frame.getAttribute("title")).toBe("地图");
  });

  it("高度 clamp 到 40–160，缺省 120", () => {
    expect(viewHeightPx({ kind: "html", title: "t", path: "views/a.html" })).toBe(120);
    expect(viewHeightPx({ kind: "html", title: "t", path: "views/a.html", height: 10 })).toBe(40);
    expect(viewHeightPx({ kind: "html", title: "t", path: "views/a.html", height: 999 })).toBe(160);
  });

  it("点开展开进应用内浮层", () => {
    const container = render({ kind: "html", title: "地图", path: "views/v1.html" });
    const expand = [...container.querySelectorAll("button")].find((b) => b.textContent === "展开")!;
    act(() => expand.click());
    const large = container.querySelector("[data-view-frame-large]")!;
    expect(large).not.toBeNull();
    expect(large.getAttribute("sandbox")).toBe("allow-scripts");
  });

  it("标题过长时仍留在标题行（不换行截断由样式负责）", () => {
    const longTitle = "很长的标题".repeat(20);
    const container = render({ kind: "html", title: longTitle, path: "views/v1.html" });
    expect(container.textContent).toContain(longTitle);
  });
});

describe("ViewCard · doc 与缺失", () => {
  it("doc 先换算绝对路径，点击交给 open_path", async () => {
    const container = render({ kind: "doc", title: "战报", path: "views/a.pdf", mime: "application/pdf" });
    await flush();
    expect(invokeMock).toHaveBeenCalledWith("session_file_abs_path", {
      conversationId: "c1",
      path: "views/a.pdf",
    });
    const button = [...container.querySelectorAll<HTMLButtonElement>("button")].find(
      (b) => b.textContent?.includes("打开") ?? false
    )!;
    expect(button.disabled).toBe(false);
    act(() => button.click());
    expect(invokeMock).toHaveBeenCalledWith("open_path", {
      path: "C:/data/session-files/c1/views/a.pdf",
    });
  });

  it("文件换不到绝对路径时显示「这块视图已不可用」", async () => {
    invokeMock.mockImplementation(() => Promise.reject("路径越界"));
    const container = render({ kind: "doc", title: "战报", path: "views/gone.pdf", mime: "application/pdf" });
    await flush();
    expect(container.textContent).toContain("这块视图已不可用");
  });
});
