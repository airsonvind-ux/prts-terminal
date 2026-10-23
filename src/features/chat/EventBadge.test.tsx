// @vitest-environment jsdom
// EventBadge · M3-A 审批第四态状态机测试
//
// 覆盖（方案 §5.3 / 子步 7 测试清单）：
//   - pending（request 无配对 result + interactive）→ 渲染确认/拒绝按钮 + 完整命令
//   - 归档轮（interactive=false）不渲染按钮
//   - 点击确认 → invoke resolve_tool_approval 参数正确；乐观态"已确认"；
//     invoke 返回 false（无 pending）→ 不置乐观态（等事件兜底）
//   - approval-result(approved) 事件 → 按钮消失回归执行中
//   - approval-result(rejected) → 预置失败态；tool/result 到达后以后端文案展示
//   - 无审批的工具流（echo）与 auto-low（无 IPC 审批事件）不受影响

import { describe, expect, it, beforeEach, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { EventBadge } from "./EventBadge";
import type { SessionEvent, ToolDisplay, ToolProvenance } from "../../types";

// React 19 的 act() 需要显式环境标志（jsdom 默认未开启，仿 Markdown.test.ts）
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

// ── 渲染清理：断言失败提前 return 也会残留 DOM（document 全局查询
// 会命中旧 container 的按钮）——afterEach 统一卸载 + 移除 ──
const mounted: Array<{ container: HTMLDivElement; root: ReturnType<typeof createRoot>; unmounted: boolean }> = [];

afterEach(() => {
  for (const m of mounted) {
    if (!m.unmounted) {
      try {
        m.root.unmount();
      } catch {
        // 卸载失败不阻塞清理
      }
    }
    m.container.remove();
  }
  mounted.length = 0;
});

// ── 渲染辅助：mount → unmount（仿 Markdown.test.ts 模式） ──

function renderBadge(
  events: SessionEvent[],
  interactive: boolean,
  showApprovalControls = true
) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => {
    root.render(
      React.createElement(EventBadge, {
        events,
        interactive,
        showApprovalControls,
      })
    );
  });
  const entry = { container, root, unmounted: false };
  mounted.push(entry);
  return {
    html: () => container.innerHTML,
    text: () => container.textContent ?? "",
    container,
    unmount: () => {
      if (!entry.unmounted) {
        act(() => root.unmount());
        entry.unmounted = true;
      }
      container.remove();
    },
  };
}

/** 模拟点击并 flush microtask（invoke promise 链） */
async function clickAndFlush(el: Element): Promise<void> {
  await act(async () => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    await new Promise((r) => setTimeout(r, 0));
  });
}

/** 展开工具阶段（点击"工具调用"toggle；审批进行中自动展开则无需调用） */
function openTools(): void {
  const toggle = Array.from(document.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("工具调用")
  );
  act(() => {
    toggle?.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

// ── 事件夹具 ──

const SID = "s1";
const C1 = "call_cmd_1";

function toolCall(name: string, callId: string): SessionEvent {
  return {
    type: "tool/call",
    sessionId: SID,
    callId,
    toolName: name,
    params: "{}",
  };
}

/** tool/call（携带展示切片：文件链接 / URL / 文本信息） */
function toolCallWithDisplay(
  name: string,
  callId: string,
  display: ToolDisplay
): SessionEvent {
  return {
    type: "tool/call",
    sessionId: SID,
    callId,
    toolName: name,
    params: "{}",
    display,
  };
}

function approvalRequest(
  callId: string,
  command: string,
  risk: "low" | "high" | "crit",
  ruleId: string | null = null,
  privilege: "restricted" | "full" = "restricted",
  justification: string | null = null
): SessionEvent {
  return {
    type: "tool/approval-request",
    sessionId: SID,
    callId,
    command,
    risk,
    mode: "ask",
    ruleId,
    privilege,
    justification,
  };
}

function approvalResult(
  callId: string,
  decision: "approved" | "rejected",
  privilege: "restricted" | "full" = "restricted"
): SessionEvent {
  return {
    type: "tool/approval-result",
    sessionId: SID,
    callId,
    decision,
    risk: "high",
    mode: "ask",
    ruleId: null,
    reason: decision === "rejected" ? "user" : null,
    privilege,
  };
}

function toolResult(
  callId: string,
  ok: boolean,
  summary: string,
  toolName = "run_command",
  provenance?: ToolProvenance
): SessionEvent {
  return {
    type: "tool/result",
    sessionId: SID,
    callId,
    toolName,
    summary,
    ok,
    provenance,
  };
}

beforeEach(() => {
  __invokeMock.mockClear();
});

describe("EventBadge · 审批 pending（M3-A）", () => {
  it("pending 渲染确认/拒绝按钮 + 类别短语行（ruleId→短语）+ 完整命令", () => {
    const events: SessionEvent[] = [
      toolCall("run_command", C1),
      approvalRequest(
        C1,
        "Remove-Item C:\\tmp\\x.txt",
        "high",
        "delete-always-ask"
      ),
    ];
    const { text, html, unmount } = renderBadge(events, true);
    expect(text()).toContain("确认执行");
    expect(text()).toContain("拒绝");
    expect(text()).toContain("Remove-Item C:\\tmp\\x.txt");
    // 类别行：ruleId→短语（删除门），渲染于完整命令区上方；高危徽章已废除
    expect(text()).toContain(
      "将执行：删除文件或目录（任何位置都需要你确认）"
    );
    expect(text()).not.toContain("高危操作");
    expect(html().indexOf("将执行：删除文件或目录")).toBeLessThan(
      html().indexOf("Remove-Item C:\\tmp\\x.txt")
    );
    expect(html()).toContain("PRTS需要你确认执行命令");
    unmount();
  });

  it("Skill 包脚本 reason → 类别短语为执行 Skill 包内脚本", () => {
    const events: SessionEvent[] = [
      toolCall("run_command", C1),
      approvalRequest(C1, "powershell -File .\\run.ps1", "high", "skill-package-script"),
    ];
    const { text, unmount } = renderBadge(events, true);
    expect(text()).toContain("将执行：Skill 包内的脚本");
    unmount();
  });

  it("Action Dock 模式（showApprovalControls=false）→ 不渲染按钮，保留只读提示", () => {
    const events: SessionEvent[] = [
      toolCall("run_command", C1),
      approvalRequest(C1, "Get-Process", "high"),
    ];
    const { text, container, unmount } = renderBadge(events, true, false);
    expect(text()).toContain("PRTS 正在等待你确认执行命令");
    expect(text()).toContain("请在输入框上方处理");
    const buttons = Array.from(container.querySelectorAll("button"));
    expect(buttons.some((b) => b.textContent?.includes("确认执行"))).toBe(false);
    expect(buttons.some((b) => b.textContent?.includes("拒绝"))).toBe(false);
    unmount();
  });

  it("full 档 pending 渲染完整权限行 + justification（M4）", () => {
    const events: SessionEvent[] = [
      toolCall("run_command", C1),
      approvalRequest(
        C1,
        "npm install --save-dev typescript",
        "high",
        "install-op",
        "full",
        "需要写用户级 npm 缓存以完成安装"
      ),
    ];
    const { text, unmount } = renderBadge(events, true);
    expect(text()).toContain("确认执行");
    // full 行：完整权限 + justification；restricted 档不出现该行
    expect(text()).toContain(
      "将以完整权限运行：需要写用户级 npm 缓存以完成安装"
    );
    expect(text()).toContain("将执行：安装或卸载软件");
    unmount();
  });

  it("full 批准后回落执行中态（无按钮；确认文案带已确认）", () => {
    const events: SessionEvent[] = [
      toolCall("run_command", C1),
      approvalRequest(
        C1,
        "npm install",
        "high",
        "install-op",
        "full",
        "需要写用户级 npm 缓存"
      ),
      approvalResult(C1, "approved", "full"),
    ];
    const { text, unmount } = renderBadge(events, true);
    expect(text()).toContain("PRTS正在执行命令...（已确认）");
    expect(text()).not.toContain("确认执行");
    unmount();
  });

  it("ruleId 为空或未知（region-in 等区内放行族）→ 不渲染类别行、无高危字样", () => {
    for (const ruleId of [null, "region-in"]) {
      const events: SessionEvent[] = [
        toolCall("run_command", C1),
        approvalRequest(C1, "Get-Process", "low", ruleId),
      ];
      const { text, unmount } = renderBadge(events, true);
      expect(text()).toContain("确认执行");
      expect(text()).not.toContain("将执行：");
      expect(text()).not.toContain("高危");
      unmount();
    }
  });

  it("pending 工具阶段强制展开（无需点击 ▸）", () => {
    const events: SessionEvent[] = [
      toolCall("run_command", C1),
      approvalRequest(C1, "Get-Process", "low"),
    ];
    const { text, unmount } = renderBadge(events, true);
    // 默认折叠时按钮不可见——pending 强制展开后可见
    expect(text()).toContain("确认执行");
    unmount();
  });

  it("归档轮（interactive=false）不渲染按钮，命令作为事件记录保留", () => {
    const events: SessionEvent[] = [
      toolCall("run_command", C1),
      approvalRequest(C1, "Get-Process", "high"),
      approvalResult(C1, "approved"),
      toolResult(C1, true, "输出内容"),
    ];
    const { text, unmount } = renderBadge(events, false);
    // 归档轮无审批进行中：工具阶段默认折叠，展开后看事件记录
    openTools();
    expect(text()).not.toContain("确认执行");
    expect(text()).not.toContain("拒绝");
    expect(text()).toContain("PRTS执行命令完成");
    unmount();
  });

  it("点击确认 → invoke resolve_tool_approval 参数正确 + 乐观态已确认", async () => {
    __invokeMock.mockImplementationOnce(() => Promise.resolve(true));
    const events: SessionEvent[] = [
      toolCall("run_command", C1),
      approvalRequest(C1, "Get-Process", "high"),
    ];
    const { text, unmount } = renderBadge(events, true);

    const buttons = Array.from(document.querySelectorAll("button"));
    const confirmBtn = buttons.find((b) => b.textContent?.includes("确认执行"));
    expect(confirmBtn).toBeDefined();
    await clickAndFlush(confirmBtn!);

    const calls = __invokeMock.mock.calls.filter(
      (c) => c[0] === "resolve_tool_approval"
    );
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toEqual({
      request: { sessionId: SID, callId: C1, decision: "approved" },
    });
    // 乐观态：按钮消失，显示"已确认"（审批进行中保持展开）
    expect(text()).toContain("已确认");
    expect(text()).not.toContain("确认执行");
    unmount();
  });

  it("点击拒绝 → invoke decision=rejected + 乐观态预置失败", async () => {
    __invokeMock.mockImplementationOnce(() => Promise.resolve(true));
    const events: SessionEvent[] = [
      toolCall("run_command", C1),
      approvalRequest(C1, "Get-Process", "high"),
    ];
    const { text, unmount } = renderBadge(events, true);

    const buttons = Array.from(document.querySelectorAll("button"));
    const rejectBtn = buttons.find((b) => b.textContent?.includes("拒绝"));
    await clickAndFlush(rejectBtn!);

    const calls = __invokeMock.mock.calls.filter(
      (c) => c[0] === "resolve_tool_approval"
    );
    expect(calls[0][1]).toEqual({
      request: { sessionId: SID, callId: C1, decision: "rejected" },
    });
    expect(text()).toContain("已拒绝该命令执行");
    unmount();
  });

  it("invoke 返回 false（无 pending，已超时）→ 不置乐观态，保持等待事件终态", async () => {
    __invokeMock.mockImplementationOnce(() => Promise.resolve(false));
    const events: SessionEvent[] = [
      toolCall("run_command", C1),
      approvalRequest(C1, "Get-Process", "high"),
    ];
    const { text, unmount } = renderBadge(events, true);

    const confirmBtn = Array.from(document.querySelectorAll("button")).find(
      (b) => b.textContent?.includes("确认执行")
    );
    await clickAndFlush(confirmBtn!);
    // 未置乐观态：按钮仍 pending（等后端 approval-result/tool/result 兜底）
    expect(text()).toContain("确认执行");
    unmount();
  });
});

describe("EventBadge · 审批终态（事件派生）", () => {
  it("approval-result(approved) → 按钮消失回归执行中（等 tool/result）", () => {
    const events: SessionEvent[] = [
      toolCall("run_command", C1),
      approvalRequest(C1, "Get-Process", "low"),
      approvalResult(C1, "approved"),
    ];
    const { text, unmount } = renderBadge(events, true);
    expect(text()).not.toContain("确认执行");
    expect(text()).toContain("PRTS正在执行命令...（已确认）");
    unmount();
  });

  it("approval-result(rejected) → 预置失败态；tool/result 到达后展示后端文案", () => {
    // 阶段 1：result(rejected) 已达，tool/result 未达
    const stage1: SessionEvent[] = [
      toolCall("run_command", C1),
      approvalRequest(C1, "Get-Process", "high"),
      approvalResult(C1, "rejected"),
    ];
    const { text, unmount } = renderBadge(stage1, true);
    expect(text()).toContain("已拒绝该命令执行");
    unmount();

    // 阶段 2：tool/result(ok=false, 后端拒绝文案) 到达 → summary 覆盖展示
    const stage2: SessionEvent[] = [
      toolCall("run_command", C1),
      approvalRequest(C1, "Get-Process", "high"),
      approvalResult(C1, "rejected"),
      toolResult(C1, false, "用户拒绝了该命令执行"),
    ];
    const { text: text2, unmount: unmount2 } = renderBadge(stage2, true);
    openTools(); // tool/result 终态到达后恢复折叠（无审批进行中）——展开后断言详情
    expect(text2()).toContain("用户拒绝了该命令执行");
    expect(text2()).toContain("PRTS执行命令失败");
    unmount2();
  });

  it("被拒命令仍产出 tool/result——tool/call 卡不孤儿（配对完整）", () => {
    const events: SessionEvent[] = [
      toolCall("run_command", C1),
      approvalRequest(C1, "Get-Process", "high"),
      approvalResult(C1, "rejected"),
      toolResult(C1, false, "用户拒绝了该命令执行"),
    ];
    const { text, unmount } = renderBadge(events, true);
    // 终态文案非"进行中"残留
    expect(text()).not.toContain("正在");
    expect(text()).toContain("失败");
    unmount();
  });
});

describe("EventBadge · 归档轮未完成工具", () => {
  it("重启后无 result 的工具显示未完成，不显示正在调用", () => {
    const { text, unmount } = renderBadge([toolCall("query_enemy", C1)], false);
    expect(text()).toContain("工具调用未完成");
    expect(text()).not.toContain("正在调用工具");
    unmount();
  });
});

describe("EventBadge · 无审批路径不受影响", () => {
  it("普通工具（echo）进行中/完成三态不变", () => {
    const running: SessionEvent[] = [toolCall("echo", "c2")];
    const { text, unmount } = renderBadge(running, true);
    openTools();
    expect(text()).toContain("PRTS正在处理...");
    unmount();
  });

  it("auto-low（无 IPC 审批事件）：tool/call → tool/result 直接完成", () => {
    const events: SessionEvent[] = [
      toolCall("run_command", C1),
      toolResult(C1, true, "查询输出"),
    ];
    const { text, unmount } = renderBadge(events, true);
    openTools();
    expect(text()).toContain("PRTS执行命令完成");
    expect(text()).not.toContain("确认执行");
    unmount();
  });
});

describe("EventBadge · 展示切片（进行中即显示 / 失败保留 / 链接）", () => {
  const fileDisplay: ToolDisplay = {
    link: { kind: "file", target: "C:/ws/notes/a.md", label: "a.md" },
  };

  it("文件链接：进行中即显示，三态均保留", () => {
    const callId = "c_read";
    // 进行中
    const running = [toolCallWithDisplay("read_file", callId, fileDisplay)];
    const r = renderBadge(running, true);
    openTools();
    expect(r.text()).toContain("PRTS正在阅读");
    expect(r.text()).toContain("a.md");
    r.unmount();

    // 完成
    const done = [
      toolCallWithDisplay("read_file", callId, fileDisplay),
      toolResult(callId, true, "（共 3 行）", "read_file"),
    ];
    const d = renderBadge(done, true);
    openTools();
    expect(d.text()).toContain("PRTS阅读完成");
    expect(d.text()).toContain("a.md");
    d.unmount();

    // 失败：链接仍保留（挂在 call 侧）
    const failed = [
      toolCallWithDisplay("read_file", callId, fileDisplay),
      toolResult(callId, false, "文件不存在", "read_file"),
    ];
    const f = renderBadge(failed, true);
    openTools();
    expect(f.text()).toContain("PRTS阅读失败：文件不存在");
    expect(f.text()).toContain("a.md");
    f.unmount();
  });

  it("文本信息：查询类展示「短语：对象」", () => {
    const events = [
      toolCallWithDisplay("query_enemy", "c_q", { text: "源石虫" }),
    ];
    const { text, unmount } = renderBadge(events, true);
    openTools();
    expect(text()).toContain("PRTS正在查询数据库：源石虫");
    // 有展示信息时不再追加「...」
    expect(text()).not.toContain("查询数据库...");
    unmount();
  });

  it("无展示信息：保留原有「...」尾缀", () => {
    const events = [toolCall("echo", "c_echo")];
    const { text, unmount } = renderBadge(events, true);
    openTools();
    expect(text()).toContain("PRTS正在处理...");
    unmount();
  });

  it("展示文本超长截断（纯长度约束 + 省略号）", () => {
    const long = "甲".repeat(120);
    const events = [
      toolCallWithDisplay("query_enemy", "c_long", { text: long }),
    ];
    const { text, unmount } = renderBadge(events, true);
    openTools();
    expect(text()).toContain("…");
    expect(text()).not.toContain(long);
    unmount();
  });

  it("截断时悬停可看全文（title）；未截断不带 title", () => {
    const full = "x".repeat(300);
    const events = [
      toolCallWithDisplay("run_command", "c_full", {
        text: `${full.slice(0, 60)}…`,
        full,
      }),
    ];
    const { unmount } = renderBadge(events, true);
    openTools();
    const node = Array.from(document.querySelectorAll("span")).find(
      (el) => el.getAttribute("title") === full
    );
    expect(node).not.toBeNull();
    expect(node!.textContent).toContain("…");
    unmount();

    // 未截断：无 full → 不带 title
    const short = [toolCallWithDisplay("run_command", "c_short", { text: "echo hi" })];
    const r2 = renderBadge(short, true);
    openTools();
    expect(
      Array.from(document.querySelectorAll("span")).some((el) => el.getAttribute("title"))
    ).toBe(false);
    r2.unmount();
  });

  it("点击文件链接 → invoke open_path（path=target）", async () => {
    __invokeMock.mockImplementation(() => Promise.resolve(undefined));
    const events = [
      toolCallWithDisplay("read_file", "c_click", fileDisplay),
    ];
    const { unmount } = renderBadge(events, true);
    openTools();
    const link = document.querySelector("[role='button']");
    expect(link).not.toBeNull();
    await clickAndFlush(link!);
    const calls = __invokeMock.mock.calls.filter((c) => c[0] === "open_path");
    expect(calls).toHaveLength(1);
    expect((calls[0][1] as { path: string }).path).toBe("C:/ws/notes/a.md");
    unmount();
  });

  it("URL 链接 → invoke open_url（系统浏览器）", async () => {
    __invokeMock.mockImplementation(() => Promise.resolve(undefined));
    const urlDisplay: ToolDisplay = {
      link: { kind: "url", target: "https://example.com/x", label: "https://example.com/x" },
    };
    const events = [
      toolCallWithDisplay("web_fetch", "c_url", urlDisplay),
    ];
    const { text, unmount } = renderBadge(events, true);
    openTools();
    expect(text()).toContain("PRTS正在浏览网页");
    const link = document.querySelector("[role='button']");
    await clickAndFlush(link!);
    const calls = __invokeMock.mock.calls.filter((c) => c[0] === "open_url");
    expect(calls).toHaveLength(1);
    expect((calls[0][1] as { url: string }).url).toBe("https://example.com/x");
    unmount();
  });
});

// ── 外部资料标签（R-02a） ──

describe("EventBadge · 外部资料标签", () => {
  it("external provenance 显示中性「外部」标签与来源标题", () => {
    const events = [
      toolCall("web_fetch", "c_ext"),
      toolResult("c_ext", true, "外部正文", "web_fetch", {
        trust: "external",
        source: "example.com/docs",
      }),
    ];
    const { text, unmount } = renderBadge(events, true);
    openTools();

    expect(text()).toContain("PRTS浏览网页完成");
    const tag = Array.from(document.querySelectorAll("span")).find(
      (el) => el.textContent === "外部"
    );
    expect(tag).not.toBeUndefined();
    expect(tag!.getAttribute("title")).toContain("example.com/docs");
    unmount();
  });

  it("旧 tool/result 无 provenance → 不显示外部标签", () => {
    const events = [
      toolCall("web_fetch", "c_legacy"),
      toolResult("c_legacy", true, "旧结果", "web_fetch"),
    ];
    const { text, unmount } = renderBadge(events, true);
    openTools();

    expect(text()).toContain("PRTS浏览网页完成");
    expect(text()).not.toContain("外部");
    unmount();
  });
});

// ── 失败行展示：单行省略 + 悬停 title 看全文（展示面收敛） ──

describe("EventBadge · 失败行展示", () => {
  it("失败 summary 单行省略：带省略类 + title 全文（全文仍在 DOM）", () => {
    const summary =
      "命令执行失败（退出码 1）：[stderr]\nSet-Content : 无法找到路径「C:\\x」";
    const events = [toolCall("run_command", C1), toolResult(C1, false, summary)];
    const { text, unmount } = renderBadge(events, true);
    openTools();
    const span = Array.from(document.querySelectorAll("span")).find((s) =>
      s.textContent?.includes("PRTS执行命令失败")
    );
    expect(span).toBeTruthy();
    expect(span!.getAttribute("title")).toBe(summary);
    expect(span!.className).toContain("toolFailLine");
    // 省略只发生在视觉层：全文仍在文本节点里
    expect(text()).toContain("无法找到路径");
    unmount();
  });
});
