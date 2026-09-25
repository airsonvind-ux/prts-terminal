// src/stores/useSteerStore.ts
// 插话（steer）待注入（pending）状态——按 conversationId 分区
//
// 设计定稿（2026-09；2026-09-25 语义修订）：生成中提交插话 → 后端入队
// （步边界领取注入，不打断当前段）；前端记录 pending 气泡（显示在对话
// 底部当前输出段之后，虚线/降透明纯样式差异），后端注入为轮内
// user/message 事件时转正（resolveInjected：FIFO 队首 + 内容校验）。
//
// 键 = conversationId（对话级，2026-09-25 修订——原为 sessionId）：
// 保留语义下未交付条目跨轮存活（中止/错误后由下一轮领取），前端 pending
// 必须同键跨轮可见；sessionId 每轮唯一，不可作跨轮键。条目内保留提交时
// 的 sessionId 供 cancel_steer 寻址。
// 生命周期：
//   addPending           插话提交成功（steer_message 返回条目 id）
//   resolveInjected      轮内 user/message 事件到达（useAgentEvents 消费）
//   removePending        pending 气泡点击取消成功（cancel_steer 返回 true）
//   clearForConversation 对话级清理（防御；轮收尾不再清理——保留语义）

import { create } from "zustand";

export interface SteerPendingEntry {
  /** 后端 steer_message 返回的条目定位 id（cancel_steer 用） */
  id: string;
  /** 提交文本（InputArea 已 trim；转正时与注入事件 content 校验） */
  text: string;
  /** 提交时的活动轮 sessionId（cancel_steer 寻址；跨轮保留件为旧轮 id） */
  sessionId: string;
}

/** 空列表常量（selector 缺省返回同一引用，防无效重渲染） */
export const EMPTY_STEER_PENDING: SteerPendingEntry[] = [];

interface SteerState {
  /** conversationId → 待注入插话（FIFO：队首 = 最先注入） */
  pendingByConversation: Record<string, SteerPendingEntry[]>;

  /** 插话提交成功（后端入队）后登记 pending */
  addPending: (conversationId: string, entry: SteerPendingEntry) => void;

  /** 取消成功 / 转正后移除指定条目 */
  removePending: (conversationId: string, entryId: string) => void;

  /**
   * 轮内 user/message 事件到达 → 转正。
   *
   * 校验：仅当 FIFO 队首内容与事件 content 一致时移除队首
   * （后端注入顺序 = FIFO；事件按序到达，队首必匹配——防御性验证
   * 防止重复气泡/错位移除）。conversationId 缺失（快照已归档的竞态
   * 窗口）时按 sessionId + 内容兜底定位。不匹配 → 不动。
   */
  resolveInjected: (
    conversationId: string | null,
    sessionId: string,
    content: string
  ) => void;

  /** 对话级清理（防御；轮收尾不再清理——保留语义） */
  clearForConversation: (conversationId: string) => void;
}

export const useSteerStore = create<SteerState>((set, get) => ({
  pendingByConversation: {},

  addPending: (conversationId, entry) =>
    set((state) => ({
      pendingByConversation: {
        ...state.pendingByConversation,
        [conversationId]: [
          ...(state.pendingByConversation[conversationId] ?? []),
          entry,
        ],
      },
    })),

  removePending: (conversationId, entryId) =>
    set((state) => {
      const list = state.pendingByConversation[conversationId];
      if (!list) return state;
      const next = list.filter((e) => e.id !== entryId);
      if (next.length === list.length) return state;
      const pendingByConversation = { ...state.pendingByConversation };
      if (next.length === 0) {
        delete pendingByConversation[conversationId];
      } else {
        pendingByConversation[conversationId] = next;
      }
      return { pendingByConversation };
    }),

  resolveInjected: (conversationId, sessionId, content) => {
    const { pendingByConversation } = get();
    if (conversationId) {
      const list = pendingByConversation[conversationId];
      if (list && list.length > 0 && list[0].text === content) {
        useSteerStore.getState().removePending(conversationId, list[0].id);
      }
      // 不匹配：协议错位防御——不动（保留语义下由后续匹配/取消收敛）
      return;
    }
    // 快照已归档的竞态窗口：按 sessionId + 内容兜底定位
    for (const [cid, list] of Object.entries(pendingByConversation)) {
      const idx = list.findIndex(
        (e) => e.sessionId === sessionId && e.text === content
      );
      if (idx >= 0) {
        useSteerStore.getState().removePending(cid, list[idx].id);
        return;
      }
    }
  },

  clearForConversation: (conversationId) =>
    set((state) => {
      if (!state.pendingByConversation[conversationId]) return state;
      const pendingByConversation = { ...state.pendingByConversation };
      delete pendingByConversation[conversationId];
      return { pendingByConversation };
    }),
}));
