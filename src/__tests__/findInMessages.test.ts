// @vitest-environment jsdom
// findInMessages · 会话内查找匹配（纯函数）
//
// 覆盖：字面匹配、大小写不敏感、文档顺序、同节点不重叠、跨节点不匹配、
//       空查询、[data-findbar] 排除。

import { afterEach, describe, expect, it } from "vitest";
import { findMatches } from "../findInMessages";

function mount(html: string): HTMLElement {
  const root = document.createElement("div");
  root.innerHTML = html;
  document.body.appendChild(root);
  return root;
}

afterEach(() => {
  document.body.replaceChildren();
});

describe("findInMessages", () => {
  it("字面匹配、大小写不敏感、按文档顺序", () => {
    const root = mount("<p>Alpha alpha</p><p>ALPHA</p>");
    const ranges = findMatches(root, "alpha");
    expect(ranges.length).toBe(3);
    expect(ranges.map((r) => r.toString())).toEqual(["Alpha", "alpha", "ALPHA"]);
  });

  it("同一文本节点内不重叠匹配", () => {
    const root = mount("<p>aaaa</p>");
    const ranges = findMatches(root, "aa");
    expect(ranges.length).toBe(2);
    expect(ranges[0].startOffset).toBe(0);
    expect(ranges[1].startOffset).toBe(2);
  });

  it("跨节点不匹配（v1 单节点内语义）", () => {
    const root = mount("<p>alp<b>ha</b></p>");
    expect(findMatches(root, "alpha").length).toBe(0);
  });

  it("空查询返回空数组", () => {
    const root = mount("<p>alpha</p>");
    expect(findMatches(root, "")).toEqual([]);
  });

  it("排除 data-findbar 容器内的文本", () => {
    const root = mount(`<p>a/b</p><div data-findbar=""><span>3/17</span></div>`);
    const ranges = findMatches(root, "/");
    expect(ranges.length).toBe(1);
    expect(ranges[0].startContainer.parentElement?.tagName).toBe("P");
  });
});
