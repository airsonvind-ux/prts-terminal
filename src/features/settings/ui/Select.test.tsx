// @vitest-environment jsdom
// Select 的键盘契约：方向键移动、Enter 选中、Escape 关闭并回焦。

import { afterEach, describe, expect, it, vi } from "vitest";
import React, { act, useState } from "react";
import { createRoot } from "react-dom/client";
import { Select } from "./Select";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

const OPTIONS = [
  { value: "a", label: "甲" },
  { value: "b", label: "乙" },
  { value: "c", label: "丙" },
];

function Harness({ onChange }: { onChange: (value: string) => void }) {
  const [value, setValue] = useState("a");
  return (
    <Select
      ariaLabel="测试选项"
      value={value}
      options={OPTIONS}
      onChange={(next) => {
        setValue(next);
        onChange(next);
      }}
    />
  );
}

function keydown(target: Element, key: string): void {
  act(() => {
    target.dispatchEvent(
      new KeyboardEvent("keydown", {
        key,
        bubbles: true,
        cancelable: true,
      })
    );
  });
}

function render(onChange = vi.fn()): void {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(React.createElement(Harness, { onChange }));
  });
}

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
});

describe("Select · 键盘", () => {
  it("ArrowDown 打开并聚焦当前项；方向键移动；Enter 选中并回焦", () => {
    const onChange = vi.fn();
    render(onChange);

    const trigger = container.querySelector<HTMLButtonElement>(
      'button[aria-label="测试选项"]'
    )!;
    trigger.focus();
    keydown(trigger, "ArrowDown");

    const list = container.querySelector<HTMLElement>('[role="listbox"]')!;
    expect(list).toBeTruthy();
    const options = Array.from(
      list.querySelectorAll<HTMLButtonElement>('[role="option"]')
    );
    expect(options).toHaveLength(3);
    expect(document.activeElement).toBe(options[0]);
    expect(options[0].getAttribute("aria-selected")).toBe("true");

    keydown(options[0], "ArrowDown");
    expect(document.activeElement).toBe(options[1]);

    keydown(options[1], "Enter");
    expect(onChange).toHaveBeenCalledWith("b");
    expect(container.querySelector('[role="listbox"]')).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("Escape 关闭菜单并把焦点还给触发按钮", () => {
    render();
    const trigger = container.querySelector<HTMLButtonElement>(
      'button[aria-label="测试选项"]'
    )!;
    trigger.focus();
    keydown(trigger, "ArrowDown");

    const option = container.querySelector<HTMLButtonElement>(
      '[role="option"]'
    )!;
    keydown(option, "Escape");

    expect(container.querySelector('[role="listbox"]')).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });
});
