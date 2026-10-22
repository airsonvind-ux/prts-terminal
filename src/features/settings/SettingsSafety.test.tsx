// @vitest-environment jsdom
// SettingsSafety · 安全页（两档命令执行模式：选项块 = 标题 + 自带说明，静态并存）

import { describe, expect, it, afterEach, vi } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { SettingsSafety } from "./SettingsSafety";
import type { ExecutionMode } from "../../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
});

function render(mode: ExecutionMode, onModeChange = () => {}): void {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(
      React.createElement(SettingsSafety, { mode, onModeChange })
    );
  });
}

function radios(): HTMLInputElement[] {
  return Array.from(
    container!.querySelectorAll<HTMLInputElement>('input[type="radio"][name="execMode"]')
  );
}

describe("SettingsSafety（草案 v2）", () => {
  it("两选项各自带说明、静态并存（与选中态无关）", () => {
    render("ask");
    const text = container!.textContent ?? "";
    expect(text).toContain("操作前询问");
    expect(text).toContain("自动化");
    // 两条说明同时存在（不随选中切换）
    expect(text).toContain("每个命令执行前弹确认卡，你确认后才执行");
    expect(text).toContain(
      "低风险命令自动执行，可能运行程序或连接网络；高风险与完整权限命令仍会询问"
    );
    expect(text).not.toContain("残余风险");
  });

  it("auto 选中态下两说明仍并存，不额外叠加提示块", () => {
    render("auto");
    const text = container!.textContent ?? "";
    expect(text).toContain("每个命令执行前弹确认卡，你确认后才执行");
    expect(text).toContain(
      "低风险命令自动执行，可能运行程序或连接网络；高风险与完整权限命令仍会询问"
    );
    expect(text).not.toContain("PRTS 不会逐条弹出确认卡");
    expect(radios()[1].checked).toBe(true);
  });

  it("点击单选 → onModeChange 回调对应档；受控回显不变", () => {
    const onModeChange = vi.fn();
    render("ask", onModeChange);
    act(() => {
      radios()[1].dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(onModeChange).toHaveBeenCalledWith("auto");
    // 外部未更新 mode → checked 仍 ask（受控）
    expect(radios()[0].checked).toBe(true);
  });
});
