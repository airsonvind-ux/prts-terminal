// @vitest-environment jsdom
// WorkspaceTree · 工作区树 + 按名搜索测试
//
// 覆盖（2026-09 空间页拆分）：
//   - 根一层按需加载；目录在前由后端定序，前端照序渲染
//   - 点击目录 → list_workspace_dir(path=目录绝对路径) → 子项展开；空目录显示「（空目录）」
//   - 点击文件 → open_path（FileLink 契约：点击必有可见反馈）
//   - 搜索：防抖后 invoke；点击命中 → 展开祖先链（逐级加载）+ 选中该行
//   - 未设置工作区 → 不发加载请求，显示提示

import { describe, expect, it, beforeEach, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { WorkspaceTree } from "./WorkspaceTree";
import { useToastStore } from "../../stores/useToastStore";
import type { WorkspaceEntry, WorkspaceSearchHit } from "../../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

const WS = "C:\\ws";

beforeEach(() => {
  __invokeMock.mockClear();
  useToastStore.setState({ toasts: [] });
});

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
});

async function flush(ms = 0): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, ms));
  });
}

function renderTree(workspaceRoot: string): void {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(React.createElement(WorkspaceTree, { root: workspaceRoot }));
  });
}

function rowByText(text: string): HTMLElement {
  const rows = Array.from(
    container.querySelectorAll<HTMLElement>("[data-ws-path]")
  );
  const hit = rows.find((r) => (r.textContent ?? "").includes(text));
  if (!hit) throw new Error(`未找到行：${text}`);
  return hit;
}

function entry(name: string, isDir: boolean, parent = WS): WorkspaceEntry {
  return { name, isDir, path: `${parent}\\${name}` };
}

/** 受控 input 设值（React 需原生 setter + input 事件） */
function setInput(el: HTMLInputElement, value: string): void {
  const setter = Object.getOwnPropertyDescriptor(
    window.HTMLInputElement.prototype,
    "value"
  )!.set!;
  act(() => {
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

/** 输入框按键（回车进入搜索 / Esc 退出） */
function pressKey(el: HTMLInputElement, key: string): void {
  act(() => {
    el.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true }));
  });
}

describe("WorkspaceTree · 树", () => {
  it("根一层加载：渲染目录与文件，目录带展开标记", async () => {
    __invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "list_workspace_dir") {
        expect((args as { path: string }).path).toBe(WS);
        return Promise.resolve([entry("docs", true), entry("a.md", false)]);
      }
      return Promise.resolve(undefined);
    });
    renderTree(WS);
    await flush();

    expect(container.textContent).toContain("docs");
    expect(container.textContent).toContain("a.md");
    const dirRow = rowByText("docs");
    expect(dirRow.textContent).toContain("▸");
    expect(
      __invokeMock.mock.calls.filter((c) => c[0] === "list_workspace_dir")
    ).toHaveLength(1);
  });

  it("点击目录 → 按其绝对路径加载子项；空目录显示「（空目录）」", async () => {
    __invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "list_workspace_dir") {
        const path = (args as { path: string }).path;
        return Promise.resolve(path === WS ? [entry("docs", true)] : []);
      }
      return Promise.resolve(undefined);
    });
    renderTree(WS);
    await flush();

    act(() => {
      rowByText("docs")
        .querySelector("button")!
        .dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();

    const paths = __invokeMock.mock.calls
      .filter((c) => c[0] === "list_workspace_dir")
      .map((c) => (c[1] as { path: string }).path);
    expect(paths).toEqual([WS, `${WS}\\docs`]);
    expect(container.textContent).toContain("（空目录）");
    expect(rowByText("docs").textContent).toContain("▾");
  });

  it("点击文件 → open_path 打开（可见反馈）", async () => {
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_workspace_dir") {
        return Promise.resolve([entry("a.md", false)]);
      }
      return Promise.resolve(undefined);
    });
    renderTree(WS);
    await flush();

    const fileRow = rowByText("a.md");
    act(() => {
      fileRow.querySelector("span")!.dispatchEvent(
        new MouseEvent("click", { bubbles: true })
      );
    });
    await flush();

    const calls = __invokeMock.mock.calls.filter((c) => c[0] === "open_path");
    expect(calls).toHaveLength(1);
    expect((calls[0][1] as { path: string }).path).toBe(`${WS}\\a.md`);
    expect(
      useToastStore
        .getState()
        .toasts.some((t) => t.message.includes("已打开文件"))
    ).toBe(true);
  });

  it("未设置工作区：不发加载请求，显示提示", async () => {
    renderTree("");
    await flush();
    expect(
      __invokeMock.mock.calls.filter((c) => c[0] === "list_workspace_dir")
    ).toHaveLength(0);
    expect(container.textContent).toContain("未设置工作空间");
  });
});

describe("WorkspaceTree · 搜索", () => {
  const HIT: WorkspaceSearchHit = {
    name: "崖.md",
    path: `${WS}\\a\\b\\崖.md`,
    relativePath: "a/b/崖.md",
    parentSubdir: "a/b",
    ancestorDirs: [`${WS}\\a`, `${WS}\\a\\b`],
    isDir: false,
  };

  function searchRouting(query: string) {
    return (cmd: string, args?: unknown) => {
      if (cmd === "search_workspace_files") {
        expect((args as { query: string }).query).toBe(query);
        return Promise.resolve([HIT]);
      }
      if (cmd === "list_workspace_dir") {
        const path = (args as { path: string }).path;
        if (path === WS) return Promise.resolve([entry("a", true)]);
        if (path === `${WS}\\a`) return Promise.resolve([entry("b", true, `${WS}\\a`)]);
        if (path === `${WS}\\a\\b`) {
          return Promise.resolve([entry("崖.md", false, `${WS}\\a\\b`)]);
        }
        return Promise.resolve([]);
      }
      return Promise.resolve(undefined);
    };
  }

  it("输入文字不搜索、树不消失；回车后才进结果态", async () => {
    __invokeMock.mockImplementation(searchRouting("崖"));
    renderTree(WS);
    await flush();

    const input = container.querySelector<HTMLInputElement>("input")!;
    setInput(input, "崖");
    await flush(260);

    // 只打字：不发搜索请求，树原样在位
    expect(
      __invokeMock.mock.calls.filter((c) => c[0] === "search_workspace_files")
    ).toHaveLength(0);
    expect(container.querySelectorAll("[data-ws-path]").length).toBeGreaterThan(0);
    expect(container.textContent).not.toContain("a/b");

    // 回车：进结果态（树替掉，命中可见）
    pressKey(input, "Enter");
    await flush(260);
    const calls = __invokeMock.mock.calls.filter(
      (c) => c[0] === "search_workspace_files"
    );
    expect(calls).toHaveLength(1);
    expect(container.textContent).toContain("崖.md");
    expect(container.textContent).toContain("a/b");
    expect(container.querySelectorAll("[data-ws-path]").length).toBe(0);
  });

  it("Esc 退出搜索：清空输入、树回来", async () => {
    __invokeMock.mockImplementation(searchRouting("崖"));
    renderTree(WS);
    await flush();

    const input = container.querySelector<HTMLInputElement>("input")!;
    setInput(input, "崖");
    pressKey(input, "Enter");
    await flush(260);
    expect(container.querySelectorAll("[data-ws-path]").length).toBe(0);

    pressKey(input, "Escape");
    await flush();
    expect(input.value).toBe("");
    expect(container.querySelectorAll("[data-ws-path]").length).toBeGreaterThan(0);
  });

  it("点击命中 → 退出搜索回到树、展开祖先链（逐级加载）并选中该行", async () => {
    __invokeMock.mockImplementation(searchRouting("崖"));
    renderTree(WS);
    await flush();

    const input = container.querySelector<HTMLInputElement>("input")!;
    setInput(input, "崖");
    pressKey(input, "Enter");
    await flush(260);

    const hitBtn = Array.from(
      container.querySelectorAll<HTMLButtonElement>("button")
    ).find((b) => (b.textContent ?? "").includes("崖.md"))!;
    act(() => {
      hitBtn.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();

    // 三个祖先/自身目录逐级拉起（b 是命中文件的父目录 → 也在链上）
    const paths = __invokeMock.mock.calls
      .filter((c) => c[0] === "list_workspace_dir")
      .map((c) => (c[1] as { path: string }).path);
    expect(paths).toContain(`${WS}\\a`);
    expect(paths).toContain(`${WS}\\a\\b`);
    // 搜索已清空、回到树：命中文件所在行在树里（data-ws-path 即其绝对路径）
    expect(input.value).toBe("");
    expect(rowByText("崖.md").getAttribute("data-ws-path")).toBe(HIT.path);
  });
});
