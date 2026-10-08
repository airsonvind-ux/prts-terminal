// src/hooks/useUIState.ts
// 派生 UI 状态查询 Hook
//
// 职责：订阅 activeId/error/agentPhase 三个原始事实，
//       调用 deriveUIState() 返回派生 UIState。
//       组件通过它获取当前 UI 状态，不跨三个 store 手工组合。
//
// 注意：从 useProcessingStore 只取 agentPhase，不订阅整个 processing 字典。
//       processing 字典在每次事件（含高频 text_delta）到达时创建新引用，
//       但 agentPhase 在流式输出期间始终为 "streaming" —— Zustand 的
//       Object.is 相等性检查阻止了无效重渲染。

import { useUIStore, deriveUIState } from "../stores/useUIStore";
import { useConversationStore } from "../stores/useConversationStore";
import { useProcessingStore } from "../stores/useProcessingStore";
import type { UIState } from "../types";

export function useUIState(): UIState {
  const errorMsg = useUIStore((s) => s.errorMsg);
  const activeId = useConversationStore((s) => s.activeId);

  // 窄 selector：仅取活跃对话的 agentPhase。
  // 按 conversationId 搜索而非直接索引——processing 字典的键是 sessionId，
  // 而 activeId 是 conversationId，两者不同。遍历 O(n) 但 n ≤ 3 可忽略。
  // agentPhase 不变时（如 streaming 期间持续为 "streaming"），
  // 返回的字符串值经 Object.is 判等，阻止组件重渲染。
  const agentPhase = useProcessingStore((s) => {
    if (!activeId) return undefined;
    return Object.values(s.processing).find(
      (p) => p.conversationId === activeId
    )?.agentPhase;
  });

  return deriveUIState(activeId, errorMsg, agentPhase);
}
