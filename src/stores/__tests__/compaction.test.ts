// src/stores/__tests__/compaction.test.ts
// 上下文压缩编排（模块 D）行为测试：runCompaction 全流程 + noop 预判 +
// 失败/取消 + cancelCompaction 中止通道 + applyCompaction 持久化 + sendMessage
// 的窗口硬拦与压缩中禁发。
//
// 架构约定（同 processing.integration）：直接调 store actions 模拟，invoke 走
// 全局 __invokeMock（setup.ts）。

import { describe, it, expect, beforeEach } from "vitest";
import { useCompactionStore, compactionSessionId } from "../useCompactionStore";
import { useConversationStore } from "../useConversationStore";
import { useProcessingStore, setModelInfo } from "../useProcessingStore";
import { useToastStore } from "../useToastStore";
import type { SessionEvent } from "../../types";

// ── 重置 ──

function resetAll(): void {
  useCompactionStore.setState({ compactingConversationId: null });
  useProcessingStore.setState({ processing: {} });
  useConversationStore.setState({
    conversations: [],
    activeId: null,
    pendingTitle: null,
  });
  useToastStore.setState({ toasts: [] });
  setModelInfo(null);
  __invokeMock.mockClear();
}

beforeEach(resetAll);

// ── 辅助 ──

function lastInvokeArg(cmd: string): Record<string, unknown> | undefined {
  const calls = __invokeMock.mock.calls;
  for (let i = calls.length - 1; i >= 0; i--) {
    if (calls[i][0] === cmd) return calls[i][1] as Record<string, unknown>;
  }
  return undefined;
}

function invokeCount(cmd: string): number {
  return __invokeMock.mock.calls.filter((c) => c[0] === cmd).length;
}

function toastMessages(): string[] {
  return useToastStore.getState().toasts.map((t) => t.message);
}

/** 构造会话：直接注入（绕开 appendTurn 的持久化噪音） */
function seedConversation(
  id: string,
  turns: SessionEvent[][],
  extra?: {
    contextTokens?: {
      inputTokens: number;
      contextInputTokens?: number;
      outputTokens: number;
    };
    lastNoticeSeq?: number;
  }
): void {
  const conv: Record<string, unknown> = {
    id,
    title: `会话 ${id}`,
    turns,
    createdAt: 1,
  };
  if (extra?.contextTokens) conv.contextTokens = extra.contextTokens;
  if (extra?.lastNoticeSeq !== undefined) conv.lastNoticeSeq = extra.lastNoticeSeq;
  useConversationStore.setState({
    conversations: [
      ...useConversationStore.getState().conversations,
      conv as never,
    ],
    activeId: id,
    pendingTitle: null,
  });
}

function userTurn(content: string): SessionEvent[] {
  return [{ type: "user/message", sessionId: "s1", content }];
}

/** 500 轮 ≈ 总估远超 20K 保留预算（每轮 320 中文字 ≈ 190 tok） */
function longConversation(): SessionEvent[][] {
  const turns: SessionEvent[][] = [];
  for (let i = 0; i < 500; i++) {
    turns.push(userTurn("问题讨论内容甲".repeat(40)));
  }
  return turns;
}

const OUTCOME = {
  summary: "## Goal\n推演完成",
  degraded: false,
  tokensBefore: 30_000,
};

/**
 * 注册一次 compact_conversation 的 mock 返回。
 * 任何不匹配命令的意外路由也返回 Promise（而非 undefined）——避免 TypeError
 * 连锁掩盖真实断言失败；每个注册点必被 runCompaction 消费（无残留 once）。
 */
function installCompactMock(
  result: unknown,
  rejectWith?: unknown
): void {
  __invokeMock.mockImplementationOnce((cmd: string) => {
    if (cmd !== "compact_conversation") {
      return Promise.resolve(undefined);
    }
    return rejectWith !== undefined
      ? Promise.reject(rejectWith)
      : Promise.resolve(result);
  });
}

// ═══════════════════════════════════════════════════════
// runCompaction —— 主路径与边界
// ═══════════════════════════════════════════════════════

describe("runCompaction 主路径", () => {
  it("成功：调用 compact_conversation（含折叠区派生）→ applyCompaction 落记录 + 归零 + toast", async () => {
    seedConversation("c1", longConversation(), {
      contextTokens: { inputTokens: 50_000, outputTokens: 500 },
      lastNoticeSeq: 7,
    });
    installCompactMock(OUTCOME);

    await useCompactionStore.getState().runCompaction("c1");

    // compact_conversation 请求契约：sessionId 约定 + foldedHistory 为折叠区派生
    const arg = lastInvokeArg("compact_conversation");
    expect(arg?.request).toBeDefined();
    const req = (arg as { request: Record<string, unknown> }).request;
    expect(req.conversationId).toBe("c1");
    expect(req.sessionId).toBe(compactionSessionId("c1"));
    const folded = req.foldedHistory as Array<{ role: string; content: string }>;
    expect(folded.length).toBeGreaterThan(0);
    expect(folded.every((m) => m.role !== "error")).toBe(true);

    // 状态：compaction 落记录 + contextTokens 归零 + busy 清除
    const conv = useConversationStore
      .getState()
      .conversations.find((c) => c.id === "c1")!;
    expect(conv.compaction).toBeDefined();
    expect(conv.compaction!.summary).toBe(OUTCOME.summary);
    expect(conv.compaction!.retainedFrom).toBeGreaterThan(0);
    expect(conv.contextTokens).toBeUndefined();
    // 批 4：游标复位（下一条消息重发三值快照）
    expect(conv.lastNoticeSeq).toBeUndefined();
    expect(useCompactionStore.getState().compactingConversationId).toBeNull();

    // 持久化提交 update_conversation_meta（compaction 记录随参数）
    const metaArg = lastInvokeArg("update_conversation_meta");
    expect(metaArg?.conversationId).toBe("c1");
    expect((metaArg as { compaction: Record<string, unknown> }).compaction)
      .toMatchObject({
        retainedFrom: conv.compaction!.retainedFrom,
        summary: OUTCOME.summary,
        tokensBefore: OUTCOME.tokensBefore,
      });
    // 游标复位的持久面（重启后仍走快照分支）
    expect(metaArg?.clearNoticeSeq).toBe(true);
    // toast：动作语义（可点击 → 查看分界）
    const toasts = useToastStore.getState().toasts;
    expect(toastMessages().join("|")).toContain("已压缩");
    expect(toastMessages().join("|")).toContain("点击查看分界");
    const last = toasts[toasts.length - 1];
    expect(typeof last.onClick).toBe("function");
  });

  it("nothing：全部轮在预算内 → info 提示，不发命令", async () => {
    seedConversation("c1", [userTurn("你好"), userTurn("再见")]);
    await useCompactionStore.getState().runCompaction("c1");
    expect(invokeCount("compact_conversation")).toBe(0);
    expect(toastMessages().join("|")).toContain("对话历史太短");
    expect(
      useConversationStore.getState().conversations[0].compaction
    ).toBeUndefined();
  });

  it("too_soon：折叠区不足 2 轮 → 非错误提示，不发命令", async () => {
    const huge = userTurn("字".repeat(40_000)); // 单轮 24K tok > 20K → 折叠它
    seedConversation("c1", [huge, userTurn("短问题")]);
    await useCompactionStore.getState().runCompaction("c1");
    expect(invokeCount("compact_conversation")).toBe(0);
    expect(toastMessages().join("|")).toContain("新内容太少");
  });

  it("失败：后端 Err → 压缩失败 toast，状态不变（幂等可重试）", async () => {
    seedConversation("c1", longConversation());
    installCompactMock(undefined, new Error("网络超时"));
    await useCompactionStore.getState().runCompaction("c1");
    expect(toastMessages().join("|")).toContain("压缩失败");
    const conv = useConversationStore.getState().conversations[0];
    expect(conv.compaction).toBeUndefined();
    expect(conv.contextTokens).toBeUndefined();
    expect(useCompactionStore.getState().compactingConversationId).toBeNull();
  });

  it("取消：后端返回「压缩已取消」→ 静默收尾（无失败 toast）", async () => {
    seedConversation("c1", longConversation());
    installCompactMock(undefined, "压缩已取消");
    await useCompactionStore.getState().runCompaction("c1");
    expect(toastMessages()).toHaveLength(0);
    expect(useCompactionStore.getState().compactingConversationId).toBeNull();
  });

  it("链式：折叠输入 = 旧摘要 + 上次边界后的轮（不回灌已折叠原文）", async () => {
    // 每轮带唯一序号标记，便于断言“已折叠区不再回灌”
    const turns: SessionEvent[][] = [];
    for (let i = 0; i < 500; i++) {
      turns.push(userTurn(`第${i}轮 ` + "问题讨论内容甲".repeat(40)));
    }
    // 第一次压缩：cut 在中间（模拟 compaction 记录 retainedFrom=250）
    seedConversation("c1", turns);
    useConversationStore.setState({
      conversations: useConversationStore
        .getState()
        .conversations.map((c) =>
          c.id === "c1"
            ? {
                ...c,
                compaction: {
                  retainedFrom: 250,
                  summary: "## Goal\n旧摘要",
                  tokensBefore: 30_000,
                  at: 1,
                },
              }
            : c
        ),
    });
    installCompactMock(OUTCOME);
    await useCompactionStore.getState().runCompaction("c1");
    const req = (
      lastInvokeArg("compact_conversation") as {
        request: { foldedHistory: Array<{ content: string }> };
      }
    ).request;
    const folded = req.foldedHistory;
    // 旧摘要前置（链式语义不变）
    expect(folded[0].content).toContain("旧摘要");
    expect(folded[0].content).toContain("[对话压缩摘要");
    // 已折叠区（第 0..249 轮）不得回灌
    expect(folded.some((m) => m.content.includes("第0轮"))).toBe(false);
    expect(folded.some((m) => m.content.includes("第249轮"))).toBe(false);
    // 上次边界后的轮（第 250 轮起）进入折叠区
    expect(folded.some((m) => m.content.includes("第250轮"))).toBe(true);
    // 载荷 = 1（旧摘要）+（新边界 − 上次边界）条轮消息
    const conv = useConversationStore
      .getState()
      .conversations.find((c) => c.id === "c1")!;
    expect(folded.length).toBe(1 + (conv.compaction!.retainedFrom - 250));
  });
});

describe("runCompaction 幂等与取消通道", () => {
  it("压缩中同会话重复 run → 直接返回（无第二次 invoke）", async () => {
    seedConversation("c1", longConversation());
    useCompactionStore.setState({ compactingConversationId: "c1" });
    await useCompactionStore.getState().runCompaction("c1");
    expect(invokeCount("compact_conversation")).toBe(0);
  });

  it("cancelCompaction → abort_turn 使用 ctxdecay-{conversationId}", async () => {
    useCompactionStore.setState({ compactingConversationId: "c1" });
    await useCompactionStore.getState().cancelCompaction();
    const arg = lastInvokeArg("abort_turn");
    expect(arg?.sessionId).toBe(compactionSessionId("c1"));
    // busy 保持（后端 Err「压缩已取消」到达时由 runCompaction 收尾）
    expect(useCompactionStore.getState().compactingConversationId).toBe("c1");
  });

  it("空闲时 cancelCompaction → 空操作", async () => {
    await useCompactionStore.getState().cancelCompaction();
    expect(invokeCount("abort_turn")).toBe(0);
  });

  it("会话不存在 → 空操作", async () => {
    await useCompactionStore.getState().runCompaction("ghost");
    expect(invokeCount("compact_conversation")).toBe(0);
  });
});

describe("applyCompaction（conversationStore 层）", () => {
  it("内存写记录 + contextTokens 置空 + 游标复位；持久化带 compaction 与清空开关", () => {
    seedConversation("c1", [userTurn("你好")], {
      contextTokens: { inputTokens: 100, outputTokens: 20 },
      lastNoticeSeq: 7,
    });
    useConversationStore
      .getState()
      .applyCompaction("c1", {
        retainedFrom: 1,
        summary: "## Goal\n摘要",
        tokensBefore: 90,
        at: 123,
      });
    const conv = useConversationStore.getState().conversations[0];
    expect(conv.compaction).toMatchObject({
      retainedFrom: 1,
      summary: "## Goal\n摘要",
      tokensBefore: 90,
    });
    expect(conv.contextTokens).toBeUndefined();
    // 批 4：游标复位 → 下一条消息走“新对话”分支重发三值快照
    expect(conv.lastNoticeSeq).toBeUndefined();
    const arg = lastInvokeArg("update_conversation_meta");
    expect(arg?.conversationId).toBe("c1");
    expect((arg as { compaction: Record<string, unknown> }).compaction).toMatchObject(
      { retainedFrom: 1, summary: "## Goal\n摘要" }
    );
    // 压缩归零 / 游标复位的持久面：显式置 null 写盘（重启后语义保持）
    expect(arg?.clearContextTokens).toBe(true);
    expect(arg?.clearNoticeSeq).toBe(true);
  });

  it("对话不存在 → 空操作且不发 invoke", () => {
    useConversationStore
      .getState()
      .applyCompaction("ghost", {
        retainedFrom: 1,
        summary: "s",
        tokensBefore: null,
        at: 1,
      });
    expect(invokeCount("update_conversation_meta")).toBe(0);
  });
});

describe("sendMessage 与压缩的互斥 + 窗口硬拦", () => {
  it("压缩中 → 禁发（无 agent_process 调用、无窗口噪音）", async () => {
    seedConversation("c1", [userTurn("你好")]);
    useCompactionStore.setState({ compactingConversationId: "c1" });
    await useProcessingStore.getState().sendMessage("再问一个");
    expect(invokeCount("agent_process")).toBe(0);
  });

  it("窗口接近满 → 发送前硬拦，提示先压缩（agent_process 未触发）", async () => {
    seedConversation("c1", [userTurn("你好")], {
      contextTokens: { inputTokens: 54_000, outputTokens: 500 },
    });
    setModelInfo({ name: "DeepSeek", contextLength: 64_000, supportsVision: false });
    await useProcessingStore.getState().sendMessage("继续聊");
    expect(invokeCount("agent_process")).toBe(0);
    expect(toastMessages().join("|")).toContain("请先压缩");
  });

  it("窗口充裕 → 正常发送（agent_process 触发）", async () => {
    seedConversation("c1", [userTurn("你好")], {
      contextTokens: { inputTokens: 10_000, outputTokens: 500 },
    });
    setModelInfo({ name: "DeepSeek", contextLength: 64_000, supportsVision: false });
    await useProcessingStore.getState().sendMessage("继续聊");
    expect(invokeCount("agent_process")).toBe(1);
  });

  it("发送前硬拦优先使用 contextInputTokens（Anthropic 缓存命中）", async () => {
    seedConversation("c1", [userTurn("你好")], {
      contextTokens: {
        inputTokens: 100,
        contextInputTokens: 60_000,
        outputTokens: 500,
      },
    });
    setModelInfo({ name: "Anthropic", contextLength: 64_000, supportsVision: false });
    await useProcessingStore.getState().sendMessage("继续聊");
    expect(invokeCount("agent_process")).toBe(0);
  });
});
