// 消息流重构 · 第 1 步：切分器测试。
//
// 断言块序列本身（形状、顺序、id、摘要），不渲染组件。
// 视图块这一步只有壳，用 `tool/result` 上的临时 view 字段驱动。

import { describe, it, expect } from "vitest";
import type { SessionEvent } from "../../types";
import { splitTurn, type Block } from "./blocks";

const S = "s1";

function text(content: string): SessionEvent {
  return { type: "assistant/chunk", sessionId: S, kind: "text", content };
}

function reasoning(content: string): SessionEvent {
  return { type: "assistant/chunk", sessionId: S, kind: "reasoning", content };
}

function call(callId: string, toolName = "echo"): SessionEvent {
  return { type: "tool/call", sessionId: S, callId, toolName, params: "{}" };
}

function result(callId: string, ok = true, summary = "ok"): SessionEvent {
  return { type: "tool/result", sessionId: S, callId, toolName: "echo", summary, ok };
}

function viewResult(callId: string, view: unknown): SessionEvent {
  return {
    type: "tool/result",
    sessionId: S,
    callId,
    toolName: "echo",
    summary: "ok",
    ok: true,
    // ② 步给类型加字段前，这里按实现里的读取方式塞进去
    view,
  } as SessionEvent;
}

function approvalRequest(callId: string): SessionEvent {
  return {
    type: "tool/approval-request",
    sessionId: S,
    callId,
    command: "rm -rf build",
    risk: "high",
    mode: "ask",
    ruleId: null,
    privilege: "restricted",
    justification: null,
  } as SessionEvent;
}

function approvalResult(callId: string, decision: "approved" | "rejected"): SessionEvent {
  return {
    type: "tool/approval-result",
    sessionId: S,
    callId,
    decision,
    risk: "high",
    mode: "ask",
    ruleId: null,
    reason: "user",
    privilege: "restricted",
  } as SessionEvent;
}

function message(content: string, reasoningContent: string | null = null): SessionEvent {
  return {
    type: "assistant/message",
    sessionId: S,
    content,
    reasoningContent,
    usage: null,
  };
}

function reset(): SessionEvent {
  return { type: "assistant/reset", sessionId: S };
}

function mood(): SessionEvent {
  return { type: "mood/event", sessionId: S, mood: "高兴", intensity: 2 };
}

function kinds(blocks: Block[]): string[] {
  return blocks.map((b) => b.kind);
}

describe("splitTurn · 切点", () => {
  it("文本之后来了工具调用：文本块在前、过程卡在后，文本是过程文本", () => {
    const blocks = splitTurn([text("先看看"), call("c1"), result("c1")]);
    expect(kinds(blocks)).toEqual(["text", "card"]);
    const [first, second] = blocks;
    if (first.kind !== "text") throw new Error("第一块应是文本");
    expect(first.transition).toBe(true);
    if (second.kind !== "card") throw new Error("第二块应是过程卡");
    expect(second.summary).toEqual({ reasoning: 0, calls: 1, failures: 0 });
  });

  it("轮尾文本不弱化", () => {
    const blocks = splitTurn([call("c1"), result("c1"), text("结论")]);
    expect(kinds(blocks)).toEqual(["card", "text"]);
    const tail = blocks[1];
    if (tail.kind !== "text") throw new Error("末块应是文本");
    expect(tail.transition).toBe(false);
  });

  it("连续多次工具调用之间没有文本：合成一张卡", () => {
    const blocks = splitTurn([
      call("c1"),
      result("c1"),
      call("c2"),
      result("c2"),
      call("c3"),
      result("c3", false, "失败了"),
    ]);
    expect(blocks).toHaveLength(1);
    const card = blocks[0];
    if (card.kind !== "card") throw new Error("应是过程卡");
    expect(card.summary).toEqual({ reasoning: 0, calls: 3, failures: 1 });
    expect(card.rows).toHaveLength(3);
  });

  it("视图随结果到达：结果行留在本卡，视图块紧随其后，再开新卡", () => {
    const blocks = splitTurn([
      reasoning("先看一眼"),
      call("c1"),
      viewResult("c1", { title: "关卡信息" }),
      call("c2"),
      result("c2"),
    ]);
    expect(kinds(blocks)).toEqual(["card", "view", "card"]);
    const view = blocks[1];
    if (view.kind !== "view") throw new Error("第二块应是视图");
    expect(view.view).toEqual({ title: "关卡信息" });

    const first = blocks[0];
    if (first.kind !== "card") throw new Error("第一块应是过程卡");
    expect(first.rows).toHaveLength(2);
    expect(first.rows[1]).toMatchObject({ kind: "tool", callId: "c1", status: "ok" });
  });
});

describe("splitTurn · 行分类", () => {
  it("思考按工具调用切段，并排在该卡最前", () => {
    const blocks = splitTurn([
      reasoning("想一下"),
      call("c1"),
      result("c1"),
      reasoning("再想一下"),
      call("c2"),
      result("c2"),
    ]);
    const card = blocks[0];
    if (card.kind !== "card") throw new Error("应是过程卡");
    expect(card.rows.map((r) => r.kind)).toEqual(["reasoning", "tool", "reasoning", "tool"]);
    expect(card.summary.reasoning).toBe(2);
    expect(card.summary.calls).toBe(2);
  });

  it("思考紧跟文本时另起一段，不混进文本块", () => {
    const blocks = splitTurn([text("先说一句"), reasoning("然后想"), call("c1"), result("c1")]);
    expect(kinds(blocks)).toEqual(["text", "card"]);
    const card = blocks[1];
    if (card.kind !== "card") throw new Error("应是过程卡");
    expect(card.rows[0]).toEqual({ kind: "reasoning", text: "然后想" });
  });

  it("审批请求把工具行标成等待确认，结果决定通过或拒绝", () => {
    const blocks = splitTurn([
      reasoning("要跑命令了"),
      call("c1"),
      approvalRequest("c1"),
      approvalResult("c1", "rejected"),
      result("c1", false, "用户拒绝"),
    ]);
    const card = blocks[0];
    if (card.kind !== "card") throw new Error("应是过程卡");
    expect(card.rows[1]).toMatchObject({ kind: "tool", approval: "rejected", status: "failed" });
  });

  it("情绪事件独立成块，不混进过程卡", () => {
    const blocks = splitTurn([reasoning("想想"), mood(), call("c1"), result("c1")]);
    expect(kinds(blocks)).toEqual(["card", "mood", "card"]);
  });

  it("空推理（只有空白）不成段", () => {
    const blocks = splitTurn([reasoning("  \n "), call("c1"), result("c1")]);
    const card = blocks[0];
    if (card.kind !== "card") throw new Error("应是过程卡");
    expect(card.summary.reasoning).toBe(0);
  });
});

describe("splitTurn · reset 与旧归档", () => {
  it("reset 之前的输出不渲染", () => {
    const blocks = splitTurn([text("被撤回的半截"), reset(), text("重说的"), call("c1"), result("c1")]);
    expect(kinds(blocks)).toEqual(["text", "card"]);
    const first = blocks[0];
    if (first.kind !== "text") throw new Error("应是文本块");
    expect(first.content).toBe("重说的");
  });

  it("没有任何 text chunk 的旧轮：回退成「过程卡 + 最终文本」", () => {
    const blocks = splitTurn([call("c1"), result("c1"), message("旧会话的回答")]);
    expect(kinds(blocks)).toEqual(["card", "text"]);
    const last = blocks[1];
    if (last.kind !== "text") throw new Error("应是文本块");
    expect(last.content).toBe("旧会话的回答");
    expect(last.transition).toBe(false);
  });

  it("空轮不产出块", () => {
    expect(splitTurn([])).toEqual([]);
    expect(splitTurn([message("")])).toEqual([]);
  });
});

describe("splitTurn · 形状", () => {
  it("块 id 是「轮序号 + 轮内序号」，不随内容变化", () => {
    const blocks = splitTurn([text("a"), call("c1"), result("c1"), text("b")], 3);
    expect(blocks.map((b) => b.id)).toEqual(["t3-b0", "t3-b1", "t3-b2"]);
  });

  it("摘要只统计非零项所需的数据（渲染层自己省略）", () => {
    const blocks = splitTurn([text("只有文本")]);
    expect(kinds(blocks)).toEqual(["text"]);
  });

  it("过程卡带上原始事件切片，供第 2 步复用槽位渲染", () => {
    const events = [call("c1"), result("c1")];
    const card = splitTurn(events)[0];
    if (card.kind !== "card") throw new Error("应是过程卡");
    expect(card.events).toEqual(events);
  });
});

describe("splitTurn · 开销基准", () => {
  it("5000 个 chunk 的切分在一次刷新预算内", () => {
    const events: SessionEvent[] = [];
    for (let i = 0; i < 5000; i++) {
      events.push(reasoning(`r${i} `));
      events.push(text(`t${i} `));
    }
    const started = performance.now();
    const blocks = splitTurn(events);
    const elapsed = performance.now() - started;
    // 只作哨兵：真慢了会先在 development 里被看见，而不是等到 CI 偶发绿。
    expect(blocks.length).toBeGreaterThan(0);
    expect(elapsed).toBeLessThan(200);
  });
});
