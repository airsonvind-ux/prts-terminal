// src/findInMessages.ts
// 会话内查找 · 匹配逻辑（纯函数）
//
// 语义：字面精确、大小写不敏感；遍历目标容器内全部文本节点，逐节点按
// 文档顺序收集命中 Range（同节点内不重叠；跨节点不匹配——v1 语义）。
// 查找条自身以 [data-findbar] 标记排除（避免自匹配）。

/** 查找条自身的容器标记（排除自匹配用） */
export const FINDBAR_ATTR = "data-findbar";

/**
 * 在 root 内查找 query 的全部命中。
 * @param root 查找范围根元素（消息区滚动容器）
 * @param query 查询词（空串返回空数组）
 * @returns 按文档顺序排列的命中 Range 列表
 */
export function findMatches(root: HTMLElement, query: string): Range[] {
  const q = query.toLowerCase();
  if (!q) return [];

  const ranges: Range[] = [];
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      if (!node.nodeValue) return NodeFilter.FILTER_REJECT;
      const parent = node.parentElement;
      if (parent && parent.closest(`[${FINDBAR_ATTR}]`)) {
        return NodeFilter.FILTER_REJECT;
      }
      return NodeFilter.FILTER_ACCEPT;
    },
  });

  let node = walker.nextNode() as Text | null;
  while (node) {
    const text = node.nodeValue!.toLowerCase();
    let from = 0;
    for (;;) {
      const idx = text.indexOf(q, from);
      if (idx === -1) break;
      const range = document.createRange();
      range.setStart(node, idx);
      range.setEnd(node, idx + q.length);
      ranges.push(range);
      from = idx + q.length;
    }
    node = walker.nextNode() as Text | null;
  }
  return ranges;
}
