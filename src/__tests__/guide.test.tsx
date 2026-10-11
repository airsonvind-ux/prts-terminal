// @vitest-environment jsdom
// src/__tests__/guide.test.tsx
// 引导说明层 · 键集去重（看过不再出）+ 兜底（量不到锚点整步跳过）+ modal 推进
//            + 局部坐标（洞相对正面定位，不取窗口）+ 步骤点 + 通知卡不串页

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { GuideHost } from "../components/guide/GuideHost";
import { GUIDE_ITEMS, WELCOME_TOUR } from "../components/guide/guideContent";
import { useGuideStore } from "../stores/useGuideStore";
import { useUIStore } from "../stores/useUIStore";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

type InvokeMock = ReturnType<typeof vi.fn> & { mock: { calls: unknown[][] } };
const g = globalThis as unknown as {
  __invokeMock: InvokeMock;
  __defaultInvokeImpl: (cmd: string) => unknown;
};

interface R {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** 重置 store（默认已 hydrated、无已看过项）。 */
function resetStore(seen: string[] = [], hydrated = true): void {
  useGuideStore.setState({
    hydrated,
    seen: new Set(seen),
    queue: [],
    tourKeys: null,
  });
}

/** 给元素一个非零矩形（jsdom 一律返回 0，会被判成"量不到"）。 */
function stubRect(
  el: HTMLElement,
  r: R = { left: 10, top: 20, width: 100, height: 20 }
): void {
  el.getBoundingClientRect = () =>
    ({
      ...r,
      right: r.left + r.width,
      bottom: r.top + r.height,
    }) as DOMRect;
}

/** 摆一个"应用表面"（外壳正面）：说明层的局部坐标以它为原点。 */
function mountFace(r: R = { left: 0, top: 0, width: 400, height: 300 }): HTMLElement {
  const face = document.createElement("div");
  face.setAttribute("data-guide", "face");
  stubRect(face, r);
  document.body.appendChild(face);
  return face;
}

/** 摆一个锚点。 */
function mountAnchor(key: string, r: R): HTMLElement {
  const el = document.createElement("div");
  el.setAttribute("data-guide", key);
  stubRect(el, r);
  document.body.appendChild(el);
  return el;
}

function mountHost(): { root: Root; unmount: () => void } {
  const el = document.createElement("div");
  document.body.appendChild(el);
  const root = createRoot(el);
  act(() => {
    root.render(<GuideHost />);
  });
  return { root, unmount: () => act(() => root.unmount()) };
}

function host(): HTMLElement | null {
  return document.querySelector("[data-guide-host]");
}

beforeEach(() => {
  g.__invokeMock.mockReset();
  g.__invokeMock.mockImplementation(g.__defaultInvokeImpl);
  useUIStore.setState({ overlay: null });
  resetStore();
  document.body.innerHTML = "";
});

afterEach(() => {
  document.body.innerHTML = "";
});

describe("useGuideStore：见过什么、展示什么", () => {
  it("hydrate 拉到的键集把已看过的项过滤掉", async () => {
    g.__invokeMock.mockImplementation((cmd: string) =>
      cmd === "get_guide_seen"
        ? Promise.resolve(["welcome.settings"])
        : g.__defaultInvokeImpl(cmd)
    );
    resetStore([], false);
    await act(async () => {
      useGuideStore.getState().hydrate();
      await Promise.resolve();
    });
    expect(useGuideStore.getState().hydrated).toBe(true);

    useGuideStore.getState().enqueue(WELCOME_TOUR);
    expect(useGuideStore.getState().queue).toEqual([
      "welcome.new-chat",
      "welcome.input",
    ]);
  });

  it("键集没拉到之前一律不出（宁可不弹，也不误弹）", () => {
    resetStore([], false);
    useGuideStore.getState().enqueue(WELCOME_TOUR);
    expect(useGuideStore.getState().queue).toEqual([]);
  });

  it("看过一次就不再出；队列内不重复", () => {
    resetStore();
    useGuideStore.getState().enqueue(["settings.app", "settings.app"]);
    expect(useGuideStore.getState().queue).toEqual(["settings.app"]);

    useGuideStore.getState().advance("settings.app"); // 记 seen + 出队
    useGuideStore.getState().enqueue(["settings.app"]);

    expect(useGuideStore.getState().queue).toEqual([]);
    expect(useGuideStore.getState().seen.has("settings.app")).toBe(true);
  });

  it("advance 会把「看过」落盘（mark_guide_seen）", () => {
    resetStore();
    useGuideStore.getState().enqueue(["settings.model"]);
    useGuideStore.getState().advance("settings.model");

    const calls = g.__invokeMock.mock.calls.filter(
      (c) => c[0] === "mark_guide_seen"
    );
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toEqual({ key: "settings.model" });
  });

  it("exit 只记当前项，其余作废（下次到那个表面再出）", () => {
    resetStore();
    useGuideStore.getState().enqueue(WELCOME_TOUR);
    useGuideStore.getState().exit("welcome.new-chat");

    const s = useGuideStore.getState();
    expect(s.queue).toEqual([]);
    expect(s.seen.has("welcome.new-chat")).toBe(true);
    expect(s.seen.has("welcome.settings")).toBe(false);
    expect(s.seen.has("welcome.input")).toBe(false);
  });
});

describe("useGuideStore：通知卡不串页", () => {
  it("切页时旧通知被换掉，不留上一页的文案", () => {
    resetStore();
    useGuideStore.getState().enqueueNotice("settings.space");
    useGuideStore.getState().enqueueNotice("settings.usage");
    expect(useGuideStore.getState().queue).toEqual(["settings.usage"]);
  });

  it("已看过的页不再排；不越过正在走的欢迎页导览", () => {
    resetStore();
    useGuideStore.getState().enqueue(WELCOME_TOUR);
    useGuideStore.getState().enqueueNotice("settings.app");
    expect(useGuideStore.getState().queue[0]).toBe("welcome.new-chat");
    expect(useGuideStore.getState().queue).toContain("settings.app");

    resetStore(["settings.model"]);
    useGuideStore.getState().enqueueNotice("settings.model");
    expect(useGuideStore.getState().queue).toEqual([]);
  });
});

describe("GuideHost：兜底、局部坐标与 modal", () => {
  it("锚点不存在 → 整步跳过，队列清空且不留半张卡", () => {
    mountFace();
    resetStore();
    useGuideStore.getState().enqueue(["first.conv-title", "first.context-ring"]);

    const { unmount } = mountHost();

    expect(useGuideStore.getState().queue).toEqual([]);
    expect(host()).toBeNull();
    unmount();
  });

  it("锚点存在 → 挖洞 + 卡片，文案取自 guideContent", () => {
    mountFace();
    mountAnchor("conv-title", { left: 10, top: 20, width: 100, height: 20 });

    resetStore();
    useGuideStore.getState().enqueue(["first.conv-title"]);

    const { unmount } = mountHost();
    expect(host()?.textContent).toContain(GUIDE_ITEMS["first.conv-title"].title);
    expect(host()?.textContent).toContain(GUIDE_ITEMS["first.conv-title"].body);
    // 未看完 → 仍在队列里（点了「下一步」才记 seen）
    expect(useGuideStore.getState().queue).toEqual(["first.conv-title"]);
    unmount();
  });

  it("洞用正面内的局部坐标（不是窗口坐标）", () => {
    // 正面本身偏移 (40,30)；锚点在 (60,70) → 局部应为 (20,40)
    mountFace({ left: 40, top: 30, width: 400, height: 300 });
    mountAnchor("conv-title", { left: 60, top: 70, width: 100, height: 20 });

    resetStore();
    useGuideStore.getState().enqueue(["first.conv-title"]);
    const { unmount } = mountHost();

    const hole = host()?.firstElementChild as HTMLElement;
    expect(hole.style.left).toBe("20px");
    expect(hole.style.top).toBe("40px");
    expect(hole.style.width).toBe("100px");
    unmount();
  });

  it("modal：点遮罩不消费（避免误消），按钮才推进", () => {
    mountFace();
    mountAnchor("conv-title", { left: 10, top: 20, width: 100, height: 20 });

    resetStore();
    useGuideStore.getState().enqueue(["first.conv-title"]);
    const { unmount } = mountHost();

    // 点遮罩（想点别处）：不能把引导吃掉
    act(() => {
      host()?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(useGuideStore.getState().queue).toEqual(["first.conv-title"]);
    expect(useGuideStore.getState().seen.has("first.conv-title")).toBe(false);

    // 点卡片本体：也不动
    const card = host()?.lastElementChild as HTMLElement;
    act(() => {
      card.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(useGuideStore.getState().queue).toEqual(["first.conv-title"]);

    // 点「下一步」：才推进
    const next = Array.from(card.querySelectorAll("button")).find((b) =>
      b.textContent?.includes("下一步")
    ) as HTMLButtonElement;
    act(() => {
      next.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(useGuideStore.getState().queue).toEqual([]);
    unmount();
  });

  it("通知式：压暗设置面板，只有「知道了」", () => {
    mountFace();
    mountAnchor("settings-panel", { left: 50, top: 50, width: 200, height: 150 });

    resetStore();
    useUIStore.setState({ overlay: "settings" });
    act(() => {
      useGuideStore.getState().enqueueNotice("settings.space");
    });

    const { unmount } = mountHost();
    expect(host()?.textContent).toContain(GUIDE_ITEMS["settings.space"].body);
    expect(host()?.textContent).toContain("知道了");
    // 通知式不给「跳过」（只有一张卡，点「知道了」即完）
    expect(host()?.textContent).not.toContain("跳过");
    unmount();
  });

  it("多步导览出 ●○○ 步骤点，当前步高亮", () => {
    mountFace();
    for (const key of ["new-chat", "settings", "input"]) {
      mountAnchor(key, { left: 10, top: 20, width: 100, height: 20 });
    }

    resetStore();
    useGuideStore.getState().enqueue(WELCOME_TOUR);
    const { unmount } = mountHost();

    const dots = document.querySelector("[data-guide-dots]");
    expect(dots?.getAttribute("data-guide-dots")).toBe("1");
    expect(dots?.querySelectorAll("span")).toHaveLength(WELCOME_TOUR.length);

    act(() => {
      useGuideStore.getState().advance("welcome.new-chat");
    });
    expect(
      document.querySelector("[data-guide-dots]")?.getAttribute("data-guide-dots")
    ).toBe("2");

    // 单张卡（通知 / 首次出现型）不给步骤点
    act(() => {
      useGuideStore.getState().exit("welcome.settings");
      useGuideStore.getState().enqueue(["first.conv-title"]);
    });
    expect(document.querySelector("[data-guide-dots]")).toBeNull();
    unmount();
  });

  it("别的表面的项不挤掉当前表面的项（模型页通知曾被吞掉）", () => {
    mountFace();
    mountAnchor("settings-panel", { left: 50, top: 50, width: 288, height: 264 });
    mountAnchor("context-ring", { left: 10, top: 20, width: 20, height: 20 });

    resetStore();
    // 对话表面的项先排队（比如刚冒出来的进度环）
    useGuideStore.getState().enqueue(["first.context-ring"]);
    // 进入设置面板 → 模型页通知入队
    useUIStore.setState({ overlay: "settings" });
    act(() => {
      useGuideStore.getState().enqueueNotice("settings.model");
    });

    const { unmount } = mountHost();
    expect(host()?.textContent).toContain(GUIDE_ITEMS["settings.model"].body);
    expect(host()?.textContent).not.toContain(
      GUIDE_ITEMS["first.context-ring"].body
    );
    // 对话项原地等着：没被吞、也没记 seen
    expect(useGuideStore.getState().queue).toContain("first.context-ring");
    expect(useGuideStore.getState().seen.has("first.context-ring")).toBe(false);
    unmount();
  });

  it("通知卡贴内容区右对齐：不盖左侧导航，也不盖满整页", () => {
    mountFace();
    mountAnchor("settings-panel", { left: 50, top: 50, width: 288, height: 264 });

    resetStore();
    useUIStore.setState({ overlay: "settings" });
    act(() => {
      useGuideStore.getState().enqueueNotice("settings.usage");
    });

    const { unmount } = mountHost();
    const card = host()?.lastElementChild as HTMLElement;
    // 宽度 = 288 − 52（导航） − 8 − 8 = 220
    expect(card.style.width).toBe("220px");
    // 局部左边界 60 → 面板 left 50 + 60 = 110：从导航右缘起，盖不住那 52px
    expect(card.style.left).toBe("110px");
    expect(card.style.top).toBe("58px"); // 挂内容区顶部，不是正中
    unmount();
  });

  it("键盘：只有 Esc 退场，寻常按键不误消", () => {
    mountFace();
    mountAnchor("conv-title", { left: 10, top: 20, width: 100, height: 20 });

    resetStore();
    useGuideStore.getState().enqueue(["first.conv-title"]);
    const { unmount } = mountHost();

    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "a" }));
    });
    expect(useGuideStore.getState().queue).toEqual(["first.conv-title"]);

    act(() => {
      window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    });
    expect(useGuideStore.getState().queue).toEqual([]);
    expect(useGuideStore.getState().seen.has("first.conv-title")).toBe(true);
    unmount();
  });

  it("缩放后重量锚点：洞跟着走", () => {
    mountFace({ left: 0, top: 0, width: 400, height: 300 });
    const anchor = mountAnchor("conv-title", {
      left: 10,
      top: 20,
      width: 100,
      height: 20,
    });

    resetStore();
    useGuideStore.getState().enqueue(["first.conv-title"]);
    const { unmount } = mountHost();
    expect((host()?.firstElementChild as HTMLElement).style.left).toBe("10px");

    // 模拟缩放：锚点挪了，观察器回调重算
    stubRect(anchor, { left: 40, top: 60, width: 120, height: 20 });
    const RO = (
      globalThis as unknown as {
        ResizeObserver: { instances: { __trigger(): void }[] };
      }
    ).ResizeObserver;
    act(() => {
      for (const inst of RO.instances) inst.__trigger();
    });

    const hole = host()?.firstElementChild as HTMLElement;
    expect(hole.style.left).toBe("40px");
    expect(hole.style.width).toBe("120px");
    unmount();
  });
});
