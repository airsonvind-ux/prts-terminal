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
  it("收起态只有一行摘要，不再有思考区与工具区两个折叠头", () => {
    const view = render([reasoning("想一下"), call("c1", "query_enemy"), result("c1", "query_enemy")]);
    expect(view.headers()).toHaveLength(1);
    expect(view.text()).toContain("思考 1 段 · 调用 1 次");
    // 展开前看不到行内容
    expect(view.text()).not.toContain("PRTS查询数据库完成");
  });

  it("摘要只列非零项，失败项数必须露出来", () => {
    const view = render([
      reasoning("先查"),
      call("c1", "query_enemy"),
      result("c1", "query_enemy", false, "数据库不可用"),
    ]);
    expect(view.text()).toContain("思考 1 段 · 调用 1 次 · 1 项失败");
    expect(view.text()).not.toContain("0 项失败");
  });

  it("展开后按到达顺序混排：思考行在工具行之前", () => {
    const view = render([reasoning("第一段思考"), call("c1", "query_enemy"), result("c1", "query_enemy")]);
    view.clickHeader();
    const text = view.text();
    expect(text.indexOf("第一段思考")).toBeGreaterThan(-1);
    expect(text.indexOf("第一段思考")).toBeLessThan(text.indexOf("PRTS查询数据库完成"));
  });

  it("连续两次调用合成一张卡，摘要写调用 2 次", () => {
    const view = render([
      call("c1", "query_enemy"),
      result("c1", "query_enemy"),
      call("c2", "query_stage"),
      result("c2", "query_stage"),
    ]);
    expect(view.headers()).toHaveLength(1);
    expect(view.text()).toContain("调用 2 次");
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
    // 卡内只有一个折叠头，没有确认/拒绝按钮（那些在 Action Dock）
    expect(view.container.querySelectorAll("button")).toHaveLength(1);
  });

  it("情绪卡独立成块，不混进过程卡", () => {
    const view = render([
      reasoning("想一下"),
      call("c1", "query_enemy"),
      result("c1", "query_enemy"),
      { type: "mood/event", sessionId: S, mood: "高兴", intensity: 2 },
    ]);
    // 一张过程卡 + 一张情绪卡（情绪卡仍走既有情绪区）
    expect(view.headers()).toHaveLength(1);
    const moodToggle = view.container.querySelectorAll('[data-event-kind="mood"]');
    expect(moodToggle).toHaveLength(1);
    // 情绪内容不出现在过程卡摘要里
    expect(view.text()).toContain("思考 1 段 · 调用 1 次");
  });
});
