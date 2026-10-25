import { describe, expect, it } from "vitest";
import type { Conversation, SessionEvent } from "../../types";
import { derivePreviousTurnStatus } from "../processingRequest";

function conv(lastTurn: SessionEvent[]): Conversation {
  return {
    id: "c1",
    title: "t",
    turns: [lastTurn],
    createdAt: 0,
  };
}

describe("derivePreviousTurnStatus", () => {
  it("识别用户中止", () => {
    expect(
      derivePreviousTurnStatus(
        conv([
          { type: "user/message", sessionId: "s1", content: "q" },
          { type: "turn/aborted", sessionId: "s1" },
        ])
      )
    ).toBe("aborted");
  });

  it("归档轮无终止标记但有半截输出 → interrupted", () => {
    expect(
      derivePreviousTurnStatus(
        conv([
          { type: "user/message", sessionId: "s1", content: "q" },
          {
            type: "assistant/chunk",
            sessionId: "s1",
            kind: "reasoning",
            content: "想到一半",
          },
        ])
      )
    ).toBe("interrupted");
  });

  it("空占位轮不遮蔽前面的中止轮", () => {
    const c = conv([
      { type: "user/message", sessionId: "s1", content: "q" },
      { type: "turn/aborted", sessionId: "s1" },
    ]);
    c.turns.push([]);
    expect(derivePreviousTurnStatus(c)).toBe("aborted");
  });

  it("最近一轮正常完成 → 不继承更早的中止状态", () => {
    const c = conv([
      { type: "user/message", sessionId: "s1", content: "q" },
      { type: "turn/aborted", sessionId: "s1" },
    ]);
    c.turns.push([
      { type: "user/message", sessionId: "s2", content: "q2" },
      {
        type: "assistant/message",
        sessionId: "s2",
        content: "答完",
        reasoningContent: null,
        usage: null,
      },
    ]);
    expect(derivePreviousTurnStatus(c)).toBeUndefined();
  });

  it("识别不可恢复错误", () => {
    expect(
      derivePreviousTurnStatus(
        conv([
          { type: "user/message", sessionId: "s1", content: "q" },
          {
            type: "error",
            sessionId: "s1",
            message: "boom",
            recoverable: false,
          },
        ])
      )
    ).toBe("error");
    expect(
      derivePreviousTurnStatus(
        conv([
          { type: "user/message", sessionId: "s1", content: "q" },
          { type: "turn/end", sessionId: "s1", reason: "error" },
        ])
      )
    ).toBe("error");
    expect(
      derivePreviousTurnStatus(
        conv([
          { type: "user/message", sessionId: "s1", content: "q" },
          { type: "turn/end", sessionId: "s1", reason: "interrupted" },
          { type: "turn/aborted", sessionId: "s1" },
        ])
      )
    ).toBe("interrupted");
    expect(
      derivePreviousTurnStatus(
        conv([
          { type: "user/message", sessionId: "s1", content: "q" },
          { type: "turn/end", sessionId: "s1", reason: "app-shutdown" },
        ])
      )
    ).toBe("app-shutdown");
  });

  it("识别截断、停滞与正常完成", () => {
    expect(
      derivePreviousTurnStatus(
        conv([
          { type: "user/message", sessionId: "s1", content: "q" },
          { type: "turn/truncated", sessionId: "s1" },
        ])
      )
    ).toBe("max-tokens");
    expect(
      derivePreviousTurnStatus(
        conv([
          { type: "user/message", sessionId: "s1", content: "q" },
          { type: "turn/stagnated", sessionId: "s1" },
        ])
      )
    ).toBe("stagnation");
    expect(
      derivePreviousTurnStatus(
        conv([
          { type: "user/message", sessionId: "s1", content: "q" },
          { type: "turn/end", sessionId: "s1", reason: "completed" },
        ])
      )
    ).toBeUndefined();
  });

  it("没有历史轮次时不注入状态", () => {
    expect(derivePreviousTurnStatus(undefined)).toBeUndefined();
    expect(
      derivePreviousTurnStatus({ id: "c1", title: "t", turns: [], createdAt: 0 })
    ).toBeUndefined();
  });
});
