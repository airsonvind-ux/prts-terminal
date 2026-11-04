// @vitest-environment jsdom
// 消息流状态矩阵（重构第 4 步）
//
// 规格 §六 的七行场景里，能由过程卡单独判定的六行在这里覆盖；
// 压缩边界一行是既有行为（retainedFrom 之前的轮次照旧），由 MessageList 的
// compaction 用例守着，不重复造。

import { describe, expect, it, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { EventBadge } from "./EventBadge";
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

function render(events: SessionEvent[], interactive: boolean) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(
      React.createElement(EventBadge, {
        events,
        interactive,
        showApprovalControls: false,
        variant: "flow",
      })
    );
  });
  mounted.push({ container, root });
  return {
    container,
    text: () => container.textContent ?? "",
    cardCount: () =>
      container.querySelectorAll('[data-event-kind="card"]').length,
  };
}

function reasoning(text: string): SessionEvent {
  return { type: "assistant/chunk", sessionId: S, kind: "reasoning", content: text };
}

function call(callId: string, toolName: string): SessionEvent {
  return { type: "tool/call", sessionId: S, callId, toolName, params: "{}" };
}

function result(callId: string, toolName: string, ok = true, summary = "完成"): SessionEvent {
  return { type: "tool/result", sessionId: S, callId, toolName, ok, summary };
}

describe("消息流状态矩阵", () => {
  it("流式中：末行未配对 → 摘要行标进行中", () => {
    const view = render([reasoning("正在想"), call("c1", "query_enemy")], true);
    expect(view.text()).toContain("· 进行中");
    expect(view.text()).toContain("调用 1 次");
  });

  it("完成：配对齐全 → 摘要行不再标进行中", () => {
    const view = render(
      [reasoning("想"), call("c1", "query_enemy"), result("c1", "query_enemy")],
      false
    );
    expect(view.text()).not.toContain("· 进行中");
    expect(view.text()).toContain("思考 1 段 · 调用 1 次");
  });

  it("含失败：摘要行标出失败项数并带 ⚠", () => {
    const view = render(
      [
        reasoning("先查"),
        call("c1", "query_enemy"),
        result("c1", "query_enemy", false, "数据库不可用"),
        call("c2", "query_stage"),
        result("c2", "query_stage"),
      ],
      false
    );
    expect(view.text()).toContain("调用 2 次 · 1 项失败");
    expect(
      view.container.querySelector('[aria-label="有失败项"]')
    ).not.toBeNull();
  });

  it("等待审批：进行中轮次强制展开；归档轮没有 pending 语义，不展开", () => {
    const pending: SessionEvent[] = [
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
    ];
    const live = render(pending, true);
    expect(live.text()).toContain("PRTS 正在等待你确认");

    const archived = render(pending, false);
    expect(archived.text()).not.toContain("PRTS 正在等待你确认");
    // 归档轮只留折叠头；未配对项按「未完成」呈现，不冒充活跃
    expect(archived.text()).not.toContain("· 进行中");
  });

  it("中止：过程卡本身不变，中止条是独立消息行（由 MessageRow 承担）", () => {
    const view = render(
      [
        reasoning("想"),
        call("c1", "query_enemy"),
        result("c1", "query_enemy"),
        { type: "turn/aborted", sessionId: S } as SessionEvent,
      ],
      false
    );
    expect(view.text()).toContain("思考 1 段 · 调用 1 次");
    expect(view.cardCount()).toBe(1);
  });

  it("归档回放：结构与进行中一致，默认收起可展开", () => {
    const events = [
      reasoning("想"),
      call("c1", "query_enemy"),
      result("c1", "query_enemy"),
    ];
    const view = render(events, false);
    expect(view.cardCount()).toBe(1);
    expect(view.text()).toContain("思考 1 段 · 调用 1 次");
    expect(view.text()).not.toContain("PRTS查询数据库完成");
    const header = view.container.querySelector<HTMLButtonElement>(
      '[data-event-kind="card"]'
    )!;
    act(() => header.click());
    expect(view.text()).toContain("PRTS查询数据库完成");
  });
});
