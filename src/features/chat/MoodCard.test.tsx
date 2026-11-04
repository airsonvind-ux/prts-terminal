// @vitest-environment jsdom
// 情绪卡（MOOD v2）渲染测试
//
// 三件事必须钉住：有心声就显示心声、没有就回落到旁白、颜色跟随偏向色相。
// 旧归档只有 mood + intensity，没有 tint/text，走的也是这两条路径。

import { afterEach, describe, expect, it } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import type { SessionEvent } from "../../types";
import { MoodCard } from "./MoodCard";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const mounted: Array<{ container: HTMLDivElement; root: ReturnType<typeof createRoot> }> = [];

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
});

const mood = (event: Record<string, unknown>): SessionEvent =>
  ({ type: "mood/event", sessionId: "s1", intensity: 0, ...event } as SessionEvent);

function render(events: SessionEvent[]) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(React.createElement(MoodCard, { events }));
  });
  mounted.push({ container, root });
  return {
    container,
    text: () => container.textContent ?? "",
    hue: () => {
      const line = container.querySelector<HTMLElement>("[data-mood-card] p");
      return line?.style.getPropertyValue("--mood-hue") ?? null;
    },
  };
}

describe("MoodCard", () => {
  it("有心声就显示心声，不显示旁白", () => {
    const view = render([
      mood({ mood: "高兴", intensity: 2, text: "对上了，就是这张。" }),
    ]);
    expect(view.text()).toBe("对上了，就是这张。");
    expect(view.text()).not.toContain("PRTS");
  });

  it("没有心声回落到旁白（旧归档、模型没写心声）", () => {
    const view = render([mood({ mood: "沮丧", intensity: 1 })]);
    expect(view.text()).toBe("PRTS 有些沮丧，沉下去就不想动");
  });

  it("未知偏向只定色相，旁白走中性兜底", () => {
    const view = render([mood({ tint: "警惕", intensity: 0 })]);
    expect(view.text()).toBe("PRTS 没说什么，安静地待着");
    expect(view.hue()).toBe("#7f8794");
  });

  it("三个已知偏向各有色相", () => {
    const cases: Array<[string, string]> = [
      ["高兴", "#d7a94e"],
      ["困惑", "#6f7c8c"],
      ["沮丧", "#5f6f95"],
    ];
    for (const [name, hue] of cases) {
      const view = render([mood({ mood: name, intensity: 0 })]);
      expect(view.hue()).toBe(hue);
      act(() => mounted.pop()!.root.unmount());
    }
  });

  it("没有情绪事件时不渲染空壳", () => {
    const view = render([]);
    expect(view.container.querySelector("[data-mood-card]")).toBeNull();
  });
});
