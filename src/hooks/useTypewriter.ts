// src/hooks/useTypewriter.ts
// 逐字展开（标题用）：把一段文字按固定间隔一个字一个字地露出来。
//
// 用途（两处共用同一动作）：
//   - 启动演出揭示那一刻：标题从空开始打
//   - 切换对话：旧标题瞬间消失，新标题从零重打
// 与启动演出的日志句同源（都是 slice 出已打出的前缀），只是节奏不同。
//
// 口径：
//   - enabled=false → 返回空串（启动演出期间标题留白，不剧透）
//   - 文本变化 → 立即清空重打；没有退出动画（旧文本干脆消失）
//   - 45ms/字（TITLE_CHAR_MS，Air 在演示件里定的手感）

import { useEffect, useRef, useState } from "react";

/** 每个字之间的间隔（ms）。 */
export const TITLE_CHAR_MS = 45;

/**
 * 返回当前应显示的文本前缀。
 *
 * @param text    完整文本（null / undefined = 无标题，返回空串）
 * @param enabled false = 不展开，恒返回空串（启动演出期间的闸）
 * @param step    每字间隔（ms）
 */
export function useTypewriter(
  text: string | null | undefined,
  enabled = true,
  step = TITLE_CHAR_MS
): string {
  const full = enabled ? (text ?? "") : "";
  const [shown, setShown] = useState(0);
  const timer = useRef<ReturnType<typeof setInterval> | undefined>(undefined);

  useEffect(() => {
    if (timer.current !== undefined) {
      clearInterval(timer.current);
      timer.current = undefined;
    }
    // 文本变化 / 闸门开合：一律从零重打
    setShown(0);
    if (!full) return;

    timer.current = setInterval(() => {
      setShown((n) => {
        if (n >= full.length) {
          if (timer.current !== undefined) {
            clearInterval(timer.current);
            timer.current = undefined;
          }
          return n;
        }
        return n + 1;
      });
    }, step);

    return () => {
      if (timer.current !== undefined) {
        clearInterval(timer.current);
        timer.current = undefined;
      }
    };
  }, [full, step]);

  return full.slice(0, Math.min(shown, full.length));
}
