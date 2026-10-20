// src/stores/__tests__/conversation.test.ts
// ConversationStore 测试：删除/重命名的后端持久化通知（对话持久化 Phase 4）
//
// 覆盖：
//   1. deleteConversation → invoke("delete_conversation")（参数 conversationId）
//   2. 删除非活跃对话同样触发
//   3. invoke 失败静默（不抛给 UI，对话照常删除）
//   4. renameConversation → invoke("update_conversation_meta")（title patch）
//   5. 重命名 invoke 失败静默（内存已改名，不抛）
//
// 旧版 clear_conversation_cache 通知已随缓存系统停用移除（命令已注释停用），
// 删除通知改为后端持久文件删除。
//
// invoke 由 setup.ts 全局 Mock，默认 resolve undefined；
// __invokeMock.mock.calls 可断言调用历史。

import { describe, it, expect, beforeEach } from "vitest";
import { useConversationStore } from "../useConversationStore";
import { useUIStore } from "../useUIStore";

/** 重置 store 并清理跨测试累积的 invoke 调用记录 */
function resetStore(): void {
  useConversationStore.setState({
    conversations: [],
    activeId: null,
    pendingTitle: null,
  });
  __invokeMock.mockClear();
}

beforeEach(resetStore);

describe("deleteConversation 删除后端持久文件", () => {
  it("删除对话时 fire-and-forget 调用 delete_conversation（带 conversationId）", () => {
    const store = useConversationStore.getState();
    const id = store.createConversation("对话A");
    // createConversation 不触发 IPC（空对话不落盘）
    expect(__invokeMock.mock.calls).toHaveLength(0);

    store.deleteConversation(id);

    const invokeCalls = __invokeMock.mock.calls;
    const del = invokeCalls.filter((c) => c[0] === "delete_conversation");
    expect(del).toHaveLength(1);
    expect(del[0][1]).toEqual({ conversationId: id });
    // 经历管线（Phase 4 任务二）：删除前先 fire-and-forget 提炼（best-effort）
    expect(invokeCalls.some((c) => c[0] === "finalize_memory")).toBe(true);
  });

  it("删除非活跃对话同样触发删除调用", () => {
    const store = useConversationStore.getState();
    const a = store.createConversation("对话A");
    store.createConversation("对话B"); // 活跃切到 B
    __invokeMock.mockClear();

    store.deleteConversation(a); // 删除非活跃对话 A

    const invokeCalls = __invokeMock.mock.calls;
    const del = invokeCalls.filter((c) => c[0] === "delete_conversation");
    expect(del).toHaveLength(1);
    expect(del[0][1]).toEqual({ conversationId: a });
    expect(invokeCalls.some((c) => c[0] === "finalize_memory")).toBe(true);
  });

  it("invoke 失败静默，不抛给 UI（对话照常删除）", async () => {
    // 模拟 invoke 拒绝（后端不可达等）：deleteConversation 内部 catch 吞掉，
    // 异常不应逃逸到调用方，UI 行为不受影响
    __invokeMock.mockRejectedValueOnce(new Error("后端不可达"));
    const store = useConversationStore.getState();
    const id = store.createConversation("对话A");

    expect(() => store.deleteConversation(id)).not.toThrow();
    // 等待 catch 微任务消化后，确认对话已删除
    await Promise.resolve();
    await Promise.resolve();
    expect(useConversationStore.getState().conversations).toHaveLength(0);
    expect(useConversationStore.getState().activeId).toBeNull();
  });
});

describe("错误状态清理", () => {
  it("切换对话 / 新建对话会清掉上一段的错误态", () => {
    const store = useConversationStore.getState();
    const a = store.createConversation("对话A");
    const b = store.createConversation("对话B");
    useUIStore.setState({ errorMsg: "HTTP 401" });

    store.switchConversation(a);
    expect(useUIStore.getState().errorMsg).toBeNull();

    useUIStore.setState({ errorMsg: "HTTP 401" });
    store.newChat();
    expect(useUIStore.getState().errorMsg).toBeNull();

    // 删除活跃对话同样解除错误态
    store.switchConversation(b);
    useUIStore.setState({ errorMsg: "HTTP 401" });
    store.deleteConversation(b);
    expect(useUIStore.getState().errorMsg).toBeNull();
  });
});

describe("renameConversation 持久化标题", () => {
  it("重命名时 fire-and-forget 调用 update_conversation_meta（title patch）", () => {
    const store = useConversationStore.getState();
    const id = store.createConversation("旧标题");

    store.renameConversation(id, "新标题");

    // 内存已改名
    expect(useConversationStore.getState().conversations[0].title).toBe(
      "新标题"
    );
    // 持久通知：只 patch title（contextTokens/lastNoticeSeq 不传——后端只更非 None）
    const invokeCalls = __invokeMock.mock.calls;
    expect(invokeCalls).toHaveLength(1);
    expect(invokeCalls[0][0]).toBe("update_conversation_meta");
    expect(invokeCalls[0][1]).toEqual({ conversationId: id, title: "新标题" });
  });

  it("重命名 invoke 失败静默（内存已改名，不抛）", async () => {
    __invokeMock.mockRejectedValueOnce(new Error("后端不可达"));
    const store = useConversationStore.getState();
    const id = store.createConversation("旧标题");

    expect(() => store.renameConversation(id, "新标题")).not.toThrow();
    await Promise.resolve();
    await Promise.resolve();
    expect(useConversationStore.getState().conversations[0].title).toBe(
      "新标题"
    );
  });
});
