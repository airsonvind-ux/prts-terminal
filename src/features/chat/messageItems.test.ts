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

describe("buildArchivedItems · 压缩卡", () => {
  it("全部折叠时在列表末尾补卡，点击分界有落点", () => {
    const items = buildArchivedItems(
      [[user("早期问题")], [user("最近问题")]],
      false,
      { retainedFrom: 2, summary: "摘要", tokensBefore: 100, at: 1 }
    );

    expect(items[items.length - 1]).toEqual({
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

    expect(items[items.length - 1]).toEqual({
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

describe("buildArchivedItems · 错误轮", () => {
  it("先渲染半截回答，再标回答未完成，最后显示错误", () => {
    const items = buildArchivedItems(
      [[user("查询 X"), chunk("已经显示的半截"), error("连接中断")]],
      false,
      undefined
    );

    const visible = items.filter((item) => item.type !== "events");
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
