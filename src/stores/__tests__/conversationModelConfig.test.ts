// 会话级模型配置：新对话继承 / 首条消息固化 / 切走丢弃 / 设置页写回。

import { describe, it, expect, beforeEach } from "vitest";
import { useConversationStore } from "../useConversationStore";

function reset(): void {
  useConversationStore.setState({
    conversations: [],
    activeId: null,
    pendingTitle: null,
    draftConfig: null,
  });
}

beforeEach(reset);

describe("会话级模型配置", () => {
  it("createConversation 固化配置；newChat 继承上一对话；首条消息后草稿清空", () => {
    const store = useConversationStore.getState();
    const a = store.createConversation("A", "DeepSeek V4", 1_000_000, {
      provider: "deepseek",
      model: "deepseek-v4-flash",
      thinkingLevel: "high",
    });
    const convA = useConversationStore
      .getState()
      .conversations.find((c) => c.id === a)!;
    expect(convA.provider).toBe("deepseek");
    expect(convA.model).toBe("deepseek-v4-flash");
    expect(convA.thinkingLevel).toBe("high");

    useConversationStore.getState().newChat();
    expect(useConversationStore.getState().draftConfig).toEqual({
      provider: "deepseek",
      model: "deepseek-v4-flash",
      thinkingLevel: "high",
    });

    const b = useConversationStore
      .getState()
      .createConversation("B", "DeepSeek V4", 1_000_000);
    const convB = useConversationStore
      .getState()
      .conversations.find((c) => c.id === b)!;
    expect(convB.provider).toBe("deepseek");
    expect(convB.model).toBe("deepseek-v4-flash");
    expect(useConversationStore.getState().draftConfig).toBeNull();
  });

  it("新对话改配置但切走 → 草稿丢弃，不污染上一对话", () => {
    const store = useConversationStore.getState();
    const a = store.createConversation("A", "DeepSeek V4", 1_000_000, {
      provider: "deepseek",
      model: "deepseek-v4-flash",
      thinkingLevel: "high",
    });
    useConversationStore.getState().newChat();
    useConversationStore.getState().setDraftConfig({
      provider: "openai",
      model: "gpt-5.4",
      thinkingLevel: "medium",
    });
    useConversationStore.getState().switchConversation(a);

    expect(useConversationStore.getState().draftConfig).toBeNull();
    const convA = useConversationStore
      .getState()
      .conversations.find((c) => c.id === a)!;
    expect(convA.provider).toBe("deepseek");
    expect(convA.model).toBe("deepseek-v4-flash");
  });

  it("updateConversationConfig 只改目标对话", () => {
    const store = useConversationStore.getState();
    const a = store.createConversation("A", "M1", 1000);
    const b = store.createConversation("B", "M2", 2000);
    useConversationStore.getState().updateConversationConfig(
      a,
      {
        provider: "zhipu",
        model: "glm-5.3",
        thinkingLevel: "max",
      },
      "GLM-5.3",
      1_000_000
    );

    const convA = useConversationStore
      .getState()
      .conversations.find((c) => c.id === a)!;
    const convB = useConversationStore
      .getState()
      .conversations.find((c) => c.id === b)!;
    expect(convA.provider).toBe("zhipu");
    expect(convA.model).toBe("glm-5.3");
    expect(convA.modelName).toBe("GLM-5.3");
    expect(convB.provider).toBeUndefined();
    expect(convB.model).toBeUndefined();
  });
});
