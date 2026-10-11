// @vitest-environment jsdom
// src/__tests__/guide.test.tsx
// 引导说明层 · 键集去重（看过不再出）+ 兜底（量不到锚点整步跳过）+ 渲染与推进

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { GuideHost } from "../components/guide/GuideHost";
import { GUIDE_ITEMS, WELCOME_TOUR } from "../components/guide/guideContent";
import { useGuideStore } from "../stores/useGuideStore";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

type InvokeMock = ReturnType<typeof vi.fn> & {
  mock: { calls: unknown[][] };
};
const g = globalThis as unknown as {
  __invokeMock: InvokeMock;
  __defaultInvokeImpl: (cmd: string) => unknown;
};

/** 重置 store（默认已 hydrated、无已看过项）。 */
function resetStore(seen: string[] = [], hydrated = true): void {
  useGuideStore.setState({ hydrated, seen: new Set(seen), queue: [] });
}

/** 给锚点一个非零矩形（jsdom 一律返回 0，会被判成"量不到"）。 */
function stubRect(el: HTMLElement): void {
  el.getBoundingClientRect = () =>
    ({
      left: 10,
      top: 20,
      width: 100,
      height: 20,
      right: 110,
      bottom: 40,
    }) as DOMRect;
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

beforeEach(() => {
  g.__invokeMock.mockReset();
  g.__invokeMock.mockImplementation(g.__defaultInvokeImpl);
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

    useGuideStore.getState().advance(); // 记 seen + 前进
    useGuideStore.getState().enqueue(["settings.app"]);

    expect(useGuideStore.getState().queue).toEqual([]);
    expect(useGuideStore.getState().seen.has("settings.app")).toBe(true);
  });

  it("advance 会把「看过」落盘（mark_guide_seen）", () => {
    resetStore();
    useGuideStore.getState().enqueue(["settings.model"]);
    useGuideStore.getState().advance();

    const calls = g.__invokeMock.mock.calls.filter(
      (c) => c[0] === "mark_guide_seen"
    );
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toEqual({ key: "settings.model" });
  });

  it("exit 只记当前项，其余作废（下次到那个表面再出）", () => {
    resetStore();
    useGuideStore.getState().enqueue(WELCOME_TOUR);
    useGuideStore.getState().exit();

    const s = useGuideStore.getState();
    expect(s.queue).toEqual([]);
    expect(s.seen.has("welcome.new-chat")).toBe(true);
    expect(s.seen.has("welcome.settings")).toBe(false);
    expect(s.seen.has("welcome.input")).toBe(false);
  });
});

describe("GuideHost：兜底与渲染", () => {
  it("锚点不存在 → 整步跳过，队列清空且不留半张卡", () => {
    resetStore();
    useGuideStore.getState().enqueue(["first.conv-title", "first.context-ring"]);

    const { unmount } = mountHost();

    expect(useGuideStore.getState().queue).toEqual([]);
    expect(document.querySelector("[data-guide-host]")).toBeNull();
    unmount();
  });

  it("锚点存在 → 挖洞 + 卡片，文案取自 guideContent", () => {
    const anchor = document.createElement("div");
    anchor.setAttribute("data-guide", "conv-title");
    stubRect(anchor);
    document.body.appendChild(anchor);

    resetStore();
    useGuideStore.getState().enqueue(["first.conv-title"]);

    const { unmount } = mountHost();
    const host = document.querySelector("[data-guide-host]");
    expect(host).not.toBeNull();
    expect(host?.textContent).toContain(GUIDE_ITEMS["first.conv-title"].title);
    expect(host?.textContent).toContain(GUIDE_ITEMS["first.conv-title"].body);
    // 未看完 → 仍在队列里（点了「下一步」才记 seen）
    expect(useGuideStore.getState().queue).toEqual(["first.conv-title"]);
    unmount();
  });

  it("通知式：锚点是设置面板，压暗 + 「知道了」", () => {
    const panel = document.createElement("div");
    panel.setAttribute("data-guide", "settings-panel");
    stubRect(panel);
    document.body.appendChild(panel);

    resetStore();
    useGuideStore.getState().enqueue(["settings.space"]);

    const { unmount } = mountHost();
    const host = document.querySelector("[data-guide-host]");
    expect(host?.textContent).toContain(GUIDE_ITEMS["settings.space"].body);
    expect(host?.textContent).toContain("知道了");
    // 通知式不给「跳过」（只有一张卡，点「知道了」即完）
    expect(host?.textContent).not.toContain("跳过");
    unmount();
  });
});
