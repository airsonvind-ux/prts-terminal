// @vitest-environment jsdom
// FindBar · 会话内查找条
//
// 覆盖：Ctrl+F 打开、输入即时匹配与计数、Enter 步进（循环）与滚动、
//       无命中 0/0、Esc 关闭并清除高亮。
// Highlight API 与 scrollIntoView 在 jsdom 不存在——本文件 stub。

import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { FindBar } from "./FindBar";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

class FakeHighlight {
  ranges: Range[];
  constructor(...ranges: Range[]) {
    this.ranges = ranges;
  }
}

const originalScrollIntoView = Element.prototype.scrollIntoView;
const originalRangeRect = Range.prototype.getBoundingClientRect;
const scrollSpy = vi.fn();

beforeAll(() => {
  (
    CSS as unknown as { highlights: Map<string, FakeHighlight> }
  ).highlights = new Map();
  (globalThis as unknown as { Highlight: typeof FakeHighlight }).Highlight =
    FakeHighlight;
  (Element.prototype as unknown as { scrollIntoView: unknown }).scrollIntoView =
    scrollSpy;
  (Range.prototype as unknown as { getBoundingClientRect: () => DOMRect }).getBoundingClientRect =
    () => new DOMRect(0, 0, 0, 0);
});

afterAll(() => {
  delete (CSS as unknown as { highlights?: unknown }).highlights;
  delete (globalThis as { Highlight?: unknown }).Highlight;
  if (originalScrollIntoView === undefined) {
    delete (Element.prototype as { scrollIntoView?: unknown }).scrollIntoView;
  } else {
    Element.prototype.scrollIntoView = originalScrollIntoView;
  }
  if (originalRangeRect === undefined) {
    delete (Range.prototype as { getBoundingClientRect?: unknown })
      .getBoundingClientRect;
  } else {
    Range.prototype.getBoundingClientRect = originalRangeRect;
  }
});

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;
let list: HTMLDivElement;

beforeEach(() => {
  scrollSpy.mockClear();
  (CSS as unknown as { highlights: Map<string, FakeHighlight> }).highlights.clear();

  list = document.createElement("div");
  list.setAttribute("data-message-list", "");
  list.innerHTML = "<p>alpha beta</p><p>alpha gamma</p>";
  document.body.appendChild(list);

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(React.createElement(FindBar));
  });
});

afterEach(() => {
  act(() => {
    root.unmount();
  });
  container.remove();
  list.remove();
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

function highlights(): Map<string, FakeHighlight> {
  return (CSS as unknown as { highlights: Map<string, FakeHighlight> }).highlights;
}

function barText(): string {
  return document.querySelector("[data-findbar]")?.textContent ?? "";
}

function getInput(): HTMLInputElement {
  return document.querySelector("input") as HTMLInputElement;
}

function pressCtrlF() {
  act(() => {
    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "f", ctrlKey: true, cancelable: true })
    );
  });
}

/** React 受控 input：原生 setter + input 事件（项目测试先例） */
function type(value: string) {
  const el = getInput();
  const setter = Object.getOwnPropertyDescriptor(
    HTMLInputElement.prototype,
    "value"
  )!.set!;
  act(() => {
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

describe("FindBar", () => {
  it("默认不渲染；Ctrl+F 打开", () => {
    expect(document.querySelector("[data-findbar]")).toBeNull();
    pressCtrlF();
    expect(document.querySelector("[data-findbar]")).toBeTruthy();
  });

  it("Ctrl+F 时页面有选中文本 → 直接作为查找词", () => {
    vi.spyOn(window, "getSelection").mockReturnValue({
      toString: () => " beta ",
    } as unknown as Selection);
    pressCtrlF();
    expect(getInput().value).toBe("beta");
    expect(barText()).toContain("1/1");
  });

  it("输入即时匹配：计数与高亮同步", () => {
    pressCtrlF();
    type("alpha");
    expect(barText()).toContain("1/2");
    expect(highlights().has("prts-find")).toBe(true);
    expect(highlights().get("prts-find")!.ranges.length).toBe(2);
    expect(highlights().has("prts-find-active")).toBe(true);
  });

  it("Enter 步进（循环）并触发滚动", () => {
    pressCtrlF();
    type("alpha");
    expect(barText()).toContain("1/2");
    act(() => {
      getInput().dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true })
      );
    });
    expect(barText()).toContain("2/2");
    expect(scrollSpy).toHaveBeenCalled();
    act(() => {
      getInput().dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true })
      );
    });
    expect(barText()).toContain("1/2");
  });

  it("无命中显示 0/0 且清空高亮", () => {
    pressCtrlF();
    type("zzzz");
    expect(barText()).toContain("0/0");
    expect(highlights().has("prts-find")).toBe(false);
  });

  it("打开时消息区获得让位类，关闭时移除", () => {
    expect(list.classList.contains("prts-find-open")).toBe(false);
    pressCtrlF();
    expect(list.classList.contains("prts-find-open")).toBe(true);
    act(() => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", cancelable: true })
      );
    });
    expect(list.classList.contains("prts-find-open")).toBe(false);
  });

  it("Esc 关闭并清除高亮", () => {
    pressCtrlF();
    type("alpha");
    expect(highlights().has("prts-find")).toBe(true);
    act(() => {
      window.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", cancelable: true })
      );
    });
    expect(document.querySelector("[data-findbar]")).toBeNull();
    expect(highlights().has("prts-find")).toBe(false);
    expect(highlights().has("prts-find-active")).toBe(false);
  });
});
