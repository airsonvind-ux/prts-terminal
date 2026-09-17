// src/hooks/useMockAgent.ts
// 前端调试 Hook · 模拟 Agent 回复（架构升级 · 模块 1 词汇表）
//
// 职责：监听 processing 字典，检测到新的 sessionId 时模拟完整 Agent 生命周期。
// 所有事件通过 useProcessingStore 现有 action 写入，UI 层完全无感。
//
// 使用时机：Rust 后端 Agent 未就绪时，用于验证前端各状态的 UI 表现。
// 后端就绪后删除此文件并从 App.tsx 中移除 useMockAgent() 调用。
//
// 词汇映射：模拟后端推送的持久事实（assistant/chunk、tool/call、
// tool/result、assistant/message、turn/end）+ phase 信号。

import { useEffect, useRef } from "react";
import { useProcessingStore } from "../stores/useProcessingStore";
import type { SessionEvent } from "../types";

/*
 * 模拟文本回复（硬编码，仅用于验证 UI 流程）
 *
 * 60 字符，30ms/字符 = 约 1.8 秒流式输出时间。
 * 内容保持明日方舟语境，确保 EventBadge 的 toolName 和
 * 回复文本在视觉上不违和。
 */
const MOCK_REPLY =
  "源石虫是游戏中最基础的敌人单位。生命值300，攻击力50，防御力0，法术抗性0。移动速度缓慢，无特殊能力。任何练度的干员均可轻松应对。";

const MOCK_TOOL_RESULT =
  "查询结果：源石虫（Originium Slug），基础敌人。HP 300 | ATK 50 | DEF 0 | RES 0 | 移速 1.0 | 重量 0。特性：无。";

const MOCK_REASONING =
  "用户询问了一个敌人单位的信息。需要查询该敌人的基本数值（生命值、攻击力、防御力、法术抗性）并给出简要分析。";

export function useMockAgent(): void {
  /*
   * 已处理的 sessionId 集合，防止重复模拟。
   *
   * 用 useRef 而非 useState 的原因：ref 的变更不触发重渲染，
   * 且值在 effect 闭包中始终为最新（ref.current 是可变容器）。
   * 如果用了 state，setState 会触发额外渲染 → effect 再次执行 →
   * 可能在模拟进行中被重复调用。
   */
  const processedRef = useRef<Set<string>>(new Set());

  /*
   * 订阅整个 processing 字典。
   *
   * Zustand selector 返回 processing 引用本身，每次 store 更新
   * 都会触发重渲染（因为引用变了）。但 processedRef 保证只有
   * 新 sessionId 才启动模拟，已有的不会重复执行。
   */
  const processing = useProcessingStore((s) => s.processing);

  useEffect(() => {
    const sessionIds = Object.keys(processing);

    for (const sessionId of sessionIds) {
      if (processedRef.current.has(sessionId)) continue;

      // 标记为已处理，后续此 sessionId 不再触发
      processedRef.current.add(sessionId);

      // 异步启动模拟（不阻塞 effect 返回）
      simulateLifecycle(sessionId);
    }
  }, [processing]);
}

/**
 * 模拟完整 Agent 生命周期。
 *
 * 所有事件通过 useProcessingStore.getState() 获取最新的 action 引用后调用。
 * 不依赖闭包中捕获的旧引用——每次 setTimeout 回调内重新 getState()，
 * 确保即使 store 在两次定时器之间被替换（React Strict Mode 双次挂载场景），
 * action 引用仍然有效。
 */
function simulateLifecycle(sessionId: string): void {
  const store = () => useProcessingStore.getState();

  // ── T+300ms：reasoning 阶段 ──
  setTimeout(() => {
    store().onPhase({ sessionId, phase: "reasoning" });

    // 追加 assistant/chunk(Reasoning) 逐段填充思维链面板
    let charIdx = 0;
    const reasoningInterval = setInterval(() => {
      if (charIdx < MOCK_REASONING.length) {
        store().appendCurrentEvent({
          type: "assistant/chunk",
          sessionId,
          kind: "reasoning",
          content: MOCK_REASONING[charIdx],
        });
        charIdx++;
      } else {
        clearInterval(reasoningInterval);
      }
    }, 20);
  }, 300);

  // ── T+1300ms：executing 阶段 → 调工具 ──
  setTimeout(() => {
    store().onPhase({ sessionId, phase: "executing" });

    // 工具调用事件（用于 EventBadge 渲染运行态脉冲动画）
    const toolCall: SessionEvent = {
      type: "tool/call",
      sessionId,
      callId: "mock_call_1",
      toolName: "query_enemy",
      params: JSON.stringify({ keyword: "源石虫" }),
    };
    store().appendCurrentEvent(toolCall);

    // 工具结果事件（EventBadge 从运行态切换到完成态）
    setTimeout(() => {
      store().appendCurrentEvent({
        type: "tool/result",
        sessionId,
        callId: "mock_call_1",
        toolName: "query_enemy",
        summary: MOCK_TOOL_RESULT,
        ok: true,
      });
    }, 400);
  }, 1300);

  // ── T+2300ms：generating 阶段 → 流式输出 ──
  setTimeout(() => {
    store().onPhase({ sessionId, phase: "generating" });
    // agentPhase 变为 "streaming" → deriveUIState 返回 "streaming"

    let charIdx = 0;
    const streamInterval = setInterval(() => {
      if (charIdx < MOCK_REPLY.length) {
        store().appendCurrentEvent({
          type: "assistant/chunk",
          sessionId,
          kind: "text",
          content: MOCK_REPLY[charIdx],
        });
        charIdx++;
      } else {
        clearInterval(streamInterval);
        // 聚合事件 + 轮次结束 → 归档整轮，清理 processing 快照
        // finishProcessing 后 processing 字典删除了该条目 →
        // deriveUIState 返回 "ready"
        store().appendCurrentEvent({
          type: "assistant/message",
          sessionId,
          content: MOCK_REPLY,
          reasoningContent: MOCK_REASONING,
          usage: { inputTokens: 1200, outputTokens: 300 },
        });
        store().appendCurrentEvent({
          type: "turn/end",
          sessionId,
          reason: "completed",
        });
      }
    }, 30);
  }, 2300);
}
