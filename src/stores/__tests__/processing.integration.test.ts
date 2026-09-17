// src/stores/__tests__/processing.integration.test.ts
// Store 集成测试：ProcessingStore ↔ ConversationStore ↔ UIStore 协作
//（架构升级 · 模块 1：投影缓存词汇表）
//
// 每个测试重置三个 Store 到初始状态。
// 不使用 useAgentEvents Hook（那是 React 层），直接调用 Store actions 模拟事件到达。
// invoke 由 setup.ts 全局 Mock，默认 resolve 为 undefined。
//
// 覆盖：
//   1. 新建对话发送消息（user/message 合成预置）
//   2. 同一对话并发保护
//   3. 正常流式完成（chunks → turn/end 归档）
//   4. 推理模型思维链（assistant/chunk(Reasoning)）
//   5. 工具调用完整流程（tool/call + tool/result）
//   6. 不可恢复错误（error 轮归档）
//   7. 切走对话后错误到达
//   8. processing 字典键类型验证（sessionId vs conversationId）
//   9. turn/end 归档携带 contextTokens（assistant/message usage 快照）
//  10. 停滞路径幂等：error 先到 failProcessing，随后 turn/end 到达不重复归档

import { describe, it, expect, beforeEach } from "vitest";
import { useProcessingStore, flushPending } from "../useProcessingStore";
import { useConversationStore } from "../useConversationStore";
import { useUIStore } from "../useUIStore";
import type { ReminderData, SessionEvent } from "../../types";

// ── 类型 ──

type ProcessingState = ReturnType<
  typeof useProcessingStore.getState
>["processing"][string];

// ── 辅助 ──

/** 重置三个 Store 到初始状态，并恢复 UIStore 的默认值 */
function resetAllStores(): void {
  useProcessingStore.setState({ processing: {} });
  useConversationStore.setState({ conversations: [], activeId: null });
  useUIStore.setState({
    hiddenMode: null,
    errorMsg: null,
    overlay: null,
  });
  // 清理跨测试累积的 invoke 调用记录，确保每个测试独立验证
  __invokeMock.mockClear();
}

/** 获取当前唯一的 processing 快照，断言正好一个 */
function getSingleSnapshot(): { sid: string; snap: ProcessingState } {
  const processing = useProcessingStore.getState().processing;
  const sids = Object.keys(processing);
  expect(sids).toHaveLength(1);
  return { sid: sids[0], snap: processing[sids[0]] };
}

/** 类型收窄辅助：先断言事件类型，再返回收窄后的引用 */
function asEvent<T extends SessionEvent["type"]>(
  e: SessionEvent,
  type: T
): SessionEvent & { type: T } {
  expect(e.type).toBe(type);
  return e as SessionEvent & { type: T };
}

beforeEach(resetAllStores);

// ═══════════════════════════════════════════════
// 场景 1：新建对话发送消息
// ═══════════════════════════════════════════════

describe("场景 1：新建对话发送消息", () => {
  it("创建 conversation + processing 快照（user/message 合成预置）+ invoke 参数正确", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("查询 1-7");

    // conversation 创建（turns 为空：user 消息在快照中，归档时随轮落库）
    const convs = useConversationStore.getState();
    expect(convs.conversations).toHaveLength(1);
    expect(convs.activeId).toBe(convs.conversations[0].id);
    expect(convs.conversations[0].turns).toHaveLength(0);
    expect(convs.conversations[0].title).toBe("查询 1-7");

    // processing 快照创建：currentEvents 预置合成 user/message
    const { sid, snap } = getSingleSnapshot();
    expect(snap.conversationId).toBe(convs.conversations[0].id);
    expect(snap.currentEvents).toHaveLength(1);
    expect(snap.currentEvents[0]).toEqual({
      type: "user/message",
      sessionId: sid,
      content: "查询 1-7",
    });
    expect(snap.agentPhase).toBe("processing");

    // invoke 调用参数正确
    // 命令序列：[get_reminder_data（拉提醒数据）, agent_process]
    // setup.ts 将 invoke mock 暴露在 globalThis.__invokeMock
    const invokeCalls = __invokeMock.mock.calls;
    expect(invokeCalls).toHaveLength(2);
    expect(invokeCalls[0][0]).toBe("get_reminder_data"); // 提醒拉取先于发送
    const agentInvoke = invokeCalls.find((c) => c[0] === "agent_process")!;
    const requestArg = agentInvoke[1].request;
    expect(requestArg.input).toBe("查询 1-7");
    expect(requestArg.sessionId).toBe(sid);
    // 模块 C：request 携带 conversationId（缓存区按对话分区，非 sessionId）
    expect(requestArg.conversationId).toBe(convs.conversations[0].id);
    expect(requestArg.history).toHaveLength(1);
    expect(requestArg.history[0]).toEqual({ role: "user", content: "查询 1-7" });
  });
});

// ═══════════════════════════════════════════════
// 场景 2：同一对话并发保护
// ═══════════════════════════════════════════════

describe("场景 2：同一对话并发保护", () => {
  it("处理中再次 sendMessage 被忽略", async () => {
    const store = useProcessingStore.getState();

    // 第一次发送
    await store.sendMessage("查询 1-7");
    const firstSid = getSingleSnapshot().sid;

    // 第二次发送：同一对话，processing 快照仍存在，应被忽略
    await store.sendMessage("这不应该被处理");

    // processing 快照只有一个，且是第一次的
    const { sid } = getSingleSnapshot();
    expect(sid).toBe(firstSid);

    // invoke：首次 sendMessage = [get_reminder_data, agent_process]；
    // 第二次被并发保护挡下（不新增调用）
    expect(__invokeMock.mock.calls).toHaveLength(2);

    // conversation 未归档任何轮次（user 消息仍在快照中）
    const conv = useConversationStore.getState().conversations[0];
    expect(conv.turns).toHaveLength(0);
  });
});

// ═══════════════════════════════════════════════
// 场景 3：正常流式完成
// ═══════════════════════════════════════════════

describe("场景 3：正常流式完成", () => {
  it("assistant/chunk 累积 → turn/end 归档整轮 → 快照清除", async () => {
    const store = useProcessingStore.getState();

    // 发送消息
    await store.sendMessage("查询 1-7");
    const { sid } = getSingleSnapshot();

    // 模拟后端推送流式文本（assistant/chunk(Text)）
    store.appendCurrentEvent({
      type: "assistant/chunk",
      sessionId: sid,
      kind: "text",
      content: "关卡 1-7",
    });
    store.appendCurrentEvent({
      type: "assistant/chunk",
      sessionId: sid,
      kind: "text",
      content: "是主线第一章，",
    });
    store.appendCurrentEvent({
      type: "assistant/chunk",
      sessionId: sid,
      kind: "text",
      content: "消耗 6 点理智。",
    });

    // 验证事件缓冲累积（chunk 走 33ms 节流缓冲，先显式 flush 使缓冲事件落位）
    flushPending(sid);
    const midSnap = useProcessingStore.getState().processing[sid];
    expect(midSnap.currentEvents).toHaveLength(4); // user/message + 3 chunks

    // 切换 phase 到 generating（模拟后端推送 phase 事件）
    store.onPhase({ sessionId: sid, phase: "generating" });
    expect(
      useProcessingStore.getState().processing[sid].agentPhase
    ).toBe("streaming");

    // 后端推聚合事件；随后 turn/end 到达 → useAgentEvents 分发为 finishProcessing
    store.appendCurrentEvent({
      type: "assistant/message",
      sessionId: sid,
      content: "关卡 1-7是主线第一章，消耗 6 点理智。",
      reasoningContent: null,
      usage: { inputTokens: 1200, outputTokens: 80 },
    });
    store.finishProcessing(sid);

    // processing 快照已清除
    expect(useProcessingStore.getState().processing).toEqual({});

    // 对话归档整轮：user/message + chunks + assistant/message
    // （turn/end 是归档触发信号，不进入事件缓冲——与旧 generation_done 一致）
    const conv = useConversationStore.getState().conversations[0];
    expect(conv.turns).toHaveLength(1);
    const turn = conv.turns[0];
    expect(asEvent(turn[0], "user/message").content).toBe("查询 1-7");
    expect(turn.filter((e) => e.type === "assistant/chunk")).toHaveLength(3);
    expect(asEvent(turn[turn.length - 1], "assistant/message").type).toBe("assistant/message");

    // contextTokens：assistant/message 的 usage 快照（覆盖式）
    expect(conv.contextTokens).toEqual({ inputTokens: 1200, outputTokens: 80 });
  });
});

// ═══════════════════════════════════════════════
// 场景 4：推理模型思维链
// ═══════════════════════════════════════════════

describe("场景 4：推理模型思维链", () => {
  it("assistant/chunk(Reasoning) → turn/end → 整轮归档", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("分析 1-7 的敌人配置");
    const { sid } = getSingleSnapshot();

    // 后端推送 reasoning 模式
    store.onPhase({ sessionId: sid, phase: "reasoning" });

    // 推理链增量（assistant/chunk(Reasoning)）
    store.appendCurrentEvent({
      type: "assistant/chunk",
      sessionId: sid,
      kind: "reasoning",
      content: "用户想了解 1-7 的敌人配置，我需要先查询该关卡的敌人信息。",
    });
    store.appendCurrentEvent({
      type: "assistant/chunk",
      sessionId: sid,
      kind: "reasoning",
      content: "该关卡包含源石虫和士兵，需要考虑低星干员的部署策略。",
    });

    // 验证事件缓冲记录了推理链（chunk 走 33ms 节流缓冲，先显式 flush 落位）
    flushPending(sid);
    const snap = useProcessingStore.getState().processing[sid];
    expect(snap.currentEvents).toHaveLength(3); // user/message + 2 reasoning chunks
    expect(asEvent(snap.currentEvents[1], "assistant/chunk").kind).toBe("reasoning");
    expect(asEvent(snap.currentEvents[2], "assistant/chunk").kind).toBe("reasoning");

    // 完成处理（turn/end 到达 → finishProcessing），归档
    store.finishProcessing(sid);

    // turns 包含推理链（含合成 user/message）
    const conv = useConversationStore.getState().conversations[0];
    expect(conv.turns).toHaveLength(1);
    expect(conv.turns[0]).toHaveLength(3);
  });
});

// ═══════════════════════════════════════════════
// 场景 5：工具调用完整流程
// ═══════════════════════════════════════════════

describe("场景 5：工具调用完整流程", () => {
  it("executing → tool/call → tool/result → generating → 完整归档", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("查询 1-7");
    const { sid } = getSingleSnapshot();

    // 后端进入执行模式
    store.onPhase({ sessionId: sid, phase: "executing" });
    expect(
      useProcessingStore.getState().processing[sid].agentPhase
    ).toBe("processing");

    // 工具调用（callId 配对）
    store.appendCurrentEvent({
      type: "tool/call",
      sessionId: sid,
      callId: "call_1",
      toolName: "query_level",
      params: '{"stageCode":"1-7"}',
    });

    // 工具返回
    store.appendCurrentEvent({
      type: "tool/result",
      sessionId: sid,
      callId: "call_1",
      toolName: "query_level",
      summary: "主线 1-7：暴君，消耗 6 理智，敌人包含源石虫和士兵。",
      ok: true,
    });

    // 后端切到生成模式
    store.onPhase({ sessionId: sid, phase: "generating" });

    // 流式文本
    store.appendCurrentEvent({
      type: "assistant/chunk",
      sessionId: sid,
      kind: "text",
      content: "博士，1-7 的主要敌人是源石虫。",
    });

    // 完成（turn/end 到达 → finishProcessing）
    store.finishProcessing(sid);

    // 验证对话：整轮归档（user/message + tool/call + tool/result + chunk）
    const conv = useConversationStore.getState().conversations[0];
    expect(conv.turns).toHaveLength(1);
    const turn = conv.turns[0];
    const types = turn.map((e) => e.type);
    expect(types).toEqual([
      "user/message",
      "tool/call",
      "tool/result",
      "assistant/chunk",
    ]);
    expect(asEvent(turn[1], "tool/call").callId).toBe("call_1");
    expect(asEvent(turn[2], "tool/result").callId).toBe("call_1");
  });
});

// ═══════════════════════════════════════════════
// 场景 6：不可恢复错误
// ═══════════════════════════════════════════════

describe("场景 6：不可恢复错误", () => {
  it("error 事件 → failProcessing → 错误轮归档 + errorMsg 设置", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("查询不存在的关卡 XYZ-999");
    const { sid } = getSingleSnapshot();

    // 后端返回不可恢复错误（error 事件，recoverable=false）
    store.failProcessing(sid, "查询失败：数据库连接超时");

    // 快照已清除
    expect(useProcessingStore.getState().processing).toEqual({});

    // 错误轮归档进对话时间线：[user/message, error]
    const conv = useConversationStore.getState().conversations[0];
    expect(conv.turns).toHaveLength(1);
    expect(conv.turns[0]).toHaveLength(2);
    expect(conv.turns[0][0].type).toBe("user/message");
    expect(conv.turns[0][1]).toEqual({
      type: "error",
      sessionId: sid,
      message: "查询失败：数据库连接超时",
      recoverable: false,
    });

    // UIStore.errorMsg 被设置（因为是活跃对话的错误）
    expect(useUIStore.getState().errorMsg).toBe("查询失败：数据库连接超时");
  });
});

// ═══════════════════════════════════════════════
// 场景 7：切走对话后错误到达
// ═══════════════════════════════════════════════

describe("场景 7：切走对话后错误到达", () => {
  it("非活跃对话的 error：错误轮归档但不触发 errorMsg", async () => {
    const store = useProcessingStore.getState();

    // 在对话 A 中发送消息
    await store.sendMessage("查询 1-7");
    const convAId = useConversationStore.getState().activeId!;
    const { sid } = getSingleSnapshot();

    // 切换到新对话（B）
    useConversationStore.getState().newChat();
    expect(useConversationStore.getState().activeId).toBeNull();

    // 对话 A 的处理失败（切走后到达）
    store.failProcessing(sid, "查询失败");

    // 错误轮写入了对话 A（不是当前活跃对话）
    const convA = useConversationStore
      .getState()
      .conversations.find((c) => c.id === convAId)!;
    expect(convA.turns).toHaveLength(1);
    expect(convA.turns[0][1].type).toBe("error");

    // 但 UIStore.errorMsg 未被设置（不是当前活跃对话，不该弹错误）
    expect(useUIStore.getState().errorMsg).toBeNull();
  });
});

// ═══════════════════════════════════════════════
// 场景 8：processing 字典键类型验证（sessionId vs conversationId）
// ═══════════════════════════════════════════════

describe("场景 8：processing 字典键类型验证", () => {
  it("processing 的键是 sessionId，不是 conversationId", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("查询 1-7");

    const { sid, snap } = getSingleSnapshot();
    const convStore = useConversationStore.getState();
    const activeId = convStore.activeId!;

    // ── 断言 1：processing 字典的键和 activeId 是不同的字符串 ──
    // sessionId（字典键）≠ conversationId（activeId）
    expect(sid).not.toBe(activeId);

    // ── 断言 2：快照内部的 conversationId 等于 activeId ──
    expect(snap.conversationId).toBe(activeId);

    // ── 断言 3：直接按 activeId 索引返回 undefined ──
    // 这正是 useUIState / useAnimationMapping 中 selector 的行为
    const processing = useProcessingStore.getState().processing;
    expect(processing[activeId]).toBeUndefined();

    // ── 断言 4：遍历查找能正确找到快照 ──
    const bySearch = Object.values(processing).find(
      (p) => p.conversationId === activeId
    );
    expect(bySearch).toBeDefined();
    expect(bySearch!.agentPhase).toBe("processing");

    // ── 断言 5：onPhase 后，直接索引仍为 undefined，遍历查找正确 ──
    store.onPhase({ sessionId: sid, phase: "generating" });

    const processing2 = useProcessingStore.getState().processing;
    expect(processing2[activeId]).toBeUndefined();

    const bySearch2 = Object.values(processing2).find(
      (p) => p.conversationId === activeId
    );
    expect(bySearch2!.agentPhase).toBe("streaming");
  });
});

// ═══════════════════════════════════════════════
// 场景 9：多轮历史投影派生（sendMessage 的 history 构造）
// ═══════════════════════════════════════════════

describe("场景 9：多轮历史投影派生", () => {
  it("第二轮 history 从已归档 turns 派生（含工具轮推理链）", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("查询 1-7");
    const { sid } = getSingleSnapshot();

    // 模拟完整的第一轮：推理 → 工具 → 回答
    store.onPhase({ sessionId: sid, phase: "reasoning" });
    store.appendCurrentEvent({
      type: "assistant/chunk",
      sessionId: sid,
      kind: "reasoning",
      content: "需要查询关卡数据",
    });
    store.appendCurrentEvent({
      type: "tool/call",
      sessionId: sid,
      callId: "c1",
      toolName: "query_level",
      params: "{}",
    });
    store.appendCurrentEvent({
      type: "tool/result",
      sessionId: sid,
      callId: "c1",
      toolName: "query_level",
      summary: "主线 1-7",
      ok: true,
    });
    store.appendCurrentEvent({
      type: "assistant/chunk",
      sessionId: sid,
      kind: "text",
      content: "查到了。",
    });
    store.appendCurrentEvent({
      type: "assistant/message",
      sessionId: sid,
      content: "查到了。",
      reasoningContent: "需要查询关卡数据",
      usage: null,
    });
    store.finishProcessing(sid);

    // 第一轮归档完成
    expect(useProcessingStore.getState().processing).toEqual({});
    const convAId = useConversationStore.getState().activeId!;

    // 第二轮发送：切回同一对话，history 从已归档 turns 投影派生
    useConversationStore.getState().switchConversation(convAId);
    await store.sendMessage("谢谢");
    const invokeCalls = __invokeMock.mock.calls;
    const secondCall = invokeCalls[invokeCalls.length - 1];
    const history = secondCall[1].request.history;

    // user + assistant(推理+工具) + tool + assistant + user
    expect(history).toHaveLength(5);
    expect(history[0]).toEqual({ role: "user", content: "查询 1-7" });
    expect(history[1]).toMatchObject({
      role: "assistant",
      content: "",
      reasoningContent: "需要查询关卡数据",
    });
    expect(history[1].toolCalls).toHaveLength(1);
    expect(history[2]).toMatchObject({
      role: "tool",
      content: "主线 1-7",
      toolCallId: "c1",
    });
    expect(history[3]).toMatchObject({ role: "assistant", content: "查到了。" });
    expect(history[4]).toEqual({ role: "user", content: "谢谢" });
  });
});

// ═══════════════════════════════════════════════
// 场景 10：停滞路径幂等（error 先到，turn/end 后到）
// ═══════════════════════════════════════════════

describe("场景 10：停滞路径幂等", () => {
  it("failProcessing 后 turn/end 到达不重复归档", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("查询 1-7");
    const { sid } = getSingleSnapshot();

    // 停滞路径：error(recoverable=false) 先到 → 终止
    store.failProcessing(sid, "调度循环检测到工具调用停滞，已终止本轮处理");

    // 随后 turn/end(stagnation) 到达 → finishProcessing（快照已清理 → 幂等忽略）
    store.finishProcessing(sid);

    // 只归档了错误轮，没有重复轮次
    const conv = useConversationStore.getState().conversations[0];
    expect(conv.turns).toHaveLength(1);
    expect(conv.turns[0][1].type).toBe("error");
  });
});

// ═══════════════════════════════════════════════
// 场景 11：Reminder 数据装配（sendMessage 拉取 → 快照/增量 → 块前置）
// ═══════════════════════════════════════════════

describe("场景 11：Reminder 数据装配", () => {
  /** 环境值快照 payload（新对话用；含事件，验证快照不含事件行） */
  function snapshotPayload(seq = 4): ReminderData {
    return {
      seq,
      dims: [
        { key: "workspace", title: "工作空间", value: "/ws", note: "/ws", stamp: 0 },
        { key: "knowledge", title: "知识库", value: "/kb", note: "/kb", stamp: 0 },
        { key: "model", title: "模型", value: "m", note: "m", stamp: 3 },
        { key: "skill_catalog", title: "技能目录", value: "[\"a\"]", note: "", stamp: 0 },
      ],
      events: [
        { seq: 2, title: "记忆", detail: "旧流水1", ts: "1" },
        { seq: 4, title: "记忆", detail: "旧流水2", ts: "2" },
      ],
    };
  }

  /** 路由 mock：get_reminder_data 返回指定载荷，其余命令 resolve undefined */
  function routeReminder(data: ReminderData | null): void {
    __invokeMock.mockReset();
    __invokeMock.mockImplementation((cmd: string) => {
      if (data !== null && cmd === "get_reminder_data") {
        return Promise.resolve(data);
      }
      return Promise.resolve(undefined);
    });
  }

  it("新对话：仅快照三行（无事件行）、游标＝payload.seq、history 含块", async () => {
    routeReminder(snapshotPayload(4));
    const store = useProcessingStore.getState();
    await store.sendMessage("帮我算伤害");

    const { snap } = getSingleSnapshot();
    const userMsg = asEvent(snap.currentEvents[0], "user/message");
    const content = userMsg.content;
    expect(content.startsWith("[prts_reminder]\n")).toBe(true);
    expect(content).toContain("- 工作空间：/ws");
    expect(content).toContain("- 知识库：/kb");
    expect(content).toContain("- 模型：m");
    expect(content).not.toContain("技能目录"); // 快照仅三键
    expect(content).not.toContain("记忆"); // 无事件行
    expect(content.endsWith("帮我算伤害")).toBe(true);

    // 游标＝payload.seq
    const conv = useConversationStore.getState().conversations[0];
    expect(conv.lastNoticeSeq).toBe(4);

    // history 末条 user 与归档同字节（含块冻结）
    const agentInvoke = __invokeMock.mock.calls.find(
      (c) => c[0] === "agent_process"
    )!;
    expect(agentInvoke[1].request.history.at(-1).content).toBe(content);
  });

  it("进行中增量：仅超游标项（维度行在上、事件行在下），旧游标不下发旧条目", async () => {
    routeReminder(snapshotPayload(4));
    const store = useProcessingStore.getState();
    await store.sendMessage("第一问");
    const { sid } = getSingleSnapshot();
    store.finishProcessing(sid); // 归档 → 游标 4

    // 增量：workspace 变更（stamp 5）＋ 新事件（seq 6；含旧事件 seq2）
    routeReminder({
      seq: 6,
      dims: [
        { key: "workspace", title: "工作空间", value: "/ws2", note: "/ws2", stamp: 5 },
        { key: "model", title: "模型", value: "m", note: "m", stamp: 3 },
      ],
      events: [
        { seq: 2, title: "记忆", detail: "旧流水1", ts: "1" },
        { seq: 6, title: "记忆", detail: "新增量", ts: "3" },
      ],
    });
    await store.sendMessage("第二问");
    const snaps = Object.values(useProcessingStore.getState().processing);
    const content = asEvent(snaps[0].currentEvents[0], "user/message").content;
    expect(content).toContain("- 工作空间：/ws2"); // 维度 note 行
    expect(content).toContain("- 记忆：新增量"); // 事件行（seq 6 > 4）
    expect(content).not.toContain("旧流水1"); // 旧游标不下发
    expect(content).not.toContain("- 模型："); // stamp 3 不超游标 4
    // 维度行在事件行之前
    expect(content.indexOf("工作空间")).toBeLessThan(content.indexOf("记忆："));

    const conv = useConversationStore.getState().conversations[0];
    expect(conv.lastNoticeSeq).toBe(6);
  });

  it("空增量不出块", async () => {
    routeReminder(snapshotPayload(4));
    const store = useProcessingStore.getState();
    await store.sendMessage("第一问");
    const { sid } = getSingleSnapshot();
    store.finishProcessing(sid);

    routeReminder({ seq: 4, dims: [], events: [] });
    await store.sendMessage("第二问");
    const snaps = Object.values(useProcessingStore.getState().processing);
    expect(asEvent(snaps[0].currentEvents[0], "user/message").content).toBe("第二问");
  });

  it("invoke 失败：进行中回滚游标；新对话不回滚为 0", async () => {
    // ① 进行中对话：失败回滚到拼装前游标
    routeReminder(snapshotPayload(4));
    const store = useProcessingStore.getState();
    await store.sendMessage("第一问");
    const { sid } = getSingleSnapshot();
    store.finishProcessing(sid); // 游标 4

    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_reminder_data") {
        return Promise.resolve({
          seq: 5,
          dims: [{ key: "model", title: "模型", value: "m2", note: "m2", stamp: 5 }],
          events: [],
        });
      }
      if (cmd === "agent_process") return Promise.reject(new Error("后端不可达"));
      return Promise.resolve(undefined);
    });
    await store.sendMessage("第二问");
    const conv1 = useConversationStore.getState().conversations[0];
    expect(conv1.lastNoticeSeq).toBe(4); // 回滚到拼装前游标

    // ② 新对话：基线推进不回滚为 0（避免重放）
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_reminder_data") return Promise.resolve(snapshotPayload(4));
      if (cmd === "agent_process") return Promise.reject(new Error("后端不可达"));
      return Promise.resolve(undefined);
    });
    useConversationStore.getState().newChat();
    await store.sendMessage("触发失败");
    const convs = useConversationStore.getState().conversations;
    const fresh = convs[convs.length - 1];
    expect(fresh.turns[0][0].type).toBe("user/message");
    expect(
      asEvent(fresh.turns[0][0], "user/message").content
    ).toBe("触发失败");
    expect(
      asEvent(fresh.turns[0][0], "user/message").content
    ).not.toContain("prts_reminder");
    expect(fresh.lastNoticeSeq).toBe(4); // 新对话基线不回滚为 0
  });
});
