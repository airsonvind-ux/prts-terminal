// src/stores/__tests__/titleGeneration.test.ts
// 会话自动标题 · 触发层测试（前端编排）
//
// 覆盖：
//   - 默认标题规则（defaultTitleFromInput / defaultTitleForContent）与跨来源一致性
//   - 阈值触发（首答可见文本达 500 字）
//   - 幂等（阈值只触发一次）
//   - 轮末兜底（阈值从未触发 / 已触发未成功）
//   - 尝试上限 2（阈值 + 兜底，不超）
//   - 生成前预检（用户已改名 → 跳过）
//   - 中止轮不兜底
//
// 直接调用 store actions 模拟事件到达（架构约定，同 processing.integration）。

import { describe, it, expect, beforeEach } from "vitest";
import { useConversationStore } from "../useConversationStore";
import { useProcessingStore } from "../useProcessingStore";
import { useUIStore } from "../useUIStore";
import { useToastStore } from "../useToastStore";
import {
  defaultTitleFromInput,
  defaultTitleForContent,
} from "../../projection";
import type { SessionEvent } from "../../types";

function resetAll(): void {
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
  useToastStore.setState({ toasts: [] });
  __invokeMock.mockClear();
  __invokeMock.mockImplementation(
    (globalThis as unknown as { __defaultInvokeImpl: (cmd: string) => unknown })
      .__defaultInvokeImpl
  );
}

beforeEach(resetAll);

function titleGenCalls(): Array<Record<string, unknown>> {
  return __invokeMock.mock.calls
    .filter((c) => c[0] === "generate_conversation_title")
    .map((c) => c[1] as Record<string, unknown>);
}

function textChunk(sessionId: string, content: string): SessionEvent {
  return { type: "assistant/chunk", sessionId, kind: "text", content };
}

/** 发消息建会话，返回 { conversationId, sessionId } */
async function startNewConversation(
  input: string
): Promise<{ conversationId: string; sessionId: string }> {
  await useProcessingStore.getState().sendMessage(input);
  const conversationId = useConversationStore.getState().conversations[0].id;
  const sessionId = Object.keys(useProcessingStore.getState().processing)[0];
  return { conversationId, sessionId };
}

// ═══════════════════════════════════════════════════════
// 默认标题规则
// ═══════════════════════════════════════════════════════

describe("默认标题规则", () => {
  it("defaultTitleFromInput = trim + 前 12 字符", () => {
    expect(defaultTitleFromInput("  查源石虫  ")).toBe("查源石虫");
    const long = "一二三四五六七八九十十一十二十三";
    expect(defaultTitleFromInput(long)).toBe(
      long.slice(0, 12)
    );
    expect(defaultTitleFromInput(long).length).toBe(12);
    expect(defaultTitleFromInput("短")).toBe("短");
  });

  it("defaultTitleForContent 剥离 [prts_reminder] 块后与原始 input 重算一致", () => {
    const input = "帮我看看这张图";
    const block =
      "[prts_reminder]\n- 模型：deepseek\n- 工作空间：/ws\n[/prts_reminder]";
    // 有块（displayContent）
    expect(defaultTitleForContent(`${block}\n\n${input}`)).toBe(
      defaultTitleFromInput(input)
    );
    // 无块
    expect(defaultTitleForContent(input)).toBe(defaultTitleFromInput(input));
  });
});

// ═══════════════════════════════════════════════════════
// 阈值触发 / 幂等
// ═══════════════════════════════════════════════════════

describe("阈值触发（500 字）与幂等", () => {
  it("首答可见文本达 500 字 → 触发一次生成（素材含首问 + 首答）", async () => {
    const { sessionId } = await startNewConversation("查源石虫");
    const text = "源".repeat(500);

    useProcessingStore.getState().appendCurrentEvent(textChunk(sessionId, text));

    const calls = titleGenCalls();
    expect(calls).toHaveLength(1);
    expect(calls[0].userText).toBe("查源石虫");
    expect(calls[0].assistantText).toBe(text);
    expect(calls[0].expectedTitle).toBe("查源石虫");
  });

  it("未达阈值不触发", async () => {
    const { sessionId } = await startNewConversation("查源石虫");
    useProcessingStore
      .getState()
      .appendCurrentEvent(textChunk(sessionId, "短".repeat(499)));
    expect(titleGenCalls()).toHaveLength(0);
  });

  it("超阈值时再多 chunk 也只触发一次（幂等）", async () => {
    const { sessionId } = await startNewConversation("查源石虫");
    const store = useProcessingStore.getState();
    store.appendCurrentEvent(textChunk(sessionId, "源".repeat(500)));
    store.appendCurrentEvent(textChunk(sessionId, "源".repeat(500)));
    store.appendCurrentEvent(textChunk(sessionId, "源".repeat(500)));
    expect(titleGenCalls()).toHaveLength(1);
  });

  it("reasoning chunk 不计入首答可见文本", async () => {
    const { sessionId } = await startNewConversation("查源石虫");
    useProcessingStore.getState().appendCurrentEvent({
      type: "assistant/chunk",
      sessionId,
      kind: "reasoning",
      content: "思".repeat(600),
    });
    expect(titleGenCalls()).toHaveLength(0);
  });
});

// ═══════════════════════════════════════════════════════
// 轮末兜底
// ═══════════════════════════════════════════════════════

describe("轮末兜底", () => {
  it("阈值从未触发 → 首轮完成时兜底触发一次", async () => {
    const { sessionId } = await startNewConversation("短问");
    useProcessingStore
      .getState()
      .appendCurrentEvent(textChunk(sessionId, "简短回答"));

    useProcessingStore.getState().finishProcessing(sessionId, "completed");

    const calls = titleGenCalls();
    expect(calls).toHaveLength(1);
    expect(calls[0].assistantText).toBe("简短回答");
  });

  it("阈值已触发但未成功 → 轮末再兜底一次（累计 2 次，不超）", async () => {
    const { sessionId } = await startNewConversation("查源石虫");
    useProcessingStore
      .getState()
      .appendCurrentEvent(textChunk(sessionId, "源".repeat(500)));
    expect(titleGenCalls()).toHaveLength(1); // 阈值触发

    // 未收到 conversation/title 事件（标题仍为默认）→ 轮末兜底
    useProcessingStore.getState().finishProcessing(sessionId, "completed");
    expect(titleGenCalls()).toHaveLength(2);

    // 轮末后现场已清理，不再触发
    expect(useProcessingStore.getState().processing).toEqual({});
  });

  it("已成功获得标题（标题非默认）→ 兜底跳过", async () => {
    const { conversationId, sessionId } = await startNewConversation("查源石虫");
    // 模拟标题事件已落地（内存标题被改）
    useConversationStore
      .getState()
      .setConversationTitle(conversationId, "源石虫数值");
    useProcessingStore
      .getState()
      .appendCurrentEvent(textChunk(sessionId, "简短回答"));

    useProcessingStore.getState().finishProcessing(sessionId, "completed");
    expect(titleGenCalls()).toHaveLength(0);
  });

  it("中止轮不兜底", async () => {
    const { sessionId } = await startNewConversation("查源石虫");
    useProcessingStore
      .getState()
      .appendCurrentEvent(textChunk(sessionId, "半截"));

    useProcessingStore.getState().finishProcessing(sessionId, "aborted");
    expect(titleGenCalls()).toHaveLength(0);
  });

  it("错误轮不兜底（failProcessing）", async () => {
    const { sessionId } = await startNewConversation("查源石虫");
    useProcessingStore
      .getState()
      .appendCurrentEvent(textChunk(sessionId, "半截"));

    useProcessingStore.getState().failProcessing(sessionId, "后端错误");
    expect(titleGenCalls()).toHaveLength(0);
  });
});

// ═══════════════════════════════════════════════════════
// 生成前预检（用户已改名）
// ═══════════════════════════════════════════════════════

describe("生成前预检", () => {
  it("用户已改名 → 阈值触发跳过（不发生成）", async () => {
    const { conversationId, sessionId } = await startNewConversation("查源石虫");
    useConversationStore.getState().renameConversation(conversationId, "我改的标题");
    __invokeMock.mockClear();

    useProcessingStore
      .getState()
      .appendCurrentEvent(textChunk(sessionId, "源".repeat(500)));
    expect(titleGenCalls()).toHaveLength(0);

    // 兜底也不发（标题仍非默认）
    useProcessingStore.getState().finishProcessing(sessionId, "completed");
    expect(titleGenCalls()).toHaveLength(0);
  });
});
