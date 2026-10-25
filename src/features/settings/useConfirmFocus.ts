// 设置页内确认浮层的焦点契约。
//
// 这些浮层嵌套在外层覆盖层对话框里，所以需要自己的初始焦点、Tab 循环、
// Escape 和关闭还原；处理过的按键要阻止冒泡，避免外层把整个设置页关掉。

import {
  useCallback,
  useEffect,
  useRef,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";

const FOCUSABLE = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

function focusableNodes(root: HTMLElement | null): HTMLElement[] {
  if (!root) return [];
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (el) =>
      el.tabIndex >= 0 &&
      !el.hasAttribute("hidden") &&
      !el.closest('[aria-hidden="true"]') &&
      !el.closest("[inert]")
  );
}

export function useConfirmFocus(open: boolean, onClose: () => void) {
  const modalRef = useRef<HTMLDivElement>(null);
  const returnFocusRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!open) return;

    returnFocusRef.current =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;

    const root = modalRef.current;
    const preferred = root?.querySelector<HTMLElement>(
      "[data-confirm-cancel]"
    );
    const target = preferred ?? focusableNodes(root)[0] ?? root;
    target?.focus();

    return () => {
      const previous = returnFocusRef.current;
      returnFocusRef.current = null;
      if (
        previous?.isConnected &&
        previous.tabIndex >= 0 &&
        !previous.hasAttribute("disabled")
      ) {
        previous.focus();
        return;
      }
      document
        .querySelector<HTMLElement>('[data-guide="settings-body"]')
        ?.focus();
    };
  }, [open]);

  const handleKeyDown = useCallback(
    (event: ReactKeyboardEvent<HTMLDivElement>) => {
      if (event.key === "Escape") {
        if (event.nativeEvent.isComposing) return;
        event.preventDefault();
        event.stopPropagation();
        onClose();
        return;
      }

      if (event.key !== "Tab") return;
      const root = modalRef.current;
      if (!root) return;

      const nodes = focusableNodes(root);
      if (nodes.length === 0) {
        event.preventDefault();
        event.stopPropagation();
        root.focus();
        return;
      }

      const active = document.activeElement as HTMLElement | null;
      const index = active ? nodes.indexOf(active) : -1;

      // 嵌套确认框不能用浏览器默认 Tab：焦点在中间时默认会走到
      // 外层设置页的控件。这里每次都手动取下一个/上一个，确保只在框内循环。
      const nextIndex = event.shiftKey
        ? index <= 0
          ? nodes.length - 1
          : index - 1
        : index === -1 || index === nodes.length - 1
          ? 0
          : index + 1;

      event.preventDefault();
      event.stopPropagation();
      nodes[nextIndex].focus();
    },
    [onClose]
  );

  return { modalRef, handleKeyDown };
}
