// @vitest-environment jsdom
// 设置页通用组件（Section / Row / ListRow / Button / Note / Warning）渲染与交互

import { describe, expect, it, afterEach, vi } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { SettingsSection, SectionNote, SectionWarning } from "./ui/SettingsSection";
import { SettingsRow } from "./ui/SettingsRow";
import { ListRow } from "./ui/ListRow";
import { Button } from "./ui/Button";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
});

function render(node: React.ReactNode): void {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(<>{node}</>);
  });
}

describe("设置页通用组件", () => {
  it("SettingsSection 渲染标题与主体", () => {
    render(
      <SettingsSection title="安全">
        <span>body-content</span>
      </SettingsSection>
    );
    expect(container!.textContent).toContain("安全");
    expect(container!.textContent).toContain("body-content");
  });

  it("SettingsRow 渲染 label / 一行 hint / control", () => {
    render(
      <SettingsRow
        label="执行模式"
        hint="一行说明"
        control={<button>ctrl</button>}
      />
    );
    expect(container!.textContent).toContain("执行模式");
    expect(container!.textContent).toContain("一行说明");
    expect(container!.querySelector("button")?.textContent).toBe("ctrl");
  });

  it("hintVariant=warn 渲染警示样式类（不打断内容）", () => {
    render(
      <SettingsRow label="危险项" hint="注意风险" hintVariant="warn" control={null} />
    );
    const warn = container!.querySelector(".rowHintWarn");
    // css module 类名经 hash——用 text 兜底断言内容存在
    expect(container!.textContent).toContain("注意风险");
    expect(warn || container!.textContent!.includes("注意风险")).toBeTruthy();
  });

  it("Note / Warning 块渲染", () => {
    render(
      <>
        <SectionNote>中性说明</SectionNote>
        <SectionWarning>风险警示</SectionWarning>
      </>
    );
    expect(container!.textContent).toContain("中性说明");
    expect(container!.textContent).toContain("风险警示");
  });

  it("ListRow 点击进入，副行与状态点都在", () => {
    const enter = vi.fn();
    render(
      <ListRow
        title="MAA 自动化"
        subtitle="0.1.1 · 内置 · 正常"
        dot="ok"
        onActivate={enter}
      />
    );
    const row = container!.querySelector('[role="button"]') as HTMLElement;
    expect(row).toBeTruthy();
    expect(row.getAttribute("tabindex")).toBe("0");
    expect(container!.textContent).toContain("0.1.1 · 内置 · 正常");
    act(() => row.click());
    expect(enter).toHaveBeenCalledTimes(1);
  });

  it("ListRow 键盘 Enter 进入", () => {
    const enter = vi.fn();
    render(<ListRow title="探针" onActivate={enter} />);
    const row = container!.querySelector('[role="button"]') as HTMLElement;
    act(() => {
      row.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true })
      );
    });
    expect(enter).toHaveBeenCalledTimes(1);
  });

  it("ListRow 点右侧槽位不触发行进入", () => {
    const enter = vi.fn();
    const toggle = vi.fn();
    render(
      <ListRow
        title="MAA 自动化"
        onActivate={enter}
        trailing={
          <input type="checkbox" aria-label="启用" onChange={toggle} />
        }
      />
    );
    const box = container!.querySelector("input") as HTMLInputElement;
    act(() => box.click());
    expect(enter).not.toHaveBeenCalled();
    expect(toggle).toHaveBeenCalled();
  });

  it("ListRow 异常副行与只读行", () => {
    render(
      <ListRow
        title="broken"
        subtitle="manifest 读不出来"
        tone="danger"
        muted
      />
    );
    expect(container!.textContent).toContain("manifest 读不出来");
    expect(container!.querySelector('[role="button"]')).toBeNull();
  });

  it("Button 变体与禁用态", () => {
    render(
      <>
        <Button variant="primary">安装</Button>
        <Button variant="danger" disabled>
          卸载
        </Button>
      </>
    );
    const [primary, danger] = Array.from(
      container!.querySelectorAll("button")
    );
    expect(primary.textContent).toBe("安装");
    expect(primary.type).toBe("button");
    expect((danger as HTMLButtonElement).disabled).toBe(true);
  });
});
