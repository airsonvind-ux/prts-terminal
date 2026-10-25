// @vitest-environment jsdom
// 覆盖层键盘契约：dialog 语义、初始焦点、Tab 循环、Escape、关闭还原。

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { OverlayContainer } from "./OverlayContainer";
import { useUIStore } from "../../stores/useUIStore";
import { useConversationStore } from "../../stores/useConversationStore";
import { useProcessingStore } from "../../stores/useProcessingStore";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

function Harness() {
  const setOverlay = useUIStore((s) => s.setOverlay);
  return (
    <>
      <button data-testid="trigger" onClick={() => setOverlay("history")}>
        open
      </button>
      <OverlayContainer />
    </>
  );
}

function keydown(
  target: Element,
  key: string,
  options: KeyboardEventInit = {}
): void {
  act(() => {
    target.dispatchEvent(
      new KeyboardEvent("keydown", {
        key,
        bubbles: true,
        cancelable: true,
        ...options,
      })
    );
  });
}

async function flushFocusRestore(): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 5));
  });
}

beforeEach(() => {
  useProcessingStore.setState({ processing: {} });
  useConversationStore.setState({
    conversations: [
      { id: "old", title: "旧对话", turns: [], createdAt: Date.now() - 60_000 },
      { id: "new", title: "新对话", turns: [], createdAt: Date.now() },
    ],
    activeId: "new",
  });
  useUIStore.setState({ overlay: null, settingsInitialTab: "app" });

  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(React.createElement(Harness));
  });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  useUIStore.setState({ overlay: null });
});

describe("OverlayContainer · 键盘契约", () => {
  it("打开历史：dialog 语义 + 初始焦点落在当前对话", async () => {
    const trigger = container.querySelector<HTMLButtonElement>(
      '[data-testid="trigger"]'
    )!;
    trigger.focus();
    await act(async () => {
      trigger.click();
      await Promise.resolve();
    });

    const dialog = container.querySelector<HTMLElement>('[role="dialog"]')!;
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(dialog.getAttribute("aria-label")).toBe("历史对话");

    const active = document.activeElement as HTMLElement;
    expect(active.tagName).toBe("BUTTON");
    expect(active.getAttribute("aria-current")).toBe("true");
  });

  it("设置加载完成后焦点落到当前页签", async () => {
    await act(async () => {
      useUIStore.getState().setOverlay("settings");
      await new Promise((r) => setTimeout(r, 20));
    });

    const tab = container.querySelector<HTMLElement>(
      '[role="tab"][aria-selected="true"]'
    );
    expect(tab).toBeTruthy();
    expect(document.activeElement).toBe(tab);
  });

  it("Tab 从最后一个控件回到第一个，Shift+Tab 反向循环", async () => {
    const trigger = container.querySelector<HTMLButtonElement>(
      '[data-testid="trigger"]'
    )!;
    await act(async () => {
      trigger.click();
      await Promise.resolve();
    });

    const dialog = container.querySelector<HTMLElement>('[role="dialog"]')!;
    const buttons = Array.from(
      dialog.querySelectorAll<HTMLButtonElement>("button")
    );
    const first = buttons[0];
    const last = buttons[buttons.length - 1];

    last.focus();
    keydown(last, "Tab");
    expect(document.activeElement).toBe(first);

    keydown(first, "Tab", { shiftKey: true });
    expect(document.activeElement).toBe(last);
  });

  it("Escape 关闭，焦点回到打开前的触发按钮", async () => {
    const trigger = container.querySelector<HTMLButtonElement>(
      '[data-testid="trigger"]'
    )!;
    trigger.focus();
    await act(async () => {
      trigger.click();
      await Promise.resolve();
    });

    const active = document.activeElement as HTMLElement;
    keydown(active, "Escape");
    await flushFocusRestore();

    expect(useUIStore.getState().overlay).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("历史标题编辑中的 Escape 只取消编辑，不关闭覆盖层", async () => {
    const trigger = container.querySelector<HTMLButtonElement>(
      '[data-testid="trigger"]'
    )!;
    await act(async () => {
      trigger.click();
      await Promise.resolve();
    });

    const dialog = container.querySelector<HTMLElement>('[role="dialog"]')!;
    const edit = dialog.querySelector<HTMLButtonElement>(
      'button[aria-label="编辑标题"]'
    )!;
    act(() => {
      edit.click();
    });
    const input = dialog.querySelector<HTMLInputElement>(
      'input[aria-label="编辑对话标题"]'
    )!;
    input.focus();
    keydown(input, "Escape");
    await flushFocusRestore();

    expect(useUIStore.getState().overlay).toBe("history");
    expect(
      dialog.querySelector('input[aria-label="编辑对话标题"]')
    ).toBeNull();
    const restored = dialog.querySelector<HTMLButtonElement>(
      'button[aria-label="编辑标题"]'
    )!;
    expect(document.activeElement).toBe(restored);
  });
});
