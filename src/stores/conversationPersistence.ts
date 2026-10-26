// 对话的轮次归档与压缩持久化。
//
// 内存先行、IPC fire-and-forget；轮归档写盘失败会留下可见的未保存标记
// （R-05），不再静默吞掉。

import { invoke } from "@tauri-apps/api/core";
import { showToast } from "./useToastStore";
import type {
  CompactionMeta,
  Conversation,
  ConversationUsage,
  SessionEvent,
  TokenUsage,
  UsageDelta,
  UsageStat,
} from "../types";
import type { ConversationStore } from "./useConversationStore";

const EMPTY_CATEGORY_USAGE = {
  inputTokens: 0,
  outputTokens: 0,
  cacheHitTokens: 0,
  cachePromptTokens: 0,
};

/** 把一条用量增量累加进投影的 ConversationUsage（后端为事实来源，此处同步内存态）。 */
export function addUsageDelta(
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

type SetStore = (
  partial:
    | Partial<ConversationStore>
    | ((state: ConversationStore) => Partial<ConversationStore>)
) => void;

export function appendTurnImpl(
  get: () => ConversationStore,
  set: SetStore,
  conversationId: string,
  events: SessionEvent[],
  usage?: TokenUsage,
  usageDeltas?: UsageDelta[]
): void {
  // 对话不存在（已被删除）→ 内存 map 空操作，且不发持久化 invoke
  // （删除后轮归档到达被前端侧吸收，后端无孤儿写入）
  const conv = get().conversations.find((c) => c.id === conversationId);
  if (!conv) return;

  // 内存归档先行（同步，UI 不等待 IPC）；用量增量同步累加进投影
  set((state) => ({
    conversations: state.conversations.map((c) => {
      if (c.id !== conversationId) return c;
      const next: Conversation = { ...c, turns: [...c.turns, events], liveContextTokens: undefined };
      if (usageDeltas && usageDeltas.length > 0) {
        next.usage = usageDeltas.reduce(addUsageDelta, c.usage);
      }
      return next;
    }),
  }));

  // 持久化提交（fire-and-forget；失败不再静默：标记会话 + warning toast）
  // metaSnapshot：首轮即携带完整 meta 建文件（title/createdAt/modelName/
  // contextLength）；contextTokens 取本轮 usage 覆盖参数（无新 usage 时不
  // 覆盖旧值）；lastNoticeSeq 携带当前内存游标（随轮提交）；usageDeltas
  // 为随轮用量增量（session + 首轮未归档时携带的 title）。
  const metaSnapshot = {
    title: conv.title,
    createdAt: conv.createdAt,
    modelName: conv.modelName ?? null,
    contextLength: conv.contextLength ?? null,
    provider: conv.provider ?? null,
    model: conv.model ?? null,
    // 空串 = 显式清除回模型默认（null 在 append_turn 里表示“不更新”）。
    thinkingLevel: conv.thinkingLevel ?? "",
    contextTokens: usage ?? conv.contextTokens ?? null,
    lastNoticeSeq: conv.lastNoticeSeq ?? null,
    usageDeltas: usageDeltas ?? null,
  };
  invoke("append_turn", {
    conversationId,
    ...metaSnapshot,
  })
    .then(() => {
      set((state) => ({
        conversations: state.conversations.map((c) =>
          c.id === conversationId && c.persistenceError !== undefined
            ? { ...c, persistenceError: undefined }
            : c
        ),
      }));
    })
    .catch((err) => {
      const message = String(err);
      console.error("[persistence] append_turn 失败", err);
      set((state) => ({
        conversations: state.conversations.map((c) =>
          c.id === conversationId ? { ...c, persistenceError: message } : c
        ),
      }));
      showToast(`会话信息未保存：${message}`, {
        kind: "warning",
        ttlMs: 8000,
        dedupeKey: `append-turn-failed-${conversationId}`,
      });
    });
}

export function applyCompactionImpl(
  get: () => ConversationStore,
  set: SetStore,
  conversationId: string,
  record: CompactionMeta,
  usage?: UsageStat
): void {
  const conv = get().conversations.find((c) => c.id === conversationId);
  if (!conv) return;
  const usageDeltas: UsageDelta[] | undefined = usage
    ? [{ category: "compaction", ...usage }]
    : undefined;
  // 内存先行（同步）：compaction 覆盖写 + contextTokens 置空（归零）
  // + liveContextTokens 清空（实时水位不跨压缩）+ 环境游标复位（批 4：undefined 走“新对话”分支，下一条消息重发
  //   三值快照，仅 workspace/knowledge/model，不回放事件）+ 用量累加
  set((state) => ({
    conversations: state.conversations.map((c) => {
      if (c.id !== conversationId) return c;
      const next: Conversation = {
        ...c,
        compaction: record,
        contextTokens: undefined,
        liveContextTokens: undefined,
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
}
