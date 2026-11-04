// @vitest-environment jsdom
// 情绪卡（情绪系统草案 3）渲染测试
//
// 要钉住的四件事：偏向词 + 强度前缀、有心声显示心声、没有就回落旁白、
// 两轴配色（竖线 / 文字）与「强度只改竖线明度」。旧归档只有 mood +
// intensity，走的是偏向词 + 旁白这条路径。

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
  const line = () => container.querySelector<HTMLElement>("[data-mood-card] p");
  return {
    container,
    line,
    bias: () => container.querySelector<HTMLElement>("[data-mood-bias]")?.textContent ?? null,
    voice: () => container.querySelector<HTMLElement>("[data-mood-voice]")?.textContent ?? null,
    varOf: (name: string) => line()?.style.getPropertyValue(name) ?? null,
  };
}

describe("MoodCard", () => {
  it("偏向词与强度在左，心声在右", () => {
    const view = render([
      mood({ mood: "高兴", tint: "高兴", intensity: 2, text: "对上了，就是这张。" }),
    ]);
    expect(view.bias()).toBe("高兴 · 中");
    expect(view.voice()).toBe("对上了，就是这张。");
  });

  it("没有强度时不写「· 弱/中/强」", () => {
    const view = render([mood({ tint: "好奇", intensity: 0, text: "这层构造有点眼熟。" })]);
    expect(view.bias()).toBe("好奇");
    expect(view.voice()).toBe("这层构造有点眼熟。");
  });

  it("自由偏向保留原词做标签，文字色走中性", () => {
    const view = render([mood({ tint: "警惕", intensity: 0 })]);
    expect(view.bias()).toBe("警惕");
    expect(view.voice()).toBe("PRTS 没说什么，安静地待着");
    expect(view.varOf("--mood-line")).toBe("hsl(210 10% 40%)");
    expect(view.varOf("--mood-text")).toBe("hsl(210 8% 58%)");
  });

  it("旧归档只有 mood 时，偏向词取 mood、旁白按强度取档", () => {
    const view = render([mood({ mood: "沮丧", intensity: 1 })]);
    expect(view.bias()).toBe("沮丧 · 弱");
    expect(view.voice()).toBe("PRTS 有些沮丧，沉下去就不想动");
  });

  it("三个已知偏向各有竖线 / 文字两色", () => {
    const cases: Array<[string, string, string]> = [
      ["高兴", "hsl(20 46% 53%)", "hsl(20 40% 74%)"],
      ["困惑", "hsl(160 26% 46%)", "hsl(160 22% 72%)"],
      ["沮丧", "hsl(215 28% 48%)", "hsl(215 28% 70%)"],
    ];
    for (const [name, line, text] of cases) {
      const view = render([mood({ mood: name, intensity: 0 })]);
      expect(view.varOf("--mood-line")).toBe(line);
      expect(view.varOf("--mood-text")).toBe(text);
      act(() => mounted.pop()!.root.unmount());
    }
  });

  it("强度只改竖线明度，不改色相与文字色", () => {
    const weak = render([mood({ mood: "高兴", intensity: 1 })]);
    const strong = render([mood({ mood: "高兴", intensity: 3 })]);
    expect(weak.varOf("--mood-line")).toBe("hsl(20 46% 47%)");
    expect(strong.varOf("--mood-line")).toBe("hsl(20 46% 59%)");
    expect(weak.varOf("--mood-text")).toBe(strong.varOf("--mood-text"));
  });

  it("没有情绪事件时不渲染空壳", () => {
    const view = render([]);
    expect(view.container.querySelector("[data-mood-card]")).toBeNull();
  });
});
