// src/stores/__tests__/persistence.test.ts
// 对话持久化（Phase 4）提交路径测试：ConversationStore 持久通知 + 轮归档链路
//
// 覆盖：
//   F2 appendTurn 提交参数（eventsJson 与 JSON.stringify(events) 字节保真 +
//      完整 metaSnapshot）
//   F3 空对话不落盘（createConversation 无 invoke）+ 首轮携带完整 meta
//   F4 错误轮提交（failProcessing：user 已剥块 + error 事件 + 游标随轮）
//   F5 游标随轮（advanceNoticeSeq 纯内存无 invoke；appendTurn metaSnapshot 携带）
//   F8 append_turn 失败静默（内存归档照常）
//   补充：finishProcessing usage 随轮覆盖（append_turn contextTokens = 本轮 usage）
//
// 直接调用 store actions 模拟事件到达（架构约定，同 processing.integration）。

import { describe, it, expect, beforeEach } from "vitest";
import { useConversationStore } from "../useConversationStore";
import {
  useProcessingStore,
  flushPending,
} from "../useProcessingStore";
import { useUIStore } from "../useUIStore";
import type { SessionEvent } from "../../types";

// ── 辅助 ──

/** 重置三个 Store + 清理 invoke 记录 */
function resetAllStores(): void {
  useProcessingStore.setState({ processing: {} });
  useConversationStore.setState({
    conversations: [],
    activeId: null,
    pendingTitle: null,
  });
  useUIStore.setState({
    errorMsg: null,
    overlay: null,
    restoring: false,
  });
  __invokeMock.mockClear();
}

beforeEach(resetAllStores);

function userEvent(sessionId: string, content: string): SessionEvent {
  return { type: "user/message", sessionId, content };
}

function textChunk(sessionId: string, content: string): SessionEvent {
  return { type: "assistant/chunk", sessionId, kind: "text", content };
}

/** 找最后一次指定命令的 invoke 参数（args[1]） */
function lastInvokeArg(cmd: string): Record<string, unknown> | undefined {
  const calls = __invokeMock.mock.calls;
  for (let i = calls.length - 1; i >= 0; i--) {
    if (calls[i][0] === cmd) return calls[i][1] as Record<string, unknown>;
  }
  return undefined;
}

// ═══════════════════════════════════════════════════════
// F2：appendTurn 提交参数（字节保真 + metaSnapshot）
// ═══════════════════════════════════════════════════════

describe("F2 appendTurn 持久提交参数", () => {
  it("eventsJson 与 JSON.stringify(events) 相等（字节保真契约）+ 完整 metaSnapshot", () => {
    const store = useConversationStore.getState();
    // createConversation 不支持注入 createdAt——断言与 store 现值一致即可；
    // 通过 advanceNoticeSeq/updateContextTokens 预置游标与 usage
    const id = store.createConversation("查询 1-7", "deepseek-chat", 64000);
    store.advanceNoticeSeq(id, 7);
    store.updateContextTokens(id, { inputTokens: 100, outputTokens: 50 });

    const events: SessionEvent[] = [
      userEvent("s1", "你好"),
      textChunk("s1", "hi"),
    ];
    store.appendTurn(id, events);

    const arg = lastInvokeArg("append_turn");
    expect(arg).toBeDefined();
    expect(arg!.conversationId).toBe(id);
    expect(arg!.eventsJson).toBe(JSON.stringify(events));
    expect(arg!.title).toBe("查询 1-7");
    const conv = useConversationStore.getState().conversations[0];
    expect(arg!.createdAt).toBe(conv.createdAt);
    expect(arg!.modelName).toBe("deepseek-chat");
    expect(arg!.contextLength).toBe(64000);
    expect(arg!.contextTokens).toEqual({ inputTokens: 100, outputTokens: 50 });
    expect(arg!.lastNoticeSeq).toBe(7);
  });

  it("对话不存在时 appendTurn 不发 invoke（删除后轮归档到达被前端侧吸收）", () => {
    const store = useConversationStore.getState();
    const id = store.createConversation("x");
    store.deleteConversation(id);
    __invokeMock.mockClear(); // 清除 delete 的通知

    store.appendTurn(id, [userEvent("s1", "x")]);

    expect(__invokeMock.mock.calls).toHaveLength(0);
    expect(useConversationStore.getState().conversations).toHaveLength(0);
  });
});

// ═══════════════════════════════════════════════════════
// F3：空对话不落盘 + 首轮完整 meta
// ═══════════════════════════════════════════════════════

describe("F3 空对话不落盘 / 首轮 meta 完整", () => {
  it("createConversation 后无任何 invoke（空对话不落盘）", () => {
    const store = useConversationStore.getState();
    store.createConversation("占位");
    expect(__invokeMock.mock.calls).toHaveLength(0);
  });

  it("首轮 appendTurn 携带完整 meta（可缺省字段转 null 建文件）", () => {
    const store = useConversationStore.getState();
    const id = store.createConversation("首问", "m1", 32000);
    // createConversation 后重新读状态（此前快照的 conversations 未含新对话）
    const createdAt = useConversationStore.getState().conversations[0].createdAt;

    const events = [userEvent("s9", "首问")];
    store.appendTurn(id, events);

    const arg = lastInvokeArg("append_turn")!;
    expect(arg.eventsJson).toBe(JSON.stringify(events));
    expect(arg.title).toBe("首问");
    expect(arg.createdAt).toBe(createdAt);
    expect(arg.modelName).toBe("m1");
    expect(arg.contextLength).toBe(32000);
    // 未设置过的可缺省字段 → null（后端 null = 缺省）
    expect(arg.contextTokens).toBeNull();
    expect(arg.lastNoticeSeq).toBeNull();
  });
});

// ═══════════════════════════════════════════════════════
// F4：错误轮提交（failProcessing）
// ═══════════════════════════════════════════════════════

describe("F4 错误轮提交", () => {
  it("failProcessing 触发 append_turn：user 已剥块 + error 事件 + 游标随轮", async () => {
    // 带快照：sendMessage 拼环境值快照 → 游标推进到 payload.seq=1
    __invokeMock.mockImplementationOnce((cmd: string) => {
      if (cmd === "get_reminder_data") {
        return Promise.resolve({
          seq: 1,
          dims: [{ key: "model", title: "模型", value: "deepseek", note: "deepseek", stamp: 1 }],
          events: [],
        });
      }
      return Promise.resolve(undefined);
    });

    const store = useProcessingStore.getState();
    await store.sendMessage("触发失败");
    const sid = Object.keys(useProcessingStore.getState().processing)[0];

    // 以 store action 模拟不可恢复错误到达（同 integration 架构约定）
    store.failProcessing(sid, "后端错误");

    const arg = lastInvokeArg("append_turn");
    expect(arg).toBeDefined();
    const events = JSON.parse(arg!.eventsJson as string) as SessionEvent[];
    // 错误轮 user 消息剥块（纯输入）——含块版本只进正常轮归档
    expect(events[0]).toEqual({
      type: "user/message",
      sessionId: sid,
      content: "触发失败",
    });
    expect(JSON.stringify(events)).not.toContain("prts_reminder");
    expect(events[1]).toEqual({
      type: "error",
      sessionId: sid,
      message: "后端错误",
      recoverable: false,
    });
    // 游标已随 sendMessage 推进 → 随错误轮提交
    expect(arg!.lastNoticeSeq).toBe(1);
    // 错误轮无 usage 覆盖 → contextTokens 保持缺省 null
    expect(arg!.contextTokens).toBeNull();
  });
});

// ═══════════════════════════════════════════════════════
// F5：advanceNoticeSeq 随轮提交（纯内存推进）
// ═══════════════════════════════════════════════════════

describe("F5 游标随轮提交", () => {
  it("advanceNoticeSeq 本身不触发任何 invoke（纯内存）", () => {
    const store = useConversationStore.getState();
    const id = store.createConversation("x");

    store.advanceNoticeSeq(id, 3);

    expect(__invokeMock.mock.calls).toHaveLength(0);
    expect(useConversationStore.getState().conversations[0].lastNoticeSeq).toBe(
      3
    );
  });

  it("下轮 appendTurn 的 metaSnapshot.lastNoticeSeq = 当前内存游标值", () => {
    const store = useConversationStore.getState();
    const id = store.createConversation("x");
    store.advanceNoticeSeq(id, 3);
    store.advanceNoticeSeq(id, 5); // 覆盖推进

    store.appendTurn(id, [userEvent("s1", "x")]);

    const arg = lastInvokeArg("append_turn")!;
    expect(arg.lastNoticeSeq).toBe(5);
  });
});

// ═══════════════════════════════════════════════════════
// F8：写盘失败静默（内存态照常）
// ═══════════════════════════════════════════════════════

describe("F8 写盘失败静默降级", () => {
  it("append_turn reject → 不抛、内存归档照常", async () => {
    __invokeMock.mockRejectedValueOnce(new Error("后端不可达"));
    const store = useConversationStore.getState();
    const id = store.createConversation("x");
    const events = [userEvent("s1", "x")];

    expect(() => store.appendTurn(id, events)).not.toThrow();
    await Promise.resolve();
    await Promise.resolve();

    const conv = useConversationStore.getState().conversations[0];
    expect(conv.turns).toEqual([events]);
  });
});

// ═══════════════════════════════════════════════════════
// 补充：finishProcessing usage 随轮覆盖
// ═══════════════════════════════════════════════════════

describe("finishProcessing usage 随轮提交", () => {
  it("正常轮归档：append_turn contextTokens = 本轮 usage；内存同步", async () => {
    const store = useProcessingStore.getState();
    await store.sendMessage("查询");
    const sid = Object.keys(useProcessingStore.getState().processing)[0];

    // 推流：chunk → assistant/message（usage 快照）→ turn/end 归档
    useProcessingStore.getState().appendCurrentEvent(textChunk(sid, "结果"));
    useProcessingStore.getState().appendCurrentEvent({
      type: "assistant/message",
      sessionId: sid,
      content: "结果",
      reasoningContent: null,
      usage: { inputTokens: 1200, outputTokens: 80 },
    });
    flushPending(sid);
    store.finishProcessing(sid);

    const arg = lastInvokeArg("append_turn")!;
    // usage 随轮覆盖提交（持久侧）
    expect(arg.contextTokens).toEqual({ inputTokens: 1200, outputTokens: 80 });
    expect(arg.eventsJson).toContain('"assistant/message"');
    // 内存同步（store 状态）
    expect(
      useConversationStore.getState().conversations[0].contextTokens
    ).toEqual({ inputTokens: 1200, outputTokens: 80 });
  });
});
