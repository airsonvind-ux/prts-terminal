// src/stores/useSteerStore.ts
// 插话（steer）待注入（pending）状态——按 sessionId 分区
//
// 设计定稿（2026-09）：生成中提交插话 → 后端入队（不打断当前段）；
// 前端在本 store 记录 pending 气泡（显示在对话底部当前输出段之后，
// 虚线/降透明纯样式差异，无文字角标），assistant 段完成注入为轮内
// user/message 事件时转正（resolveInjected：FIFO 队首 + 内容双校验）。
//
// 键 = sessionId（与 processing 快照 key / 后端 steer 队列 / TurnRegistry
// 同键）。生命周期：
//   addPending      插话提交成功（steer_message 返回条目 id）
//   resolveInjected 轮内 user/message 事件到达（useAgentEvents 消费）
//   removePending   pending 气泡点击取消成功（cancel_steer 返回 true）
//   clearForSession 中止（乐观清）/ turn-end / fail 轮收尾（防御清理）

import { create } from "zustand";

export interface SteerPendingEntry {
  /** 后端 steer_message 返回的条目定位 id（cancel_steer 用） */
  id: string;
  /** 提交文本（InputArea 已 trim；转正时与注入事件 content 双校验） */
  text: string;
}

/** 空列表常量（selector 缺省返回同一引用，防无效重渲染） */
export const EMPTY_STEER_PENDING: SteerPendingEntry[] = [];

interface SteerState {
  /** sessionId → 待注入插话（FIFO：队首 = 最先注入） */
  pendingBySession: Record<string, SteerPendingEntry[]>;

  /** 插话提交成功（后端入队）后登记 pending */
  addPending: (sessionId: string, entry: SteerPendingEntry) => void;

  /** 取消成功 / 转正后移除指定条目 */
  removePending: (sessionId: string, entryId: string) => void;

  /**
   * 轮内 user/message 事件到达 → 转正。
   *
   * 双校验：仅当 FIFO 队首内容与事件 content 一致时移除队首
   * （后端注入顺序 = FIFO；事件按序到达，队首必匹配——防御性验证
   * 防止重复气泡/错位移除）。不匹配 → 不动（交给 clearForSession 兜底）。
   */
  resolveInjected: (sessionId: string, content: string) => void;

  /** 中止（乐观清）/ 轮收尾（turn/end、fail）防御清理 */
  clearForSession: (sessionId: string) => void;
}

export const useSteerStore = create<SteerState>((set, get) => ({
  pendingBySession: {},

  addPending: (sessionId, entry) =>
    set((state) => ({
      pendingBySession: {
        ...state.pendingBySession,
        [sessionId]: [
          ...(state.pendingBySession[sessionId] ?? []),
          entry,
        ],
      },
    })),

  removePending: (sessionId, entryId) =>
    set((state) => {
      const list = state.pendingBySession[sessionId];
      if (!list) return state;
      const next = list.filter((e) => e.id !== entryId);
      if (next.length === list.length) return state;
      const pendingBySession = { ...state.pendingBySession };
      if (next.length === 0) {
        delete pendingBySession[sessionId];
      } else {
        pendingBySession[sessionId] = next;
      }
      return { pendingBySession };
    }),

  resolveInjected: (sessionId, content) => {
    const { pendingBySession } = get();
    const list = pendingBySession[sessionId];
    if (!list || list.length === 0) return;
    // FIFO 队首 = 最先注入：仅队首与事件内容一致才转正移除
    if (list[0].text === content) {
      useSteerStore.getState().removePending(sessionId, list[0].id);
    }
    // 不匹配：协议错位防御——不动，轮收尾 clearForSession 兜底清空
  },

  clearForSession: (sessionId) =>
    set((state) => {
      if (!state.pendingBySession[sessionId]) return state;
      const pendingBySession = { ...state.pendingBySession };
      delete pendingBySession[sessionId];
      return { pendingBySession };
    }),
}));
