// src/stores/useUIStore.ts
// UI 元数据容器 + uiState 派生计算
//
// 职责边界：
//   持有 errorMsg、overlay 与启动恢复 / 自检相关的原始事实。
//   不持有 uiState（uiState 是派生值，由 deriveUIState() 纯函数计算）。
//   不依赖任何 store，不被任何 store 依赖。
//
// uiState 推导规则：
//   errorMsg !== null            → "error"
//   activeId === null            → "ready"
//   活跃对话无 processing 快照    → "ready"
//   agentPhase === "streaming"   → "streaming"
//   其余                          → "processing"

import { create } from "zustand";
import type { BootStatus, OverlayType, UIState } from "../types";

// ── 派生计算（纯函数，模块级导出） ──

/**
 * 从三个原始事实推导当前 UI 状态。
 * 纯函数：相同输入永远返回相同输出，无副作用。
 * 不存储任何状态——每次组件渲染时由 useUIState() Hook 调用。
 *
 * @param activeId          来自 useConversationStore.activeId
 * @param error             来自 useUIStore.errorMsg
 * @param processingAgentPhase  活跃对话的 processing 快照的 agentPhase，
 *                              无快照时传 undefined。来自 useProcessingStore
 */
export function deriveUIState(
  activeId: string | null,
  error: string | null,
  processingAgentPhase: "reasoning" | "processing" | "streaming" | undefined
): UIState {
  if (error !== null) return "error";
  if (!activeId) return "ready";
  if (!processingAgentPhase) return "ready";
  if (processingAgentPhase === "streaming") return "streaming";
  return "processing";
}

/** 设置页记忆（上次所在页）：ContextIndicator 跳模型页时覆盖一次。
 *  与 SettingsPanel 的 TABS 保持一致（新增子页时两处同改）。 */
export type SettingsTabKey =
  | "memory"
  | "model"
  | "safety"
  | "space"
  | "skills"
  | "usage"
  | "extension"
  | "app"
  | "about";

/**
 * 启动恢复的真实工序进度（只喂启动演出 BootSequence；不参与业务判定）。
 * - index：读会话索引（list_conversations）阶段；
 * - read：逐份读取作战记录（readDone 随每份完成 +1）；
 * - ready：全部就绪（finally 置位，随后 restoring 复位）。
 */
export interface RestoreInfo {
  phase: "index" | "read" | "ready";
  /** 已读完的作战记录份数 */
  readDone: number;
  /** 作战记录总份数（0 = 无历史/首次使用） */
  readTotal: number;
}

// ── Store ──

interface UIStore {
  // ── State ──
  /**
   * 启动会话恢复中（对话持久化 Phase 4）。
   * true 期间：欢迎界面判定被抑制（ChatView isFirstUse 加 !restoring）
   * 且输入禁发（InputArea disabled）——防恢复完成前闪欢迎/发消息竞态。
   */
  restoring: boolean;
  /**
   * 启动恢复的真实工序进度（只喂启动演出；不参与任何业务判定）。
   */
  restoreInfo: RestoreInfo;
  /**
   * 启动自检报告（get_boot_status）：三件内置资源是否就位。
   * 只喂启动演出（清单行翻牌），不参与任何业务判定。null = 尚未取到。
   */
  bootStatus: BootStatus | null;
  /**
   * 启动会话恢复是否失败（list/get 失败 → 降级为空历史）。
   * 只喂启动演出（失败时多报一行），不参与任何业务判定。
   */
  restoreFailed: boolean;
  /**
   * 当前错误信息。null = 无错误。
   * 设非 null 时，deriveUIState() 自动返回 "error"。
   */
  errorMsg: string | null;
  /** 当前打开的覆盖层。null = 无覆盖层 */
  overlay: OverlayType;
  /** 设置面板上次所在页（打开时读、切页时写；ContextIndicator 跳模型页覆盖一次） */
  settingsInitialTab: SettingsTabKey;

  // ── Actions ──
  /** 设置会话恢复中状态（恢复 hook 调用） */
  setRestoring: (value: boolean) => void;
  /** 推进启动恢复工序进度（部分字段合并；恢复 hook 调用） */
  setRestoreInfo: (patch: Partial<RestoreInfo>) => void;
  /** 写入启动自检报告（恢复 hook 调用；取不到时保持 null） */
  setBootStatus: (status: BootStatus | null) => void;
  /** 记录启动恢复失败（恢复 hook 调用） */
  setRestoreFailed: (failed: boolean) => void;
  /** 记录错误信息 */
  setError: (msg: string) => void;
  /** 清除错误 */
  clearError: () => void;
  /** 打开/切换覆盖层。null 关闭 */
  setOverlay: (type: OverlayType) => void;
  /** 设置设置面板初始/上次所在页 */
  setSettingsInitialTab: (tab: SettingsTabKey) => void;
}

export const useUIStore = create<UIStore>((set) => ({
  /* 初值 true：启动即视为“恢复中”，直到恢复流程落定。
     原因：首帧若为 false，会先闪一下欢迎界面/空对话区（无任何提示），
     而恢复要读全量会话（长对话可能耗时秒级）。组件只需消费这一个事实：
     restoring 期间对话区显示占位、输入禁发。 */
  restoring: true,
  restoreInfo: { phase: "index", readDone: 0, readTotal: 0 },
  bootStatus: null,
  restoreFailed: false,
  errorMsg: null,
  overlay: null,
  settingsInitialTab: "app",

  setRestoring: (value) => set({ restoring: value }),

  setRestoreInfo: (patch) =>
    set((s) => ({ restoreInfo: { ...s.restoreInfo, ...patch } })),

  setBootStatus: (status) => set({ bootStatus: status }),

  setRestoreFailed: (failed) => set({ restoreFailed: failed }),

  setError: (msg) => set({ errorMsg: msg }),

  clearError: () => set({ errorMsg: null }),

  setOverlay: (type) => set({ overlay: type }),

  setSettingsInitialTab: (tab) => set({ settingsInitialTab: tab }),
}));
