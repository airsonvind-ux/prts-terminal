// src/stores/useToastStore.ts
// Toast 轻提示 Store（M3-B §5.2 · 前端拦截反馈通道）
//
// 职责：持有一条轻提示队列（toasts），支持：
//   - show(message, opts?)：追加（默认 4s 自动消失）
//   - dismiss(id)：手动关闭（点击 ToastHost 小条）
//   - dedupeKey：命中现存条目 → 不新增，仅重置该条 ttl（拦截场景连点不刷屏）
//
// 定位：模块级独立 store（不依赖 UI/Conversation/Processing 任何 store——
// 拦截场景正是「任何 store 都不该被写入」时，用 toast 而非状态表达）。
//
// 定时器放模块级 Map（id → setTimeout）：跨组件生命周期稳定，
// 与 useProcessingStore 的 chunk 缓冲同哲学（store 内原子，Hook 层零负担）。
// showToast 为模块级便捷导出：非 React 上下文（事件处理器、SettingsPanel
// 守卫）可直接调用。

import { create } from "zustand";

// ── 类型 ──

/** toast 轻提示种类（视觉由 ToastHost 区分：warning = 警示色描边等） */
export type ToastKind = "warning" | "info";

/** show() 的选项 */
export interface ToastOptions {
  /** 提示种类（默认 warning——拦截/中止类提醒用警示色） */
  kind?: ToastKind;
  /** 自动消失时长 ms（默认 4000） */
  ttlMs?: number;
  /** 去重键：命中现存条目 → 不新增，仅重置该条 ttl */
  dedupeKey?: string;
  /**
   * 点击动作（模块 D：压缩完成 toast → 滚动到分界卡）。设置后 toast 呈
   * 可点击暗示样式，点击 = 执行动作 + dismiss；缺省点击 = 直接 dismiss。
   * 调用方自行处理空操作语义（目标不存在/会话已切走时不滚动）。
   */
  onClick?: () => void;
}

/** 单条 toast */
export interface ToastItem {
  id: string;
  message: string;
  kind: ToastKind;
  ttlMs: number;
  /** 去重键（无 dedupeKey 的条目为 undefined） */
  dedupeKey?: string;
  /** 点击动作（模块 D：可点 toast）；undefined = 纯关闭语义 */
  onClick?: () => void;
}

interface ToastStore {
  toasts: ToastItem[];
  /** 追加一条 toast（默认 warning / 4s 自动消失）；dedupeKey 命中 → 重置 ttl */
  show: (message: string, opts?: ToastOptions) => void;
  /** 手动关闭一条 toast（清定时器） */
  dismiss: (id: string) => void;
  /** 带动作的 toast 点击入口：执行动作后关闭（无动作 = 仅关闭，由宿主调 dismiss） */
  activate: (id: string) => void;
}

// ── 常量与模块级定时器表 ──

export const DEFAULT_TTL_MS = 4000;

/** id → 自动消失定时器（模块级：跨组件生命周期稳定，dismiss/重置可清理） */
const timers = new Map<string, ReturnType<typeof setTimeout>>();

function clearTimer(id: string): void {
  const t = timers.get(id);
  if (t) clearTimeout(t);
  timers.delete(id);
}

/** 为该条 toast 安排自动消失定时器 */
function armTimer(id: string, ttlMs: number): void {
  clearTimer(id);
  timers.set(
    id,
    setTimeout(() => {
      timers.delete(id);
      // 到点自动消失：走 dismiss 同一移除路径（若条目已被手动关闭则幂等）
      useToastStore.getState().dismiss(id);
    }, ttlMs)
  );
}

// ── Store ──

export const useToastStore = create<ToastStore>((set, get) => ({
  toasts: [],

  show: (message, opts = {}) => {
    const kind = opts.kind ?? "warning";
    const ttlMs = opts.ttlMs ?? DEFAULT_TTL_MS;
    const dedupeKey = opts.dedupeKey;
    const onClick = opts.onClick;

    // ── dedupe：命中现存条目 → 不新增，仅更新文案/动作 + 重置 ttl ──
    // （拦截场景连点/连续改多个受控字段共享同 key 时刷屏）
    if (dedupeKey !== undefined) {
      const hit = get().toasts.find((t) => t.dedupeKey === dedupeKey);
      if (hit) {
        armTimer(hit.id, ttlMs);
        if (hit.message !== message || hit.onClick !== onClick) {
          set((state) => ({
            toasts: state.toasts.map((t) =>
              t.id === hit.id ? { ...t, message, onClick } : t
            ),
          }));
        }
        return;
      }
    }

    const id = Math.random().toString(36).slice(2) + Date.now().toString(36);
    armTimer(id, ttlMs);
    set((state) => ({
      toasts: [
        ...state.toasts,
        { id, message, kind, ttlMs, dedupeKey, onClick },
      ],
    }));
  },

  dismiss: (id) => {
    clearTimer(id);
    set((state) => ({ toasts: state.toasts.filter((t) => t.id !== id) }));
  },

  // 模块 D：可点 toast——点击 = 执行动作 + 关闭（宿主调；无动作条目纯关闭）。
  // 动作错误不阻断关闭也不外泄（toast 动作是锦上添花，失败静默）
  activate: (id) => {
    const toast = get().toasts.find((t) => t.id === id);
    if (!toast) return;
    try {
      toast.onClick?.();
    } catch {
      // 动作实现错误：忽略（滚动目标不存在等空操作已由调用方处理）
    } finally {
      get().dismiss(id);
    }
  },
}));

// ── 模块级便捷导出（非 React 上下文可调；拦截守卫主入口） ──

/**
 * 全局弹一条 toast（等价 useToastStore.getState().show）。
 * 供事件处理器 / 设置守卫等非组件上下文调用。
 */
export function showToast(message: string, opts?: ToastOptions): void {
  useToastStore.getState().show(message, opts);
}
