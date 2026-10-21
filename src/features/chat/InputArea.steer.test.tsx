// @vitest-environment jsdom
// src/components/content/InputArea.steer.test.tsx
// 三态按钮 + Enter 行为（插话设计定稿）：
// - 空闲有字 → 发送（onSubmit）；生成中有字 → 插话（onSteer，成功才清空）
// - 生成中无字 → 中止方块形态（data-mode=stop），点击 → onStop
// - 无字 / 硬禁回车 → 忽略且保留输入（修复原 busy 回车静默丢字 bug）
// - 插话失败 → 输入保留
//
// InputArea 为纯 props 组件（无 tauri 依赖），直接注入 mock props。

import { describe, it, expect, vi, beforeAll, afterAll, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { InputArea } from "./InputArea";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

// jsdom 无 WAAPI getAnimations/animate（InputArea 高度/清空复位依赖）：
// stub 最小实现（仿 MessageList 测试 ResizeObserver stub 模式）
const originalGetAnimations = Element.prototype.getAnimations;
const originalAnimate = Element.prototype.animate;

beforeAll(() => {
  (Element.prototype as { getAnimations?: unknown }).getAnimations = () => [];
  (Element.prototype as { animate?: unknown }).animate = () => ({});
});

afterAll(() => {
  if (originalGetAnimations === undefined) {
    delete (Element.prototype as { getAnimations?: unknown }).getAnimations;
  } else {
    Element.prototype.getAnimations = originalGetAnimations;
  }
  if (originalAnimate === undefined) {
    delete (Element.prototype as { animate?: unknown }).animate;
  } else {
    Element.prototype.animate = originalAnimate;
  }
});

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

function renderInput(props: Partial<React.ComponentProps<typeof InputArea>>) {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  const callbacks = {
    onSubmit: vi.fn<(text: string) => void>(),
    onSteer: vi.fn<(text: string) => Promise<boolean>>(() =>
      Promise.resolve(true)
    ),
    onStop: vi.fn<() => void>(),
    generating: false,
    disabled: false,
    aborting: false,
    ...props,
  };
  act(() => {
    root.render(
      React.createElement(InputArea, {
        onSubmit: callbacks.onSubmit,
        onSteer: callbacks.onSteer,
        onStop: callbacks.onStop,
        generating: callbacks.generating,
        disabled: callbacks.disabled,
        aborting: callbacks.aborting,
      })
    );
  });
  const textarea = container.querySelector("textarea") as HTMLTextAreaElement;
  // 发送按钮以 data-mode 标记（新增附件按钮不带该属性，避免误选）
  const button = container.querySelector("button[data-mode]") as HTMLButtonElement;
  return { callbacks, textarea, button };
}

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
});

/** React 受控 textarea：原生 setter + input 事件驱动 onChange */
function typeText(ta: HTMLTextAreaElement, value: string): void {
  act(() => {
    const setter = Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value"
    )!.set!;
    setter.call(ta, value);
    ta.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function pressEnter(ta: HTMLTextAreaElement): void {
  act(() => {
    ta.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "Enter",
        bubbles: true,
        cancelable: true,
      })
    );
  });
}

function click(btn: HTMLButtonElement): void {
  act(() => {
    btn.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

async function flush(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
  });
}

describe("InputArea · 三态按钮", () => {
  it("空闲无字 → idle 禁用；输入后 → send（同款箭头充能形态）", () => {
    const { button, textarea } = renderInput({});
    expect(button.dataset.mode).toBe("idle");
    expect(button.disabled).toBe(true);
    typeText(textarea, "查一下源石虫");
    expect(button.dataset.mode).toBe("send");
    expect(button.disabled).toBe(false);
  });

  it("生成中无字 → stop（中止方块，可用）；点击 → onStop 且不清输入", () => {
    const { callbacks, button, textarea } = renderInput({ generating: true });
    expect(button.dataset.mode).toBe("stop");
    expect(button.disabled).toBe(false);
    click(button);
    expect(callbacks.onStop).toHaveBeenCalledTimes(1);
    expect(callbacks.onSubmit).not.toHaveBeenCalled();
    expect(callbacks.onSteer).not.toHaveBeenCalled();
    expect(textarea.value).toBe("");
  });

  it("生成中有字 → steer（发送样式同款 data-mode=steer）；点击 → 插话提交", async () => {
    const { callbacks, button, textarea } = renderInput({ generating: true });
    typeText(textarea, "插句话");
    expect(button.dataset.mode).toBe("steer");
    expect(button.disabled).toBe(false);
    click(button);
    expect(callbacks.onSteer).toHaveBeenCalledWith("插句话");
    expect(callbacks.onSubmit).not.toHaveBeenCalled();
    // 成功后清空输入（异步 resolve 后）
    await flush();
    expect(textarea.value).toBe("");
  });

  it("生成中插话失败 → toast 已由上层处理；输入保留（不清空）", async () => {
    const onSteer = vi.fn<(text: string) => Promise<boolean>>(() =>
      Promise.resolve(false)
    );
    const { textarea, button } = renderInput({ generating: true, onSteer });
    typeText(textarea, "保留我");
    click(button);
    await flush();
    expect(textarea.value).toBe("保留我");
  });

  it("硬禁（disabled）→ 按钮禁用；无字不现 stop、有字也不提交", () => {
    const { button, textarea } = renderInput({ generating: true, disabled: true });
    // disabled 优先：生成中被硬禁 → 无中止块、按钮禁用
    expect(button.dataset.mode).toBe("idle");
    expect(button.disabled).toBe(true);
    typeText(textarea, "有字也禁");
    expect(button.disabled).toBe(true);
    click(button);
    expect(button.dataset.mode).toBe("send"); // 样式仍为发送（按钮禁用不可点）
  });

  it("aborting（正在中止）→ 细环图标 + 输入框提示；按钮禁用", () => {
    const { button, textarea } = renderInput({ generating: true, aborting: true });
    expect(textarea.placeholder).toBe("正在中止…");
    expect(button.disabled).toBe(true);
    // 细环旋转图标（替下中止方块）
    expect(button.querySelector("circle")).not.toBeNull();
    expect(button.querySelector("rect")).toBeNull();
  });
});

describe("InputArea · Enter 行为", () => {
  it("空闲有字回车 → onSubmit；输入清空", () => {
    const { callbacks, textarea } = renderInput({});
    typeText(textarea, "发送这条");
    pressEnter(textarea);
    expect(callbacks.onSubmit).toHaveBeenCalledWith("发送这条");
    expect(callbacks.onSteer).not.toHaveBeenCalled();
    expect(textarea.value).toBe("");
  });

  it("生成中有字回车 → 插话提交（不再静默丢字）", async () => {
    const { callbacks, textarea } = renderInput({ generating: true });
    typeText(textarea, "插话内容");
    pressEnter(textarea);
    expect(callbacks.onSteer).toHaveBeenCalledWith("插话内容");
    expect(callbacks.onSubmit).not.toHaveBeenCalled();
    await flush();
    expect(textarea.value).toBe("");
  });

  it("生成中无字回车 → 忽略（无调用、无副作用）", () => {
    const { callbacks, textarea } = renderInput({ generating: true });
    pressEnter(textarea);
    expect(callbacks.onSteer).not.toHaveBeenCalled();
    expect(callbacks.onSubmit).not.toHaveBeenCalled();
    expect(callbacks.onStop).not.toHaveBeenCalled();
  });

  it("硬禁（error/恢复中/压缩中）有字回车 → 忽略且**保留输入**（杜绝丢字）", () => {
    const { callbacks, textarea } = renderInput({ disabled: true });
    typeText(textarea, "不能丢的字");
    pressEnter(textarea);
    expect(callbacks.onSubmit).not.toHaveBeenCalled();
    expect(callbacks.onSteer).not.toHaveBeenCalled();
    expect(textarea.value).toBe("不能丢的字");
  });
});
