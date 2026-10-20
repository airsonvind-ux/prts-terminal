// src/hooks/useAgentEvents.ts
// Agent 事件监听 Hook
//
// 职责：注册 Tauri "agent-event" 事件监听，将后端推送的 AgentEvent
//       （持久事实 SessionEvent + 瞬时信号 phase）按类型分发到
//       useProcessingStore 的对应 action。组件卸载时自动取消监听。
//
// 词汇表（架构升级 · 模块 1 + 插话设计定稿）：
//   持久事实 → appendCurrentEvent（进入本轮投影缓存）
//     assistant/chunk、assistant/message、tool/call、tool/result、error(可恢复)、
//     user/message（插话注入：轮内下一条 user——由 sendMessage 预置首条之外，
//     dispatch Completed 出口注入的后续 user 也在此消费：进入投影缓存 + pending 转正）
//   轮次边界 → finishProcessing（turn/end 归档整轮）
//   错误     → error(recoverable=false) → failProcessing（终止）
//   瞬时信号 → onPhase（phase）
//   结构事件（turn/start、step/*、request/header）→ 忽略（log 结构性事实）

import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useProcessingStore, stashTitleUsage, titleExpectedFor } from "../stores/useProcessingStore";
import { useConversationStore } from "../stores/useConversationStore";
import { useSteerStore } from "../stores/useSteerStore";
import { defaultTitleForContent } from "../projection";
import type { AgentEvent, UsageDelta, UserMessageEvent } from "../types";

export function useAgentEvents(): void {
  useEffect(() => {
    // Zustand action 引用稳定，effect 顶层捕获一次，
    // 不在每个事件回调中重复调用 getState()
    const { appendCurrentEvent, finishProcessing, onPhase, failProcessing } =
      useProcessingStore.getState();

    const setup = listen<AgentEvent>("agent-event", (event) => {
      const e = event.payload;

      switch (e.type) {
        // ── 持久事实：追加到本轮投影缓存 ──
        case "assistant/chunk":
        case "assistant/message":
        case "tool/call":
        case "tool/result":
        case "tool/approval-request":
        case "tool/approval-result":
          appendCurrentEvent(e);
          break;

        // ── 插话注入（轮内下一条 user 消息）──
        // 进入投影缓存渲染为正常 user 段（多段展示改造后各成气泡）；
        // FIFO 队首 pending 内容校验转正移除（防重复气泡；2026-09-25
        // 按 sessionId → conversationId 反查对话键）。
        case "user/message": {
          appendCurrentEvent(e);
          const processing = useProcessingStore.getState().processing;
          const convId =
            Object.entries(processing).find(
              ([sid]) => sid === e.sessionId
            )?.[1].conversationId ?? null;
          useSteerStore.getState().resolveInjected(convId, e.sessionId, e.content);
          break;
        }

        // ── 轮次结束：归档整轮（快照已被 failProcessing 清理时幂等） ──
        // M3-B：reason 透传——turn/end(aborted) → finishProcessing 归档前
        // 合成轮中止标记（前端私有 turn/aborted 事件）。
        // 插话（2026-09-25 保留语义）：轮收尾不再清 pending——正常轮条目
        // 已全部注入转正（后端 turn/end 只在队列空时发）；中止/错误的
        // 未交付条目由后端保留、下一轮领取，气泡跨轮留存（可点击取消）。
        case "turn/end":
          finishProcessing(e.sessionId, e.reason, e.usage);
          break;

        // ── 瞬时信号：更新 agentPhase ──
        case "phase":
          onPhase({
            sessionId: e.sessionId,
            phase: e.phase,
          });
          break;

        // ── 会话标题更新（后端生成标题后推送；非 session log 事实）──
        // 应用前复检：仅当当前标题仍等于默认标题哨兵时才接受（用户已改名 /
        // 已生成过 → 丢弃）。首轮未归档 → 仅置内存（后续 append_turn 的
        // metaSnapshot 携带落盘）；首轮已归档 → 带 CAS 守卫的补丁（写盘侧
        // 复检在后端锁内）。
        case "conversation/title": {
          const convStore = useConversationStore.getState();
          const conv = convStore.conversations.find(
            (c) => c.id === e.conversationId
          );
          if (!conv) break;
          const snap = useProcessingStore
            .getState()
            .getProcessingForConversation(e.conversationId);
          const snapUser = snap?.currentEvents.find(
            (ev): ev is UserMessageEvent => ev.type === "user/message"
          );
          const archivedUser = conv.turns[0]?.[0];
          const firstUserContent =
            snapUser?.content ??
            (archivedUser?.type === "user/message"
              ? (archivedUser as UserMessageEvent).content
              : undefined);
          if (firstUserContent === undefined) break;
          const expected =
            titleExpectedFor(e.conversationId) ??
            defaultTitleForContent(firstUserContent);
          if (conv.title !== expected) break; // 复检：标题已被改 → 丢弃
          convStore.setConversationTitle(e.conversationId, e.title);
          const titleDelta: UsageDelta | null = e.usage
            ? { category: "title", ...e.usage }
            : null;
          if (conv.turns.length > 0) {
            if (titleDelta) {
              convStore.accumulateUsage(e.conversationId, [titleDelta]);
            }
            invoke("update_conversation_meta", {
              conversationId: e.conversationId,
              title: e.title,
              expectedTitle: expected,
              usageDeltas: titleDelta ? [titleDelta] : null,
            }).catch(() => {});
          } else if (titleDelta) {
            // 首轮未归档 → 暂存，随下一次 append_turn 提交
            stashTitleUsage(e.conversationId, titleDelta);
          }
          break;
        }

        // ── 记忆用量（后端内部累加后推送；前端镜像进投影）──
        case "memory/usage": {
          useConversationStore
            .getState()
            .accumulateUsage(e.conversationId, [{ category: "memory", ...e.usage }]);
          break;
        }

        // ── 错误：可恢复 → 追加事件展示警告；不可恢复 → 终止处理 ──
        case "error":
          if (e.recoverable) {
            appendCurrentEvent(e);
          } else {
            failProcessing(e.sessionId, e.message);
            // 不可恢复错误终止轮（2026-09-25 保留语义）：未交付插话由
            // 后端保留、下一轮领取——前端不清 pending（气泡留存）。
          }
          break;

        // ── 结构性事实（turn/start、step/*、request/header）──
        // 前端不消费
        default:
          break;
      }
    });

    // listen 返回 Promise<UnlistenFn>，组件卸载时取消监听
    return () => {
      setup.then((unlisten) => unlisten());
    };
  }, []);
}
