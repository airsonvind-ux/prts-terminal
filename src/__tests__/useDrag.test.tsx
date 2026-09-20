// @vitest-environment jsdom
// useDrag：起拖阈值 + 点击不移动窗口 + 位移相对按下点
//
// 两次回归留下的契约：
// 1. 手柄容器里挤着标题与按钮，所以"点击"必须不被拖拽打断——起拖要过 4px 阈值，
//    且不再用原生 startDragging（它会吞掉 click，还会把窗口挡在屏幕顶边内）。
// 2. 监听在 mousedown 内同步装配，并对"mouseup 丢失"兜底（buttons=0 即收摊）。

import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";

const { outerPosition, setPosition } = vi.hoisted(() => ({
  outerPosition: vi.fn(() => Promise.resolve({ x: 200, y: 300 })),
  setPosition: vi.fn((_pos: { x: number; y: number }) => Promise.resolve()),
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ outerPosition, setPosition }),
}));

import { useDrag } from "../hooks/useDrag";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

/** 手柄容器内放一个按钮：按钮上的点击不得引发窗口移动 */
function Harness() {
  const { onDragHandle } = useDrag();
  return React.createElement(
    "div",
    { "data-handle": true, onMouseDown: onDragHandle },
    React.createElement("button", { "data-btn": true, type: "button" }, "按钮")
  );
}

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

beforeEach(() => {
  outerPosition.mockClear();
  setPosition.mockClear();
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(React.createElement(Harness));
  });
});

afterEach(() => {
  // 收掉可能残留的 document 监听（用例中途未松开的拖动）
  document.dispatchEvent(new MouseEvent("mouseup", { buttons: 0 }));
  act(() => root.unmount());
  container.remove();
});

/** 让 outerPosition 的 then 落地（锚点就绪） */
async function flush(): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
}

async function press(target: Element, x = 100, y = 100): Promise<void> {
  await act(async () => {
    target.dispatchEvent(
      new MouseEvent("mousedown", {
        bubbles: true,
        button: 0,
        screenX: x,
        screenY: y,
      })
    );
  });
  await flush();
}

async function move(x: number, y: number, buttons = 1): Promise<void> {
  await act(async () => {
    document.dispatchEvent(
      new MouseEvent("mousemove", { buttons, screenX: x, screenY: y })
    );
  });
}

async function release(): Promise<void> {
  await act(async () => {
    document.dispatchEvent(new MouseEvent("mouseup", { buttons: 0 }));
  });
}

describe("useDrag", () => {
  it("按钮上按下但未越阈值 → 不移动窗口（点击照常派发）", async () => {
    await press(container.querySelector("[data-btn]")!);
    await move(103, 100); // 3px < 4px
    await release();
    expect(setPosition).not.toHaveBeenCalled();
  });

  it("越过阈值 → 位置 = 按下时窗口原点 + 位移（相对按下点，不跳变）", async () => {
    await press(container.querySelector("[data-btn]")!);
    await move(130, 140); // +30 / +40
    expect(setPosition).toHaveBeenCalledTimes(1);
    expect(setPosition.mock.calls[0][0]).toMatchObject({ x: 230, y: 340 });
  });

  it("持续移动：位移始终相对按下点累计", async () => {
    await press(container.querySelector("[data-handle]")!);
    await move(120, 100);
    await move(180, 130);
    expect(setPosition).toHaveBeenCalledTimes(2);
    expect(setPosition.mock.calls[1][0]).toMatchObject({ x: 280, y: 330 });
  });

  it("已松开（buttons=0）的残余移动不移动窗口，且收摊后不再跟随", async () => {
    await press(container.querySelector("[data-handle]")!);
    await move(160, 100, 0); // 松开后残余移动
    expect(setPosition).not.toHaveBeenCalled();
    await move(200, 100); // 收摊后再动
    expect(setPosition).not.toHaveBeenCalled();
  });

  it("非左键不参与拖动", async () => {
    await act(async () => {
      container
        .querySelector("[data-handle]")!
        .dispatchEvent(
          new MouseEvent("mousedown", {
            bubbles: true,
            button: 2,
            screenX: 20,
            screenY: 20,
          })
        );
    });
    await flush();
    await move(120, 20);
    expect(setPosition).not.toHaveBeenCalled();
  });
});
