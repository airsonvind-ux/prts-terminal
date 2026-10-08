// Store 集成测试 · M3-A 审批事件收容（processing 投影缓存）
//
// 直接调用 Store actions 模拟事件到达（仿 processing.integration.test.ts 模式，
// 不经过 useAgentEvents React 层——其 case 分支仅追加收容，此处验证落位语义）：
//   tool/approval-request / tool/approval-result 与 tool 事件同缓冲、
//   同 sessionId 路由、按到达顺序入 currentEvents；turn/end 归档时完整落库
//   （持久事实，审计可回放）。审批被拒仍产出 tool/result（配对不变量）。

import { describe, it, expect, beforeEach } from "vitest";
import { useProcessingStore, flushPending } from "../useProcessingStore";
import { useConversationStore } from "../useConversationStore";
import { useUIStore } from "../useUIStore";
import type { SessionEvent } from "../../types";

function resetAllStores(): void {
  useProcessingStore.setState({ processing: {} });
  useConversationStore.setState({ conversations: [], activeId: null });
  useUIStore.setState({
    errorMsg: null,
    overlay: null,
  });
  __invokeMock.mockClear();
}

async function startTurn(input: string): Promise<string> {
  await useProcessingStore.getState().sendMessage(input);
  const sids = Object.keys(useProcessingStore.getState().processing);
  expect(sids).toHaveLength(1);
  return sids[0];
}

function eventsOf(sid: string): SessionEvent[] {
  return useProcessingStore.getState().processing[sid].currentEvents;
}

beforeEach(resetAllStores);

describe("M3-A · 审批事件收容与归档", () => {
  it("request/result 按到达顺序入 currentEvents（tool/call 与 tool/result 之间）", async () => {
    const sid = await startTurn("帮我查一下源石虫");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent({
      type: "tool/call",
      sessionId: sid,
      callId: "call_1",
      toolName: "run_command",
      params: '{"command":"Get-Process"}',
    });
    store.appendCurrentEvent({
      type: "tool/approval-request",
      sessionId: sid,
      callId: "call_1",
      command: "Get-Process",
      risk: "high",
      mode: "ask",
      ruleId: null,
      privilege: "restricted",
      justification: null,
    });
    store.appendCurrentEvent({
      type: "tool/approval-result",
      sessionId: sid,
      callId: "call_1",
      decision: "rejected",
      risk: "high",
      mode: "ask",
      ruleId: null,
      reason: "user",
      privilege: "restricted",
    });
    store.appendCurrentEvent({
      type: "tool/result",
      sessionId: sid,
      callId: "call_1",
      toolName: "run_command",
      summary: "用户拒绝了该命令执行",
      ok: false,
    });

    const types = eventsOf(sid).map((e) => e.type);
    expect(types).toEqual([
      "user/message",
      "tool/call",
      "tool/approval-request",
      "tool/approval-result",
      "tool/result",
    ]);
  });

  it("拒绝路径归档完整：approval 两事件 + ok=false tool/result 同轮落库", async () => {
    const sid = await startTurn("跑个命令");

    const store = useProcessingStore.getState();
    store.appendCurrentEvent({
      type: "tool/call",
      sessionId: sid,
      callId: "call_1",
      toolName: "run_command",
      params: '{"command":"Get-Process"}',
    });
    store.appendCurrentEvent({
      type: "tool/approval-request",
      sessionId: sid,
      callId: "call_1",
      command: "Get-Process",
      risk: "low",
      mode: "ask",
      ruleId: null,
      privilege: "restricted",
      justification: null,
    });
    store.appendCurrentEvent({
      type: "tool/approval-result",
      sessionId: sid,
      callId: "call_1",
      decision: "approved",
      risk: "low",
      mode: "ask",
      ruleId: null,
      reason: null,
      privilege: "restricted",
    });
    store.appendCurrentEvent({
      type: "tool/result",
      sessionId: sid,
      callId: "call_1",
      toolName: "run_command",
      summary: "查询输出",
      ok: true,
    });
    flushPending(sid);
    store.finishProcessing(sid);

    const conv = useConversationStore.getState().conversations[0];
    expect(conv.turns).toHaveLength(1);
    const archived = conv.turns[0];
    const types = archived.map((e) => e.type);
    expect(types).toContain("tool/call");
    expect(types).toContain("tool/approval-request");
    expect(types).toContain("tool/approval-result");
    expect(types).toContain("tool/result");
    expect(types.indexOf("tool/call")).toBeLessThan(
      types.indexOf("tool/approval-request")
    );
    expect(types.indexOf("tool/approval-request")).toBeLessThan(
      types.indexOf("tool/approval-result")
    );
    expect(types.indexOf("tool/approval-result")).toBeLessThan(
      types.indexOf("tool/result")
    );
  });
});
