// @vitest-environment jsdom
// Markdown 渲染器测试
//
// 模块 F · 批 2-A+B 扩展：
//   - allocateCols 纯函数测试（原有，jsdom 环境下同样成立）
//   - reconcileTopLevelChildren 顶层节点 reconcile（设计三）
//   - getTableLayouts 表格测量缓存（设计二）
//   - 组件级端到端：假表格重建的内联宽度 style 不被消毒剥离（设计四管线顺序）
//
// jsdom 无 ResizeObserver / Canvas 2D：组件级用例 stub 最小实现；
// 纯函数与 reconcile 用例不依赖这两者。

import { describe, expect, it, beforeAll, afterAll } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import {
  Markdown,
  allocateCols,
  getTableLayouts,
  reconcileTopLevelChildren,
} from "./Markdown";

// ═════════════════════════════════════════════════════
// allocateCols（原有测试，保持不动）
// ═════════════════════════════════════════════════════

describe("allocateCols", () => {
  it("总需求不超过预算时全部按需求宽度", () => {
    // 两列窄表头长内容：预算充足场景，内容应完整撑开
    const demands = [60, 400];
    const mins = [40, 40];
    const cols = allocateCols(demands, mins, 500);
    expect(cols).toEqual([60, 400]);
  });

  it("budget 非正视为未测量，全部按需", () => {
    const cols = allocateCols([100, 200], [40, 40], 0);
    expect(cols).toEqual([100, 200]);
  });

  it("超预算时按可压缩空间占比削减，每列不低于最小宽", () => {
    // 预算 300，需求 60+400=460，需削减 160
    // 可压缩空间：列0 = 60-40=20，列1 = 400-40=360，总计 380
    // 列0 削减 160×20/380≈8.4 → 51.6；列1 削减 160×360/380≈151.6 → 248.4
    const demands = [60, 400];
    const mins = [40, 40];
    const cols = allocateCols(demands, mins, 300);

    // 每列不低于最小宽
    expect(cols[0]).toBeGreaterThanOrEqual(40);
    expect(cols[1]).toBeGreaterThanOrEqual(40);
    // 总宽贴近预算（浮点削减后之和 ≤ 预算 + 小数误差）
    const total = cols.reduce((s, w) => s + w, 0);
    expect(total).toBeLessThanOrEqual(300 + 1e-6);
    expect(total).toBeGreaterThan(299 - 1e-6);
    // 削减比例与可压缩空间占比一致（列1 削减远大于列0）
    const cut0 = 60 - cols[0];
    const cut1 = 400 - cols[1];
    expect(cut1).toBeGreaterThan(cut0 * 10);
  });

  it("最小宽之和达到预算（极端窄窗）时全压到最小宽，交给滚动兜底", () => {
    const demands = [200, 300];
    const mins = [100, 100];
    const cols = allocateCols(demands, mins, 150);
    expect(cols).toEqual([100, 100]);
  });

  it("空表格退化：需求即最小宽时按需返回", () => {
    const cols = allocateCols([42, 42], [42, 42], 100);
    expect(cols).toEqual([42, 42]);
  });
});

// ═════════════════════════════════════════════════════
// reconcileTopLevelChildren（设计三 · DOM 层）
// ═════════════════════════════════════════════════════

describe("reconcileTopLevelChildren", () => {
  it("同签名顶层块保留 DOM 节点身份（引用不变，不打断选区）", () => {
    const root = document.createElement("div");
    root.innerHTML = "<p>一段</p><p>二段</p><pre><code>code</code></pre>";
    const firstP = root.childNodes[0];
    const secondP = root.childNodes[1];

    reconcileTopLevelChildren(
      root,
      "<p>一段</p><p>二段</p><pre><code>code</code></pre>"
    );

    expect(root.childNodes).toHaveLength(3);
    expect(root.childNodes[0]).toBe(firstP);
    expect(root.childNodes[1]).toBe(secondP);
    expect(root.childNodes[2].textContent).toBe("code");
  });

  it("异签名顶层块被替换，未变块保留", () => {
    const root = document.createElement("div");
    root.innerHTML = "<p>一段</p><p>二段</p>";
    const firstP = root.childNodes[0];

    reconcileTopLevelChildren(root, "<p>一段</p><p>二段改</p>");

    expect(root.childNodes[0]).toBe(firstP); // 未变块保留
    expect(root.childNodes).toHaveLength(2);
    expect(root.childNodes[1].textContent).toBe("二段改"); // 变化块替换
  });

  it("多裁少补：新内容短裁多余，长则追加", () => {
    const root = document.createElement("div");
    root.innerHTML = "<p>a</p><p>b</p><p>c</p>";

    // 少一个 → 裁掉多余旧节点
    reconcileTopLevelChildren(root, "<p>a</p><p>b</p>");
    expect(root.childNodes).toHaveLength(2);

    // 多一个 → 追加新节点
    reconcileTopLevelChildren(root, "<p>a</p><p>b</p><p>d</p>");
    expect(root.childNodes).toHaveLength(3);
    expect(root.childNodes[2].textContent).toBe("d");
  });

  it("假表格顶层 div（单个 tableWrap）同签名保留、内容变化替换", () => {
    const wrap = '<div class="wrap"><div class="ft">1</div></div>';
    const root = document.createElement("div");
    root.innerHTML = wrap;
    const node = root.childNodes[0];

    reconcileTopLevelChildren(root, wrap);
    expect(root.childNodes[0]).toBe(node); // 同签名保留
    reconcileTopLevelChildren(
      root,
      '<div class="wrap"><div class="ft">2</div></div>'
    );
    expect(root.childNodes[0].textContent).toBe("2");
    expect(root.childNodes[0]).not.toBe(node); // 内容变化替换
  });

  it("文本节点按 #text: 签名比较（含裸文本顶层节点场景）", () => {
    const root = document.createElement("div");
    root.innerHTML = "裸文本<p>p</p>";
    const textNode = root.childNodes[0];

    reconcileTopLevelChildren(root, "裸文本<p>p</p>");
    expect(root.childNodes[0]).toBe(textNode);
    expect(root.childNodes[0].nodeType).toBe(3);
  });
});

// ═════════════════════════════════════════════════════
// getTableLayouts（设计二 · 表格测量缓存）
// ═════════════════════════════════════════════════════

// calcLayout 依赖 Canvas 2D 测量，jsdom 无实现 → stub 最小 measureText
beforeAll(() => {
  HTMLCanvasElement.prototype.getContext = (() => ({
    measureText: (text: string) => ({ width: String(text).length * 8 }),
  })) as unknown as typeof HTMLCanvasElement.prototype.getContext;
});

afterAll(() => {
  // 恢复原型（jsdom 无 Canvas 2D 实现，避免影响其他测试文件）
  HTMLCanvasElement.prototype.getContext = (() => null) as unknown as typeof HTMLCanvasElement.prototype.getContext;
});

describe("getTableLayouts · 表格测量缓存", () => {
  it("相同表格内容 + 相同容器宽 → 返回同一引用（不重测）", () => {
    const tables = [{ headers: ["名称"], rows: [["源石虫"]], aligns: [] }];
    const a = getTableLayouts(tables, 400);
    const b = getTableLayouts(tables, 400);
    expect(a).toBe(b);
  });

  it("容器宽变化 → 重算；缩放回原宽 → 命中缓存", () => {
    const tables = [{ headers: ["名称"], rows: [["源石虫"]], aligns: [] }];
    const a = getTableLayouts(tables, 400);
    const b = getTableLayouts(tables, 600);
    expect(a).not.toBe(b);
    const c = getTableLayouts(tables, 400);
    expect(c).toBe(a);
  });

  it("表格内容变化 → 重算（新 key）", () => {
    const t1 = [{ headers: ["名称"], rows: [["源石虫"]], aligns: [] }];
    const t2 = [
      { headers: ["名称"], rows: [["源石虫"], ["士兵"]], aligns: [] },
    ];
    expect(getTableLayouts(t1, 400)).not.toBe(getTableLayouts(t2, 400));
  });

  it("容量上限 64：最旧条目被淘汰，最新条目命中", () => {
    const firstKey = [{ headers: ["h0"], rows: [["r0"]], aligns: [] }];
    const first = getTableLayouts(firstKey, 300);

    // 连续插入 64 个新 key（连同 first 共 65 个不同 key，超上限 64）
    for (let i = 1; i <= 64; i++) {
      getTableLayouts(
        [{ headers: [`h${i}`], rows: [[`r${i}`]], aligns: [] }],
        300
      );
    }

    // 最旧的 first 已被淘汰（重新测量 → 新引用）
    expect(getTableLayouts(firstKey, 300)).not.toBe(first);

    // 最新插入的 key 仍在缓存中（引用稳定）
    const lastKey = [{ headers: ["h64"], rows: [["r64"]], aligns: [] }];
    expect(getTableLayouts(lastKey, 300)).toBe(getTableLayouts(lastKey, 300));
  });
});

// ═════════════════════════════════════════════════════
// 组件级端到端（设计四 · 管线顺序）
// ═════════════════════════════════════════════════════

describe("Markdown 组件 · 渲染端到端", () => {
  const originalRO = (globalThis as { ResizeObserver?: unknown })
    .ResizeObserver;
  const originalActEnv = (
    globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }
  ).IS_REACT_ACT_ENVIRONMENT;

  beforeAll(() => {
    // jsdom 无 ResizeObserver，stub 最小实现（组件 useEffect 注册用）
    (globalThis as { ResizeObserver?: unknown }).ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    };
    // React 19 的 act() 需要显式环境标志（vitest jsdom 环境默认未开启）
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
      true;
  });

  afterAll(() => {
    if (originalRO === undefined) {
      delete (globalThis as { ResizeObserver?: unknown }).ResizeObserver;
    } else {
      (globalThis as { ResizeObserver?: unknown }).ResizeObserver = originalRO;
    }
    if (originalActEnv === undefined) {
      delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean })
        .IS_REACT_ACT_ENVIRONMENT;
    } else {
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
        originalActEnv;
    }
  });

  it("假表格重建的内联宽度 style 不被消毒剥离（消毒在重建之前）", () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root = createRoot(container);

    act(() => {
      root.render(
        React.createElement(Markdown, {
          content: "|名称|数值|\n|-|-|\n|源石虫|300|",
        })
      );
    });

    const html = container.innerHTML;
    // CSS modules 哈希类名含源类名片段
    expect(html).toContain("tableWrap");
    expect(html).toContain("fakeTable");
    // 重建注入的内联宽度保留（未被消毒剥离——消毒只作用于用户内容）
    expect(html).toContain("width:");
    expect(html).toContain("源石虫");

    act(() => root.unmount());
    container.remove();
  });

  it("GFM 对齐标记（:--- 语法）经消毒→提取→重建后应用 text-align", () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root = createRoot(container);

    act(() => {
      root.render(
        React.createElement(Markdown, {
          content: "|名称|数值|\n|:---|---:|\n|左|右|",
        })
      );
    });

    const html = container.innerHTML;
    // 对齐标记经过：marked(align 属性) → 消毒(白名单放行) → parseTables(读取)
    // → 假表格重建(inline text-align)，两端应分别应用 left / right
    expect(html).toContain("text-align:left");
    expect(html).toContain("text-align:right");

    act(() => root.unmount());
    container.remove();
  });
});
