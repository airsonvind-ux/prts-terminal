// @vitest-environment jsdom
// 设置页「记忆」子页（2026-09 结构定稿）渲染/交互冒烟
//
// 覆盖：主列表（成长／经历两区、按钮集）／成长阅读→编辑→保存（memory_update）／
// 新建专题（memory_create）／删除（memory_delete）／「关于博士」显示复位／
// 「查看当下记忆」（近期只读＋先前，编辑只挂长期记忆 → memory_set_longterm）／
// 「查看记忆」（按日期分组、只标题、点开看内容、无来源标签、封存/收纳小标）／
// 清除二次确认（memory_experience_clear）／整理（memory_tidy）／busy 禁用／返回／
// 无改动保存提示（后端返回未改动）。

import { describe, expect, it, beforeEach, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { SettingsMemory } from "./SettingsMemory";
import { useConversationStore } from "../../stores/useConversationStore";
import { useToastStore } from "../../stores/useToastStore";
import type { MemoryOverview } from "../../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

// jsdom 未实现 scrollIntoView（视图框架打开时会调用）
if (typeof Element.prototype.scrollIntoView !== "function") {
  Element.prototype.scrollIntoView = () => {};
}

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

/** 写命令的“是否有实际改动”应答（测试可覆写） */
let updateChanged = true;
let longtermChanged = true;

const UPDATED_AT = Date.UTC(2026, 8, 20, 3, 0, 0); // 2026-09-20（本地 +8 同日）

const OVERVIEW: MemoryOverview = {
  growth: [
    { name: "关于博士", brief: "画像与约定", tags: [], updatedAt: 1, isProfile: true },
    { name: "伤害计算", brief: "暴击与减伤", tags: ["数值"], updatedAt: 2, isProfile: false },
  ],
  recent: [
    { date: "2026-09-16", title: "伤害公式", summary: "确认相乘" },
    { date: "2026-09-15", title: "轴推进", summary: "确认了 3 波" },
  ],
  previous: "老事概要",
  experienceCount: 3,
};

function route(cmd: string, args?: Record<string, unknown>): Promise<unknown> {
  if (cmd === "memory_overview") return Promise.resolve(OVERVIEW);
  if (cmd === "memory_get") {
    return Promise.resolve({
      kind: "growth",
      page: {
        name: args?.name,
        brief: "暴击与减伤",
        body: "相乘关系",
        tags: ["数值"],
        source: "工具",
        createdAt: 1,
        updatedAt: UPDATED_AT,
        isProfile: args?.name === "关于博士",
      },
      entry: null,
    });
  }
  if (cmd === "memory_experience_list") {
    return Promise.resolve([
      {
        id: "e1",
        date: "2026-09-16",
        title: "伤害",
        summary: "s1",
        tags: ["数值"],
        absorbed: false,
        sealed: false,
        conversationId: "c1",
        turnFrom: 0,
        turnTo: 2,
      },
      {
        id: "e2",
        date: "2026-09-16",
        title: "封存的",
        summary: "s2",
        tags: [],
        absorbed: false,
        sealed: true,
        conversationId: "c1",
        turnFrom: 0,
        turnTo: 2,
      },
      {
        id: "e3",
        date: "2026-09-10",
        title: "收纳的",
        summary: "s3",
        tags: [],
        absorbed: true,
        sealed: false,
        conversationId: "c1",
        turnFrom: 0,
        turnTo: 2,
      },
    ]);
  }
  if (cmd === "memory_update") return Promise.resolve(updateChanged);
  if (cmd === "memory_set_longterm") {
    return Promise.resolve({
      version: 2,
      chars: String(args?.text ?? "").length,
      changed: longtermChanged,
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

function findButton(text: string): HTMLButtonElement | undefined {
  return [...container.querySelectorAll("button")].find((b) =>
    (b.textContent ?? "").includes(text)
  );
}

function clickText(text: string): void {
  const el = findButton(text);
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
  updateChanged = true;
  longtermChanged = true;
  useConversationStore.setState({ conversations: [], activeId: "c1" });
  useToastStore.setState({ toasts: [] });
});

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
});

describe("SettingsMemory（结构定稿）", () => {
  it("主列表：成长/经历两区与按钮集；旧的内联编辑与计数不在", async () => {
    render();
    await flush();
    expect(container.textContent).toContain("关于博士");
    expect(container.textContent).toContain("伤害计算");
    for (const label of [
      "新建专题",
      "查看当下记忆",
      "整理当下记忆",
      "查看记忆",
      "清除记忆",
    ]) {
      expect(findButton(label), `缺少按钮：${label}`).toBeTruthy();
    }
    expect(container.textContent).not.toContain("编辑长期记忆");
    expect(container.textContent).not.toContain("查看记忆（");
    expect(container.textContent).not.toContain("当前注入");
  });

  it("点成长条目 → 阅读视图（含更新日期与标签）；编辑 → 保存走 memory_update", async () => {
    render();
    await flush();
    clickText("伤害计算");
    await flush();
    expect(container.textContent).toContain("相乘关系");
    expect(container.textContent).toContain("2026-09-20");
    expect(container.textContent).toContain("数值");

    clickText("编辑");
    const nameInput = container.querySelector<HTMLInputElement>("input[disabled]");
    expect(nameInput?.value).toBe("伤害计算");
    const briefInput = container.querySelector<HTMLInputElement>(
      "input:not([disabled])"
    )!;
    setInput(briefInput, "改简述");
    clickText("保存");
    await flush();
    const calls = invokeCalls("memory_update");
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toMatchObject({ name: "伤害计算", brief: "改简述" });
    // 保存后回到阅读态
    expect(container.querySelector("textarea")).toBeNull();
    expect(findButton("编辑")).toBeTruthy();
  });

  it("「关于博士」阅读视图显示「复位」而非「删除」", async () => {
    render();
    await flush();
    clickText("关于博士");
    await flush();
    expect(findButton("复位")).toBeTruthy();
    expect(findButton("删除")).toBeUndefined();
  });

  it("新建专题 → 空白表单 → 保存走 memory_create 并返回列表", async () => {
    render();
    await flush();
    clickText("新建专题");
    await flush();
    const nameInput = container.querySelector<HTMLInputElement>(
      "input:not([disabled])"
    );
    expect(nameInput).toBeTruthy();
    setInput(nameInput!, "新专题");
    clickText("保存");
    await flush();
    const calls = invokeCalls("memory_create");
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toMatchObject({ name: "新专题" });
    // 回到主列表
    expect(findButton("查看当下记忆")).toBeTruthy();
  });

  it("阅读视图删除 → memory_delete 并返回列表", async () => {
    render();
    await flush();
    clickText("伤害计算");
    await flush();
    clickText("删除");
    await flush();
    expect(invokeCalls("memory_delete")).toHaveLength(1);
    expect(findButton("查看当下记忆")).toBeTruthy();
  });

  it("查看当下记忆：近期＋先前；编辑只挂长期记忆 → memory_set_longterm", async () => {
    render();
    await flush();
    clickText("查看当下记忆");
    await flush();
    expect(container.textContent).toContain("伤害公式：确认相乘");
    expect(container.textContent).toContain("轴推进：确认了 3 波");
    expect(container.textContent).toContain("老事概要");
    // 「编辑」只出现一次（先前块）
    const editBtns = [...container.querySelectorAll("button")].filter(
      (b) => (b.textContent ?? "").trim() === "编辑"
    );
    expect(editBtns).toHaveLength(1);

    clickText("编辑");
    const ta = container.querySelector<HTMLTextAreaElement>("[data-pre-edit]");
    expect(ta?.value).toBe("老事概要");
    setTextarea(ta!, "新长期");
    clickText("保存");
    await flush();
    const calls = invokeCalls("memory_set_longterm");
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toMatchObject({ text: "新长期" });
  });

  it("查看记忆：按日期分组（新在前）、默认只标题、点开看内容；无来源与标签", async () => {
    render();
    await flush();
    clickText("查看记忆");
    await flush();
    const text = container.textContent ?? "";
    expect(text).toContain("2026-09-16");
    expect(text).toContain("2026-09-10");
    expect(text.indexOf("2026-09-16")).toBeLessThan(text.indexOf("2026-09-10"));
    expect(text).toContain("已封存");
    expect(text).toContain("已收纳");
    // 默认不展开内容
    expect(text).not.toContain("s1");
    // 无来源与标签信息
    expect(text).not.toContain("对话");
    expect(text).not.toContain("轮次");
    expect(text).not.toContain("标签");
    // 点开 → 内容；再点收起
    clickText("伤害");
    await flush();
    expect(container.textContent).toContain("s1");
    clickText("伤害");
    await flush();
    expect(container.textContent).not.toContain("s1");
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

  it("成长页无改动保存：后端未写入 → 提示「无改动」且不重取总览", async () => {
    updateChanged = false;
    render();
    await flush();
    clickText("伤害计算");
    await flush();
    clickText("编辑");
    clickText("保存");
    await flush();
    expect(invokeCalls("memory_update")).toHaveLength(1);
    expect(
      useToastStore.getState().toasts.some((t) => t.message.includes("无改动"))
    ).toBe(true);
    expect(invokeCalls("memory_overview")).toHaveLength(1);
    expect(container.querySelector("textarea")).toBeNull();
  });

  it("长期记忆无改动保存：后端未写入 → 提示「无改动」且不重取总览", async () => {
    longtermChanged = false;
    render();
    await flush();
    clickText("查看当下记忆");
    await flush();
    clickText("编辑");
    clickText("保存");
    await flush();
    expect(invokeCalls("memory_set_longterm")).toHaveLength(1);
    expect(
      useToastStore.getState().toasts.some((t) => t.message.includes("无改动"))
    ).toBe(true);
    expect(invokeCalls("memory_overview")).toHaveLength(1);
  });

  it("整理当下记忆 → memory_tidy + 反馈文案", async () => {
    render();
    await flush();
    clickText("整理当下记忆");
    await flush();
    expect(invokeCalls("memory_tidy")).toHaveLength(1);
    expect(container.textContent).toContain("已整理：新建 1 条、重写 0 条");
  });

  it("「← 返回」从二级视图回到主列表", async () => {
    render();
    await flush();
    clickText("伤害计算");
    await flush();
    clickText("返回");
    await flush();
    expect(findButton("查看当下记忆")).toBeTruthy();
    expect(container.textContent).not.toContain("相乘关系");
  });

  it("busy：写按钮禁用", async () => {
    render(true);
    await flush();
    const newBtn = findButton("新建专题") as HTMLButtonElement;
    expect(newBtn.disabled).toBe(true);
    expect(container.textContent).toContain("对话处理中");
  });
});
