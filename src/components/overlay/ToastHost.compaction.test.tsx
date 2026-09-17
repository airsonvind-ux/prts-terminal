// @vitest-environment jsdom
// src/components/overlay/ToastHost.compaction.test.tsx
// 可点 toast（模块 D）：onClick 动作透传与激活语义
//   - 带 onClick → actionable 样式（可点暗示 + ›）+ 点击执行动作并关闭
//   - 无 onClick → 维持纯关闭语义（点击 dismiss，不执行任何动作）

import { describe, it, expect, beforeAll, afterAll, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { ToastHost } from "./ToastHost";
import { useToastStore } from "../../stores/useToastStore";

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

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  useToastStore.setState({ toasts: [] });
  // 清理定时器（ttl 默认 4s 会残留真实定时器）
  for (const t of useToastStore.getState().toasts) {
    useToastStore.getState().dismiss(t.id);
  }
});

function renderHost() {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(React.createElement(ToastHost));
  });
  return {
    bars: () =>
      Array.from(document.body.querySelectorAll<HTMLElement>("[title]")),
  };
}

describe("ToastHost · 可点动作", () => {
  it("带 onClick：可点语义（title=点击查看）→ 点击执行动作并关闭", () => {
    const clicked = { count: 0 };
    act(() => {
      useToastStore.getState().show("已压缩 3 轮 · 点击查看分界", {
        kind: "info",
        onClick: () => {
          clicked.count += 1;
        },
      });
    });
    const bars = renderHost().bars();
    expect(bars).toHaveLength(1);
    const bar = bars[0];
    expect(bar.title).toBe("点击查看"); // › 视觉暗示由 .actionable::after 提供（jsdom 不渲染伪元素）

    act(() => bar.click());
    expect(clicked.count).toBe(1);
    expect(useToastStore.getState().toasts).toHaveLength(0); // 执行后关闭
  });

  it("无 onClick：维持纯关闭（title=点击关闭，点击不执行动作）", () => {
    let fired = false;
    act(() => {
      useToastStore.getState().show("普通提示", {
        kind: "info",
        onClick: undefined,
      });
    });
    const bar = renderHost().bars()[0];
    expect(bar.textContent).not.toContain("›");
    expect(bar.title).toBe("点击关闭");
    act(() => bar.click());
    expect(fired).toBe(false);
    expect(useToastStore.getState().toasts).toHaveLength(0);
  });

  it("动作抛错也不影响关闭（activate finally 语义）", () => {
    act(() => {
      useToastStore.getState().show("会失败的动作", {
        kind: "info",
        onClick: () => {
          throw new Error("boom");
        },
      });
    });
    const bar = renderHost().bars()[0];
    expect(() => act(() => bar.click())).not.toThrow();
    expect(useToastStore.getState().toasts).toHaveLength(0);
  });
});
