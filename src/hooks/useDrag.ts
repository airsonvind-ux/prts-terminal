// src/hooks/useDrag.ts
// 窗口拖拽 Hook（自绘跟手）
//
// 职责：mousedown 记锚点 → 按住移动越过阈值 → 跟随光标写窗口位置。
//
// 为什么不交给系统原生拖动（2026-09 二次修订）：
// 1. 原生 startDragging 走的是 Windows 的窗口移动循环，它会把窗口挡在屏幕
//    顶边之内——向上拖过那条线，松开时窗口会跳回边界。自绘 setPosition 没有
//    这个约束：桌面挂件需要能贴边、能半出屏。
// 2. 原生拖动一起手就吞掉随后的 click：手柄容器里挤着标题与按钮，按钮会点不动。
//
// 两条既有教训保留：
// - 监听在 mousedown 内**同步**装上（旧实现 await 后再经状态/effect 装配：
//   主线程繁忙时整段手势会落在空窗里，漏掉的 mouseup 还会让拖动态一直挂着）；
// - 起拖阈值 4px：普通点击（含手抖）不移动窗口，click 照常派发。

import { useCallback } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalPosition } from "@tauri-apps/api/dpi";

/** 起拖阈值（屏幕像素）：与系统拖拽阈值同量级，过滤点击时的手抖 */
const DRAG_THRESHOLD_PX = 4;

export function useDrag(): {
  onDragHandle: (e: React.MouseEvent) => void;
} {
  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    if (e.button !== 0) return; // 只认左键

    const startX = e.screenX;
    const startY = e.screenY;
    const scale = window.devicePixelRatio || 1;
    const win = getCurrentWindow();

    /*
     * 按下瞬间的窗口原点（逻辑像素）。
     *
     * 一次性 IPC：起拖前窗口不动，这个锚点恒有效；位移一律"相对按下点"计算，
     * 因此即使它晚一点回来，跟随也不会跳变——只是迟一帧才开始动。
     */
    let origin: { x: number; y: number } | null = null;
    void win
      .outerPosition()
      .then((pos) => {
        origin = { x: pos.x / scale, y: pos.y / scale };
      })
      .catch(() => {});

    let dragging = false;

    function finish(): void {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", finish);
    }

    function onMove(ev: MouseEvent): void {
      // 兜底：mouseup 被吞（主线程繁忙）→ 已松开即收摊，不留在拖动态
      if (ev.buttons === 0) {
        finish();
        return;
      }
      if (!dragging) {
        // 未越阈值：仍视作一次点击，不移动窗口、不打断 click 派发
        if (
          Math.abs(ev.screenX - startX) < DRAG_THRESHOLD_PX &&
          Math.abs(ev.screenY - startY) < DRAG_THRESHOLD_PX
        ) {
          return;
        }
        dragging = true;
      }
      if (!origin) return;
      void win
        .setPosition(
          new LogicalPosition(
            origin.x + (ev.screenX - startX),
            origin.y + (ev.screenY - startY)
          )
        )
        .catch(() => {});
    }

    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", finish);
  }, []);

  return { onDragHandle: handleMouseDown };
}
