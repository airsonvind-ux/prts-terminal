// src/projection.compaction.test.ts
// 上下文压缩投影纯函数（模块 D）：估算 / 保留段截断 / 摘要消息 / 窗口检查
//
// 覆盖：
//   1. estimateTextTokens（0.3 ascii + 0.6 cjk，跳过空白——与后端同构）
//   2. estimateTurnTokens / computeRetainedFrom（轮边界对齐、不拆轮）
//   3. buildSummaryUserMessage（定界块 + 非用户输入声明 + checkpoint 正文）
//   4. deriveActiveHistory（压缩后 = 摘要消息 + 保留段；未压缩 = 全量派生）
//   5. willNextTurnOverflow / outputReserveTokens（纯比例硬拦）

import { describe, it, expect } from "vitest";
import {
  SUMMARY_BLOCK_START,
  SUMMARY_BLOCK_END,
  RETAIN_BUDGET_TOKENS,
  computeRetainedFrom,
  estimateTextTokens,
  estimateTurnTokens,
  deriveActiveHistory,
  deriveMessages,
  buildSummaryUserMessage,
  willNextTurnOverflow,
  outputReserveTokens,
} from "./projection";
import type { CompactionMeta, SessionEvent } from "./types";

// ── 轮次构造辅助 ──

function userTurn(sessionId: string, content: string): SessionEvent[] {
  return [{ type: "user/message", sessionId, content }];
}

function assistantTurn(
  sessionId: string,
  content: string,
  usage?: { inputTokens: number; outputTokens: number }
): SessionEvent[] {
  return [
    { type: "assistant/chunk", sessionId, kind: "text", content },
    {
      type: "assistant/message",
      sessionId,
      content,
      reasoningContent: null,
      usage: usage ?? null,
    },
  ];
}

function toolTurn(sessionId: string): SessionEvent[] {
  return [
    {
      type: "tool/call",
      sessionId,
      callId: "c1",
      toolName: "query_enemy",
      params: '{"names":["源石虫"]}',
    },
    {
      type: "tool/result",
      sessionId,
      callId: "c1",
      toolName: "query_enemy",
      summary: "源石虫 DEF=0",
      ok: true,
    },
  ];
}

// ═══════════════════════════════════════════════════════
// estimateTextTokens / estimateTurnTokens
// ═══════════════════════════════════════════════════════

describe("estimateTextTokens", () => {
  it("空/纯空白返回 1（对齐后端 estimate_tokens 兜底）", () => {
    expect(estimateTextTokens("")).toBe(1);
    expect(estimateTextTokens("   \n\t")).toBe(1);
  });

  it("纯 ASCII ≈ 0.3/字符", () => {
    expect(estimateTextTokens("HelloWorld")).toBe(3); // 10 × 0.3 = 3
  });

  it("纯中文 ≈ 0.6/字符", () => {
    expect(estimateTextTokens("你好世界")).toBe(3); // 4 × 0.6 → ceil 3
  });

  it("空白不计入", () => {
    expect(estimateTextTokens("a b c")).toBe(1); // 3 × 0.3 → ceil 1
  });
});

describe("estimateTurnTokens", () => {
  it("只统计派生文本字段（结构事件不计）", () => {
    const events = [
      ...userTurn("s1", "甲".repeat(10)), // 6 tok
      ...toolTurn("s1"), // params + summary + toolName
    ];
    const total = estimateTurnTokens(events);
    expect(total).toBeGreaterThan(6);
    // tool/call: toolName(query_enemy 11 ascii → ceil 3.3→4) + params(≈?) > 0
    expect(total).toBeGreaterThan(10);
  });
});

// ═══════════════════════════════════════════════════════
// computeRetainedFrom —— 保留段起点（轮边界）
// ═══════════════════════════════════════════════════════

describe("computeRetainedFrom", () => {
  it("全部轮 ≤ 预算 → 0（无可折叠 nothing）", () => {
    const turns = [
      userTurn("s1", "你好"),
      assistantTurn("s1", "你好博士"),
    ];
    expect(computeRetainedFrom(turns)).toBe(0);
    expect(computeRetainedFrom([], RETAIN_BUDGET_TOKENS)).toBe(0);
  });

  it("超预算：保留最近且 ≤ budget 的轮，切点对齐轮边界", () => {
    // 每轮 ≈ 60 tok（100 中文 ×0.6）→ 400 轮 = 24K tok > 20K budget
    const turns: SessionEvent[][] = [];
    for (let i = 0; i < 400; i++) {
      turns.push(userTurn("s1", `问题${i} `.repeat(30))); // ~90 chars
    }
    // 400 轮 × ~54tok ≈ 21.6K；应保留尾部 ≈ 20K/54 ≈ 370 轮 → retainedFrom ≈ 30
    const from = computeRetainedFrom(turns);
    expect(from).toBeGreaterThan(0);
    expect(from).toBeLessThan(400);
    // 保留段总估 ≤ budget（切点语义）
    let kept = 0;
    for (let i = from; i < turns.length; i++) kept += estimateTurnTokens(turns[i]);
    expect(kept).toBeLessThanOrEqual(RETAIN_BUDGET_TOKENS);
  });

  it("单轮即超预算：起点 = 该轮下标（其余全折叠）", () => {
    const huge = "字".repeat(40_000); // 24K tok > 20K
    const turns = [userTurn("s1", huge), userTurn("s1", "短问题")];
    // 尾部短轮 ≤ budget → 保留它；头轮被挤出
    expect(computeRetainedFrom(turns)).toBe(1);
  });
});

// ═══════════════════════════════════════════════════════
// buildSummaryUserMessage / deriveActiveHistory
// ═══════════════════════════════════════════════════════

const compaction: CompactionMeta = {
  retainedFrom: 1,
  summary: "## Goal\n推演 1-7 过关配置",
  tokensBefore: 1234,
  at: 456,
};

describe("buildSummaryUserMessage", () => {
  it("user 形态 + 定界块 + 非用户输入声明 + checkpoint 正文", () => {
    const msg = buildSummaryUserMessage(compaction);
    expect(msg.role).toBe("user");
    expect(msg.content).toContain(SUMMARY_BLOCK_START);
    expect(msg.content).toContain(SUMMARY_BLOCK_END);
    expect(msg.content).toContain("非用户输入");
    expect(msg.content).toContain("需要时可重新查询");
    expect(msg.content).toContain(compaction.summary);
    // 摘要正文位置在声明之后
    const decl = msg.content.indexOf("非用户输入");
    const body = msg.content.indexOf("## Goal");
    expect(decl).toBeGreaterThan(-1);
    expect(body).toBeGreaterThan(decl);
  });
});

describe("deriveActiveHistory", () => {
  const turns = [
    userTurn("s1", "较早问题"),
    assistantTurn("s1", "较早回答"),
    userTurn("s1", "近期问题"),
    assistantTurn("s1", "近期回答"),
  ];

  it("无 compaction → 与 deriveMessages 全量输出一致（回归锚点）", () => {
    expect(deriveActiveHistory(turns, undefined)).toEqual(deriveMessages(turns));
  });

  it("压缩后 = 摘要消息（首条）+ 保留段派生；旧轮不再出现", () => {
    const cut = { ...compaction, retainedFrom: 2 }; // 保留 [user近期, assistant近期]
    const msgs = deriveActiveHistory(turns, cut);
    expect(msgs.length).toBe(3); // 摘要 + 保留 2 轮派生
    expect(msgs[0].role).toBe("user");
    expect(msgs[0].content).toContain(SUMMARY_BLOCK_START);
    const joined = msgs.map((m) => m.content).join("|");
    expect(joined).not.toContain("较早问题"); // 折叠轮已离开派生
    expect(joined).not.toContain("较早回答");
    expect(joined).toContain("近期问题");
    expect(joined).toContain("近期回答");
  });

  it("compaction 存在但 retainedFrom=0（异常/边界）→ 退化为全量", () => {
    const zero = { ...compaction, retainedFrom: 0 };
    expect(deriveActiveHistory(turns, zero)).toEqual(deriveMessages(turns));
  });
});

// ═══════════════════════════════════════════════════════
// willNextTurnOverflow / outputReserveTokens
// ═══════════════════════════════════════════════════════

describe("上下文窗口检查", () => {
  it("output 预留 = max(4096, 5% 窗口)", () => {
    expect(outputReserveTokens(64_000)).toBe(4096);
    expect(outputReserveTokens(128_000)).toBe(6400);
    expect(outputReserveTokens(200_000)).toBe(10_000);
  });

  it("projected ≥ 0.9×窗口 → 拦；window<=0 → 不拦", () => {
    const windowSize = 64_000; // hard = 57600
    // used 50000 + reserve 4096 = 54096，再加空文本 1 → 54097 < 57600 不拦
    expect(willNextTurnOverflow(50_000, "", windowSize)).toBe(false);
    // used 54000 + 4096 + 1 = 58097 ≥ 57600 拦
    expect(willNextTurnOverflow(54_000, "", windowSize)).toBe(true);
    // 文本增量计入
    expect(willNextTurnOverflow(53_000, "甲".repeat(10_000), windowSize)).toBe(true);
    // 未知窗口
    expect(willNextTurnOverflow(10_000, "x", 0)).toBe(false);
  });
});
