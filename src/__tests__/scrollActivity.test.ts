// @vitest-environment jsdom
// scrollActivity · 滚动显色节奏与 CSS 契约
//
// 覆盖：
//   - 滚动目标获得 prts-scroll-active；1000ms 无滚动后移除；再次滚动刷新计时
//   - CSS 契约：伪元素路径（不设压制性标准属性）；悬停加深由 :hover 驱动；
//     视觉收窄参数；按钮隐藏（防回归）
//   注：悬停/拖动加深由伪元素 :hover 直接驱动（无 JS 判定），故无对应行为用例。

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import scrollbarCss from "../styles/scrollbar.css?raw";
import tokensCss from "../styles/tokens.css?raw";
import { installScrollActivity } from "../scrollActivity";

const ACTIVE = "prts-scroll-active";

let dispose: () => void;

beforeEach(() => {
  vi.useFakeTimers();
  dispose = installScrollActivity();
});

afterEach(() => {
  dispose();
  vi.useRealTimers();
  document.body.replaceChildren();
});

function scrollOn(el: Element): void {
  el.dispatchEvent(new Event("scroll"));
}

describe("scrollActivity · 滚动显色节奏", () => {
  it("滚动目标获得显色类，1000ms 无滚动后移除", () => {
    const el = document.createElement("div");
    document.body.appendChild(el);
    scrollOn(el);
    expect(el.classList.contains(ACTIVE)).toBe(true);

    vi.advanceTimersByTime(999);
    expect(el.classList.contains(ACTIVE)).toBe(true);

    vi.advanceTimersByTime(1);
    expect(el.classList.contains(ACTIVE)).toBe(false);
  });

  it("滚动中再次滚动刷新计时（不在首个 1000ms 到点时误移除）", () => {
    const el = document.createElement("div");
    document.body.appendChild(el);
    scrollOn(el);
    vi.advanceTimersByTime(600);
    scrollOn(el);
    vi.advanceTimersByTime(600); // 累计 1200ms，但计时已被刷新
    expect(el.classList.contains(ACTIVE)).toBe(true);

    vi.advanceTimersByTime(400); // 距第二次滚动满 1000ms
    expect(el.classList.contains(ACTIVE)).toBe(false);
  });
});

describe("scrollActivity · CSS 契约（防回归）", () => {
  it("tokens 记录滚动条半透明档与方舟青备选", () => {
    expect(tokensCss).toMatch(
      /--scrollbar-thumb:\s*rgba\(95,\s*107,\s*120,\s*0?\.5\)/
    );
    expect(tokensCss).toMatch(
      /--scrollbar-thumb-hover:\s*rgba\(135,\s*149,\s*163,\s*0?\.8\)/
    );
    expect(tokensCss).toContain("#18d1ff");
    expect(tokensCss).toMatch(/--cyan-soft:\s*rgba\(24,\s*209,\s*255,\s*0?\.4\)/);
    expect(tokensCss).toMatch(/--cyan-strong:\s*rgba\(24,\s*209,\s*255,\s*0?\.7\)/);
  });

  it("scrollbar.css：伪元素路径——不设压制性标准属性，:hover/类驱动", () => {
    // 不得出现非 auto 的滚动条标准属性声明（压制源，防回归）
    expect(scrollbarCss).not.toMatch(
      /scrollbar-(color|width)\s*:\s*(?!auto\b)[^;\n]+;/
    );
    // 悬停加深只由 :hover（滑块本体）驱动，不残留坐标判定类（防误判回归）
    expect(scrollbarCss).not.toContain("prts-scroll-hover");
    // 着色必须用 background-color（background 简写会重置 background-clip，
    // 导致视觉收窄失效——满宽显示）
    expect(scrollbarCss).not.toMatch(/background:\s*var\(--scrollbar/);
    // 伪元素层关键规则
    expect(scrollbarCss).toMatch(
      /::-webkit-scrollbar-thumb\s*\{[^}]*background-color:\s*transparent/s
    );
    expect(scrollbarCss).toMatch(
      /::-webkit-scrollbar-thumb\s*\{[^}]*background-clip:\s*padding-box/s
    );
    expect(scrollbarCss).toMatch(
      /\.prts-scroll-active::-webkit-scrollbar-thumb\s*\{[^}]*var\(--scrollbar-thumb\)/s
    );
    expect(scrollbarCss).toMatch(
      /::-webkit-scrollbar-thumb:hover\s*\{[^}]*var\(--scrollbar-thumb-hover\)/s
    );
    expect(scrollbarCss).toMatch(
      /::-webkit-scrollbar-thumb:vertical\s*\{[^}]*border-left:\s*var\(--scrollbar-inset,\s*1px\)/s
    );
    expect(scrollbarCss).toMatch(
      /::-webkit-scrollbar-button\s*\{[^}]*display:\s*none/s
    );
  });
});
