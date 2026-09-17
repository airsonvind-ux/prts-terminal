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

import { useState, useRef, useCallback, useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalPosition, LogicalSize } from "@tauri-apps/api/dpi";
import {
  anchorPoint,
  computeResize,
  RATIO,
  RESIZE_SPECS,
  type ResizeAnchor,
  type ResizeDirection,
} from "./resizeGeometry";

export function useResize(): {
  onResizeHandle: (e: React.MouseEvent, dir: ResizeDirection) => void;
} {
  const [resizing, setResizing] = useState(false);

  // mousedown 快照：锚角屏幕坐标（逻辑域）+ 方向。拖动期间不变。
  const session = useRef<{ anchor: ResizeAnchor; dir: ResizeDirection } | null>(
    null
  );

  // ── mousedown：快照锚角，启动缩放 ──
  const handleMouseDown = useCallback(
    async (e: React.MouseEvent, dir: ResizeDirection) => {
      e.stopPropagation(); // 阻断拖拽：热区与拖动面各自独立

      const win = getCurrentWindow();
      const scale = window.devicePixelRatio || 1;
      const [pos, size] = await Promise.all([
        win.outerPosition(),
        win.outerSize(),
      ]);

      const winX = pos.x / scale;
      const winY = pos.y / scale;
      const winSize = size.width / scale; // 正方窗口

      session.current = {
        anchor: anchorPoint(RESIZE_SPECS[dir].corner, winX, winY, winSize),
        dir,
      };
      setResizing(true);
    },
    []
  );

  // ── resizing 变为 true 时注册 document 级监听 ──
  useEffect(() => {
    if (!resizing) return;

    const win = getCurrentWindow();

    const onMouseMove = (e: MouseEvent) => {
      const s = session.current;
      if (!s) return;

      const { face, x, y } = computeResize(s.dir, e.screenX, e.screenY, s.anchor);
      const size = face * RATIO;

      // 先更新 CSS 自定义属性（同步），再并行提交位置与尺寸（异步）。
      document.documentElement.style.setProperty("--face-size", `${face}px`);
      void Promise.all([
        win.setPosition(new LogicalPosition(x, y)),
        win.setSize(new LogicalSize(size, size)),
      ]);
    };

    const onMouseUp = () => setResizing(false);

    document.addEventListener("mousemove", onMouseMove);
    document.addEventListener("mouseup", onMouseUp);

    return () => {
      document.removeEventListener("mousemove", onMouseMove);
      document.removeEventListener("mouseup", onMouseUp);
    };
  }, [resizing]);

  return { onResizeHandle: handleMouseDown };
}
