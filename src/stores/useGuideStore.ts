// src/stores/useGuideStore.ts
// 引导（应用内说明层）状态 · 只有两件事：见过什么、现在展示什么
//
// 「看过没」持久化在 settings.json 的 `ui.guide_seen`（界面态，不走配置写闸
// ——见 `settings::UiSection`），前端启动拉一次，之后只往后端追加。
//
// 触发原则（台账）：由「表面真的出现过」决定，不由「装完第几次启动」决定。
// 故本 store 不做任何"第几次启动"的判断，只做去重与排队。

import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { GUIDE_ITEMS } from "../components/guide/guideContent";

interface GuideState {
  /** 键集是否已从后端拉到。未拉到一律不展示——宁可不弹，也不误弹。 */
  hydrated: boolean;
  /** 已看过的键集 */
  seen: Set<string>;
  /** 待展示队列（队首 = 当前展示项）。空 = 没在展示。 */
  queue: string[];
  /** 本轮导览的完整键序（>1 项才记，用于气泡里的 ●○○）；单张卡为 null */
  tourKeys: string[] | null;
  /** 启动拉一次键集（App 挂载时调用） */
  hydrate: () => void;
  /** 排队展示；已看过、队列已有、未登记的 key 都忽略 */
  enqueue: (keys: string[]) => void;
  /**
   * 排一张设置页通知卡：**同一时刻只留一张**——把队里旧的设置页项换成当前这页，
   * 否则切页后会把上一页的文案举在眼前。不越过正在走的欢迎页导览。
   */
  enqueueNotice: (key: string) => void;
  /** 当前项看完：记 seen + 前进 */
  advance: () => void;
  /** 退出本轮导览（跳过 / 用户动了键盘）：当前项记 seen，其余作废 */
  exit: () => void;
}

export const useGuideStore = create<GuideState>((set, get) => ({
  hydrated: false,
  seen: new Set<string>(),
  queue: [],
  tourKeys: null,

  hydrate: () => {
    invoke<string[]>("get_guide_seen")
      .then((keys) => set({ seen: new Set(keys), hydrated: true }))
      .catch(() => {
        // 后端未就绪（启动早期 / 测试环境）：保持未 hydrated，本程不展示引导
      });
  },

  enqueue: (keys) => {
    const { hydrated, seen, queue } = get();
    if (!hydrated) return;
    const add: string[] = [];
    for (const k of keys) {
      if (GUIDE_ITEMS[k] === undefined) continue;
      if (seen.has(k) || queue.includes(k) || add.includes(k)) continue;
      add.push(k);
    }
    if (add.length === 0) return;
    set({
      queue: [...get().queue, ...add],
      // 成组入队（欢迎页三步）= 一轮导览，记下完整键序供 ●○○ 用
      tourKeys: keys.length > 1 ? [...keys] : get().tourKeys,
    });
  },

  enqueueNotice: (key) => {
    const { hydrated, seen, queue } = get();
    if (!hydrated) return;
    if (GUIDE_ITEMS[key] === undefined || seen.has(key)) return;
    // 先走完非设置页项（欢迎页导览 / 首次出现型），再把当前这页的通知挂上；
    // 队里若有旧的设置页通知，直接换掉——通知必须对应眼前这一页。
    const rest = queue.filter((k) => !k.startsWith("settings."));
    if (queue.length === 1 && queue[0] === key) return;
    set({ queue: [...rest, key] });
  },

  advance: () => {
    const [cur, ...rest] = get().queue;
    if (cur === undefined) return;
    markSeen(cur);
    set({ queue: rest, tourKeys: rest.length === 0 ? null : get().tourKeys });
  },

  exit: () => {
    const [cur] = get().queue;
    if (cur !== undefined) markSeen(cur);
    set({ queue: [], tourKeys: null });
  },
}));

/** 记一个 key 为「已看过」：本地即时生效 + 后端落盘（失败不回滚）。 */
function markSeen(key: string): void {
  useGuideStore.setState((s) => ({ seen: new Set(s.seen).add(key) }));
  invoke("mark_guide_seen", { key }).catch(() => {
    // 落盘失败（只读降级等）：本程仍按"看过"处理，重启后会再出一次——
    // 这是可接受的退化（引导不是数据）。
  });
}

/** 当前展示项 key（null = 没在展示）。 */
export function useActiveGuideKey(): string | null {
  return useGuideStore((s) => (s.queue.length > 0 ? s.queue[0] : null));
}
