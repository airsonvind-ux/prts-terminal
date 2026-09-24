// @vitest-environment jsdom
// MessageList · 窗口缩放期间不钉底
//
// 滚动所有权规则之一（2026-09）：窗口缩放是用户在调整视图，不是内容变化。
// 若缩放期间仍钉底，对话内容会随缩放手势上下滚动。
//
// 覆盖：resize 打头后，列表尺寸变化（ResizeObserver 回调）不写滚动位置；
//       保护窗过后恢复钉底。
// jsdom 无布局，只能断言"写没写"：用可触发回调的 ResizeObserver stub +
// scrollTop 设值 spy。

import {
  describe,
  expect,
  it,
  beforeAll,
  afterAll,
  beforeEach,
  afterEach,
  vi,
} from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { MessageList, anchorDelta } from "./MessageList";
import type { SessionEvent } from "../../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

type ROCallback = () => void;
let lastROCallback: ROCallback | null = null;
const originalRO = (globalThis as { ResizeObserver?: unknown }).ResizeObserver;
const originalScrollIntoView = Element.prototype.scrollIntoView;

beforeAll(() => {
  (globalThis as { ResizeObserver?: unknown }).ResizeObserver = class {
    constructor(cb: ROCallback) {
      lastROCallback = cb;
    }
    observe() {}
    unobserve() {}
    disconnect() {
      lastROCallback = null;
    }
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

beforeEach(() => {
  lastROCallback = null;
});

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
});

async function flush(): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
}

/** 挂载一个空列表（滚动行为与内容无关） */
async function renderList(): Promise<HTMLElement> {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(
      React.createElement(MessageList, {
        turns: [],
        currentEvents: [],
        isFirstUse: false,
      })
    );
  });
  await flush();
  return container.querySelector<HTMLElement>("[data-message-list]")!;
}

/** 重渲染当前列表（用 currentEvents 变化模拟流式推进） */
function rerenderList(currentEvents: SessionEvent[]): void {
  act(() => {
    root.render(
      React.createElement(MessageList, {
        turns: [],
        currentEvents,
        isFirstUse: false,
      })
    );
  });
}

/** 伪造列表几何（jsdom 无布局，scrollHeight/clientHeight 恒 0） */
function fakeMetrics(el: HTMLElement, scrollHeight: number, clientHeight: number): void {
  Object.defineProperty(el, "scrollHeight", { configurable: true, get: () => scrollHeight });
  Object.defineProperty(el, "clientHeight", { configurable: true, get: () => clientHeight });
}

const wait = (ms: number) =>
  act(async () => {
    await new Promise((r) => setTimeout(r, ms));
  });

const chunk = (content: string): SessionEvent => ({
  type: "assistant/chunk",
  sessionId: "s1",
  kind: "text",
  content,
});

/** scrollTop 读写替身（jsdom 无布局）：set 记调用，get 回存值 */
function spyOnScrollTop(el: HTMLElement) {
  let value = 0;
  const spy = vi.fn((v: number) => {
    value = v;
  });
  Object.defineProperty(el, "scrollTop", {
    configurable: true,
    get: () => value,
    set: spy,
  });
  return spy;
}

/** 直接改写存值（不经过 set 替身），模拟浏览器/用户改变了位置 */
function forceScrollTop(el: HTMLElement, v: number): void {
  // 经由 set 替身写入，再清掉这次调用记录（只关心后续自动写入）
  el.scrollTop = v;
}

function triggerSizeChange(): void {
  act(() => {
    lastROCallback?.();
  });
}

describe("MessageList · 回到底部按钮", () => {
  it("上滑后出现；点击回到最新且按钮消失", async () => {
    await renderList();
    const listEl = container.querySelector<HTMLElement>("[data-message-list]")!;
    const spy = spyOnScrollTop(listEl);
    fakeMetrics(listEl, 1000, 100); // 离底 900px
    await wait(450);
    spy.mockClear();

    // 初始贴底 → 无按钮
    expect(container.querySelector('[aria-label="回到最新"]')).toBeNull();

    // 用户上滑（滚轮向上）→ 按钮出现
    act(() => {
      listEl.dispatchEvent(
        new WheelEvent("wheel", { deltaY: -120, bubbles: true })
      );
    });
    const btn = container.querySelector<HTMLButtonElement>(
      '[aria-label="回到最新"]'
    );
    expect(btn).toBeTruthy();

    // 点击 → 写回位置 + 恢复跟随 + 按钮消失
    act(() => {
      btn!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(spy).toHaveBeenCalled();
    expect(container.querySelector('[aria-label="回到最新"]')).toBeNull();
  });
});

describe("MessageList · 窗口缩放保护", () => {
  it("缩放期间容器尺寸变化不钉底；保护窗过后恢复", async () => {
    await renderList();
    const listEl = container.querySelector<HTMLElement>("[data-message-list]")!;
    const spy = spyOnScrollTop(listEl);

    // 常规尺寸变化（如输入框展开）→ 钉底
    triggerSizeChange();
    expect(spy).toHaveBeenCalled();

    // 窗口缩放打头 → 尺寸变化不再写滚动位置
    spy.mockClear();
    act(() => {
      window.dispatchEvent(new Event("resize"));
    });
    triggerSizeChange();
    expect(spy).not.toHaveBeenCalled();

    // 保护窗过后恢复钉底
    await act(async () => {
      await new Promise((r) => setTimeout(r, 220));
    });
    triggerSizeChange();
    expect(spy).toHaveBeenCalled();
  });

  it("缩放期间的滚动事件不改贴底标志；保护窗过后照常判定", async () => {
    await renderList();
    const listEl = container.querySelector<HTMLElement>("[data-message-list]")!;
    const spy = spyOnScrollTop(listEl);
    fakeMetrics(listEl, 1000, 100); // 离底 900px（模拟重排后“看着很远”）
    await wait(450); // 让挂载时的收敛窗跑完
    spy.mockClear();

    // 缩放开保护窗：此时滚来的 scroll 事件不改标志（重排不是用户意图）
    act(() => {
      window.dispatchEvent(new Event("resize"));
    });
    act(() => {
      listEl.dispatchEvent(new Event("scroll"));
    });
    // 保护窗期间不钉底（乙行为：缩放中什么都不写）
    rerenderList([chunk("a")]);
    await flush();
    expect(spy).not.toHaveBeenCalled();

    // 保护窗过后：标志仍是“贴底”（缩放中的滚动不算上滑）→ 照旧钉底
    await wait(220);
    spy.mockClear();
    rerenderList([chunk("a"), chunk("a2")]);
    await flush();
    expect(spy).toHaveBeenCalled();

    // 非缩放期间：同样的滚动事件应被认作“用户上滑” → 不再钉底
    await wait(450);
    act(() => {
      forceScrollTop(listEl, 50);
      listEl.dispatchEvent(new Event("scroll"));
    });
    spy.mockClear();
    rerenderList([chunk("a"), chunk("a2"), chunk("b")]);
    await flush();
    expect(spy).not.toHaveBeenCalled();
  });

  it("界面缩放（--face-size 变化）即便无 resize 事件：开保护窗、不钉底", async () => {
    document.documentElement.style.setProperty("--face-size", "360px");
    await renderList();
    const listEl = container.querySelector<HTMLElement>("[data-message-list]")!;
    const spy = spyOnScrollTop(listEl);
    await wait(450);

    // 基线：先触发一次，让缩放基准被记录，普通尺寸变化照常钉底
    triggerSizeChange();
    expect(spy).toHaveBeenCalled();
    spy.mockClear();

    // 缩放基准变了（模拟拖拽缩放；故意不发 resize 事件、视口也不变）
    document.documentElement.style.setProperty("--face-size", "300px");
    triggerSizeChange();
    expect(spy).not.toHaveBeenCalled();

    // 保护窗过后，基准未变的普通变化仍照常钉底
    await wait(220);
    spy.mockClear();
    triggerSizeChange();
    expect(spy).toHaveBeenCalled();
  });

  it("缩放锚校正量（纯函数）：把锚行送回原偏移所需的增量", () => {
    // 行在视口内的位置从 10 被挤到 120（内容变高 110）→ 需 +110 才回原位
    expect(anchorDelta(120, 0, 10)).toBe(110);
    // 未挪位 → 0
    expect(anchorDelta(10, 0, 10)).toBe(0);
    // 列表顶部本身也动了（整页位移）→ 抵消掉
    expect(anchorDelta(30, 20, 10)).toBe(0);
    // 内容变矮（行上移）→ 负增量
    expect(anchorDelta(-40, 0, 10)).toBe(-50);
  });

  it("缩放期间真正的上滑手势（滚轮向上）仍被尊重", async () => {
    await renderList();
    const listEl = container.querySelector<HTMLElement>("[data-message-list]")!;
    const spy = spyOnScrollTop(listEl);
    await wait(450);

    act(() => {
      window.dispatchEvent(new Event("resize"));
    });
    act(() => {
      listEl.dispatchEvent(
        new WheelEvent("wheel", { deltaY: -120, bubbles: true })
      );
    });
    spy.mockClear();
    rerenderList([chunk("a")]);
    await flush();
    expect(spy).not.toHaveBeenCalled();
  });
});
