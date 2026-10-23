// src/projection.test.ts
// 投影派生纯函数测试（架构升级 · 模块 1：前端 = log 投影缓存）
//
// 覆盖：
//   1. deriveMessages：轮次事件 → LLM 历史（镜像后端 derive_messages）
//   2. deriveAssistantFromEvents：归档轮（assistant/message）与流式中（chunks）
//   3. deriveTextSegments：分段回溯弱化（归档轮单段 / 流式中按 tool/call 分段）
//   4. 错误轮/空轮次等边界

import { describe, it, expect } from "vitest";
import {
  deriveMessages,
  deriveActiveHistory,
  deriveAssistantFromEvents,
  deriveTextSegments,
  splitTurnByUserBoundary,
  isMetaEvent,
  isErrorOnlyTurn,
  stripLeadingErrorTurns,
  firstUserContent,
  stripReminderBlock,
  buildReminderBlock,
  type ReminderLine,
} from "./projection";
import type { SessionEvent } from "./types";

// ── 测试数据构造辅助 ──

function user(sessionId: string, content: string): SessionEvent {
  return { type: "user/message", sessionId, content };
}

function textChunk(sessionId: string, content: string): SessionEvent {
  return { type: "assistant/chunk", sessionId, kind: "text", content };
}

function assistantReset(sessionId: string): SessionEvent {
  return { type: "assistant/reset", sessionId };
}

function reasoningChunk(sessionId: string, content: string): SessionEvent {
  return { type: "assistant/chunk", sessionId, kind: "reasoning", content };
}

function toolCall(
  sessionId: string,
  callId: string,
  toolName: string,
  params: string
): SessionEvent {
  return { type: "tool/call", sessionId, callId, toolName, params };
}

function toolResult(
  sessionId: string,
  callId: string,
  summary: string
): SessionEvent {
  return { type: "tool/result", sessionId, callId, toolName: "echo", summary, ok: true };
}

function assistantMessage(
  sessionId: string,
  content: string,
  reasoningContent: string | null
): SessionEvent {
  return {
    type: "assistant/message",
    sessionId,
    content,
    reasoningContent,
    usage: null,
  };
}

function turnEnd(sessionId: string): SessionEvent {
  return { type: "turn/end", sessionId, reason: "completed" };
}

// ═══════════════════════════════════════════════
// deriveMessages
// ═══════════════════════════════════════════════

describe("deriveMessages", () => {
  it("简单轮次：user + assistant", () => {
    const turns: SessionEvent[][] = [
      [user("s1", "你好"), textChunk("s1", "你好博士"), assistantMessage("s1", "你好博士", null), turnEnd("s1")],
    ];
    const msgs = deriveMessages(turns);
    expect(msgs).toHaveLength(2);
    expect(msgs[0]).toEqual({ role: "user", content: "你好" });
    expect(msgs[1]).toEqual({ role: "assistant", content: "你好博士" });
  });

  it("历史派生优先使用 rawContent：模型能看到自己写的 MOOD 标签", () => {
    const turns: SessionEvent[][] = [
      [
        user("s1", "你好"),
        {
          type: "assistant/chunk",
          sessionId: "s1",
          kind: "text",
          content: "\n\n晚上好",
          rawContent: "【mood:高兴】\n\n晚上好",
        },
      ],
    ];
    expect(deriveMessages(turns)[1].content).toBe("【mood:高兴】\n\n晚上好");
  });

  it("流式重试：reset 之前的半截内容不进入下一轮历史", () => {
    const turns: SessionEvent[][] = [
      [user("s1", "你好"), textChunk("s1", "半截回答"), assistantReset("s1"), textChunk("s1", "重试成功")],
    ];
    expect(deriveMessages(turns)).toEqual([
      { role: "user", content: "你好" },
      { role: "assistant", content: "重试成功" },
    ]);
  });

  it("多轮：user + assistant + user + assistant", () => {
    const turns: SessionEvent[][] = [
      [user("s1", "第一问"), textChunk("s1", "第一答"), turnEnd("s1")],
      [user("s1", "第二问"), textChunk("s1", "第二答"), turnEnd("s1")],
    ];
    const msgs = deriveMessages(turns);
    expect(msgs.map((m) => m.content)).toEqual([
      "第一问",
      "第一答",
      "第二问",
      "第二答",
    ]);
  });

  it("工具轮：assistant(推理+工具调用) + tool 消息合并", () => {
    const turns: SessionEvent[][] = [
      [
        user("s1", "查敌人"),
        reasoningChunk("s1", "需要工具"),
        toolCall("s1", "c1", "query_enemy", "{}"),
        toolCall("s1", "c2", "query_enemy", '{"x":2}'),
        toolResult("s1", "c1", "结果1"),
        toolResult("s1", "c2", "结果2"),
        textChunk("s1", "最终回答"),
        turnEnd("s1"),
      ],
    ];
    const msgs = deriveMessages(turns);
    expect(msgs).toHaveLength(5);
    // 一次 assistant 消息携带全部工具调用
    expect(msgs[1]).toMatchObject({
      role: "assistant",
      reasoningContent: "需要工具",
    });
    expect(msgs[1].toolCalls).toHaveLength(2);
    expect(msgs[1].toolCalls![0].id).toBe("c1");
    expect(msgs[1].toolCalls![1].id).toBe("c2");
    expect(msgs[2]).toEqual({ role: "tool", content: "结果1", toolCallId: "c1" });
    expect(msgs[3]).toEqual({ role: "tool", content: "结果2", toolCallId: "c2" });
    expect(msgs[4]).toEqual({ role: "assistant", content: "最终回答" });
  });

  it("assistant/message 不参与派生（与 chunk 同源，避免双计数）", () => {
    const turns: SessionEvent[][] = [
      [
        user("s1", "你好"),
        textChunk("s1", "你好博士"),
        assistantMessage("s1", "你好博士", "推理"),
        turnEnd("s1"),
      ],
    ];
    const msgs = deriveMessages(turns);
    // 只有一条 assistant（chunk 累积），不因 assistant/message 重复
    expect(msgs.filter((m) => m.role === "assistant")).toHaveLength(1);
    expect(msgs[1].content).toBe("你好博士");
  });

  it("error 轮：错误事件被过滤，user 保留", () => {
    const turns: SessionEvent[][] = [
      [
        user("s1", "查询 X"),
        { type: "error", sessionId: "s1", message: "查询失败", recoverable: false },
      ],
    ];
    const msgs = deriveMessages(turns);
    // user 保留（旧行为一致），error 不进入 LLM 上下文
    expect(msgs).toHaveLength(1);
    expect(msgs[0]).toEqual({ role: "user", content: "查询 X" });
  });

  it("空 assistant 跳过（无内容无工具）", () => {
    const turns: SessionEvent[][] = [
      [user("s1", "你好"), turnEnd("s1")],
    ];
    const msgs = deriveMessages(turns);
    expect(msgs).toHaveLength(1);
  });
});

// ═══════════════════════════════════════════════
// deriveAssistantFromEvents
// ═══════════════════════════════════════════════

describe("deriveAssistantFromEvents", () => {
  it("归档轮：优先取 assistant/message 聚合", () => {
    const events: SessionEvent[] = [
      user("s1", "你好"),
      textChunk("s1", "逐"),
      textChunk("s1", "段输出"),
      assistantMessage("s1", "逐段输出", "推理内容"),
      turnEnd("s1"),
    ];
    const proj = deriveAssistantFromEvents(events)!;
    expect(proj.content).toBe("逐段输出");
    expect(proj.reasoningContent).toBe("推理内容");
  });

  it("流式中：chunks 累积（text 拼接）", () => {
    const events: SessionEvent[] = [
      user("s1", "你好"),
      textChunk("s1", "第一段"),
      textChunk("s1", "第二段"),
    ];
    const proj = deriveAssistantFromEvents(events)!;
    expect(proj.content).toBe("第一段第二段");
    expect(proj.reasoningContent).toBe("");
  });

  it("空回答返回 null", () => {
    const events: SessionEvent[] = [user("s1", "你好"), turnEnd("s1")];
    expect(deriveAssistantFromEvents(events)).toBeNull();
  });

  it("流式重试：只取最后一次 reset 之后的内容", () => {
    const events: SessionEvent[] = [
      user("s1", "你好"),
      textChunk("s1", "第一段半截"),
      assistantReset("s1"),
      textChunk("s1", "第二段半截"),
      assistantReset("s1"),
      textChunk("s1", "最终回答"),
    ];
    const proj = deriveAssistantFromEvents(events)!;
    expect(proj.content).toBe("最终回答");
    expect(proj.reasoningContent).toBe("");
  });

  it("推理-only 轮：content 空但存在（reasoning 由 EventBadge 展示）", () => {
    const events: SessionEvent[] = [
      user("s1", "分析"),
      reasoningChunk("s1", "思考中"),
      toolCall("s1", "c1", "echo", "{}"),
      toolResult("s1", "c1", "结果"),
    ];
    const proj = deriveAssistantFromEvents(events)!;
    expect(proj.content).toBe("");
    expect(proj.reasoningContent).toBe("思考中");
  });
});

// ═══════════════════════════════════════════════
// deriveTextSegments
// ═══════════════════════════════════════════════

describe("deriveTextSegments", () => {
  it("归档轮：assistant/message → 单段 transition=false（过渡文本被最终文本替换）", () => {
    const events: SessionEvent[] = [
      user("s1", "查敌人"),
      textChunk("s1", "我来查一下"),
      toolCall("s1", "c1", "query_enemy", "{}"),
      toolResult("s1", "c1", "结果"),
      textChunk("s1", "最终回答"),
      assistantMessage("s1", "最终回答", null),
      turnEnd("s1"),
    ];
    expect(deriveTextSegments(events)).toEqual([
      { content: "最终回答", transition: false },
    ]);
  });

  it("归档轮：assistant/message 内容为空 → null", () => {
    const events: SessionEvent[] = [
      user("s1", "你好"),
      assistantMessage("s1", "", null),
      turnEnd("s1"),
    ];
    expect(deriveTextSegments(events)).toBeNull();
  });

  it("流式：工具前 text + tool/call → 段1 transition=true；工具后 text → 段2 transition=false", () => {
    const events: SessionEvent[] = [
      user("s1", "查敌人"),
      textChunk("s1", "我来查一下"),
      toolCall("s1", "c1", "query_enemy", "{}"),
      toolResult("s1", "c1", "结果"),
      textChunk("s1", "敌人是"),
      textChunk("s1", "整合运动"),
    ];
    expect(deriveTextSegments(events)).toEqual([
      { content: "我来查一下", transition: true },
      { content: "敌人是整合运动", transition: false },
    ]);
  });

  it("流式重试：reset 之前的半截文本被丢弃", () => {
    const events: SessionEvent[] = [
      user("s1", "你好"),
      textChunk("s1", "半截回答"),
      assistantReset("s1"),
      textChunk("s1", "重试成功"),
    ];
    expect(deriveTextSegments(events)).toEqual([
      { content: "重试成功", transition: false },
    ]);
  });

  it("流式：无工具 text → 单段 transition=false", () => {
    const events: SessionEvent[] = [
      user("s1", "你好"),
      textChunk("s1", "第一"),
      textChunk("s1", "段"),
    ];
    expect(deriveTextSegments(events)).toEqual([
      { content: "第一段", transition: false },
    ]);
  });

  it("流式多工具：text → tool/call → text → tool/call → text → 三段 transition 依次 true/true/false", () => {
    const events: SessionEvent[] = [
      user("s1", "综合查询"),
      textChunk("s1", "先查敌人"),
      toolCall("s1", "c1", "query_enemy", "{}"),
      toolResult("s1", "c1", "r1"),
      textChunk("s1", "再查干员"),
      toolCall("s1", "c2", "query_operator", "{}"),
      toolResult("s1", "c2", "r2"),
      textChunk("s1", "综上"),
    ];
    expect(deriveTextSegments(events)).toEqual([
      { content: "先查敌人", transition: true },
      { content: "再查干员", transition: true },
      { content: "综上", transition: false },
    ]);
  });

  it("空事件 / 无文本内容（仅工具） → null", () => {
    expect(deriveTextSegments([])).toBeNull();
    const onlyTools: SessionEvent[] = [
      user("s1", "查"),
      toolCall("s1", "c1", "echo", "{}"),
      toolResult("s1", "c1", "r"),
    ];
    expect(deriveTextSegments(onlyTools)).toBeNull();
  });

  it("连续 tool/call 不产生空段；推理 chunk 不参与分段", () => {
    const events: SessionEvent[] = [
      user("s1", "查"),
      reasoningChunk("s1", "思考中"),
      textChunk("s1", "先"),
      toolCall("s1", "c1", "echo", "{}"),
      toolCall("s1", "c2", "echo", "{}"),
      toolResult("s1", "c1", "r1"),
      toolResult("s1", "c2", "r2"),
      textChunk("s1", "后"),
    ];
    expect(deriveTextSegments(events)).toEqual([
      { content: "先", transition: true },
      { content: "后", transition: false },
    ]);
  });

  it("text 后无尾段：末段为过渡段（光标位于 .streaming 内需 .streaming .cursor 兜底的场景）", () => {
    const events: SessionEvent[] = [
      user("s1", "查"),
      textChunk("s1", "我来"),
      toolCall("s1", "c1", "echo", "{}"),
      toolResult("s1", "c1", "r"),
    ];
    expect(deriveTextSegments(events)).toEqual([
      { content: "我来", transition: true },
    ]);
  });
});

// ═══════════════════════════════════════════════
// isMetaEvent / firstUserContent
// ═══════════════════════════════════════════════

describe("isMetaEvent / firstUserContent", () => {
  it("isMetaEvent：推理/工具事件为元事件，结构事件不是", () => {
    expect(isMetaEvent(reasoningChunk("s1", "想"))).toBe(true);
    expect(isMetaEvent(toolCall("s1", "c1", "echo", "{}"))).toBe(true);
    expect(isMetaEvent(toolResult("s1", "c1", "r"))).toBe(true);
    expect(isMetaEvent(user("s1", "你好"))).toBe(false);
    expect(isMetaEvent(turnEnd("s1"))).toBe(false);
    expect(isMetaEvent({ type: "step/start", sessionId: "s1", step: 1, estimatedTokens: 0 })).toBe(false);
  });

  it("firstUserContent：取第一条 user 消息", () => {
    const turns: SessionEvent[][] = [
      [user("s1", "第一条"), turnEnd("s1")],
      [user("s1", "第二条"), turnEnd("s1")],
    ];
    expect(firstUserContent(turns)).toBe("第一条");
    expect(firstUserContent([])).toBeNull();
  });
});

// ═══════════════════════════════════════════════
// stripReminderBlock（M1：防御性剥离）
// ═══════════════════════════════════════════════

describe("stripReminderBlock", () => {
  // 后端提醒数据（notices）经前端组块输出：
  //   [prts_reminder]\n- 标题：详情\n[/prts_reminder]
  // 前置场景（orchestrator process）：块 + "\n\n" + 用户原文
  const single = "[prts_reminder]\n- 工作空间：/ws\n[/prts_reminder]";

  it("块在消息开头（前置场景）：剥离后剩用户原文，不留空行", () => {
    const content = single + "\n\n你好，查一下源石虫";
    expect(stripReminderBlock(content)).toBe("你好，查一下源石虫");
  });

  it("多条目块（内部任意内容）整块剥离", () => {
    const block =
      "[prts_reminder]\n" +
      "- 工作空间：C:\\ws\n" +
      "- 知识库：C:\\kb（多行？不——条目按行）\n" +
      "- 模型：deepseek/deepseek-chat\n" +
      "[/prts_reminder]";
    expect(stripReminderBlock(block + "\n\n继续上次的分析")).toBe(
      "继续上次的分析"
    );
  });

  it("未闭合块（有开始无结束）：fail closed 剥到文本末尾", () => {
    expect(stripReminderBlock("[prts_reminder]\n- 工作空间：/ws")).toBe("");
    // 前文保留，块部分（未闭合）剥掉
    expect(stripReminderBlock("历史内容\n[prts_reminder]\n- 半截条目")).toBe(
      "历史内容\n"
    );
  });

  it("无块：原样返回（文本不变）", () => {
    const plain = "你好，源石虫的血量是多少？";
    expect(stripReminderBlock(plain)).toBe(plain);
    expect(stripReminderBlock("")).toBe("");
  });

  it("块在中间/末尾位置也能剥离", () => {
    // 中间（防御：未来若块不在前置位置）
    const mid =
      "前文\n" + single + "\n\n后文";
    expect(stripReminderBlock(mid)).toBe("前文\n后文");
    // 末尾（块后无内容）
    expect(stripReminderBlock("前文\n" + single)).toBe("前文\n");
  });

  it("块后紧跟 \r\n 换行也能剥净", () => {
    expect(stripReminderBlock(single + "\r\n\r\n原文")).toBe("原文");
  });

  it("无块文本不受影响（不含完整开始标记 → 原样）", () => {
    // 含类似字样的普通文本但无完整 [prts_reminder] 开始标记 → 原样
    const t = "参考 prts_reminder 规则（无方括号标记）";
    expect(stripReminderBlock(t)).toBe(t);
  });
});

// ═══════════════════════════════════════════════
// deriveMessages 保留块（M1 修复版：含块 user 消息是冻结历史的一部分）
// ═══════════════════════════════════════════════

describe("deriveMessages 保留 reminder 块", () => {
  it("user/message 含 reminder 块 → 派生历史保留块（与归档/后端 log 一致）", () => {
    const block =
      "[prts_reminder]\n- 工具目录：新增：write_file\n[/prts_reminder]";
    const turns: SessionEvent[][] = [
      [
        user("s1", block + "\n\n帮我算伤害"),
        textChunk("s1", "好的"),
        turnEnd("s1"),
      ],
    ];
    const msgs = deriveMessages(turns);
    // 块必须保留：priorMessages 发给后端播种，LLM 历史 = 前端归档（缓存一致性）
    expect(msgs[0].content).toBe(block + "\n\n帮我算伤害");
    expect(msgs[0].content).toContain("prts_reminder");
  });

  it("无块 user/message 派生不受影响", () => {
    const turns: SessionEvent[][] = [[user("s1", "你好"), turnEnd("s1")]];
    expect(deriveMessages(turns)[0].content).toBe("你好");
  });
});

// ═══════════════════════════════════════════════
// 缓存前缀不变量（压缩摘要 run 重放的前提）
// ═══════════════════════════════════════════════
//
// 摘要 run 的折叠区载荷 = deriveMessages(轮前缀)，与主历史 deriveMessages(全量)
// 共享同一构造。一旦派生引入跨轮状态（去重/省略等），该不变量破裂会静默击穿
// 缓存前缀同源——本测试把它钉住。

describe("deriveMessages 逐轮纯函数（切片 = 前缀）", () => {
  it("任意前缀切片派生 = 全量派生的前缀（折叠区载荷与主历史逐字节同源）", () => {
    const turns: SessionEvent[][] = [
      [user("s1", "第一问"), textChunk("s1", "第一答"), turnEnd("s1")],
      [
        user("s1", "第二问"),
        toolCall("s1", "c1", "echo", "{}"),
        toolResult("s1", "c1", "结果"),
        textChunk("s1", "第二答"),
        turnEnd("s1"),
      ],
      [user("s1", "第三问"), textChunk("s1", "第三答"), turnEnd("s1")],
    ];
    const full = deriveMessages(turns);
    for (let k = 1; k <= turns.length; k++) {
      const sliced = deriveMessages(turns.slice(0, k));
      expect(sliced).toEqual(full.slice(0, sliced.length));
    }
  });
});

// ═══════════════════════════════════════════════
// buildReminderBlock（提醒拼装侧）
// ═══════════════════════════════════════════════

/** 提醒条目行（维度行 / 事件行） */
function line(title: string, detail: string): ReminderLine {
  return { title, detail };
}

describe("buildReminderBlock", () => {
  it("组块：标记独占行 + 条目行，空列表返回空串", () => {
    expect(buildReminderBlock([])).toBe("");
    const block = buildReminderBlock([
      line("工作空间", "/ws"),
      line("模型", "deepseek/deepseek-chat"),
    ]);
    expect(block).toBe(
      "[prts_reminder]\n- 工作空间：/ws\n- 模型：deepseek/deepseek-chat\n[/prts_reminder]"
    );
  });

  it("多行 detail 续行缩进（事件里嵌的条目行 / 长期梗概 / 画像全文）", () => {
    const block = buildReminderBlock([
      line("记忆", "经历更新：新建 1 条、重写 0 条\n2026-09-16·伤害公式：确认相乘"),
      line("工作空间", "/ws"),
    ]);
    expect(block).toBe(
      "[prts_reminder]\n- 记忆：经历更新：新建 1 条、重写 0 条\n  2026-09-16·伤害公式：确认相乘\n- 工作空间：/ws\n[/prts_reminder]"
    );
    // 与 stripReminderBlock 仍可整块剥净（含多行）
    expect(stripReminderBlock(block + "\n\n正文")).toBe("正文");
  });

  it("与 stripReminderBlock 往返剥净（拼装 → 展示剥离）", () => {
    const block = buildReminderBlock([
      line("工作空间", "/ws"),
      line("模型", "deepseek/deepseek-chat"),
    ]);
    const content = block + "\n\n帮我算伤害";
    expect(stripReminderBlock(content)).toBe("帮我算伤害");
  });
});

describe("splitTurnByUserBoundary（插话轮内多段切分）", () => {
  it("单段轮（1 user + 1 assistant 内容）→ [user][assistant]", () => {
    const events = [
      user("s1", "原始问题"),
      textChunk("s1", "回答"),
      {
        type: "assistant/message" as const,
        sessionId: "s1",
        content: "回答",
        reasoningContent: null,
        usage: null,
      },
    ];
    const segs = splitTurnByUserBoundary(events);
    expect(segs).toEqual([
      { kind: "user", content: "原始问题" },
      { kind: "assistant", events: [textChunk("s1", "回答"), { type: "assistant/message", sessionId: "s1", content: "回答", reasoningContent: null, usage: null }] },
    ]);
  });

  it("多问答段：每条 user 起新段，assistant 内容按段闭合", () => {
    const events = [
      user("s1", "一问"),
      textChunk("s1", "答A"),
      user("s1", "插话一"),
      textChunk("s1", "答B"),
      user("s1", "插话二"),
      textChunk("s1", "答C"),
    ];
    const segs = splitTurnByUserBoundary(events);
    expect(segs.map((s) => s.kind)).toEqual(["user", "assistant", "user", "assistant", "user", "assistant"]);
    expect(segs).toMatchObject([
      { kind: "user", content: "一问" },
      { kind: "assistant" },
      { kind: "user", content: "插话一" },
      { kind: "assistant" },
      { kind: "user", content: "插话二" },
      { kind: "assistant" },
    ]);
    const assistants = segs.filter((s) => s.kind === "assistant") as {
      kind: "assistant";
      events: SessionEvent[];
    }[];
    expect(assistants.map((a) => a.events.map((e) => e.type))).toEqual([
      ["assistant/chunk"],
      ["assistant/chunk"],
      ["assistant/chunk"],
    ]);
  });

  it("轮级事件（error/turn/aborted/turn-end）归属最后 assistant 段，不截断 user 切分", () => {
    const events: SessionEvent[] = [
      user("s1", "问题"),
      textChunk("s1", "半截"),
      { type: "turn/aborted", sessionId: "s1" },
      { type: "turn/end", sessionId: "s1", reason: "aborted" as const },
    ];
    const segs = splitTurnByUserBoundary(events);
    expect(segs).toHaveLength(2);
    expect(segs[0]).toEqual({ kind: "user", content: "问题" });
    expect((segs[1] as { events: SessionEvent[] }).events.map((e) => e.type)).toEqual([
      "assistant/chunk",
      "turn/aborted",
      "turn/end",
    ]);
  });

  it("空轮 / 无 user 轮（历史播种兜底）→ 全部归一个 assistant 段或空", () => {
    expect(splitTurnByUserBoundary([])).toEqual([]);
    const segs = splitTurnByUserBoundary([textChunk("s1", "孤儿内容")]);
    expect(segs).toEqual([
      { kind: "assistant", events: [textChunk("s1", "孤儿内容")] },
    ]);
  });
});

describe("deriveActiveHistory · 首部错误轮", () => {
  it("首部连续错误轮被排除，首次正常轮成为 LLM 历史首条", () => {
    const errorTurn: SessionEvent[] = [
      user("s1", "第一问"),
      { type: "error", sessionId: "s1", message: "HTTP 401", recoverable: false },
    ];
    const normalTurn: SessionEvent[] = [
      user("s2", "第二问"),
      textChunk("s2", "正常回答"),
      { type: "assistant/message", sessionId: "s2", content: "正常回答", reasoningContent: null, usage: null },
    ];
    expect(isErrorOnlyTurn(errorTurn)).toBe(true);
    expect(isErrorOnlyTurn(normalTurn)).toBe(false);
    const history = deriveActiveHistory([errorTurn, normalTurn]);
    expect(history.some((m) => m.content.includes("第一问"))).toBe(false);
    expect(history.some((m) => m.content === "第二问")).toBe(true);
  });

  it("后续错误轮不误删（只剥离开头连续段）", () => {
    const normal: SessionEvent[] = [
      user("s1", "正常问"),
      textChunk("s1", "正常答"),
      { type: "assistant/message", sessionId: "s1", content: "正常答", reasoningContent: null, usage: null },
    ];
    const laterError: SessionEvent[] = [
      user("s2", "后来问"),
      { type: "error", sessionId: "s2", message: "超时", recoverable: false },
    ];
    expect(stripLeadingErrorTurns([normal, laterError])).toHaveLength(2);
  });
});
