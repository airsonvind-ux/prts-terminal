// @vitest-environment jsdom
// ContextMenuHost · 全局兜底集成
//
// 覆盖：input 上右键 → 编辑菜单（含阻止默认）；空白右键 → 无菜单且阻止默认；
//       input 有选中 → 四件套；非编辑域有选中 → 复制；已处理事件跳过。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { ContextMenuHost } from "./ContextMenuHost";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(React.createElement(ContextMenuHost));
  });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

/** 在元素上派发可取消的 contextmenu 事件（act 包裹以刷新 React 状态） */
function dispatchContextMenu(target: Element): MouseEvent {
  let e!: MouseEvent;
  act(() => {
    e = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    target.dispatchEvent(e);
  });
  return e;
}

/** portal 渲染到 body：按文本查菜单按钮 */
function hasMenuItem(label: string): boolean {
  return Array.from(document.body.querySelectorAll("button")).some(
    (b) => b.textContent === label
  );
}

describe("ContextMenuHost · 全局兜底", () => {
  it("input 上右键 → 显示粘贴/全选，并阻止浏览器菜单", () => {
    document.body.insertAdjacentHTML("beforeend", `<input id="f" />`);
    const input = document.getElementById("f")!;

    const e = dispatchContextMenu(input);

    expect(e.defaultPrevented).toBe(true);
    expect(hasMenuItem("粘贴")).toBe(true);
    expect(hasMenuItem("全选")).toBe(true);
    expect(hasMenuItem("复制")).toBe(false);
    expect(hasMenuItem("剪切")).toBe(false);
  });

  it("input 有选中 → 剪切/复制/粘贴/全选齐全", () => {
    document.body.insertAdjacentHTML("beforeend", `<textarea id="t">hello</textarea>`);
    const ta = document.getElementById("t") as HTMLTextAreaElement;
    ta.setSelectionRange(0, 2);

    dispatchContextMenu(ta);

    for (const label of ["剪切", "复制", "粘贴", "全选"]) {
      expect(hasMenuItem(label), label).toBe(true);
    }
  });

  it("非编辑域有选中 → 复制菜单", () => {
    vi.spyOn(window, "getSelection").mockReturnValue({
      toString: () => "选中了",
    } as unknown as Selection);
    const div = document.createElement("div");
    document.body.appendChild(div);

    dispatchContextMenu(div);

    expect(hasMenuItem("复制")).toBe(true);
    expect(hasMenuItem("粘贴")).toBe(false);
  });

  it("空白区域右键 → 无菜单，且阻止浏览器菜单", () => {
    const div = document.createElement("div");
    document.body.appendChild(div);

    const e = dispatchContextMenu(div);

    expect(e.defaultPrevented).toBe(true);
    expect(hasMenuItem("粘贴")).toBe(false);
    expect(hasMenuItem("复制")).toBe(false);
    expect(hasMenuItem("全选")).toBe(false);
  });

  it("已被处理的事件（defaultPrevented）跳过", () => {
    document.body.insertAdjacentHTML("beforeend", `<input id="f2" />`);
    const input = document.getElementById("f2")!;
    // 模拟组件级处理：在 input 自身的监听里先行阻止
    input.addEventListener("contextmenu", (ev) => ev.preventDefault());

    dispatchContextMenu(input);

    expect(hasMenuItem("粘贴")).toBe(false);
  });
});
