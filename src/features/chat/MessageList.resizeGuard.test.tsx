// @vitest-environment jsdom
// MessageList · 窗口缩放期间不钉底
//
// 滚动所有权规则之一（2026-09）：窗口缩放是用户在调整视图，不是内容变化。
// 若缩放期间仍钉底，对话内容会随缩放手势上下滚动。
//
// 覆盖：resize 打头后，列表尺寸变化（ResizeObserver 回调）不写滚动位置；
//       保护窗过后恢复钉底。
//
// 2026-10-08 补：「回到最新」按钮改由几何事实驱动（视口下方还有内容）。这里
//       覆盖卡片展开/收起这两个内容尺寸变化——它们不产生任何滚动事件
//       （scrollTop 停在 0，不需要钳位），旧实现下按钮会永久占位。
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
        showWelcome: false,
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
        showWelcome: false,
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

/** 带铐位的 scrollTop 替身（模拟浏览器：越界时铐到最大） */
function spyOnScrollTopClamped(el: HTMLElement, max: number) {
  let value = 0;
  const spy = vi.fn((v: number) => {
    value = Math.max(0, Math.min(v, max));
  });
  Object.defineProperty(el, "scrollTop", {
    configurable: true,
    get: () => value,
    set: spy,
  });
  return spy;
}

function triggerSizeChange(): void {
  act(() => {
    lastROCallback?.();
  });
}

describe("MessageList · 回到底部按钮", () => {
  it("保护窗开着时滚到底也必须恢复贴底（按钮消失）", async () => {
    await renderList();
    const listEl = container.querySelector<HTMLElement>("[data-message-list]")!;
    const spy = spyOnScrollTopClamped(listEl, 900);
    fakeMetrics(listEl, 1000, 100);
    await wait(450);
    spy.mockClear();

    // 用户上滑 → 按钮出现
    act(() => {
      forceScrollTop(listEl, 100);
      listEl.dispatchEvent(new Event("scroll"));
    });
    expect(container.querySelector('[aria-label="回到最新"]')).toBeTruthy();

    // 缩放开着保护窗（resize 事件），紧接着滚到底：
    // 逃生门在屏蔽之前，所以必须恢复——这正是之前不稳定的根源
    act(() => {
      window.dispatchEvent(new Event("resize"));
    });
    act(() => {
      forceScrollTop(listEl, 900);
      listEl.dispatchEvent(new Event("scroll"));
    });
    expect(container.querySelector('[aria-label="回到最新"]')).toBeNull();
  });

  it("缩放基准不一致时，滚到底仍能恢复贴底（按钮消失）", async () => {
    document.documentElement.style.setProperty("--face-size", "360px");
    await renderList();
    const listEl = container.querySelector<HTMLElement>("[data-message-list]")!;
    const spy = spyOnScrollTopClamped(listEl, 900);
    fakeMetrics(listEl, 1000, 100);
    await wait(450);
    spy.mockClear();

    // 用户上滑 → 按钮出现
    act(() => {
      forceScrollTop(listEl, 100);
      listEl.dispatchEvent(new Event("scroll"));
    });
    expect(container.querySelector('[aria-label="回到最新"]')).toBeTruthy();

    // 缩放一次（缩放基准变了；故意不给 ResizeObserver 回调，模拟实际情形）
    document.documentElement.style.setProperty("--face-size", "320px");
    await wait(200); // 让保护窗过期

    // 手动滚到底：即便命中“基准不一致”分支，也必须恢复贴底
    act(() => {
      forceScrollTop(listEl, 900);
      listEl.dispatchEvent(new Event("scroll"));
    });
    expect(container.querySelector('[aria-label="回到最新"]')).toBeNull();
  });

  it("自己写入的回声被认领；稍后用户滚到底仍恢复贴底（按钮消失）", async () => {
    await renderList();
    const listEl = container.querySelector<HTMLElement>("[data-message-list]")!;
    const spy = spyOnScrollTopClamped(listEl, 900); // 上限 = 1000 − 100
    fakeMetrics(listEl, 1000, 100);
    await wait(450); // 挂载收敛窗钉底：写入并记录（浏览器铐位后落点 = 900）
    spy.mockClear();

    // 用户上滑 → 贴底标志翻掉 → 按钮出现
    act(() => {
      forceScrollTop(listEl, 0);
      listEl.dispatchEvent(new Event("scroll"));
    });
    expect(container.querySelector('[aria-label="回到最新"]')).toBeTruthy();

    // 稍后（已超出程序化认领窗）手动滚回底部：必须被认作“回到贴底”
    await wait(300);
    act(() => {
      forceScrollTop(listEl, 900);
      listEl.dispatchEvent(new Event("scroll"));
    });
    expect(container.querySelector('[aria-label="回到最新"]')).toBeNull();
  });

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

  it("卡片展开：内容撑出底部即出现按钮（不改位置、无滚动事件）", async () => {
    await renderList();
    const listEl = container.querySelector<HTMLElement>("[data-message-list]")!;
    spyOnScrollTopClamped(listEl, 900);
    fakeMetrics(listEl, 100, 100); // 初始放得下
    await wait(450);
    expect(container.querySelector('[aria-label="回到最新"]')).toBeNull();

    // 展开思考卡片：内容变高（一次 DOM 变更，位置由浏览器保持不动）
    act(() => {
      forceScrollTop(listEl, 0);
    });
    fakeMetrics(listEl, 1000, 100);
    act(() => {
      listEl.appendChild(document.createElement("div"));
    });
    await flush();
    expect(container.querySelector('[aria-label="回到最新"]')).toBeTruthy();
  });

  it("卡片收起：内容缩回、下方已空 → 按钮自动消失", async () => {
    await renderList();
    const listEl = container.querySelector<HTMLElement>("[data-message-list]")!;
    spyOnScrollTopClamped(listEl, 900);
    fakeMetrics(listEl, 1000, 100);
    await wait(450);

    // 上滑 → 按钮出现（离底 800px）
    act(() => {
      forceScrollTop(listEl, 100);
      listEl.dispatchEvent(new Event("scroll"));
    });
    expect(container.querySelector('[aria-label="回到最新"]')).toBeTruthy();

    // 内容缩回：不再溢出。此刻不会有任何 scroll 事件（位置停在原处、无需钳位），
    // 事实只能靠内容变更观测纠正回来——这正是旧实现里按钮永久占位的那条路径。
    fakeMetrics(listEl, 100, 100);
    act(() => {
      listEl.appendChild(document.createElement("div"));
    });
    await flush();
    expect(container.querySelector('[aria-label="回到最新"]')).toBeNull();
  });

  it("贴底跟随时，新内容到来不点亮按钮（钉底把事实压回底部）", async () => {
    await renderList();
    const listEl = container.querySelector<HTMLElement>("[data-message-list]")!;
    spyOnScrollTopClamped(listEl, 900);
    fakeMetrics(listEl, 1000, 100); // 有溢出，但钉在底部
    await wait(450);

    act(() => {
      rerenderList([chunk("新内容")]);
    });
    await flush();
    expect(container.querySelector('[aria-label="回到最新"]')).toBeNull();
  });

  it("内容整体放得下时，上滑不点亮按钮（“离开底部”没有引用对象）", async () => {
    await renderList();
    const listEl = container.querySelector<HTMLElement>("[data-message-list]")!;
    fakeMetrics(listEl, 100, 100); // 无溢出：欢迎页 / 短对话的同款几何
    await wait(450);

    act(() => {
      listEl.dispatchEvent(
        new WheelEvent("wheel", { deltaY: -120, bubbles: true })
      );
    });
    expect(container.querySelector('[aria-label="回到最新"]')).toBeNull();
  });

  it("欢迎页（showWelcome）：即便被压出溢出几何也不出现按钮", async () => {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
    act(() => {
      root.render(
        React.createElement(MessageList, {
          turns: [],
          currentEvents: [],
          showWelcome: true,
        })
      );
    });
    await flush();
    const listEl = container.querySelector<HTMLElement>("[data-message-list]")!;
    spyOnScrollTopClamped(listEl, 900);
    fakeMetrics(listEl, 1000, 100); // 极小窗口：几何上有溢出，语义上照旧无对象
    await wait(450);

    act(() => {
      listEl.dispatchEvent(
        new WheelEvent("wheel", { deltaY: -120, bubbles: true })
      );
    });
    act(() => {
      forceScrollTop(listEl, 0);
      listEl.dispatchEvent(new Event("scroll"));
    });
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

  it("视口快照停留在缩放前尺寸时，流式更新仍钉底", async () => {
    const originalW = window.innerWidth;
    const originalH = window.innerHeight;
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 900 });
    Object.defineProperty(window, "innerHeight", { configurable: true, value: 700 });
    try {
      await renderList();
      const listEl = container.querySelector<HTMLElement>("[data-message-list]")!;
      const spy = spyOnScrollTopClamped(listEl, 900);
      fakeMetrics(listEl, 1000, 100);
      await wait(450);

      // ResizeObserver 记下的是缩放中的 900；窗口随后异步落定到 901。
      triggerSizeChange();
      spy.mockClear();
      Object.defineProperty(window, "innerWidth", { configurable: true, value: 901 });

      rerenderList([chunk("新内容")]);
      await flush();
      expect(spy).toHaveBeenCalled();
    } finally {
      Object.defineProperty(window, "innerWidth", { configurable: true, value: originalW });
      Object.defineProperty(window, "innerHeight", { configurable: true, value: originalH });
    }
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
