// src/components/overlay/ContextMenu.tsx
// 右键菜单 · 统一渲染组件
//
// Portal 到 body（避免 3D transform / overflow:hidden 祖先破坏 fixed 定位）。
// 全应用共用：外观与交互的唯一来源（此前 MessageList / InputArea 各有一份拷贝）。

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import styles from "./ContextMenu.module.css";

export interface ContextMenuItem {
  label: string;
  onSelect: () => void;
}

interface Props {
  x: number;
  y: number;
  items: ContextMenuItem[];
  onClose: () => void;
}

/** 贴边夹紧：菜单完整落在视口内（右下越界时向左/上收） */
export function clampMenuPosition(
  x: number,
  y: number,
  w: number,
  h: number,
  vw: number,
  vh: number
): { x: number; y: number } {
  const m = 4;
  return {
    x: Math.max(m, Math.min(x, vw - w - m)),
    y: Math.max(m, Math.min(y, vh - h - m)),
  };
}

export function ContextMenu({ x, y, items, onClose }: Props) {
  const menuRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ x, y });

  // 首帧测量后贴边夹紧（先按原坐标渲染以避免闪跳）
  useLayoutEffect(() => {
    const el = menuRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    setPos(
      clampMenuPosition(x, y, rect.width, rect.height, window.innerWidth, window.innerHeight)
    );
  }, [x, y]);

  // 点击其他区域关闭
  useEffect(() => {
    const close = () => onClose();
    document.addEventListener("click", close);
    return () => document.removeEventListener("click", close);
  }, [onClose]);

  return createPortal(
    <>
      <div className={styles.ctxOverlay} onContextMenu={(e) => e.preventDefault()} />
      <div
        ref={menuRef}
        className={styles.ctxMenu}
        style={{ left: pos.x, top: pos.y }}
        onContextMenu={(e) => e.preventDefault()}
      >
        {items.map((item) => (
          <button
            key={item.label}
            className={styles.ctxItem}
            onClick={() => {
              onClose();
              item.onSelect();
            }}
          >
            {item.label}
          </button>
        ))}
      </div>
    </>,
    document.body
  );
}
