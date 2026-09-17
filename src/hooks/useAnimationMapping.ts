// src/hooks/useAnimationMapping.ts
// 动画状态映射 Hook（批 2-D 重写）
//
// 职责：订阅 uiState + agentPhase + mood 三个信号，用纯函数 mapTargets
//       计算目标参数集（散热分段状态 + 情绪动作信号），喂给统一 rAF 引擎
//       （droneEngine）。不再写 CSS 变量——引擎逐帧直写 DOM。
//
// 情绪动作是一次性触发：仅当 mood 值变化时下发动作信号（useRef 记录上一值）。
// - 同值不重复触发：流式中多次渲染但 mood 未变 → 不重播动作
// - mood 回到平静（快照删除 / 切换对话）→ 下发 null，不打断进行中的动作，
//   让其自然衰减归零（引擎语义：null = 无新动作）
// - StrictMode 双挂载：effect 重跑时 prevMood 已同步 → 第二次不下发动作信号

import { useEffect, useRef } from "react";
import { useUIState } from "./useUIState";
import { useConversationStore } from "../stores/useConversationStore";
import { useProcessingStore } from "../stores/useProcessingStore";
import { droneEngine } from "../animation/droneEngine";
import { mapTargets } from "../animation/animationMapping";

export function useAnimationMapping(): void {
  const uiState = useUIState();

  // 读取活跃对话的 agentPhase 与 mood。订阅粒度比 useUIState 更细：
  // useUIState 只要 agentPhase 不变就不重渲染（如 streaming 期间），
  // 但动画映射关心 reasoning/processing 的切换（两者 uiState 都是 "processing"）。
  // 按 conversationId 搜索而非直接索引——processing 键是 sessionId，与 activeId 不同。
  const activeId = useConversationStore((s) => s.activeId);
  const agentPhase = useProcessingStore((s) => {
    if (!activeId) return undefined;
    return Object.values(s.processing).find(
      (p) => p.conversationId === activeId
    )?.agentPhase;
  });
  // mood 随快照生命周期：无快照（未处理/已归档）→ 平静
  const mood = useProcessingStore((s) => {
    if (!activeId) return "平静";
    return (
      Object.values(s.processing).find((p) => p.conversationId === activeId)
        ?.mood ?? "平静"
    );
  });
  // 情绪强度档位（0 = 基础）：与 mood 一同参与变化检测（同情绪加深也是一次新表达）
  const moodIntensity = useProcessingStore((s) => {
    if (!activeId) return 0;
    return (
      Object.values(s.processing).find((p) => p.conversationId === activeId)
        ?.moodIntensity ?? 0
    );
  });

  // 上一次下发的情绪信号（mood + 强度组合，用于检测「变化」——动作是一次性触发）
  const prevMoodKeyRef = useRef<string>("平静:0");

  useEffect(() => {
    const targets = mapTargets(uiState, agentPhase, mood, moodIntensity);
    const moodKey = `${mood}:${moodIntensity}`;
    const moodChanged = moodKey !== prevMoodKeyRef.current;
    prevMoodKeyRef.current = moodKey;

    droneEngine.setTargets({
      thermal: targets.thermal,
      // 仅值变化时下发动作信号；平静 / 未变化 → null（不打断进行中的动作）
      moodAction: moodChanged ? targets.moodAction : null,
    });
  }, [uiState, agentPhase, mood, moodIntensity]);
}
