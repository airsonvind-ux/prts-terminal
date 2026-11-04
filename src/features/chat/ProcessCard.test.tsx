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

function call(
  callId: string,
  toolName: string,
  action?: string,
  display?: { text?: string; link?: { kind: "file" | "url"; target: string; label: string }; full?: string }
): SessionEvent {
  return {
    type: "tool/call",
    sessionId: S,
    callId,
    toolName,
    params: "{}",
    ...(action ? { action } : {}),
    ...(display ? { display } : {}),
  };
}

function result(
  callId: string,
  toolName: string,
  ok = true,
  summary = "完成",
  provenance?: { trust: "external"; source: string; version?: string }
): SessionEvent {
  return {
    type: "tool/result",
    sessionId: S,
    callId,
    toolName,
    ok,
    summary,
    ...(provenance ? { provenance } : {}),
  };
}

function approvalRequest(callId: string, command = "Get-Process"): SessionEvent {
  return {
    type: "tool/approval-request",
    sessionId: S,
    callId,
    command,
    risk: "low",
    mode: "ask",
    ruleId: null,
    privilege: "restricted",
    justification: null,
  };
}

function approvalResult(
  callId: string,
  decision: "approved" | "rejected" | "blocked"
): SessionEvent {
  return {
    type: "tool/approval-result",
    sessionId: S,
    callId,
    decision,
    risk: "low",
    mode: "ask",
    ruleId: null,
    reason: "user",
    privilege: "restricted",
  };
}

describe("过程卡 · 单卡顺序行", () => {
  it("收起态只有一行标题：思考与工具 + 完成 + 用量", () => {
    const view = render([reasoning("想一下"), call("c1", "query_enemy"), result("c1", "query_enemy")]);
    expect(view.headers()).toHaveLength(1);
    const text = view.text();
    expect(text).toContain("思考与工具");
    expect(text).toContain("✓ 完成");
    expect(text).toContain("1 调用");
    // 数量信息齐全：思考段数 + 调用次数（规格 §四：收起态摘要只列非零项）
    expect(text).toContain("思考 1 段");
    expect(text).toContain("思考 1 段 · 1 调用");
    // 展开前看不到行内容
    expect(text).not.toContain("PRTS查询数据库完成");
  });

  it("失败项数在收起态就标出来，但不掩盖「完成了」", () => {
    const view = render([
      reasoning("先查"),
      call("c1", "query_enemy"),
      result("c1", "query_enemy", false, "数据库不可用"),
    ]);
    const text = view.text();
    // 失败是补充说明，不是结论：先写完成，再写含几项失败
    expect(text).toContain("完成（含 1 项失败）");
    expect(text).not.toContain("✕");
    expect(text).not.toContain("0 项失败");
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

  it("思考行与工具行同一套语域：生成中说「正在思考」，结束后说「思考了一阵」", () => {
    const live = render([reasoning("还在想")], true);
    live.clickHeader();
    expect(live.text()).toContain("PRTS正在思考...");

    const done = render([reasoning("想完了")]);
    done.clickHeader();
    expect(done.text()).toContain("PRTS思考了一阵");
    expect(done.text()).not.toContain("PRTS正在思考");
  });

  it("进行中的卡有失败时先写进行中，再把失败数放进括号", () => {
    const view = render(
      [call("c1", "query_enemy"), result("c1", "query_enemy", false, "数据库不可用")],
      true
    );
    expect(view.text()).toContain("进行中（含 1 项失败）");
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

// ── 中止留下的悬空项：归档回放必须看得见 ──────────────────────────

describe("过程卡 · 未完成态", () => {
  it("归档轮里没有结果的调用：标题写「工具调用未完成」", () => {
    const view = render([reasoning("先查一下"), call("c1", "run_command")]);
    expect(view.text()).toContain("工具调用未完成");
    expect(view.text()).not.toContain("✓ 完成");
    expect(view.text()).not.toContain("· 进行中");
  });

  it("多个悬空调用带项数", () => {
    const view = render([call("c1", "run_command"), call("c2", "query_enemy")]);
    expect(view.text()).toContain("工具调用未完成（2 项）");
  });

  it("末尾思考没写完：标题写「思考未完成」", () => {
    const view = render([
      call("c1", "query_enemy"),
      result("c1", "query_enemy"),
      reasoning("还没想完"),
    ]);
    expect(view.text()).toContain("思考未完成");
  });

  it("展开后悬空行仍按进行中样式显示，但圆点不呼吸", () => {
    const view = render([call("c1", "run_command")]);
    view.clickHeader();
    expect(view.text()).toContain("PRTS正在执行命令");
    const dot = Array.from(view.container.querySelectorAll("span")).find((s) =>
      s.className.includes("dotRunning")
    );
    expect(dot).toBeTruthy();
    expect(dot!.className).toContain("dotStill");
  });

  it("进行中的轮次不显示未完成（那时还没回来是常态）", () => {
    const view = render([call("c1", "run_command")], true);
    expect(view.text()).toContain("· 进行中");
    expect(view.text()).not.toContain("未完成");
  });
});

// ── 审批四态（从 EventBadge.test 迁移：按钮在 Action Dock，卡内只有行） ──

describe("过程卡 · 审批四态", () => {
  it("等待确认：强制展开 + 只读提示", () => {
    const view = render([call("c1", "run_command"), approvalRequest("c1")], true);
    expect(view.text()).toContain("PRTS 正在等待你确认执行命令");
    expect(view.text()).toContain("请在输入框上方处理");
    // 卡内只有卡头一个按钮；确认/拒绝按钮在 Action Dock
    expect(view.container.querySelectorAll("button")).toHaveLength(1);
  });

  it("已确认：行文案带「已确认」", () => {
    const view = render(
      [call("c1", "run_command"), approvalRequest("c1"), approvalResult("c1", "approved")],
      true
    );
    view.clickHeader();
    expect(view.text()).toContain("PRTS正在执行命令...（已确认）");
  });

  it("已拒绝：按失败行渲染", () => {
    const view = render(
      [call("c1", "run_command"), approvalRequest("c1"), approvalResult("c1", "rejected")],
      true
    );
    view.clickHeader();
    expect(view.text()).toContain("PRTS执行命令失败：已拒绝该命令执行");
  });

  it("被拦截（blocked）同样按已拒绝渲染", () => {
    const view = render(
      [call("c1", "run_command"), approvalRequest("c1"), approvalResult("c1", "blocked")],
      true
    );
    view.clickHeader();
    expect(view.text()).toContain("PRTS执行命令失败：已拒绝该命令执行");
  });

  it("归档轮等待过的调用：显示「（等待确认）」且不算进行中", () => {
    const view = render([call("c1", "run_command"), approvalRequest("c1")]);
    view.clickHeader();
    expect(view.text()).toContain("PRTS正在执行命令...（等待确认）");
    expect(view.text()).toContain("工具调用未完成");
  });
});

// ── 外部资料标签与失败行展示（从 EventBadge.test 迁移） ──────────────

describe("过程卡 · 外部来源与失败行", () => {
  it("external provenance 显示中性「外部」标签，title 带来源", () => {
    const view = render([
      call("c1", "web_fetch"),
      result("c1", "web_fetch", true, "外部正文", {
        trust: "external",
        source: "example.com/docs",
      }),
    ]);
    view.clickHeader();
    expect(view.text()).toContain("PRTS浏览网页完成");
    const tag = Array.from(view.container.querySelectorAll("span")).find(
      (el) => el.textContent === "外部"
    );
    expect(tag).toBeTruthy();
    expect(tag!.getAttribute("title")).toContain("example.com/docs");
  });

  it("没有 provenance 的旧结果不显示外部标签", () => {
    const view = render([call("c1", "web_fetch"), result("c1", "web_fetch")]);
    view.clickHeader();
    expect(view.text()).toContain("PRTS浏览网页完成");
    expect(view.text()).not.toContain("外部");
  });

  it("失败摘要单行省略，悬停 title 看全文", () => {
    const summary =
      "命令执行失败（退出码 1）：[stderr]\nSet-Content : 无法找到路径「C:\\x」";
    const view = render([call("c1", "run_command"), result("c1", "run_command", false, summary)]);
    view.clickHeader();
    const line = view.container.querySelector("[data-failed-line]");
    expect(line).toBeTruthy();
    const text = line!.querySelector("span:nth-child(2)") as HTMLElement;
    expect(text.getAttribute("title")).toBe(summary);
    expect(text.className).toContain("lineText");
    // 省略只在视觉层：全文仍在文本节点里
    expect(view.text()).toContain("无法找到路径");
  });
});

// ── 展示切片（从 EventBadge.test 迁移：渲染函数还在 eventDisplay.tsx） ──

describe("过程卡 · 展示切片", () => {
  const fileDisplay = {
    link: { kind: "file" as const, target: "C:/ws/notes/a.md", label: "a.md" },
  };

  it("查询类展示「短语：对象」，有展示信息就不补「...」", () => {
    const view = render([call("c1", "query_enemy", undefined, { text: "源石虫" })], true);
    view.clickHeader();
    expect(view.text()).toContain("PRTS正在查询数据库：源石虫");
    expect(view.text()).not.toContain("查询数据库...");
  });

  it("无展示信息：保留「...」尾缀", () => {
    const view = render([call("c1", "echo")], true);
    view.clickHeader();
    expect(view.text()).toContain("PRTS正在处理...");
  });

  it("展示文本超长截断，悬停 title 看全文", () => {
    const long = "甲".repeat(120);
    const view = render([call("c1", "query_enemy", undefined, { text: long, full: long })], true);
    view.clickHeader();
    const span = Array.from(view.container.querySelectorAll("span")).find(
      (el) => el.getAttribute("title") === long
    ) as HTMLElement | undefined;
    expect(span).toBeTruthy();
    expect(span!.textContent!.length).toBeLessThan(long.length);
    expect(span!.textContent!.endsWith("…")).toBe(true);
  });

  it("文件链接三态均保留；点击走 open_path", async () => {
    const running = render([call("c1", "read_file", undefined, fileDisplay)], true);
    running.clickHeader();
    expect(running.text()).toContain("PRTS正在阅读");
    expect(running.text()).toContain("a.md");

    const failed = render(
      [call("c1", "read_file", undefined, fileDisplay), result("c1", "read_file", false, "文件不存在")],
      true
    );
    failed.clickHeader();
    expect(failed.text()).toContain("PRTS阅读失败：文件不存在");
    expect(failed.text()).toContain("a.md");

    const invokeMock = (
      globalThis as unknown as {
        __invokeMock: { mockClear(): void; mock: { calls: unknown[][] } };
      }
    ).__invokeMock;
    invokeMock.mockClear();
    const link = Array.from(failed.container.querySelectorAll('[role="button"]')).find(
      (el) => el.textContent === "a.md"
    ) as HTMLElement;
    expect(link).toBeTruthy();
    act(() => link.click());
    await Promise.resolve();
    expect(invokeMock.mock.calls.some((c) => c[0] === "open_path")).toBe(true);
  });
});
