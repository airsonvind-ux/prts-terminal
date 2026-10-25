// src/components/overlay/OverlayContainer.tsx
// 覆盖层调度器
//
// 职责：订阅 useUIStore.overlay，按类型路由到对应面板；接管覆盖层的
// 焦点契约（初始焦点、Tab 循环、Escape、关闭还原）与遮罩关闭。
// 自身不承载业务状态，只保留焦点/还原所需的 ref 与 effect。
//
// 渲染位置：frontFace 内部，position:absolute 叠在 ChatView 之上。

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { useUIStore } from "../../stores/useUIStore";
import { hasPendingApproval, useProcessingStore } from "../../stores/useProcessingStore";
import { HistoryPanel } from "./HistoryPanel";
import { SettingsPanel } from "../settings/SettingsPanel";
import styles from "./OverlayContainer.module.css";

const FOCUSABLE = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

/** 覆盖层内可聚焦节点；排除 hidden、aria-hidden 和 inert 子树。 */
function focusableNodes(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (el) =>
      el.tabIndex >= 0 &&
      !el.hasAttribute("hidden") &&
      !el.closest('[aria-hidden="true"]') &&
      !el.closest("[inert]")
  );
}

export function OverlayContainer() {
  const overlay = useUIStore((s) => s.overlay);
  const setOverlay = useUIStore((s) => s.setOverlay);
  const pendingApproval = useProcessingStore(hasPendingApproval);

  const dialogRef = useRef<HTMLDivElement>(null);
  const returnFocusRef = useRef<HTMLElement | null>(null);
  const lastOverlayRef = useRef<typeof overlay>(null);

  // 打开前保存触发点。layout effect 早于 ChatView 的 inert 副作用，
  // 否则 activeElement 会先被重置为 body。
  useLayoutEffect(() => {
    if (overlay !== null && lastOverlayRef.current === null) {
      returnFocusRef.current =
        document.activeElement instanceof HTMLElement
          ? document.activeElement
          : null;
    }
  }, [overlay]);

  // 初始焦点与关闭还原。关闭时等 ChatView 撤掉 inert 再聚焦，
  // 避免把焦点送到一个仍然不可交互的节点上。
  useEffect(() => {
    const previous = lastOverlayRef.current;
    lastOverlayRef.current = overlay;

    if (overlay !== null) {
      const root = dialogRef.current;
      if (!root) return;
      const preferred = root.querySelector<HTMLElement>(
        "[data-overlay-autofocus]"
      );
      const target = preferred ?? focusableNodes(root)[0] ?? root;
      target.focus();
      return;
    }

    if (previous === null) return;

    const target = returnFocusRef.current;
    returnFocusRef.current = null;
    const timer = window.setTimeout(() => {
      if (
        target?.isConnected &&
        target.tabIndex >= 0 &&
        !target.hasAttribute("disabled")
      ) {
        target.focus();
        return;
      }
      const fallback =
        document.querySelector<HTMLElement>('[data-guide="new-chat"]') ??
        document.querySelector<HTMLElement>('[data-guide="input"] textarea');
      fallback?.focus();
    }, 0);
    return () => window.clearTimeout(timer);
  }, [overlay]);

  const close = useCallback(() => setOverlay(null), [setOverlay]);

  const handleKeyDown = useCallback(
    (event: ReactKeyboardEvent<HTMLDivElement>) => {
      if (event.key === "Escape") {
        // 输入法组合中的 Escape 属于输入法；内层编辑/确认框已处理的事件
        // 不应再被覆盖层消费。
        if (event.defaultPrevented || event.nativeEvent.isComposing) return;
        event.preventDefault();
        close();
        return;
      }

      if (event.key !== "Tab") return;
      const root = dialogRef.current;
      if (!root) return;

      const nodes = focusableNodes(root);
      if (nodes.length === 0) {
        event.preventDefault();
        root.focus();
        return;
      }

      const first = nodes[0];
      const last = nodes[nodes.length - 1];
      const active = document.activeElement as HTMLElement | null;

      if (!active || !root.contains(active)) {
        event.preventDefault();
        (event.shiftKey ? last : first).focus();
        return;
      }

      if (event.shiftKey && (active === first || active === root)) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && active === last) {
        event.preventDefault();
        first.focus();
      }
    },
    [close]
  );

  // 无覆盖层时完全不渲染，DOM 树中无额外节点
  if (overlay === null) return null;

  /*
   * 面板路由：switch 语句根据 overlay 类型决定渲染哪个组件。
   *
   * 同时返回面板尺寸（calc 绑定 --face-size，随窗口缩放自动调整）：
   *   history  → 0.67×0.64，窄面板容纳"标题+时间+删除键"三列
   *   settings → 0.72×0.66，双栏表单所需最小宽度
   */
  const renderPanel = () => {
    switch (overlay) {
      case "history":
        return {
          node: <HistoryPanel />,
          style: {
            width: "calc(var(--face-size) * 0.67)",
            maxHeight: "calc(var(--face-size) * 0.64)",
          },
        };

      case "settings":
        return {
          node: <SettingsPanel />,
          style: {
            width: "calc(var(--face-size) * 0.72)",
            height: "calc(var(--face-size) * 0.66)",
          },
        };

      default:
        return null;
    }
  };

  const panel = renderPanel();
  const label = overlay === "history" ? "历史对话" : "设置";

  return (
    <div
      ref={dialogRef}
      className={styles.overlay}
      role="dialog"
      aria-modal="true"
      aria-label={label}
      tabIndex={-1}
      onKeyDown={handleKeyDown}
    >
      {/* 遮罩层：绝对定位覆盖整个正面，半透明灰底 */}
      <div
        className={styles.backdrop}
        aria-hidden="true"
        onClick={close}
      />
      {/* 面板：定位在遮罩之上，内容由 renderPanel 路由 */}
      {panel && (
        <div
          className={styles.panel}
          style={panel.style}
          data-overlay-panel=""
          /* 阻止面板内点击冒泡到遮罩——否则点面板也会关闭 */
          onClick={(e) => e.stopPropagation()}
        >
          {panel.node}
        </div>
      )}
      {pendingApproval && (
        <button
          className={styles.pendingNotice}
          onClick={close}
          type="button"
        >
          命令待确认 · 返回对话
        </button>
      )}
    </div>
  );
}
