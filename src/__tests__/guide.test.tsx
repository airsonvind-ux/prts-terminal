// @vitest-environment jsdom
// src/__tests__/guide.test.tsx
// 引导说明层 · 键集去重（看过不再出）+ 兜底（量不到锚点整步跳过）+ modal 推进
//            + 局部坐标（洞相对正面定位，不取窗口）+ 步骤点 + 通知卡不串页

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { GuideHost } from "../components/guide/GuideHost";
import { SettingsPanel } from "../components/settings/SettingsPanel";
import { GUIDE_ITEMS, SETTINGS_TAB_GUIDE, WELCOME_TOUR } from "../components/guide/guideContent";
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

describe("引导数据完整性（抓“不该不出现”那类真故障）", () => {
  it("每一项都有正文；聚焦式必须有锚点", () => {
    // 缺锚点的聚焦项会被“量不到就跳过”的兜底静默吃掉——引导凭空少一条
    for (const [key, item] of Object.entries(GUIDE_ITEMS)) {
      expect(item.body.length, `${key} 缺正文`).toBeGreaterThan(0);
      if (item.form === "focus") {
        expect(item.anchor, `${key} 是聚焦式却没给锚点`).toBeTruthy();
      }
    }
  });

  it("设置页九页的映射都指向存在的设置面项", () => {
    // 映射写错一个字，那一页的通知就永远不出
    for (const [tab, key] of Object.entries(SETTINGS_TAB_GUIDE)) {
      const item = GUIDE_ITEMS[key];
      expect(item, `tab ${tab} 指向了不存在的 key：${key}`).toBeDefined();
      expect(item.surface, `${key} 应属于设置表面`).toBe("settings");
    }
  });

  it("欢迎页导览每一项都存在且属于对话表面", () => {
    for (const key of WELCOME_TOUR) {
      const item = GUIDE_ITEMS[key];
      expect(item, `导览里的 ${key} 不存在`).toBeDefined();
      expect(item.surface).toBe("chat");
    }
  });
});

describe("SettingsPanel + GuideHost：面板挂载在默认页", () => {
  it("应用页（默认页）的通知也要出卡", async () => {
    // jsdom 无布局引擎：给所有元素一个非零矩形，让面板可量（真环境它本来就有尺寸）
    const real = HTMLElement.prototype.getBoundingClientRect;
    HTMLElement.prototype.getBoundingClientRect = function () {
      return {
        left: 0,
        top: 0,
        width: 288,
        height: 264,
        right: 288,
        bottom: 264,
      } as DOMRect;
    };
    try {
      mountFace();
      resetStore();
      useUIStore.setState({ overlay: "settings", settingsInitialTab: "app" });

      const el = document.createElement("div");
      document.body.appendChild(el);
      const root = createRoot(el);
      await act(async () => {
        root.render(
          <>
            <SettingsPanel />
            <GuideHost />
          </>
        );
        await Promise.resolve();
      });

      const hostEl = document.querySelector("[data-guide-host]");
      expect(hostEl?.textContent).toContain(GUIDE_ITEMS["settings.app"].body);
      act(() => root.unmount());
    } finally {
      HTMLElement.prototype.getBoundingClientRect = real;
    }
  });
});

describe("GuideHost：兜底、局部坐标与 modal", () => {
  it("锚点不存在 → 整步跳过，且不记「看过」（下次还能出）", () => {
    mountFace();
    resetStore();
    useGuideStore.getState().enqueue(["first.conv-title", "first.context-ring"]);

    const { unmount } = mountHost();

    expect(useGuideStore.getState().queue).toEqual([]);
    expect(host()).toBeNull();
    // 量不到只是"这次看不到"，不能当成"看过"——否则一次环境没就绪就永久烧掉
    expect(useGuideStore.getState().seen.size).toBe(0);
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

  it("通知式：压暗子页内容区，只有「知道了」", () => {
    mountFace();
    mountAnchor("settings-body", { left: 64, top: 78, width: 212, height: 150 });

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

  it("欢迎卡：只有一个「开始探索」，点了推上三步导览", () => {
    mountFace();
    // 三步的锚点得在位（不在位会被兜底跳过——那是正确行为，不是本例要测的）
    for (const key of ["new-chat", "settings", "input"]) {
      mountAnchor(key, { left: 10, top: 20, width: 100, height: 20 });
    }
    resetStore();
    useGuideStore.getState().enqueue(["welcome.intro"]);
    const a = mountHost();

    const card = host()?.lastElementChild as HTMLElement;
    const labels = Array.from(card.querySelectorAll("button")).map(
      (b) => b.textContent
    );
    expect(labels).toEqual(["开始探索"]);
    // 不给「跳过」：后续每一步自己有跳过，这里再放一个只会让人问"跳过什么"
    expect(labels.join()).not.toContain("跳过");

    act(() => {
      (card.querySelector("button") as HTMLButtonElement).dispatchEvent(
        new MouseEvent("click", { bubbles: true })
      );
    });
    // 卡自己记成看过并出队，三步推上
    expect(useGuideStore.getState().queue).toEqual(WELCOME_TOUR);
    expect(useGuideStore.getState().seen.has("welcome.intro")).toBe(true);
    a.unmount();
  });

  it("换步不重新等待：不会消失一下再回来（不闪）", async () => {
    vi.useFakeTimers();
    try {
      mountFace();
      for (const key of ["new-chat", "settings", "input"]) {
        mountAnchor(key, { left: 10, top: 20, width: 100, height: 20 });
      }
      resetStore();
      useGuideStore.getState().enqueue(WELCOME_TOUR);
      const { unmount } = mountHost();

      // 首步：等延迟（+ 稳定等待）后才现身
      expect(host()?.getAttribute("data-guide-hidden")).toBe("1");
      await act(async () => {
        for (let i = 0; i < 8; i++) {
          vi.advanceTimersByTime(60);
          await Promise.resolve();
        }
      });
      expect(host()?.getAttribute("data-guide-hidden")).toBeNull();

      // 「下一步」：直接用下一步接上，不再进 hidden
      act(() => {
        useGuideStore.getState().advance("welcome.new-chat");
      });
      expect(
        document.querySelector("[data-guide-dots]")?.getAttribute("data-guide-dots")
      ).toBe("2");
      expect(host()?.getAttribute("data-guide-hidden")).toBeNull();
      unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("等待期间用户已动手 → 这次不弹，也不写看过", () => {
    mountFace();
    mountAnchor("conv-title", { left: 10, top: 20, width: 100, height: 20 });
    resetStore();
    useGuideStore.getState().enqueue(["first.conv-title"]);
    const { unmount } = mountHost();

    // 到点前用户点了别处（或聚焦了输入）
    act(() => {
      document.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true }));
    });
    expect(useGuideStore.getState().queue).toEqual([]);
    expect(useGuideStore.getState().seen.size).toBe(0);
    unmount();
  });

  it("引导延后出现：先给页面留出渲染时间", async () => {
    vi.useFakeTimers();
    try {
      mountFace();
      mountAnchor("conv-title", { left: 10, top: 20, width: 100, height: 20 });
      resetStore();
      useGuideStore.getState().enqueue(["first.conv-title"]);
      const { unmount } = mountHost();

      // 到点前：已排好位（元素在、尺寸在），只是不可见
      expect(host()?.getAttribute("data-guide-hidden")).toBe("1");
      await act(async () => {
        for (let i = 0; i < 8; i++) {
          vi.advanceTimersByTime(60);
          await Promise.resolve();
        }
      });
      expect(host()?.getAttribute("data-guide-hidden")).toBeNull();
      unmount();
    } finally {
      vi.useRealTimers();
    }
  });

  it("别的表面的项不挤掉当前表面的项（模型页通知曾被吞掉）", () => {
    mountFace();
    mountAnchor("settings-body", { left: 64, top: 78, width: 212, height: 150 });
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

  it("通知卡在子页内容区里双向居中（导航与页标题露在暗色外）", () => {
    mountFace();
    mountAnchor("settings-body", { left: 64, top: 78, width: 212, height: 150 });

    resetStore();
    useUIStore.setState({ overlay: "settings" });
    act(() => {
      useGuideStore.getState().enqueueNotice("settings.usage");
    });

    const { unmount } = mountHost();
    const card = host()?.lastElementChild as HTMLElement;
    // 宽度 = 内容区宽 212 − 8×2 = 196（上限 300 不再卡成靠左的小块）
    expect(card.style.width).toBe("196px");
    // 水平居中于内容区：64 + 212/2 = 170
    expect(card.style.left).toBe("170px");
    expect(card.style.transform).toContain("translateX");
    // 纵向：卡片尺寸量不到时从内容区顶部起（jsdom 无布局），不跳出去
    expect(card.style.top).toBe("86px");
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
