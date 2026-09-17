// src/components/overlay/ContextMenuHost.tsx
// 右键菜单 · 全局宿主
//
// 唯一监听者：document 冒泡阶段的 contextmenu。
// 已被组件处理的（defaultPrevented）跳过；其余按统一规则接住：
//   可编辑域 → 剪切/复制/粘贴/全选（按可用性）
//   非编辑域有选中 → 复制
//   其余 → 静默阻止（浏览器菜单在全应用不再出现）
//
// 监听放 document（冒泡末梢）：React 合成处理器（root 容器）先执行，
// defaultPrevented 可准确区分"已处理"与"未处理"。

import { useEffect, useState } from "react";
import { ContextMenu } from "./ContextMenu";
import {
  findEditable,
  getPageSelection,
  hasFieldSelection,
  performCtxAction,
  resolveMenuItems,
  type CtxMenuDef,
} from "../../contextMenu";

type Editable = HTMLInputElement | HTMLTextAreaElement;

interface MenuState {
  x: number;
  y: number;
  defs: CtxMenuDef[];
  editable: Editable | null;
  pageText: string;
}

export function ContextMenuHost() {
  const [menu, setMenu] = useState<MenuState | null>(null);

  useEffect(() => {
    const onContextMenu = (e: MouseEvent) => {
      if (e.defaultPrevented) return; // 组件已处理

      const editable = findEditable(e.target);
      const selectedText = getPageSelection();

      const defs = resolveMenuItems({
        editable: editable != null,
        hasFieldSelection: editable ? hasFieldSelection(editable) : false,
        selectedText,
      });

      e.preventDefault(); // 统一阻止浏览器菜单（无论是否显示自绘菜单）
      if (!defs) return;

      setMenu({
        x: e.clientX,
        y: e.clientY,
        defs,
        editable,
        pageText: selectedText,
      });
    };

    document.addEventListener("contextmenu", onContextMenu);
    return () => document.removeEventListener("contextmenu", onContextMenu);
  }, []);

  if (!menu) return null;

  return (
    <ContextMenu
      x={menu.x}
      y={menu.y}
      items={menu.defs.map((d) => ({
        label: d.label,
        onSelect: () => void performCtxAction(d.id, menu.editable, menu.pageText),
      }))}
      onClose={() => setMenu(null)}
    />
  );
}
