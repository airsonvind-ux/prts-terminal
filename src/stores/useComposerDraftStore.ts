// 输入草稿与附件：按对话隔离（S-02）。
//
// key = conversationId；新对话（activeId=null）用 NEW_DRAFT_KEY。
// 新对话草稿在切走/新建时丢弃；已有对话草稿在本次运行内保留。
// 发送失败（上下文超窗、并发保护等）不清草稿——清空只发生在发送被接受后。

import { create } from "zustand";
import type { Attachment } from "../types";

export const NEW_DRAFT_KEY = "__new__";

export function draftKeyFor(conversationId: string | null): string {
  return conversationId ?? NEW_DRAFT_KEY;
}

interface ComposerDraftStore {
  textByKey: Record<string, string>;
  attachmentsByKey: Record<string, Attachment[]>;
  setText: (key: string, text: string) => void;
  addAttachments: (key: string, attachments: Attachment[]) => void;
  removeAttachment: (key: string, index: number) => void;
  clear: (key: string) => void;
  clearNewDraft: () => void;
}

export const useComposerDraftStore = create<ComposerDraftStore>((set) => ({
  textByKey: {},
  attachmentsByKey: {},

  setText: (key, text) =>
    set((state) => ({
      textByKey: { ...state.textByKey, [key]: text },
    })),

  addAttachments: (key, attachments) => {
    if (attachments.length === 0) return;
    set((state) => ({
      attachmentsByKey: {
        ...state.attachmentsByKey,
        [key]: [...(state.attachmentsByKey[key] ?? []), ...attachments],
      },
    }));
  },

  removeAttachment: (key, index) =>
    set((state) => ({
      attachmentsByKey: {
        ...state.attachmentsByKey,
        [key]: (state.attachmentsByKey[key] ?? []).filter(
          (_, i) => i !== index
        ),
      },
    })),

  clear: (key) =>
    set((state) => {
      const textByKey = { ...state.textByKey };
      const attachmentsByKey = { ...state.attachmentsByKey };
      delete textByKey[key];
      delete attachmentsByKey[key];
      return { textByKey, attachmentsByKey };
    }),

  clearNewDraft: () =>
    set((state) => {
      const textByKey = { ...state.textByKey };
      const attachmentsByKey = { ...state.attachmentsByKey };
      delete textByKey[NEW_DRAFT_KEY];
      delete attachmentsByKey[NEW_DRAFT_KEY];
      return { textByKey, attachmentsByKey };
    }),
}));
