// src/hooks/useResize.ts
// 窗口缩放 Hook（多方向）
//
// 职责：热区 mousedown(dir) → 快照锚角屏幕坐标 → mousemove 反算 face →
//       同步 CSS 变量 + setPosition/setSize。
//
// 锚点 = 热区对角（永远固定于屏幕）："拖哪边，窗口向哪边生长"。
// 几何反算与跟手性推导见 resizeGeometry.ts。
//
// 注意：左/上系方向拖动期间窗口位置会变，因此位置换算一律以
// "锚角屏幕坐标 + 光标"为基准，不依赖 outerPosition 的实时值。
//
// 装配时机（2026-09 加固）：监听在 mousedown 内**同步**装上，不再等 IPC
// 再经状态/effect 装配——与 useDrag 同源的竞态（主线程繁忙时整段手势落在
// 装配空窗里、漏掉的 mouseup 让状态一直挂着）。另加"鼠标已松开即收摊"兜底。
// 锚角仍来自一次 IPC：到位前只等不动作（computeResize 是绝对反算，晚到
// 只会迟一帧，不会跳变）。

import { useCallback } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalPosition, LogicalSize } from "@tauri-apps/api/dpi";
import {
  anchorPoint,
  computeResize,
  RATIO_X,
  RATIO_Y,
  RESIZE_SPECS,
  type ResizeAnchor,
  type ResizeDirection,
} from "./resizeGeometry";

export function useResize(): {
  onResizeHandle: (e: React.MouseEvent, dir: ResizeDirection) => void;
} {
  const handleMouseDown = useCallback(
    (e: React.MouseEvent, dir: ResizeDirection) => {
      e.stopPropagation(); // 阻断拖拽：热区与拖动面各自独立
      if (e.button !== 0) return; // 只认左键

      const win = getCurrentWindow();
      const scale = window.devicePixelRatio || 1;
      /** 锚角（逻辑域）。IPC 到位前为 null：此间的移动只等不动作 */
      let anchor: ResizeAnchor | null = null;

      function finish(): void {
        document.removeEventListener("mousemove", onMouseMove);
        document.removeEventListener("mouseup", finish);
      }

      function onMouseMove(ev: MouseEvent): void {
        // 兜底：mouseup 被吞（主线程繁忙）→ 松开即收摊，不留在拖动态
        if (ev.buttons === 0) {
          finish();
          return;
        }
        if (!anchor) return;
        const { face, x, y } = computeResize(dir, ev.screenX, ev.screenY, anchor);
        const w = face * RATIO_X;
        const h = face * RATIO_Y;

        // 先更新 CSS 自定义属性（同步），再并行提交位置与尺寸（异步）。
        document.documentElement.style.setProperty("--face-size", `${face}px`);
        void Promise.all([
          win.setPosition(new LogicalPosition(x, y)),
          win.setSize(new LogicalSize(w, h)),
        ]);
      }

      document.addEventListener("mousemove", onMouseMove);
      document.addEventListener("mouseup", finish);

      void Promise.all([win.outerPosition(), win.outerSize()]).then(
        ([pos, size]) => {
          anchor = anchorPoint(
            RESIZE_SPECS[dir].corner,
            pos.x / scale,
            pos.y / scale,
            size.width / scale,
            size.height / scale
          );
        }
      );
    },
    []
  );

  return { onResizeHandle: handleMouseDown };
}
