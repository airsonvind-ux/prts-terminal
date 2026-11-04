// @vitest-environment jsdom
// 视图卡渲染与交互测试
//
// 钉住：html 进沙箱 iframe（sandbox 只有 allow-scripts）、高度 clamp 到
// 40–160、doc 用 session_file_abs_path 换绝对路径后交给 open_path、
// 文件不在时显示占位、以及沙箱消息的来源校验与落点（活动轮→插话，
// 空闲→输入框草稿）。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import type { ToolResultView } from "../../types";
import { useComposerDraftStore } from "../../stores/useComposerDraftStore";
import { ViewCard, viewHeightPx } from "./ViewCard";
import { resetViewOrigin, VIEW_TEXT_LIMIT, viewUrl } from "./viewBridge";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const invokeMock = (globalThis as { __invokeMock?: ReturnType<typeof vi.fn> }).__invokeMock!;
const origin = "http://prts-view.localhost";

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

function post(source: Window | null, data: Record<string, unknown>) {
  act(() => {
    window.dispatchEvent(new MessageEvent("message", { data, source }));
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
  useComposerDraftStore.getState().setText("c1", "");
});

beforeEach(() => {
  invokeMock.mockClear();
  resetViewOrigin();
  invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "view_origin") {
      return Promise.resolve({ scheme: "prts-view", host: "prts-view.localhost", origin });
    }
    if (cmd === "view_prompt") return Promise.resolve({ delivered: true });
    return Promise.resolve("C:/data/session-files/c1/views/a.pdf");
  });
});

describe("ViewCard · html", () => {
  it("进沙箱 iframe，只有 allow-scripts，src 指向会话目录", async () => {
    const container = render({ kind: "html", title: "地图", path: "views/v1.html" });
    await flush();
    const frame = container.querySelector<HTMLIFrameElement>("[data-view-frame]")!;
    expect(frame.getAttribute("sandbox")).toBe("allow-scripts");
    expect(frame.getAttribute("src")).toBe(viewUrl(origin, "c1", "views/v1.html"));
    expect(frame.getAttribute("src")).toContain("/c1/views/v1.html");
    expect(frame.getAttribute("title")).toBe("地图");
  });

  it("高度 clamp 到 40–160，缺省 120", () => {
    expect(viewHeightPx({ kind: "html", title: "t", path: "views/a.html" })).toBe(120);
    expect(viewHeightPx({ kind: "html", title: "t", path: "views/a.html", height: 10 })).toBe(40);
    expect(viewHeightPx({ kind: "html", title: "t", path: "views/a.html", height: 999 })).toBe(160);
  });

  it("点开展开进应用内浮层", async () => {
    const container = render({ kind: "html", title: "地图", path: "views/v1.html" });
    await flush();
    const expand = [...container.querySelectorAll("button")].find((b) => b.textContent === "展开")!;
    act(() => expand.click());
    const large = container.querySelector("[data-view-frame-large]")!;
    expect(large).not.toBeNull();
    expect(large.getAttribute("sandbox")).toBe("allow-scripts");
  });

  it("标题过长时仍留在标题行（不换行截断由样式负责）", async () => {
    const longTitle = "很长的标题".repeat(20);
    const container = render({ kind: "html", title: longTitle, path: "views/v1.html" });
    await flush();
    expect(container.textContent).toContain(longTitle);
  });
});

describe("ViewCard · 沙箱消息", () => {
  it("不是这张视图发来的消息一律不理", async () => {
    const container = render({ kind: "html", title: "地图", path: "views/v1.html" });
    await flush();
    post(window, { source: "prts-view", type: "prompt", text: "第三格" });
    post(null, { source: "prts-view", type: "prompt", text: "第三格" });
    expect(invokeMock).not.toHaveBeenCalledWith("view_prompt", expect.anything());
    expect(container.textContent).toContain("地图");
  });

  it("活动轮里 prompt 走插话，并带来源标注", async () => {
    const container = render({ kind: "html", title: "地图", path: "views/v1.html" });
    await flush();
    const frame = container.querySelector<HTMLIFrameElement>("[data-view-frame]")!;
    post(frame.contentWindow, { source: "prts-view", type: "prompt", text: "看看第三格" });
    await flush();
    expect(invokeMock).toHaveBeenCalledWith("view_prompt", {
      conversationId: "c1",
      text: "来自《地图》的交互：看看第三格",
    });
  });

  it("没人跑的时候 prompt 落进输入框，不替用户发言", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "view_origin") {
        return Promise.resolve({ scheme: "prts-view", host: "prts-view.localhost", origin });
      }
      if (cmd === "view_prompt") return Promise.resolve({ delivered: false });
      return Promise.resolve(undefined);
    });
    const container = render({ kind: "html", title: "地图", path: "views/v1.html" });
    await flush();
    const frame = container.querySelector<HTMLIFrameElement>("[data-view-frame]")!;
    post(frame.contentWindow, { source: "prts-view", type: "prompt", text: "看看第三格" });
    await flush();
    expect(useComposerDraftStore.getState().textByKey.c1).toBe(
      "来自《地图》的交互：看看第三格"
    );
  });

  it("超长文字被拒，不发出去", async () => {
    const container = render({ kind: "html", title: "地图", path: "views/v1.html" });
    await flush();
    const frame = container.querySelector<HTMLIFrameElement>("[data-view-frame]")!;
    post(frame.contentWindow, {
      source: "prts-view",
      type: "prompt",
      text: "长".repeat(VIEW_TEXT_LIMIT + 1),
    });
    await flush();
    expect(invokeMock).not.toHaveBeenCalledWith("view_prompt", expect.anything());
  });

  it("一分钟内超过 10 次就拦下", async () => {
    const container = render({ kind: "html", title: "地图", path: "views/v1.html" });
    await flush();
    const frame = container.querySelector<HTMLIFrameElement>("[data-view-frame]")!;
    for (let i = 0; i < 12; i += 1) {
      post(frame.contentWindow, { source: "prts-view", type: "toast", text: `第 ${i} 次` });
    }
    await flush();
    const prompts = invokeMock.mock.calls.filter((call) => call[0] === "view_prompt");
    expect(prompts.length).toBe(0);
  });

  it("call 交给后端排队，带上来源视图标题", async () => {
    const container = render({ kind: "html", title: "地图", path: "views/v1.html" });
    await flush();
    const frame = container.querySelector<HTMLIFrameElement>("[data-view-frame]")!;
    post(frame.contentWindow, {
      source: "prts-view",
      type: "call",
      tool: "pic_catalog_list_images",
      args: { limit: 3 },
    });
    await flush();
    expect(invokeMock).toHaveBeenCalledWith("view_call_tool", {
      request: {
        conversationId: "c1",
        tool: "pic_catalog_list_images",
        args: JSON.stringify({ limit: 3 }),
        viewTitle: "地图",
      },
    });
  });

  it("没有进行中的轮次时 call 落进输入框，不替用户发起", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "view_origin") {
        return Promise.resolve({ scheme: "prts-view", host: "prts-view.localhost", origin });
      }
      if (cmd === "view_call_tool") return Promise.resolve({ delivered: false });
      return Promise.resolve(undefined);
    });
    const container = render({ kind: "html", title: "地图", path: "views/v1.html" });
    await flush();
    const frame = container.querySelector<HTMLIFrameElement>("[data-view-frame]")!;
    post(frame.contentWindow, {
      source: "prts-view",
      type: "call",
      tool: "pic_catalog_list_images",
      args: {},
    });
    await flush();
    expect(useComposerDraftStore.getState().textByKey.c1).toContain(
      "请调用工具 pic_catalog_list_images"
    );
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
    invokeMock.mockImplementation((cmd: string) =>
      cmd === "session_file_abs_path" ? Promise.reject("路径越界") : Promise.resolve(undefined)
    );
    const container = render({ kind: "doc", title: "战报", path: "views/gone.pdf", mime: "application/pdf" });
    await flush();
    expect(container.textContent).toContain("这块视图已不可用");
  });
});
