// @vitest-environment jsdom
// hasPendingApproval 纯函数测试：设置入口收窄的判据。
//
// 口径：任一处理中快照里存在未配对的 tool/approval-request（有 request 无 result）
// 即视为有待审批。与 EventBadge 的配对规则同源（callId 精确配对）。

import { describe, it, expect } from "vitest";
import { hasPendingApproval } from "../useProcessingStore";

type Ev = { type: string; callId?: string };

function stateWith(events: Ev[]) {
  return {
    processing: {
      "sess-1": { currentEvents: events } as never,
    },
  } as never;
}

describe("hasPendingApproval", () => {
  it("无处理中快照 → false", () => {
    expect(hasPendingApproval({ processing: {} } as never)).toBe(false);
  });

  it("有 request 无 result → true（待审批）", () => {
    expect(
      hasPendingApproval(
        stateWith([
          { type: "user/message" },
          { type: "tool/call", callId: "c1" },
          { type: "tool/approval-request", callId: "c1" },
        ])
      )
    ).toBe(true);
  });

  it("request 已配对 result → false", () => {
    expect(
      hasPendingApproval(
        stateWith([
          { type: "tool/approval-request", callId: "c1" },
          { type: "tool/approval-result", callId: "c1" },
        ])
      )
    ).toBe(false);
  });

  it("别的 callId 的 result 不算配对 → true", () => {
    expect(
      hasPendingApproval(
        stateWith([
          { type: "tool/approval-request", callId: "c1" },
          { type: "tool/approval-result", callId: "c2" },
        ])
      )
    ).toBe(true);
  });
});
