// src/components/overlay/ToastHost.tsx
// Toast 浮层宿主（M3-B §5.2）
//
// 职责：订阅 useToastStore.toasts，把队列渲染为顶部轻提示小条。
// 点击小条 dismiss（手动关闭）。
//
// 挂载：App 内 OverlayContainer 同级（overlay=null 也渲染本宿主）；
// 实际渲染 createPortal 到 document.body——DroneShellScene 3D transform
// 祖先会破坏 fixed 定位（同 MessageList 右键菜单先例），body 直挂保证
// 「fixed 最高 z-index 覆盖 settings overlay 之上」。

import { createPortal } from "react-dom";
import { useToastStore } from "../../stores/useToastStore";
import styles from "./ToastHost.module.css";

export function ToastHost() {
  const toasts = useToastStore((s) => s.toasts);
  const dismiss = useToastStore((s) => s.dismiss);
  const activate = useToastStore((s) => s.activate);

  if (toasts.length === 0) return null;

  return createPortal(
    <div className={styles.host}>
      {toasts.map((t) => (
        <div
          key={t.id}
          className={`${styles.toast} ${
            t.kind === "warning" ? styles.warning : styles.info
          }${t.onClick ? ` ${styles.actionable}` : ""}`}
          onClick={() => (t.onClick ? activate(t.id) : dismiss(t.id))}
          title={t.onClick ? "点击查看" : "点击关闭"}
        >
          {t.message}
        </div>
      ))}
    </div>,
    document.body
  );
}
