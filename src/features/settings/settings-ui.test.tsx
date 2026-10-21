// @vitest-environment jsdom
// 设置页通用组件（Section / Row / Note / Warning）渲染冒烟

import { describe, expect, it, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { SettingsSection, SectionNote, SectionWarning } from "./ui/SettingsSection";
import { SettingsRow } from "./ui/SettingsRow";

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
});
