// 发送前的对话选择与自动标题现场注册。
//
// - 新对话：创建 Conversation（默认标题 / 模型名 / 上下文窗口）并注册标题现场。
// - 已有对话：若首轮报错后还没有成功轮，重新挂标题现场；必要时用本次输入替换
//   默认标题（CAS 守卫，不覆盖用户已改的标题）。

import type { Conversation } from "../types";
import { defaultTitleFromInput } from "../projection";
import { useConversationStore } from "./useConversationStore";
import { getModelInfo } from "./processingModelInfo";
import {
  conversationNeedsTitle,
  hasTitleGen,
  registerTitleGen,
} from "./processingTitle";

export function ensureActiveConversation(
  isNewNow: boolean,
  conv: Conversation | undefined,
  input: string
): string {
  if (isNewNow) {
    const info = getModelInfo();
    const dTitle = defaultTitleFromInput(input);
    const conversationId = useConversationStore
      .getState()
      .createConversation(dTitle, info?.name, info?.contextLength);
    // 自动标题现场注册（仅新对话首轮）：哨兵 + 素材累积起点
    registerTitleGen(conversationId, dTitle);
    return conversationId;
  }

  const conversationId = useConversationStore.getState().activeId!;
  // 首轮报错后对话已存在，但还没有成功轮：下次正常发送仍要重新挂标题现场。
  // 哨兵用当前标题（首条错误消息的截断名），生成成功后由事件替换。
  if (conversationNeedsTitle(conv) && !hasTitleGen(conversationId)) {
    // 首轮错误标题不再作为长期默认：首个正常消息先替换截断名，
    // 再按正常标题流程生成。CAS 守卫防止覆盖用户已改的标题。
    const nextDefault = defaultTitleFromInput(input);
    if (nextDefault && nextDefault !== conv!.title) {
      useConversationStore
        .getState()
        .replaceDefaultTitle(conversationId, nextDefault, conv!.title);
      registerTitleGen(conversationId, nextDefault);
    } else {
      registerTitleGen(conversationId, conv!.title);
    }
  }
  return conversationId;
}
