// @vitest-environment jsdom
// useTypewriter 逐字展开测试（fake timers 推进）
//
// 覆盖：
//   - 45ms/字 逐字露出，打完停住
//   - enabled=false 恒为空串（启动演出期间留白），开闸后从零打
//   - 文本变化立即清空重打（旧文本没有退出动画）

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { useTypewriter, TITLE_CHAR_MS } from "../useTypewriter";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;
let props: { text: string | undefined; enabled: boolean };

function Probe() {
  const t = useTypewriter(props.text, props.enabled);
  return <span data-title>{t}</span>;
}

function title(): string {
  return container.querySelector("[data-title]")?.textContent ?? "";
}

async function tick(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  props = { text: "苔手", enabled: true };
  container = document.createElement("div");
  document.body.appendChild(container);
  act(() => {
    root = createRoot(container);
  });
});

afterEach(() => {
  act(() => {
    root.unmount();
  });
  container.remove();
  vi.useRealTimers();
});

describe("useTypewriter 逐字展开", () => {
  it("按 45ms/字 逐字露出，打完停在全文", async () => {
    await act(async () => {
      root.render(<Probe />);
    });
    expect(title()).toBe("");

    await tick(TITLE_CHAR_MS);
    expect(title()).toBe("苔");

    await tick(TITLE_CHAR_MS);
    expect(title()).toBe("苔手");

    await tick(TITLE_CHAR_MS * 4);
    expect(title()).toBe("苔手");
  });

  it("enabled=false：恒为空串（启动演出期间留白），开闸后从零打", async () => {
    props.enabled = false;
    await act(async () => {
      root.render(<Probe />);
    });
    expect(title()).toBe("");

    await tick(TITLE_CHAR_MS * 3);
    expect(title()).toBe("");

    props.enabled = true;
    await act(async () => {
      root.render(<Probe />);
    });
    expect(title()).toBe("");

    await tick(TITLE_CHAR_MS * 2);
    expect(title()).toBe("苔手");
  });

  it("文本变化：立刻清空重打（旧文本没有退出动画）", async () => {
    await act(async () => {
      root.render(<Probe />);
    });
    await tick(TITLE_CHAR_MS * 2);
    expect(title()).toBe("苔手");

    props.text = "事不过四";
    await act(async () => {
      root.render(<Probe />);
    });
    expect(title()).toBe("");

    await tick(TITLE_CHAR_MS);
    expect(title()).toBe("事");
  });
});
