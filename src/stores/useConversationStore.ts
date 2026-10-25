// src/stores/useConversationStore.ts
// 对话数据 CRUD · 纯数据管理层（架构升级 · 模块 1：投影缓存）
//
// 职责边界：
//   管理 conversations 列表和 activeId 指针。
//   不涉及 Agent 处理逻辑、不操作 UI 状态。
//   持久化（Phase 4）：本 store 是**唯一组织态入口**——appendTurn/rename/
//   delete 在内存更新后 fire-and-forget 通知后端落盘（轮归档失败会标记
//   persistenceError，不阻断对话；R-05）。createConversation/advanceNoticeSeq
//   不直接触达后端（游标与
//   usage 随轮经 append_turn 的 metaSnapshot 提交）。启动恢复由恢复 hook
//   （useConversationHydration）单点负责，本 store 只提供 setState 装载。
//   被 useProcessingStore 单向依赖（跨 store 调 getState）。
//
// Conversation 是后端 session log 的投影缓存：`turns` 每轮一组
// SessionEvent 序列，消息气泡/事件卡片由投影函数派生，
// 不再单独维护 messages 数组。
//
// 处理现场恢复（switchConversation 时）由组件层协调：
//   组件读取 useProcessingStore.getProcessingForConversation(id)，
//   存在则从快照恢复 UI 状态，不存在则切 ready。

import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import type {
  CompactionMeta,
  Conversation,
  ConversationModelConfig,
  SessionEvent,
  TokenUsage,
  UsageDelta,
  UsageStat,
} from "../types";
import { generateId } from "../utils";
import { useUIStore } from "./useUIStore";
import { useComposerDraftStore } from "./useComposerDraftStore";
import {
  addUsageDelta,
  appendTurnImpl,
  applyCompactionImpl,
} from "./conversationPersistence";

export interface ConversationStore {
  conversations: Conversation[];
  activeId: string | null;
  /** 新对话占位标题：newChat 点击后显示，首条消息发送时清除。不进入 conversations 数组 */
  pendingTitle: string | null;
  /** 新对话草稿配置：newChat 继承上一对话；首条消息固化；切走丢弃 */
  draftConfig: ConversationModelConfig | null;

  /** 新建对话，设为活跃，返回对话 ID。
   *  title / modelName / contextLength 由调用方传入
   *  （title 从首条 user 消息派生，与消息归档解耦）。 */
  createConversation: (
    title: string,
    modelName?: string,
    contextLength?: number,
    config?: ConversationModelConfig | null
  ) => string;
  /** 向已有对话归档一轮事件序列（turn/end 或错误发生时）。
   *  内存归档先行（同步）；随后 fire-and-forget 提交后端持久化
   *  （append_turn）。写盘失败会写入 persistenceError 并弹 warning toast，
   *  下一次成功归档清除；对话不存在（已删除）→ 内存空操作且不发 invoke。
   *  @param usage 本轮归档的上下文 Token 用量快照（finishProcessing 从轮内
   *   最后一个 assistant/message 扫出；随轮持久覆盖。缺省时用对话现值——
   *   错误轮无新 usage 不覆盖）。liveContextTokens 随归档清空。 */
  appendTurn: (
    conversationId: string,
    events: SessionEvent[],
    usage?: TokenUsage,
    usageDeltas?: UsageDelta[]
  ) => void;
  /** 累加用量增量到投影缓存（内存态先行；后端为事实来源，重启 hydration 覆盖）。 */
  accumulateUsage: (conversationId: string, deltas: UsageDelta[]) => void;
  /** 切换活跃对话（仅设 activeId，不恢复处理现场） */
  switchConversation: (id: string) => void;
  /** 取消选中 */
  newChat: () => void;
  /** 删除对话。若为活跃对话则取消选中 */
  deleteConversation: (id: string) => void;
  /** 重命名对话 */
  renameConversation: (id: string, title: string) => void;
  /**
   * 用首条正常消息的截断名替换“首轮失败留下的默认标题”（CAS 守卫）。
   * 仅用于自动标题路径：用户已改名时 expectedTitle 不匹配 → 不覆盖。
   */
  replaceDefaultTitle: (id: string, title: string, expectedTitle: string) => void;
  /**
   * 仅更新内存标题（自动标题应用路径）。
   *
   * 与 renameConversation 的区别：不直接写盘（不 invoke）——持久化由调用方
   * （标题事件处理）按「首轮是否已归档」决定：未归档 → 后续 append_turn 的
   * metaSnapshot 携带；已归档 → 调用带 CAS 守卫的 update_conversation_meta
   * 补丁。保证生成标题的"写盘侧复检"不被绕过。
   */
  setConversationTitle: (id: string, title: string) => void;
  /** 更新对话当前使用的模型展示元数据（切换供应商/模型时同步；
   *  持久化随下一次 append_turn 的 metaSnapshot 提交）。 */
  updateConversationModel: (
    id: string,
    modelName?: string,
    contextLength?: number
  ) => void;
  /** 更新会话级请求身份 + 展示元数据（设置页写入；持久化由调用方提交 meta）。 */
  updateConversationConfig: (
    id: string,
    config: ConversationModelConfig,
    modelName?: string,
    contextLength?: number
  ) => void;
  /** 更新新对话草稿配置（未落盘；切走/关闭即丢）。 */
  setDraftConfig: (config: ConversationModelConfig | null) => void;
  /** 更新对话的上下文 Token 用量（覆盖式，非累加）。
   *  归档时从 assistant/message 的 usage 快照读取。
   *  纯内存 action：持久化随 append_turn 的 metaSnapshot（usage 参数）提交，
   *  本 action 不再发 IPC（内存 = updateContextTokens 管、持久 = append_turn 管）。 */
  updateContextTokens: (conversationId: string, usage: TokenUsage) => void;
  /** 更新当前轮实时上下文用量（仅内存，不落盘）。step/end 使用。 */
  updateLiveContextTokens: (conversationId: string, usage: TokenUsage) => void;

  /**
   * 推进对话的环境通知游标（M1 Reminder，覆盖式）。
   * 发送时只拼装、不提前推进；turn/end 正常完成后由 finishProcessing 提交
   * （失败/中止不提交，下一轮重发同一份提醒）。对话不存在时防御性跳过。
   * 游标为会话元数据的一部分——**持久化随轮提交**：本 action 保持纯内存
   * （推进/回滚语义不动），appendTurn 的 metaSnapshot 携带当前内存游标值经
   * append_turn 落盘（随轮提交决策；丢失窗口 = 游标推进后、轮末提交前崩溃，
   * 后果仅下轮重复告知一次，自愈）。
   */
  advanceNoticeSeq: (conversationId: string, seq: number) => void;
  /**
   * 应用压缩结果（模块 D · 上下文衰减）：内存写 compaction 记录 + 置空
   * contextTokens（进度环归零——下一次真实轮 usage 自然覆盖，不估算）+
   * 环境游标复位（批 4：下一条消息重发三值快照）；持久化经
   * update_conversation_meta（fire-and-forget，失败静默——重启丢最近一次
   * 压缩记录，turns 仍在，仅折叠边界与摘要丢失，回退未压缩态）。
   * 对话不存在（已删除）→ 空操作且不发 invoke（同 appendTurn 防御模式）。
   * 幂等：重复调用覆盖为同一记录。
   */
  applyCompaction: (conversationId: string, record: CompactionMeta, usage?: UsageStat) => void;
}

export const useConversationStore = create<ConversationStore>((set, get) => ({
  conversations: [],
  activeId: null,
  pendingTitle: null,
  draftConfig: null,

  createConversation: (title, modelName, contextLength, config) => {
    const id = generateId();
    const inherited = config ?? get().draftConfig;
    const conv: Conversation = {
      id,
      title,
      // 占位标题（首条消息截断）——真标题由 conversation/title 事件置位
      titleGenerated: false,
      turns: [],
      createdAt: Date.now(),
      modelName,
      contextLength,
      provider: inherited?.provider,
      model: inherited?.model,
      thinkingLevel: inherited?.thinkingLevel ?? null,
    };
    set({
      conversations: [...get().conversations, conv],
      activeId: id,
      pendingTitle: null,
      draftConfig: null,
    });
    return id;
  },

  appendTurn: (conversationId, events, usage, usageDeltas) =>
    appendTurnImpl(get, set, conversationId, events, usage, usageDeltas),

  accumulateUsage: (conversationId, deltas) => {
    if (deltas.length === 0) return;
    set((state) => ({
      conversations: state.conversations.map((c) =>
        c.id === conversationId
          ? { ...c, usage: deltas.reduce(addUsageDelta, c.usage) }
          : c
      ),
    }));
  },

  switchConversation: (id) => {
    // 错误属于上一段对话的失败轮，切走后不再锁当前界面。
    useUIStore.getState().clearError();
    // 新对话草稿不跨对话保留（已有对话草稿按 key 保留）。
    useComposerDraftStore.getState().clearNewDraft();
    // 经历管线触发（Phase 4 任务二）：切走旧对话前 fire-and-forget 提炼其增量。
    const prev = get().activeId;
    if (prev && prev !== id) {
      void invoke("finalize_memory", { conversationId: prev }).catch(() => {});
    }
    set({ activeId: id, pendingTitle: null, draftConfig: null });
  },

  newChat: () => {
    useUIStore.getState().clearError();
    useComposerDraftStore.getState().clearNewDraft();
    const { conversations, activeId } = get();
    // 新对话继承上一个对话的配置；无活跃对话时取列表最后一个（最近创建）。
    const source = activeId
      ? conversations.find((c) => c.id === activeId)
      : conversations[conversations.length - 1];
    const inherited: ConversationModelConfig | null =
      source?.provider && source.model
        ? {
            provider: source.provider,
            model: source.model,
            thinkingLevel: source.thinkingLevel ?? null,
          }
        : null;
    set({
      activeId: null,
      pendingTitle: conversations.length > 0 ? "新对话" : null,
      draftConfig: inherited,
    });
  },

  deleteConversation: (id) => {
    const { conversations, activeId } = get();
    const remaining = conversations.filter((c) => c.id !== id);

    let newActiveId = activeId;
    let newPendingTitle = get().pendingTitle;

    if (activeId === id) {
      useUIStore.getState().clearError();
      if (remaining.length > 0) {
        // 删除当前对话 → 自动切到最近一条剩余对话
        newActiveId = remaining[remaining.length - 1].id;
        newPendingTitle = null;
      } else {
        newActiveId = null;
        newPendingTitle = null;
      }
    }

    set({
      conversations: remaining,
      activeId: newActiveId,
      pendingTitle: newPendingTitle,
      draftConfig: activeId === id ? null : get().draftConfig,
    });

    // 对话删除 → 后端持久文件随之删除（Phase 4）。
    // 先 fire-and-forget 触发一次经历提炼（删除即丢弃档案，尽力捕获记忆；
    // 后端在命令入口读盘，与删除并发时可能 no-op——尽力而为，符合设计）。
    void invoke("finalize_memory", { conversationId: id }).catch(() => {});
    // fire-and-forget：invoke 失败静默，不抛给 UI——
    // 后端对已不存在的会话幂等返回 Ok，重复删除无害。
    invoke("delete_conversation", { conversationId: id }).catch(() => {});
  },

  renameConversation: (id, title) => {
    set((state) => ({
      conversations: state.conversations.map((c) =>
        c.id === id ? { ...c, title, titleGenerated: true } : c
      ),
    }));
    // 标题持久化（fire-and-forget：失败静默，重启丢重命名——低频低危）
    invoke("update_conversation_meta", { conversationId: id, title }).catch(
      () => {}
    );
  },

  replaceDefaultTitle: (id, title, expectedTitle) => {
    let applied = false;
    set((state) => ({
      conversations: state.conversations.map((c) => {
        if (c.id !== id || c.title !== expectedTitle) return c;
        applied = true;
        return { ...c, title };
      }),
    }));
    if (!applied) return;
    // CAS 写盘：用户改名 / 已生成标题时不覆盖。
    invoke("update_conversation_meta", {
      conversationId: id,
      title,
      expectedTitle,
    }).catch(() => {});
  },

  setConversationTitle: (id, title) => {
    set((state) => ({
      conversations: state.conversations.map((c) =>
        c.id === id ? { ...c, title, titleGenerated: true } : c
      ),
    }));
  },

  updateConversationModel: (id, modelName, contextLength) =>
    set((state) => ({
      conversations: state.conversations.map((c) => {
        if (c.id !== id) return c;
        const nextName = modelName ?? c.modelName;
        const nextContext = contextLength ?? c.contextLength;
        if (nextName === c.modelName && nextContext === c.contextLength) {
          return c;
        }
        return { ...c, modelName: nextName, contextLength: nextContext };
      }),
    })),

  updateConversationConfig: (id, config, modelName, contextLength) =>
    set((state) => ({
      conversations: state.conversations.map((c) => {
        if (c.id !== id) return c;
        const next = {
          ...c,
          provider: config.provider,
          model: config.model,
          thinkingLevel: config.thinkingLevel,
          modelName: modelName ?? (config.model ? c.modelName : ""),
          contextLength:
            contextLength ?? (config.model ? c.contextLength : 0),
        };
        return next;
      }),
    })),

  setDraftConfig: (config) => set({ draftConfig: config }),

  updateContextTokens: (conversationId, usage) =>
    set((state) => ({
      conversations: state.conversations.map((c) =>
        c.id === conversationId
          ? { ...c, contextTokens: usage, liveContextTokens: undefined }
          : c
      ),
    })),

  updateLiveContextTokens: (conversationId, usage) =>
    set((state) => ({
      conversations: state.conversations.map((c) =>
        c.id === conversationId ? { ...c, liveContextTokens: usage } : c
      ),
    })),

  advanceNoticeSeq: (conversationId, seq) =>
    set((state) => ({
      conversations: state.conversations.map((c) =>
        c.id === conversationId
          ? { ...c, lastNoticeSeq: seq }
          : c
      ),
    })),

  applyCompaction: (conversationId, record, usage) =>
    applyCompactionImpl(get, set, conversationId, record, usage),
}));
