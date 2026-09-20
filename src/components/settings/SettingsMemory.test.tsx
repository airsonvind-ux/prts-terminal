// @vitest-environment jsdom
// 设置页「记忆」子页（初步）渲染/交互冒烟
//
// 覆盖：overview 渲染（成长列表 + 当前注入）／单条读取与保存（memory_update）／
// 新建（memory_create）／整理（memory_tidy）／编辑长期记忆（memory_set_longterm）／
// 清除二次确认（memory_experience_clear）／对话处理中写按钮禁用。

import { describe, expect, it, beforeEach, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { SettingsMemory } from "./SettingsMemory";
import { useConversationStore } from "../../stores/useConversationStore";
import { useToastStore } from "../../stores/useToastStore";
import type { MemoryOverview } from "../../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

const OVERVIEW: MemoryOverview = {
  growth: [
    { name: "关于博士", brief: "画像与约定", tags: [], updatedAt: 1, isProfile: true },
    { name: "伤害计算", brief: "暴击与减伤", tags: ["数值"], updatedAt: 2, isProfile: false },
  ],
  recent: [{ date: "2026-09-16", title: "伤害公式", summary: "确认相乘" }],
  previous: "老事概要",
  experienceCount: 3,
};

function route(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
  if (cmd === "memory_overview") return Promise.resolve(OVERVIEW);
  if (cmd === "memory_get") {
    if (args?.kind === "growth") {
      return Promise.resolve({
        kind: "growth",
        page: {
          name: args.name,
          brief: "b",
          body: "正文",
          tags: ["t"],
          source: "手动",
          createdAt: 1,
          updatedAt: 1,
          isProfile: args.name === "关于博士",
        },
        entry: null,
      });
    }
    return Promise.resolve({
      kind: "experience",
      page: null,
      entry: {
        id: args?.id,
        date: "2026-09-16",
        title: "t",
        summary: "状态句",
        tags: ["数值"],
        absorbed: false,
        conversationId: "c1",
        turnFrom: 0,
        turnTo: 1,
      },
    });
  }
  if (cmd === "memory_experience_list") {
    return Promise.resolve([
      {
        id: "e1",
        date: "2026-09-16",
        title: "伤害",
        summary: "s",
        tags: [],
        absorbed: false,
        conversationId: "c1",
        turnFrom: 0,
        turnTo: 2,
      },
    ]);
  }
  if (cmd === "memory_set_longterm") {
    return Promise.resolve({
      version: 2,
      chars: String(args?.text ?? "").length,
    });
  }
  if (cmd === "memory_tidy") {
    return Promise.resolve({
      status: "created",
      message: "已整理：新建 1 条、重写 0 条",
      created: 1,
      rewritten: 0,
      compressed: false,
    });
  }
  return Promise.resolve(undefined);
}

function render(busy = false): void {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(React.createElement(SettingsMemory, { busy }));
  });
}

async function flush(): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
}

function clickText(text: string): void {
  const el = [...container.querySelectorAll("button")].find((b) =>
    (b.textContent ?? "").includes(text)
  );
  if (!el) throw new Error(`未找到按钮：${text}`);
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
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

/** 受控 textarea 设值（React 需原生 setter + input 事件） */
function setTextarea(el: HTMLTextAreaElement, value: string): void {
  const setter = Object.getOwnPropertyDescriptor(
    window.HTMLTextAreaElement.prototype,
    "value"
  )!.set!;
  act(() => {
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function invokeCalls(cmd: string): unknown[][] {
  return __invokeMock.mock.calls.filter((c) => c[0] === cmd);
}

beforeEach(() => {
  __invokeMock.mockClear();
  __invokeMock.mockImplementation(route);
  useConversationStore.setState({ conversations: [], activeId: "c1" });
  useToastStore.setState({ toasts: [] });
});

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
});

describe("SettingsMemory", () => {
  it("渲染成长列表 + 当前注入（近期/先前）+ 计数", async () => {
    render();
    await flush();
    expect(container.textContent).toContain("关于博士");
    expect(container.textContent).toContain("伤害计算");
    expect(container.textContent).toContain("伤害公式：确认相乘");
    expect(container.textContent).toContain("老事概要");
    expect(container.textContent).toContain("查看记忆（3）");
  });

  it("点开单条 → 载入表单 → 保存走 memory_update", async () => {
    render();
    await flush();
    clickText("伤害计算");
    await flush();
    const nameInput = container.querySelector<HTMLInputElement>("input[disabled]");
    expect(nameInput?.value).toBe("伤害计算");
    clickText("保存");
    await flush();
    const calls = invokeCalls("memory_update");
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toMatchObject({ name: "伤害计算" });
  });

  it("新建页 → 填名保存走 memory_create", async () => {
    render();
    await flush();
    clickText("新建页");
    await flush();
    const nameInput = container.querySelector<HTMLInputElement>("input:not([disabled])");
    expect(nameInput).toBeTruthy();
    setInput(nameInput!, "新页");
    clickText("保存");
    await flush();
    const calls = invokeCalls("memory_create");
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toMatchObject({ name: "新页" });
  });

  it("整理当下记忆 → memory_tidy（conversationId）+ 反馈文案", async () => {
    render();
    await flush();
    clickText("整理当下记忆");
    await flush();
    const calls = invokeCalls("memory_tidy");
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toMatchObject({ conversationId: "c1" });
    expect(container.textContent).toContain("已整理：新建 1 条、重写 0 条");
  });

  it("编辑长期记忆：预填现正文 → 保存走 memory_set_longterm", async () => {
    render();
    await flush();
    clickText("编辑长期记忆");
    const ta = container.querySelector<HTMLTextAreaElement>("[data-pre-edit]");
    expect(ta).toBeTruthy();
    expect(ta!.value).toBe("老事概要");
    setTextarea(ta!, "## 先前\n新的长期记忆");
    clickText("保存");
    await flush();
    const calls = invokeCalls("memory_set_longterm");
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toMatchObject({ text: "## 先前\n新的长期记忆" });
    // 保存后回到只读态
    expect(container.querySelector("[data-pre-edit]")).toBeNull();
  });

  it("清除记忆 → 二次确认 → memory_experience_clear", async () => {
    render();
    await flush();
    clickText("清除记忆");
    await flush();
    expect(container.textContent).toContain("确认清除");
    clickText("确认清除");
    await flush();
    expect(invokeCalls("memory_experience_clear")).toHaveLength(1);
  });

  it("查看记忆 → 打开记忆库浮层（只读列表）", async () => {
    render();
    await flush();
    clickText("查看记忆");
    await flush();
    expect(container.textContent).toContain("记忆库 · 全部经历（1）");
  });

  it("busy：写按钮禁用", async () => {
    render(true);
    await flush();
    const newBtn = [...container.querySelectorAll("button")].find(
      (b) => (b.textContent ?? "") === "新建页"
    ) as HTMLButtonElement;
    expect(newBtn.disabled).toBe(true);
    expect(container.textContent).toContain("对话处理中");
  });
});
