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
