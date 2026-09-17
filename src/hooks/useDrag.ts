// src/hooks/useDrag.ts
// 窗口拖拽 Hook
//
// 职责：管理窗口拖拽的完整生命周期（mousedown → mousemove → mouseup），
//       处理 DPI 缩放修正。返回 onDragHandle 回调，调用方挂到任意元素的
//       onMouseDown 即可。
//
// 从旧 App.tsx 的 handleMouseDown + dragging useEffect 提取。

import { useState, useRef, useCallback, useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { LogicalPosition } from "@tauri-apps/api/dpi";

export function useDrag(): {
  onDragHandle: (e: React.MouseEvent) => void;
} {
  const [dragging, setDragging] = useState(false);

  // 锚点：鼠标按下瞬间的快照。用 ref 而非 state，
  // 因为 mousemove 每帧更新直接用最新值即可，不需要触发重渲染
  const anchor = useRef({ mouseX: 0, mouseY: 0, winX: 0, winY: 0 });

  // ── mousedown：记录锚点，启动拖拽 ──
  const handleMouseDown = useCallback(async (e: React.MouseEvent) => {
    // outerPosition 返回物理像素坐标
    const pos = await getCurrentWindow().outerPosition();
    const scale = window.devicePixelRatio || 1;

    anchor.current = {
      mouseX: e.screenX,            // 鼠标屏幕坐标（物理像素）
      mouseY: e.screenY,
      winX: pos.x / scale,          // 窗口左上角 → 逻辑像素
      winY: pos.y / scale,
    };
    setDragging(true);
  }, []);

  // ── dragging 变为 true 时注册 document 级监听 ──
  useEffect(() => {
    if (!dragging) return;

    const win = getCurrentWindow();

    const onMouseMove = async (e: MouseEvent) => {
      // 新窗口位置 = 锚点窗口坐标 + 鼠标移动偏移量（全部逻辑像素）
      const newX = anchor.current.winX + (e.screenX - anchor.current.mouseX);
      const newY = anchor.current.winY + (e.screenY - anchor.current.mouseY);
      await win.setPosition(new LogicalPosition(newX, newY));
    };

    const onMouseUp = () => setDragging(false);

    // 挂在 document 上，鼠标移出窗口边界后拖拽不中断
    document.addEventListener("mousemove", onMouseMove);
    document.addEventListener("mouseup", onMouseUp);

    return () => {
      document.removeEventListener("mousemove", onMouseMove);
      document.removeEventListener("mouseup", onMouseUp);
    };
  }, [dragging]);

  return { onDragHandle: handleMouseDown };
}
