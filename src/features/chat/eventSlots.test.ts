import { describe, expect, it } from "vitest";
import type { SessionEvent } from "../../types";
import { buildToolSlots } from "./eventSlots";

function call(callId: string): SessionEvent {
  return {
    type: "tool/call",
    sessionId: "s",
    callId,
    toolName: "query_enemy",
    params: "{}",
  };
}

function result(callId: string, summary: string): SessionEvent {
  return {
    type: "tool/result",
    sessionId: "s",
    callId,
    toolName: "query_enemy",
    summary,
    ok: true,
  };
}

describe("buildToolSlots · 重复/空 callId", () => {
  it("空 callId 按事件顺序逐个配对，不让结果全部覆盖第一个槽", () => {
    const events: SessionEvent[] = [];
    for (let i = 0; i < 5; i += 1) {
      events.push(call(""), result("", `第${i + 1}次结果`));
    }

    const slots = buildToolSlots(events);
    expect(slots).toHaveLength(5);
    expect(slots.every((slot) => slot.ok === true)).toBe(true);
    expect(slots.map((slot) => slot.summary)).toEqual([
      "第1次结果",
      "第2次结果",
      "第3次结果",
      "第4次结果",
      "第5次结果",
    ]);
  });

  it("重复非空 callId 也按未完成槽位顺序配对", () => {
    const events: SessionEvent[] = [
      call("dup"),
      call("dup"),
      result("dup", "第一次"),
      result("dup", "第二次"),
    ];

    const slots = buildToolSlots(events);
    expect(slots).toHaveLength(2);
    expect(slots.map((slot) => slot.summary)).toEqual(["第一次", "第二次"]);
    expect(slots.every((slot) => slot.ok === true)).toBe(true);
  });
});
