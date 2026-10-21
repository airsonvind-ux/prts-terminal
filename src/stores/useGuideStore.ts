// src/stores/useGuideStore.ts
// 引导（应用内说明层）状态 · 只有两件事：见过什么、还有哪些待展示
//
// 「看过没」持久化在 settings.json 的 `ui.guide_seen`（界面态，不走配置写闸
// ——见 `settings::UiSection`），前端启动拉一次，之后只往后端追加。
//
// 触发原则（台账）：由「表面真的出现过」决定，不由「装完第几次启动」决定。
//
// 队列是"待展示集合"，不是"播放列表"：每一项归属于一个表面（对话 / 设置面板），
// **只有在它那个表面上才展示**。故"当前展示哪一项"由 `pickActive` 按当前表面挑，
// 不是一律看队首——否则对话里的待展示项会把设置页的通知堵在队尾（2026-10-11 实测）。

import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import { GUIDE_ITEMS, type GuideSurface } from "../features/guide/guideContent";

interface GuideState {
  /** 键集是否已从后端拉到。未拉到一律不展示——宁可不弹，也不误弹。 */
  hydrated: boolean;
  /** 已看过的键集 */
  seen: Set<string>;
  /** 待展示队列（无序播放；展示哪一项由表面决定） */
  queue: string[];
  /** 本轮导览的完整键序（>1 项才记，用于气泡里的 ●○○）；单张卡为 null */
  tourKeys: string[] | null;
  /** 启动拉一次键集（App 挂载时调用） */
  hydrate: () => void;
  /** 排队展示；已看过、队列已有、未登记的 key 都忽略 */
  enqueue: (keys: string[]) => void;
  /**
   * 排一张设置页通知卡：**同一时刻只留一张**——把队里旧的设置页项换成当前这页，
   * 否则切页后会把上一页的文案举在眼前。
   */
  enqueueNotice: (key: string) => void;
  /** `key` 看完：记 seen + 从队列移除（只有它，别的项照旧等着） */
  advance: (key: string) => void;
  /**
   * `key` 暂时看不到（锚点没量到）：只出队，**不记 seen**。
   * 与 advance 分开是有原因的：量不到常常是环境短暂没就绪（元素还没渲染、
   * 尺寸还是 0），把它当成"看过"就永久烧掉了（2026-10-11 实测：设置面板
   * 还在"加载中"时入队，锚点不存在 → 应用页引导一闪不出却被标记看过）。
   */
  skip: (key: string) => void;
  /** 退出：`key` 记 seen，并把同一轮导览的其余项一并作废（跳过 = 这轮不看了） */
  exit: (key: string) => void;
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
      queue: [...queue, ...add],
      // 成组入队（欢迎页三步）= 一轮导览，记下完整键序供 ●○○ 用
      tourKeys: keys.length > 1 ? [...keys] : get().tourKeys,
    });
  },

  enqueueNotice: (key) => {
    const { hydrated, seen, queue } = get();
    if (!hydrated) return;
    if (GUIDE_ITEMS[key] === undefined || seen.has(key)) return;
    // 队里若有旧的设置页通知，直接换掉——通知必须对应眼前这一页
    const rest = queue.filter((k) => GUIDE_ITEMS[k]?.surface !== "settings");
    if (queue.length === 1 && queue[0] === key) return;
    set({ queue: [...rest, key] });
  },

  advance: (key) => {
    const queue = get().queue;
    if (!queue.includes(key)) return;
    markSeen(key);
    const rest = queue.filter((k) => k !== key);
    set({ queue: rest, tourKeys: rest.length === 0 ? null : get().tourKeys });
  },

  skip: (key) => {
    const queue = get().queue;
    if (!queue.includes(key)) return;
    const rest = queue.filter((k) => k !== key);
    set({ queue: rest, tourKeys: rest.length === 0 ? null : get().tourKeys });
  },

  exit: (key) => {
    const { queue, tourKeys } = get();
    if (!queue.includes(key)) return;
    markSeen(key);
    // 跳过 = 这轮不看了：同一轮导览（tourKeys 成员）一并作废，别项照旧等着
    const drop = new Set<string>([key]);
    if (tourKeys?.includes(key)) for (const k of tourKeys) drop.add(k);
    const rest = queue.filter((k) => !drop.has(k));
    set({ queue: rest, tourKeys: rest.length === 0 ? null : get().tourKeys });
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

/**
 * 当前该展示哪一项：队列里第一个**属于当前表面**的项。
 *
 * 不属于当前表面的项原地等着（不丢弃、不记 seen），等它那个表面回来再出。
 * 这修掉了"设置面板里高亮背后的对话标题"与"通知被对话项堵在队尾"两个毛病。
 */
export function pickActiveGuideKey(
  queue: string[],
  surface: GuideSurface | null
): string | null {
  if (surface === null) return null;
  for (const k of queue) {
    if (GUIDE_ITEMS[k]?.surface === surface) return k;
  }
  return null;
}
