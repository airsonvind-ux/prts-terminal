// PRTS 终端 · 全局类型出口
// 类型已按域拆到 types/ 目录，此文件保留统一 import 路径与少量 UI 基础类型。

export * from "./types/llm";
export * from "./types/settings";
export * from "./types/events";
export * from "./types/chat";
export * from "./types/conversation";

// ── UI 状态机（四状态） ──

/**
 * 前端 UI 状态机：当前画面处于哪个阶段。
 * 无 idle 状态：应用启动时初始为 ready，配合 activeId=null 表现与 idle 完全一致。
 * conversations.length === 0 用于判断首次使用，不单独维护状态。
 * 无 hidden 状态（2026-10-08 删除）：窗口隐藏体系（全局快捷键 / 边缘吸附）已撤销，
 * 该状态与 HiddenMode 一并清掉；将来若真要做隐藏，加回来只是几行。
 */
export type UIState =
  | "ready"
  | "processing"
  | "streaming"
  | "error";

// ── 情绪（批 2-D 动画映射 · 情绪通道） ──

/**
 * 情绪标签枚举。标签格式：【mood:高兴】（全角冒号容错）。
 * 显式枚举：高兴 / 困惑 / 沮丧；省略（无标签）= 平静（默认态）。
 * 显式 "平静" 标签不在枚举中，视为非法值（回退平静）。
 */
export type Mood = "高兴" | "困惑" | "沮丧" | "平静";

/**
 * 情绪强度档位（标签格式：【mood:情绪:强度】）。
 * 0 = 基础（省略强度参数）；1~3 = 加深（各情绪逐档交替改变幅度/频率/停留）。
 * 非法值（0 显式、4+、非数字）回退 0。
 */
export type MoodIntensity = 0 | 1 | 2 | 3;

// ── 覆盖层 ──

/** 覆盖层类型：同一时刻最多一个打开 */
export type OverlayType = null | "history" | "settings" | "more";
