// src/stores/useCompactionStore.ts
// 上下文压缩编排（模块 D · 上下文衰减）
//
// 职责：压缩 = 前端发起的一次对话级状态转换。本 store 承载编排：
//   1. 前端计算保留段起点（computeRetainedFrom，轮边界对齐）并做 noop 预判
//      （对话太短 nothing / 新内容太少 too_soon——非错误提示，不发命令）；
//   2. 折叠区派生态（链式 = 旧摘要 + 上次边界后的轮；已折叠原文不回灌）
//      发给后端 `compact_conversation`（静默独立 LLM 摘要 run：system/工具面
//      与主请求逐字节同源以复用 KV cache 前缀；无可见事件）；
//   3. 成功后 conversationStore.applyCompaction 落记录 + 归零进度环 + 环境游标复位（下轮重发三值快照）；
//   4. 失败：状态不变（幂等可重试），toast 直显后端错误文案；
//   5. 取消：`abort_turn(sessionId = ctxdecay-{conversationId})` 走既有中止
//      通道（与主对话同一 TurnRegistry/AbortSignal 机制）。
//
// 互斥：compactingConversationId 非空 = 压缩进行中（UI 禁发 + 环动画）；
// 与生成互斥由后端 busy 闸兜底（active_count > 0 拒绝压缩）、本 store 预置
// 防用户连点（同会话重复 run 直接 return）。
//
// 状态位放本 store 而非 conversationStore：与 processing 快照同层（会话级
// busy 语义），单活跃对话场景下取 active 会话即可。

import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { useConversationStore } from "./useConversationStore";
import { showToast } from "./useToastStore";
import type { CompactOutcome } from "../types";
import {
  buildSummaryUserMessage,
  computeRetainedFrom,
  deriveMessages,
  estimateTurnTokens,
  NOOP_MIN_FOLD_TOKENS,
  NOOP_MIN_FOLD_TURNS,
} from "../projection";

/// 压缩注册/取消用 session 标识（与后端命令层约定；abort_turn 同一 id）。
export function compactionSessionId(conversationId: string): string {
  return `ctxdecay-${conversationId}`;
}

/**
 * 滚动到指定会话的压缩点提示卡（可点 toast 的点击动作；用户裁决方案 A：
 * 压缩完成 toast 可点击 → 查看分界卡；不做自动滚动）。
 *
 * 空操作语义：目标会话非活跃（已切走）、卡片不存在（会话已删 / 尚未渲染）
 * 或非浏览器环境 → 返回 false，调用方无需处理（toast 已关闭）。
 * 滚动走 `scrollIntoView`：由浏览器滚动到最近的 scrollable 祖先（消息列表
 * 容器），居中显示分界卡。
 */
export function scrollToCompactionCard(conversationId: string): boolean {
  if (typeof document === "undefined") return false;
  const activeId = useConversationStore.getState().activeId;
  if (activeId !== conversationId) return false;
  const card = document.querySelector("[data-compaction-card]");
  if (!card) return false;
  card.scrollIntoView({ block: "center", behavior: "smooth" });
  return true;
}

interface CompactionStore {
  /** 压缩中的对话 id（null = 空闲）。UI（ContextIndicator/InputArea）据此切换态 */
  compactingConversationId: string | null;
  /** 发起一次压缩（作用指定会话）。幂等：同会话压缩中重复调用直接返回。 */
  runCompaction: (conversationId: string) => Promise<void>;
  /** 取消当前压缩（走 abort_turn 中止通道）。无压缩进行中 → 空操作。 */
  cancelCompaction: () => Promise<void>;
}

export const useCompactionStore = create<CompactionStore>((set, get) => ({
  compactingConversationId: null,

  runCompaction: async (conversationId) => {
    const convStore = useConversationStore.getState();
    const conv = convStore.conversations.find((c) => c.id === conversationId);
    if (!conv) return; // 已删除 → 空操作（防御）

    if (get().compactingConversationId !== null) return; // 已有压缩进行中

    // ── 1. 保留段起点（轮边界对齐）与 noop 预判（非错误，info toast）──
    const retainedFrom = computeRetainedFrom(conv.turns);
    if (retainedFrom === 0) {
      showToast("对话历史太短，暂无可压缩内容", { kind: "info" });
      return;
    }
    // 链式折叠起点 = 上次压缩边界（2026-09-25 裁决）：已折叠原文不回灌——
    // 对 LLM 而言原文已被摘要取代，重读等于让前次压缩失去意义。
    const foldFrom = conv.compaction?.retainedFrom ?? 0;
    const foldedTurns = conv.turns.slice(foldFrom, retainedFrom);
    const foldEstimate = foldedTurns.reduce(
      (sum, t) => sum + estimateTurnTokens(t),
      0
    );
    if (
      foldedTurns.length < NOOP_MIN_FOLD_TURNS ||
      foldEstimate < NOOP_MIN_FOLD_TOKENS
    ) {
      showToast("距上次压缩后的新内容太少，暂不压缩", { kind: "info" });
      return;
    }

    // ── 2. 折叠区派生态（error 等结构事件已过滤；链式：旧摘要代表已折叠区）──
    const folded = deriveMessages(foldedTurns);
    if (folded.length === 0) {
      showToast("没有可压缩的内容", { kind: "info" });
      return;
    }
    const payload = conv.compaction
      ? [buildSummaryUserMessage(conv.compaction), ...folded]
      : folded;

    const sessionId = compactionSessionId(conversationId);
    set({ compactingConversationId: conversationId });
    try {
      const outcome = await invoke<CompactOutcome>("compact_conversation", {
        request: { conversationId, sessionId, foldedHistory: payload },
      });
      convStore.applyCompaction(
        conversationId,
        {
          retainedFrom,
          summary: outcome.summary,
          tokensBefore: outcome.tokensBefore,
          at: Date.now(),
        },
        outcome.usage ?? undefined
      );
      const degraded = outcome.degraded ? "（规则摘要）" : "";
      // 可点 toast（用户裁决方案 A）：点击滚动到分界卡；自动滚动已移除
      // （压缩完成不打断当前阅读位置，可发现性交由此动作入口承担）
      showToast(
        `已压缩 ${foldedTurns.length} 轮${degraded} · 点击查看分界`,
        {
          kind: "info",
          onClick: () => {
            scrollToCompactionCard(conversationId);
          },
        }
      );
    } catch (err) {
      const message = String(err);
      // 用户取消（走 abort 通道）→ 非错误，静默
      if (message.includes("取消")) return;
      showToast(`压缩失败：${message}`, { kind: "warning" });
    } finally {
      set({ compactingConversationId: null });
    }
  },

  cancelCompaction: async () => {
    const conversationId = get().compactingConversationId;
    if (!conversationId) return;
    // fire-and-forget：abort 是置位语义（幂等）；后端 run_summary 在取消点
    // 返回「压缩已取消」→ invoke reject → runCompaction 的 catch 静默收尾。
    invoke("abort_turn", { sessionId: compactionSessionId(conversationId) }).catch(
      () => {}
    );
  },
}));
