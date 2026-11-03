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

describe("buildToolSlots · 调用文案三级派生", () => {
  it("作者声明了 action 就用它", () => {
    const slots = buildToolSlots([
      {
        type: "tool/call",
        sessionId: "s",
        callId: "c1",
        toolName: "pic_catalog_list_images",
        params: "{}",
        action: "清点图片",
        pluginTool: true,
      },
    ]);
    expect(slots[0].phrase).toBe("清点图片");
  });

  it("插件工具没写 action：保底给完整工具名", () => {
    const slots = buildToolSlots([
      {
        type: "tool/call",
        sessionId: "s",
        callId: "c1",
        toolName: "pic_catalog_list_images",
        params: "{}",
        pluginTool: true,
      },
    ]);
    expect(slots[0].phrase).toBe("调用插件工具：pic_catalog_list_images");
  });

  it("宿主工具沿用工具名表（旧归档同样走这条）", () => {
    const slots = buildToolSlots([call("c1")]);
    expect(slots[0].phrase).toBe("查询数据库");
  });
});

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
