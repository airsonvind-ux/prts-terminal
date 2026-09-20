// src/components/content/messageScroll.ts
// 对话区滚动宿主 · 供“布局测量”借用一段稳定的滚动窗口
//
// 背景（Chromium 实测复现，2026-09 排查）：
// 输入框测量内容高度时会先复位自身高度（不清零则 scrollHeight 读的是客户区，
// 删字不回落），对话区随之变高、可滚动余量变小，浏览器趁机把 scrollTop 向下
// 钳位；即使紧接着把高度设回、可滚动余量恢复，scrollTop 也不会自己回来——
// 结果是距底恰好差“输入框增高的高度”，表现为“打字时内容回滚一次，输入框挡住
// 对话内容底部”。且不会自愈：容器净尺寸没变，ResizeObserver 不会触发。
//
// 解法：测量期间把滚动行为接管下来，测量结束后写回。
// - 测量前贴底 → 测量后仍贴底（既覆盖“复位测量”，也覆盖输入框收起）；
// - 否则保持原位不动（用户正在翻历史，不该被测量扰动）。
// 写回必须发生在“被测量元素高度已恢复”之后——顺序见各调用点。

/** 滚动行为宿主：实现由 MessageList 提供（滚动区归它所有） */
export interface MessageScrollHost {
  /**
   * 在 fn 期间做布局测量，结束后按“贴底则续贴底、否则原位”写回滚动位置。
   * fn 需把被测量元素的高度恢复包含在内（写回在 fn 返回之后）。
   */
  measure<T>(fn: () => T): T;
}

let host: MessageScrollHost | null = null;

/** 登记/注销宿主（MessageList 挂载与卸载时调用） */
export function registerMessageScrollHost(next: MessageScrollHost | null): void {
  host = next;
}

/** 在布局测量期间保持对话区滚动稳定；未登记宿主时直接执行 */
export function measureWithoutScrollJump<T>(fn: () => T): T {
  return host ? host.measure(fn) : fn();
}
