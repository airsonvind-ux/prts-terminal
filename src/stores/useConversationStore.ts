// src/stores/useConversationStore.ts
// 对话数据 CRUD · 纯数据管理层（架构升级 · 模块 1：投影缓存）
//
// 职责边界：
//   管理 conversations 列表和 activeId 指针。
//   不涉及 Agent 处理逻辑、不操作 UI 状态。
//   持久化（Phase 4）：本 store 是**唯一组织态入口**——appendTurn/rename/
//   delete 在内存更新后 fire-and-forget 通知后端落盘（失败静默降级，内存态先行
//   不阻断对话）；createConversation/advanceNoticeSeq 不直接触达后端（游标与
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
  ConversationUsage,
  SessionEvent,
  TokenUsage,
  UsageDelta,
  UsageStat,
} from "../types";
import { generateId } from "../utils";

interface ConversationStore {
  conversations: Conversation[];
  activeId: string | null;
  /** 新对话占位标题：newChat 点击后显示，首条消息发送时清除。不进入 conversations 数组 */
  pendingTitle: string | null;

  /** 新建对话，设为活跃，返回对话 ID。
   *  title / modelName / contextLength 由调用方传入
   *  （title 从首条 user 消息派生，与消息归档解耦）。 */
  createConversation: (
    title: string,
    modelName?: string,
    contextLength?: number
  ) => string;
  /** 向已有对话归档一轮事件序列（turn/end 或错误发生时）。
   *  内存归档先行（同步）；随后 fire-and-forget 提交后端持久化
   *  （append_turn，失败静默——重启丢最近轮，与无持久化现状等价）。
   *  对话不存在（已删除）→ 内存空操作且不发 invoke。
   *  @param usage 本轮归档的上下文 Token 用量快照（finishProcessing 从轮内
   *   最后一个 assistant/message 扫出；随轮持久覆盖。缺省时用对话现值——
   *   错误轮无新 usage 不覆盖） */
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
   * 仅更新内存标题（自动标题应用路径）。
   *
   * 与 renameConversation 的区别：不直接写盘（不 invoke）——持久化由调用方
   * （标题事件处理）按「首轮是否已归档」决定：未归档 → 后续 append_turn 的
   * metaSnapshot 携带；已归档 → 调用带 CAS 守卫的 update_conversation_meta
   * 补丁。保证生成标题的"写盘侧复检"不被绕过。
   */
  setConversationTitle: (id: string, title: string) => void;
  /** 更新对话的上下文 Token 用量（覆盖式，非累加）。
   *  归档时从 assistant/message 的 usage 快照读取。
   *  纯内存 action：持久化随 append_turn 的 metaSnapshot（usage 参数）提交，
   *  本 action 不再发 IPC（内存 = updateContextTokens 管、持久 = append_turn 管）。 */
  updateContextTokens: (conversationId: string, usage: TokenUsage) => void;
  /**
   * 推进对话的环境通知游标（M1 Reminder，覆盖式）。
   * sendMessage 拼装通知后推进（防同对话重复拼）；invoke 失败时以旧值回滚
   * （防丢失——下轮重拉重拼）。对话不存在时防御性跳过。
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

const EMPTY_CATEGORY_USAGE = {
  inputTokens: 0,
  outputTokens: 0,
  cacheHitTokens: 0,
  cachePromptTokens: 0,
};

/** 把一条用量增量累加进投影的 ConversationUsage（后端为事实来源，此处同步内存态）。 */
function addUsageDelta(
  u: ConversationUsage | undefined,
  d: UsageDelta
): ConversationUsage {
  const base: ConversationUsage = u ?? {
    session: { ...EMPTY_CATEGORY_USAGE },
    compaction: { ...EMPTY_CATEGORY_USAGE },
    memory: { ...EMPTY_CATEGORY_USAGE },
    title: { ...EMPTY_CATEGORY_USAGE },
  };
  const cur = base[d.category];
  return {
    ...base,
    [d.category]: {
      inputTokens: cur.inputTokens + d.inputTokens,
      outputTokens: cur.outputTokens + d.outputTokens,
      cacheHitTokens: cur.cacheHitTokens + (d.cacheHitTokens ?? 0),
      cachePromptTokens: cur.cachePromptTokens + (d.cachePromptTokens ?? 0),
    },
  };
}

export const useConversationStore = create<ConversationStore>((set, get) => ({
  conversations: [],
  activeId: null,
  pendingTitle: null,

  createConversation: (title, modelName, contextLength) => {
    const id = generateId();
    const conv: Conversation = {
      id,
      title,
      turns: [],
      createdAt: Date.now(),
      modelName,
      contextLength,
    };
    set({
      conversations: [...get().conversations, conv],
      activeId: id,
      pendingTitle: null,
    });
    return id;
  },

  appendTurn: (conversationId, events, usage, usageDeltas) => {
    // 对话不存在（已被删除）→ 内存 map 空操作，且不发持久化 invoke
    // （删除后轮归档到达被前端侧吸收，后端无孤儿写入）
    const conv = get().conversations.find((c) => c.id === conversationId);
    if (!conv) return;

    // 内存归档先行（同步，UI 不等待 IPC）；用量增量同步累加进投影
    set((state) => ({
      conversations: state.conversations.map((c) => {
        if (c.id !== conversationId) return c;
        const next: Conversation = { ...c, turns: [...c.turns, events] };
        if (usageDeltas && usageDeltas.length > 0) {
          next.usage = usageDeltas.reduce(addUsageDelta, c.usage);
        }
        return next;
      }),
    }));

    // 持久化提交（fire-and-forget：失败静默降级，内存态照常）
    // metaSnapshot：首轮即携带完整 meta 建文件（title/createdAt/modelName/
    // contextLength）；contextTokens 取本轮 usage 覆盖参数（无新 usage 时不
    // 覆盖旧值）；lastNoticeSeq 携带当前内存游标（随轮提交）；usageDeltas
    // 为随轮用量增量（session + 首轮未归档时携带的 title）。
    const metaSnapshot = {
      title: conv.title,
      createdAt: conv.createdAt,
      modelName: conv.modelName ?? null,
      contextLength: conv.contextLength ?? null,
      contextTokens: usage ?? conv.contextTokens ?? null,
      lastNoticeSeq: conv.lastNoticeSeq ?? null,
      usageDeltas: usageDeltas ?? null,
    };
    invoke("append_turn", {
      conversationId,
      eventsJson: JSON.stringify(events),
      ...metaSnapshot,
    }).catch(() => {
      // 写盘失败静默：本轮不落盘，内存态继续（重启丢最近轮）
    });
  },

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
    // 经历管线触发（Phase 4 任务二）：切走旧对话前 fire-and-forget 提炼其增量。
    const prev = get().activeId;
    if (prev && prev !== id) {
      void invoke("finalize_memory", { conversationId: prev }).catch(() => {});
    }
    set({ activeId: id, pendingTitle: null });
  },

  newChat: () => {
    const { conversations } = get();
    set({
      activeId: null,
      pendingTitle: conversations.length > 0 ? "新对话" : null,
    });
  },

  deleteConversation: (id) => {
    const { conversations, activeId } = get();
    const remaining = conversations.filter((c) => c.id !== id);

    let newActiveId = activeId;
    let newPendingTitle = get().pendingTitle;

    if (activeId === id) {
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
        c.id === id ? { ...c, title } : c
      ),
    }));
    // 标题持久化（fire-and-forget：失败静默，重启丢重命名——低频低危）
    invoke("update_conversation_meta", { conversationId: id, title }).catch(
      () => {}
    );
  },

  setConversationTitle: (id, title) => {
    set((state) => ({
      conversations: state.conversations.map((c) =>
        c.id === id ? { ...c, title } : c
      ),
    }));
  },

  updateContextTokens: (conversationId, usage) =>
    set((state) => ({
      conversations: state.conversations.map((c) =>
        c.id === conversationId
          ? { ...c, contextTokens: usage }
          : c
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

  applyCompaction: (conversationId, record, usage) => {
    const conv = get().conversations.find((c) => c.id === conversationId);
    if (!conv) return;
    const usageDeltas: UsageDelta[] | undefined = usage
      ? [{ category: "compaction", ...usage }]
      : undefined;
    // 内存先行（同步）：compaction 覆盖写 + contextTokens 置空（归零）
    // + 环境游标复位（批 4：undefined 走“新对话”分支，下一条消息重发
    //   三值快照，仅 workspace/knowledge/model，不回放事件）+ 用量累加
    set((state) => ({
      conversations: state.conversations.map((c) => {
        if (c.id !== conversationId) return c;
        const next: Conversation = {
          ...c,
          compaction: record,
          contextTokens: undefined,
          lastNoticeSeq: undefined,
        };
        if (usageDeltas) next.usage = usageDeltas.reduce(addUsageDelta, c.usage);
        return next;
      }),
    }));
    // 持久化（fire-and-forget：失败静默降级，内存态先行）。
    // clearContextTokens = 压缩归零的持久面：contextTokens 显式置 null 写盘，
    // 重启 hydration 读回 null → 环保持归零（下轮真实 usage 到达前显示 0）。
    // clearNoticeSeq 同理：游标显式置 null 写盘，重启读回“空”→ 下一条消息
    // 仍走快照分支重发三值。（两处清空均因 None 表达“不更新”，需独立布尔——
    // 后端 MetaPatch.context_tokens_clear / last_notice_seq_clear）
    invoke("update_conversation_meta", {
      conversationId,
      compaction: {
        retainedFrom: record.retainedFrom,
        summary: record.summary,
        tokensBefore: record.tokensBefore,
        at: record.at,
      },
      clearContextTokens: true,
      clearNoticeSeq: true,
      usageDeltas: usageDeltas ?? null,
    }).catch(() => {
      // 写盘失败静默：内存态继续（重启丢最近压缩记录）
    });
  },
}));
