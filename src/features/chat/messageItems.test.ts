import { describe, expect, it } from "vitest";
import type { SessionEvent } from "../../types";
import { buildArchivedItems } from "./messageItems";

function user(content: string): SessionEvent {
  return { type: "user/message", sessionId: "s1", content };
}

function chunk(content: string): SessionEvent {
  return { type: "assistant/chunk", sessionId: "s1", kind: "text", content };
}

function error(message: string): SessionEvent {
  return {
    type: "error",
    sessionId: "s1",
    message,
    recoverable: false,
  };
}

function assistantMessage(content: string): SessionEvent {
  return {
    type: "assistant/message",
    sessionId: "s1",
    content,
    reasoningContent: null,
    usage: null,
  };
}

describe("buildArchivedItems · 交错顺序", () => {
  it("文本、过程卡、文本按到达顺序出条目，不再整段先卡后文", () => {
    const items = buildArchivedItems(
      [
        [
          user("查一下"),
          chunk("我先查一下。"),
          {
            type: "tool/call",
            sessionId: "s1",
            callId: "c1",
            toolName: "query_enemy",
            params: "{}",
          },
          {
            type: "tool/result",
            sessionId: "s1",
            callId: "c1",
            toolName: "query_enemy",
            ok: true,
            summary: "找到 2 条",
          },
          chunk("查到了，共 2 条。"),
        ],
      ],
      false,
      undefined
    );

    expect(
      items.map((item) => (item.type === "message" ? item.role : item.type))
    ).toEqual(["user", "assistant", "process", "assistant"]);
    expect(items[1]).toMatchObject({
      segments: [{ content: "我先查一下。", transition: true }],
    });
    expect(items[3]).toMatchObject({
      segments: [{ content: "查到了，共 2 条。", transition: false }],
    });
  });
});

describe("buildArchivedItems · 压缩卡", () => {
  it("全部折叠时在列表末尾补卡，点击分界有落点", () => {
    const items = buildArchivedItems(
      [[user("早期问题")], [user("最近问题")]],
      false,
      { retainedFrom: 2, summary: "摘要", tokensBefore: 100, at: 1 }
    );

    expect(items[items.length - 1]).toMatchObject({
      type: "compaction",
      summary: "摘要",
      foldedCount: 2,
    });
    expect(items.filter((item) => item.type === "compaction")).toHaveLength(1);
  });

  it("retainedFrom 超出 turns 长度时仍在末尾补卡", () => {
    const items = buildArchivedItems(
      [[user("早期问题")], [user("最近问题")]],
      false,
      { retainedFrom: 3, summary: "摘要", tokensBefore: 100, at: 1 }
    );

    expect(items[items.length - 1]).toMatchObject({
      type: "compaction",
      summary: "摘要",
      foldedCount: 3,
    });
  });

  it("正常切点仍在保留段第一轮之前", () => {
    const items = buildArchivedItems(
      [[user("早期问题")], [user("最近问题")]],
      false,
      { retainedFrom: 1, summary: "摘要", tokensBefore: 100, at: 1 }
    );

    expect(items.map((item) => item.type)).toEqual([
      "message",
      "compaction",
      "message",
    ]);
  });
});

describe("buildArchivedItems · 归档展示清洗", () => {
  it("归档 assistant/message 仍带 mood 标签时，展示副本剥离、原文不改", () => {
    const turn = [user("测试"), assistantMessage("【mood:困惑】\n\n正文")];
    const items = buildArchivedItems([turn], false, undefined);
    const assistant = items.find(
      (item) => item.type === "message" && item.role === "assistant"
    );

    expect(assistant).toMatchObject({
      type: "message",
      role: "assistant",
      segments: [{ content: "\n正文", transition: false }],
    });
    expect((turn[1] as { content: string }).content).toBe("【mood:困惑】\n\n正文");

    const mood = items.find((item) => item.type === "mood");
    expect(mood).toMatchObject({
      type: "mood",
      events: [
        {
          type: "mood/event",
          mood: "困惑",
          tint: "困惑",
          intensity: 0,
        },
      ],
    });
  });
});

describe("buildArchivedItems · 错误轮", () => {
  it("先渲染半截回答，再标回答未完成，最后显示错误", () => {
    const items = buildArchivedItems(
      [[user("查询 X"), chunk("已经显示的半截"), error("连接中断")]],
      false,
      undefined
    );

    const visible = items.filter((item) => item.type !== "mood");
    expect(
      visible.map((item) => (item.type === "message" ? item.role : item.type))
    ).toEqual(["user", "assistant", "incomplete", "error"]);
    expect(visible[1]).toMatchObject({
      type: "message",
      role: "assistant",
      segments: [{ content: "已经显示的半截", transition: false }],
    });
  });

  it("没有任何输出时不加“回答未完成”标记", () => {
    const items = buildArchivedItems(
      [[user("查询 X"), error("连接中断")]],
      false,
      undefined
    );

    expect(items.map((item) => (item.type === "message" ? item.role : item.type)))
      .toEqual(["user", "error"]);
  });
});
