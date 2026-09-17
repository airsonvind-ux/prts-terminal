// src/contextMenu.ts
// 右键菜单 · 统一判定与应用级动作
//
// 全应用唯一规则（不看页面，只看"目标元素 + 选择状态"）：
//   可编辑域（input/textarea）：
//     有选中 → 剪切 / 复制 / 粘贴 / 全选
//     无选中 → 粘贴 / 全选
//   非编辑域有选中 → 复制
//   其余 → 无菜单（静默阻止浏览器菜单）
//
// 动作实现：剪贴板走 navigator.clipboard（菜单点击 = 用户手势，授权可靠）；
// 文本修改走 setRangeText + 手动 input 事件——React 受控同步的标准路径：
// 原生 setRangeText 绕过 React 的实例级 value 追踪（tracker），dispatch 后
// updateValueIfChanged 检测到 DOM 值变化，正常触发 onChange。

export type CtxAction = "cut" | "copy" | "paste" | "selectAll";

export interface CtxMenuDef {
  id: CtxAction;
  label: string;
}

type Editable = HTMLInputElement | HTMLTextAreaElement;

/** 命中可编辑域（input / textarea，向上查找） */
export function findEditable(target: EventTarget | null): Editable | null {
  if (!(target instanceof HTMLElement)) return null;
  const el = target.closest("input, textarea");
  return el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement
    ? el
    : null;
}

/** 编辑域内是否有选中（number 等不支持 selection 的类型恒为 false） */
export function hasFieldSelection(el: Editable): boolean {
  const { selectionStart, selectionEnd } = el;
  return (
    selectionStart != null && selectionEnd != null && selectionStart !== selectionEnd
  );
}

/** 编辑域内的选中文本 */
export function getFieldSelection(el: Editable): string {
  const { selectionStart, selectionEnd } = el;
  if (selectionStart == null || selectionEnd == null) return "";
  return el.value.slice(selectionStart, selectionEnd);
}

/** 页面（非编辑域）选中文本；trim 后为空视为无选中 */
export function getPageSelection(): string {
  return window.getSelection()?.toString().trim() ?? "";
}

/** 统一判定：给出菜单项；null = 无菜单（静默） */
export function resolveMenuItems(opts: {
  editable: boolean;
  hasFieldSelection: boolean;
  selectedText: string;
}): CtxMenuDef[] | null {
  if (opts.editable) {
    const items: CtxMenuDef[] = [];
    if (opts.hasFieldSelection) {
      items.push({ id: "cut", label: "剪切" });
      items.push({ id: "copy", label: "复制" });
    }
    items.push({ id: "paste", label: "粘贴" });
    items.push({ id: "selectAll", label: "全选" });
    return items;
  }
  if (opts.selectedText) return [{ id: "copy", label: "复制" }];
  return null;
}

// ── 动作 ──

/** 复制文本到剪贴板；返回是否成功（失败静默——剪切以此判断是否删除） */
async function copyText(text: string): Promise<boolean> {
  if (!text) return false;
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/** 替换编辑域 [start, end) 并触发 input（光标落到插入内容之后） */
function replaceRange(el: Editable, text: string, start: number, end: number): void {
  el.focus();
  try {
    el.setRangeText(text, start, end, "end");
  } catch {
    // number 等不支持选区操作的类型：降级为原型 setter（绕过实例级追踪，
    // input 事件仍触发 React onChange）
    const proto = Object.getPrototypeOf(el) as object;
    const setter = Object.getOwnPropertyDescriptor(proto, "value")?.set;
    const next = el.value.slice(0, start) + text + el.value.slice(end);
    setter?.call(el, next);
  }
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

/**
 * 执行菜单动作。
 * @param el 可编辑域（剪切/粘贴/全选需要；复制可空）
 * @param pageText 非编辑域选中文本（复制用）
 */
export async function performCtxAction(
  action: CtxAction,
  el: Editable | null,
  pageText: string
): Promise<void> {
  switch (action) {
    case "copy":
      await copyText(el ? getFieldSelection(el) : pageText);
      break;
    case "cut": {
      if (!el) return;
      const start = el.selectionStart ?? 0;
      const end = el.selectionEnd ?? start;
      // 先复制，成功才删除（失败时保持原状，避免"剪切"变"删除"）
      if (!(await copyText(el.value.slice(start, end)))) return;
      replaceRange(el, "", start, end);
      break;
    }
    case "paste": {
      if (!el) return;
      let text = "";
      try {
        text = await navigator.clipboard.readText();
      } catch {
        return; // clipboard read 可能被拒绝
      }
      if (!text) return;
      const start = el.selectionStart ?? el.value.length;
      const end = el.selectionEnd ?? start;
      replaceRange(el, text, start, end);
      break;
    }
    case "selectAll":
      if (!el) return;
      el.focus();
      el.select();
      break;
  }
}
