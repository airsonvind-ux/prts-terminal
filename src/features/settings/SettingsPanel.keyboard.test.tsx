// @vitest-environment jsdom
// 设置页签的 roving tabindex 与方向键契约。

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { SettingsPanel } from "./SettingsPanel";
import { useUIStore } from "../../stores/useUIStore";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

async function flush(): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
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

beforeEach(async () => {
  useUIStore.setState({ overlay: null, settingsInitialTab: "app" });
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(React.createElement(SettingsPanel));
  });
  await flush();
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("SettingsPanel · 页签键盘契约", () => {
  it("tablist 只有当前页签可 Tab，内容区是关联的 tabpanel", () => {
    const tablist = container.querySelector<HTMLElement>('[role="tablist"]')!;
    expect(tablist.getAttribute("aria-orientation")).toBe("vertical");

    const tabs = Array.from(
      tablist.querySelectorAll<HTMLButtonElement>('[role="tab"]')
    );
    expect(tabs.length).toBeGreaterThan(1);
    expect(tabs.filter((t) => t.tabIndex === 0)).toHaveLength(1);
    expect(tabs[0].getAttribute("aria-selected")).toBe("true");

    const panel = container.querySelector<HTMLElement>('[role="tabpanel"]')!;
    expect(panel.id).toBe("settings-panel");
    expect(panel.getAttribute("aria-labelledby")).toBe("settings-tab-app");
  });

  it("ArrowDown / ArrowUp 切换并移动焦点，Home / End 到首尾", () => {
    const tablist = container.querySelector<HTMLElement>('[role="tablist"]')!;
    const tabs = Array.from(
      tablist.querySelectorAll<HTMLButtonElement>('[role="tab"]')
    );
    const app = tabs[0];
    app.focus();

    keydown(app, "ArrowDown");
    const model = tabs[1];
    expect(model.getAttribute("aria-selected")).toBe("true");
    expect(model.tabIndex).toBe(0);
    expect(app.tabIndex).toBe(-1);
    expect(document.activeElement).toBe(model);

    keydown(model, "ArrowUp");
    expect(app.getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(app);

    keydown(app, "End");
    const about = tabs[tabs.length - 1];
    expect(about.getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(about);

    keydown(about, "Home");
    expect(app.getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(app);
  });
});
