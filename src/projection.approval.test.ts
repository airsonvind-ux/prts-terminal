// projection · M3-A 审批事件投影收容测试
//
// 覆盖：approval 两事件进 isMetaEvent（与 tool/call 同段归档）；
// deriveMessages / deriveTextSegments 不消费审批事件（结构事件，
// LLM 感知走 tool/result 文案——与后端 derive_messages 语义对齐）。

import { describe, expect, it } from "vitest";
import {
  deriveMessages,
  deriveTextSegments,
  isMetaEvent,
} from "./projection";
import type { SessionEvent } from "./types";

const SID = "s1";
const C1 = "call_1";

function requestEvent(): SessionEvent {
  return {
    type: "tool/approval-request",
    sessionId: SID,
    callId: C1,
    command: "Get-Process",
    risk: "high",
    mode: "ask",
    ruleId: null,
    privilege: "restricted",
    justification: null,
  };
}

function resultEvent(): SessionEvent {
  return {
    type: "tool/approval-result",
    sessionId: SID,
    callId: C1,
    decision: "approved",
    risk: "high",
    mode: "ask",
    ruleId: null,
    reason: null,
    privilege: "restricted",
  };
}

describe("isMetaEvent · 审批事件收容（M3-A）", () => {
  it("tool/approval-request 与 tool/approval-result 均为元事件（事件卡）", () => {
    expect(isMetaEvent(requestEvent())).toBe(true);
    expect(isMetaEvent(resultEvent())).toBe(true);
  });

  it("非元事件（turn 边界等）不受影响", () => {
    expect(isMetaEvent({ type: "turn/start", sessionId: SID })).toBe(false);
    expect(isMetaEvent({ type: "user/message", sessionId: SID, content: "x" })).toBe(false);
  });
});

describe("deriveMessages · 审批为结构事件不派生（LLM 经 tool/result 感知）", () => {
  it("含审批事件的完整工具轮派生等价于无审批事件轮（内容一致）", () => {
    const base: SessionEvent[] = [
      { type: "user/message", sessionId: SID, content: "跑命令" },
      { type: "assistant/chunk", sessionId: SID, kind: "text", content: "" },
      { type: "tool/call", sessionId: SID, callId: C1, toolName: "run_command", params: "{\"command\":\"Get-Process\"}" },
      { type: "tool/result", sessionId: SID, callId: C1, toolName: "run_command", summary: "输出", ok: true },
    ];
    const withApproval: SessionEvent[] = [
      ...base.slice(0, 3),
      requestEvent(),
      resultEvent(),
      ...base.slice(3),
    ];

    const plain = deriveMessages([base]);
    const approved = deriveMessages([withApproval]);
    expect(approved).toEqual(plain);
    // tool 消息 = 结果文案（LLM 可见面没有审批事件噪音）
    expect(approved[approved.length - 1]).toEqual({
      role: "tool",
      content: "输出",
      toolCallId: C1,
    });
  });
});

describe("deriveTextSegments · 审批事件不影响文本分段", () => {
  it("审批事件不闭合文本段、不产生分段噪音", () => {
    const events: SessionEvent[] = [
      { type: "user/message", sessionId: SID, content: "跑命令" },
      { type: "assistant/chunk", sessionId: SID, kind: "text", content: "我先执行" },
      { type: "tool/call", sessionId: SID, callId: C1, toolName: "run_command", params: "{}" },
      requestEvent(),
      resultEvent(),
      { type: "tool/result", sessionId: SID, callId: C1, toolName: "run_command", summary: "输出", ok: true },
      { type: "assistant/chunk", sessionId: SID, kind: "text", content: "完成" },
    ];
    const segments = deriveTextSegments(events);
    // 与无审批事件时的分段结构一致：工具调用前文本 = transition 段，其后 = 最终段
    expect(segments).toEqual([
      { content: "我先执行", transition: true },
      { content: "完成", transition: false },
    ]);
  });
});
