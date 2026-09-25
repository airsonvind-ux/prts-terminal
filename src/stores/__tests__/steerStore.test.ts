// src/stores/__tests__/steerStore.test.ts
// useSteerStore：插话 pending 状态（add/remove/resolveInjected FIFO 校验/
// clearForConversation）——纯 store 行为测试（无 DOM、无 invoke）。
// 2026-09-25 语义修订：键 = conversationId（跨轮保留可见）。

import { describe, it, expect, beforeEach } from "vitest";
import { useSteerStore } from "../useSteerStore";

beforeEach(() => {
  useSteerStore.setState({ pendingByConversation: {} });
});

describe("useSteerStore · pending 生命周期", () => {
  it("addPending 按 conversationId 分区追加（FIFO）", () => {
    useSteerStore.getState().addPending("c1", { id: "0", text: "甲", sessionId: "s1" });
    useSteerStore.getState().addPending("c1", { id: "1", text: "乙", sessionId: "s1" });
    useSteerStore.getState().addPending("c2", { id: "0", text: "丙", sessionId: "s2" });
    const s = useSteerStore.getState();
    expect(s.pendingByConversation["c1"]).toEqual([
      { id: "0", text: "甲", sessionId: "s1" },
      { id: "1", text: "乙", sessionId: "s1" },
    ]);
    expect(s.pendingByConversation["c2"]).toEqual([
      { id: "0", text: "丙", sessionId: "s2" },
    ]);
  });

  it("removePending 移除指定条目并清空分区（幂等）", () => {
    const store = useSteerStore.getState();
    store.addPending("c1", { id: "0", text: "甲", sessionId: "s1" });
    store.addPending("c1", { id: "1", text: "乙", sessionId: "s1" });
    useSteerStore.getState().removePending("c1", "0");
    expect(useSteerStore.getState().pendingByConversation["c1"]).toEqual([
      { id: "1", text: "乙", sessionId: "s1" },
    ]);
    useSteerStore.getState().removePending("c1", "999"); // 不存在 → 幂等
    expect(useSteerStore.getState().pendingByConversation["c1"]).toHaveLength(1);
    useSteerStore.getState().removePending("c1", "1");
    expect(useSteerStore.getState().pendingByConversation).toEqual({});
  });

  it("resolveInjected：对话键 FIFO 队首 + 内容一致 → 移除队首（转正）", () => {
    const store = useSteerStore.getState();
    store.addPending("c1", { id: "0", text: "插话一", sessionId: "s1" });
    store.addPending("c1", { id: "1", text: "插话二", sessionId: "s1" });
    useSteerStore.getState().resolveInjected("c1", "s1", "插话一");
    expect(useSteerStore.getState().pendingByConversation["c1"]).toEqual([
      { id: "1", text: "插话二", sessionId: "s1" },
    ]);
    useSteerStore.getState().resolveInjected("c1", "s1", "插话二");
    expect(useSteerStore.getState().pendingByConversation).toEqual({});
  });

  it("resolveInjected 校验：队首不匹配 → 不移除（协议错位防御）", () => {
    const store = useSteerStore.getState();
    store.addPending("c1", { id: "0", text: "插话一", sessionId: "s1" });
    useSteerStore.getState().resolveInjected("c1", "s1", "不相干的内容");
    expect(useSteerStore.getState().pendingByConversation["c1"]).toEqual([
      { id: "0", text: "插话一", sessionId: "s1" },
    ]);
    // 未知对话键同样无效果
    useSteerStore.getState().resolveInjected("nope", "x", "x");
    expect(useSteerStore.getState().pendingByConversation["c1"]).toHaveLength(1);
  });

  it("resolveInjected：conversationId 缺失 → 按 sessionId + 内容兜底", () => {
    const store = useSteerStore.getState();
    store.addPending("c1", { id: "0", text: "甲", sessionId: "s1" });
    // sessionId 不匹配 → 不动
    useSteerStore.getState().resolveInjected(null, "s9", "甲");
    expect(useSteerStore.getState().pendingByConversation["c1"]).toHaveLength(1);
    // 匹配兜底移除
    useSteerStore.getState().resolveInjected(null, "s1", "甲");
    expect(useSteerStore.getState().pendingByConversation).toEqual({});
  });

  it("clearForConversation 整清指定对话（幂等）", () => {
    const store = useSteerStore.getState();
    store.addPending("c1", { id: "0", text: "甲", sessionId: "s1" });
    store.addPending("c1", { id: "1", text: "乙", sessionId: "s1" });
    store.addPending("c2", { id: "0", text: "丙", sessionId: "s2" });
    useSteerStore.getState().clearForConversation("c1");
    expect(useSteerStore.getState().pendingByConversation).toEqual({
      c2: [{ id: "0", text: "丙", sessionId: "s2" }],
    });
    useSteerStore.getState().clearForConversation("c1"); // 幂等
    expect(useSteerStore.getState().pendingByConversation).toEqual({
      c2: [{ id: "0", text: "丙", sessionId: "s2" }],
    });
  });
});
