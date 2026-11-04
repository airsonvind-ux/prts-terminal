// @vitest-environment jsdom
// 过程卡（消息流重构第 2 步）· 单卡顺序行的形态测试
//
// 覆盖规格 §四：收起态摘要只列非零项、失败必须露出来、展开后按到达顺序混排、
// 等待审批时该卡强制展开、情绪卡独立成块不进过程卡。

import { describe, expect, it, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { MessageRow } from "./MessageRow";
import { buildArchivedItems, buildLiveItems } from "./messageItems";
import { stampEvent } from "./eventTiming";
import type { SessionEvent } from "../../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

const S = "s1";
const mounted: Array<{ container: HTMLDivElement; root: ReturnType<typeof createRoot> }> = [];

afterEach(() => {
  for (const m of mounted) {
    try {
      act(() => m.root.unmount());
    } catch {
      // 卸载失败不阻塞清理
    }
    m.container.remove();
  }
  mounted.length = 0;
});

function render(events: SessionEvent[], interactive = false) {
  const items = interactive
    ? buildLiveItems(events, undefined, false)
    : buildArchivedItems([events], false, undefined);
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(
      React.createElement(
        "div",
        null,
        items.map((item) => React.createElement(MessageRow, { key: item.id, item }))
      )
    );
  });
  mounted.push({ container, root });
  return {
    container,
    text: () => container.textContent ?? "",
    headers: () =>
      Array.from(container.querySelectorAll('[data-event-kind="card"]')),
    clickHeader: (index = 0) => {
      const header = container.querySelectorAll<HTMLButtonElement>(
        '[data-event-kind="card"]'
      )[index];
      act(() => header.click());
    },
  };
}

function reasoning(text: string): SessionEvent {
  return { type: "assistant/chunk", sessionId: S, kind: "reasoning", content: text };
}

function call(callId: string, toolName: string, action?: string): SessionEvent {
  return { type: "tool/call", sessionId: S, callId, toolName, params: "{}", ...(action ? { action } : {}) };
}

function result(callId: string, toolName: string, ok = true, summary = "完成"): SessionEvent {
  return { type: "tool/result", sessionId: S, callId, toolName, ok, summary };
}

describe("过程卡 · 单卡顺序行", () => {
  it("收起态只有一行标题：思考与工具 + 完成 + 用量", () => {
    const view = render([reasoning("想一下"), call("c1", "query_enemy"), result("c1", "query_enemy")]);
    expect(view.headers()).toHaveLength(1);
    const text = view.text();
    expect(text).toContain("思考与工具");
    expect(text).toContain("✓ 完成");
    expect(text).toContain("1 调用");
    // 收起态不写思考段数（规格 §四：标题只写「思考」）
    expect(text).not.toContain("思考 1 段");
    // 展开前看不到行内容
    expect(text).not.toContain("PRTS查询数据库完成");
  });

  it("失败项数在收起态就标出来", () => {
    const view = render([
      reasoning("先查"),
      call("c1", "query_enemy"),
      result("c1", "query_enemy", false, "数据库不可用"),
    ]);
    expect(view.text()).toContain("1 项失败");
    expect(view.text()).toContain("✕");
    expect(view.text()).not.toContain("0 项失败");
    expect(view.text()).not.toContain("✓ 完成");
  });

  it("展开后按到达顺序混排：思考块在工具行之前，思考块默认收起", () => {
    const view = render([reasoning("第一段思考"), call("c1", "query_enemy"), result("c1", "query_enemy")]);
    view.clickHeader();
    // 卡展开后思考仍是第二层收起，先什么都不显示
    expect(view.text()).toContain("思考");
    expect(view.text()).not.toContain("第一段思考");
    act(() => {
      view.container
        .querySelector<HTMLButtonElement>('[data-event-kind="reasoning"]')!
        .click();
    });
    const text = view.text();
    expect(text.indexOf("第一段思考")).toBeGreaterThan(-1);
    expect(text.indexOf("第一段思考")).toBeLessThan(text.indexOf("PRTS查询数据库完成"));
  });

  it("连续两次调用合成一张卡，用量写调用 2 次", () => {
    const view = render([
      call("c1", "query_enemy"),
      result("c1", "query_enemy"),
      call("c2", "query_stage"),
      result("c2", "query_stage"),
    ]);
    expect(view.headers()).toHaveLength(1);
    expect(view.text()).toContain("2 调用");
  });

  it("有到达时刻时用量带耗时，没有时不猜", () => {
    const events = [
      call("c1", "query_enemy"),
      result("c1", "query_enemy"),
      call("c2", "query_stage"),
      result("c2", "query_stage"),
    ];
    // 模拟处理管道打的时间戳：第一次调用 0.0s，最后一次结果 3.4s
    stampEvent(events[0], 1_000);
    stampEvent(events[1], 1_400);
    stampEvent(events[2], 2_000);
    stampEvent(events[3], 4_400);
    const view = render(events);
    expect(view.text()).toContain("2 调用 · 3.4s");
  });

  it("等待审批时该卡强制展开，且不渲染确认按钮（按钮在 Action Dock）", () => {
    const view = render(
      [
        reasoning("要跑命令了"),
        call("c1", "run_command"),
        {
          type: "tool/approval-request",
          sessionId: S,
          callId: "c1",
          command: "rm -rf ./tmp",
          risk: "high",
          mode: "ask",
          ruleId: "delete-always-ask",
          privilege: "restricted",
          justification: null,
        } as SessionEvent,
      ],
      true
    );
    expect(view.text()).toContain("PRTS 正在等待你确认");
    expect(view.text()).toContain("请在输入框上方处理");
    // 卡内只有「卡头 + 思考」两个折叠头，没有确认/拒绝按钮（那些在 Action Dock）
    expect(view.container.querySelectorAll("button")).toHaveLength(2);
    expect(view.text()).not.toContain("拒绝");
  });

  it("情绪卡独立成块，不混进过程卡", () => {
    const view = render([
      reasoning("想一下"),
      call("c1", "query_enemy"),
      result("c1", "query_enemy"),
      { type: "mood/event", sessionId: S, mood: "高兴", intensity: 2 },
    ]);
    // 一张过程卡 + 一张情绪卡（情绪卡是独立块，不进卡内）
    expect(view.headers()).toHaveLength(1);
    const moodCard = view.container.querySelectorAll("[data-mood-card]");
    expect(moodCard).toHaveLength(1);
    // 没有心声时回落到旁白
    expect(moodCard[0].textContent).toContain("PRTS 高兴坏了");
    // 情绪内容不出现在过程卡摘要里
    expect(view.text()).toContain("思考与工具");
    expect(view.text()).toContain("1 调用");
  });
});
