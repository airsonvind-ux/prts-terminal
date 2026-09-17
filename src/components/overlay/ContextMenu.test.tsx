// @vitest-environment jsdom
// ContextMenu · 统一菜单组件
//
// 覆盖：菜单项渲染与点击（先关闭后执行）、点击外部关闭、贴边夹紧纯函数。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { ContextMenu, clampMenuPosition } from "./ContextMenu";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  document.body.replaceChildren();
});

function renderMenu(props: {
  x?: number;
  y?: number;
  items: { label: string; onSelect: () => void }[];
  onClose: () => void;
}) {
  act(() => {
    root.render(
      React.createElement(ContextMenu, {
        x: props.x ?? 10,
        y: props.y ?? 20,
        items: props.items,
        onClose: props.onClose,
      })
    );
  });
}

/** portal 渲染到 body：按文本查菜单按钮 */
function findButton(label: string): HTMLButtonElement | undefined {
  return Array.from(document.body.querySelectorAll("button")).find(
    (b) => b.textContent === label
  );
}

describe("ContextMenu · 渲染与交互", () => {
  it("渲染菜单项并响应点击（先关闭后执行）", () => {
    const onClose = vi.fn();
    const onSelect = vi.fn();
    renderMenu({ items: [{ label: "复制", onSelect }], onClose });

    const btn = findButton("复制");
    expect(btn).toBeTruthy();
    act(() => {
      btn!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalled();
  });

  it("点击菜单外部关闭", () => {
    const onClose = vi.fn();
    renderMenu({ items: [{ label: "复制", onSelect: () => {} }], onClose });

    act(() => {
      document.body.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(onClose).toHaveBeenCalled();
  });
});

describe("ContextMenu · clampMenuPosition", () => {
  it("未越界时保持原位", () => {
    expect(clampMenuPosition(10, 10, 100, 80, 200, 200)).toEqual({ x: 10, y: 10 });
  });

  it("右下越界时向左/上收，保留边距", () => {
    expect(clampMenuPosition(180, 180, 100, 80, 200, 200)).toEqual({ x: 96, y: 116 });
  });

  it("负坐标夹到边距内侧", () => {
    expect(clampMenuPosition(-50, -50, 100, 80, 200, 200)).toEqual({ x: 4, y: 4 });
  });
});
