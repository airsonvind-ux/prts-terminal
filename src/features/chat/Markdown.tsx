// src/components/content/Markdown.tsx
// Markdown 渲染器
//
// 表格处理：marked → DOMPurify 消毒 → DOM 解析表格数据 → Canvas 测量列宽 →
// 渲染为 div+flex 结构。列宽分配走总预算制（allocateCols）：
// 总需求 ≤ 容器宽 → 全部按需；超预算 → 按可压缩空间占比削减；
// 极端窄窗 → 压到最小宽（表头宽），由 overflow-x 滚动兜底。
//
// 流式渲染体系（模块 F · 批 2-A+B）：
// - 频率层：assistant/chunk 由 useProcessingStore 33ms 节流缓冲，本组件
//   收到的 content 更新频率已降至 ~30FPS
// - 转换层：marked → 消毒 → 表格提取拆为 [content] 依赖的 useMemo；列宽
//   （getTableLayouts，同内容同宽复用）+ 假表格重建拆为 [tables, containerW]
//   依赖——窗口缩放不再重跑 marked/DOM 解析
// - DOM 层：reconcileTopLevelChildren 顶层节点签名比较，未变块保留 DOM 节点
//   身份（不打断用户选区），多裁少补
// - 安全层：markdownSanitize（DOMPurify 白名单）拦截 LLM 注入的 raw HTML

import {
  memo,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { marked } from "marked";
import type { Tokenizer } from "marked";
import { invoke } from "@tauri-apps/api/core";
import { showToast } from "../../stores/useToastStore";
import { hardenRemoteImages, sanitizeMarkdownHtml } from "./markdownSanitize";
import styles from "./Markdown.module.css";

marked.setOptions({ breaks: true, gfm: true });

/**
 * GFM 删除线只认双波浪 `~~`。
 *
 * marked 的实现把左定界符放宽到单个 `~`（源码规则 `~~?`），于是
 * 「12~30，3~24」这类数值区间会被配成一对，中间那段被渲染成删除线。
 * 覆盖 `del` 分词器：非双波浪一律返回 undefined（= 不消费，交回普通文字）。
 * 注：marked 的 use() 约定——返回 false 会回落到内置实现，返回 undefined 不会。
 */
const STRICT_DEL = /^~~(?=[^\s~])([\s\S]*?[^\s~])~~(?=[^~]|$)/;

marked.use({
  tokenizer: {
    del(this: Tokenizer, src: string) {
      const m = STRICT_DEL.exec(src);
      if (!m) return undefined;
      return {
        type: "del",
        raw: m[0],
        text: m[1],
        tokens: this.lexer.inlineTokens(m[1]),
      };
    },
  },
});

interface Props {
  content: string;
}

// ── 表格数据 ──

interface ParsedTable {
  headers: string[];    // innerHTML（保留格式）
  rows: string[][];     // innerHTML
  /** 每列对齐标记（GFM :--- 语法 → marked 输出 th align 属性；无标记为空串） */
  aligns: string[];
}

/** DOM 解析表格，保留格内 HTML */
function parseTables(html: string): { tables: ParsedTable[]; cleanHtml: string } {
  const temp = document.createElement("div");
  temp.innerHTML = html;

  const tables: ParsedTable[] = [];
  const tableEls = temp.querySelectorAll("table");

  for (const tableEl of tableEls) {
    const table: ParsedTable = { headers: [], rows: [], aligns: [] };
    const ths = tableEl.querySelectorAll("th");
    table.headers = Array.from(ths).map((th) => th.innerHTML);
    table.aligns = Array.from(ths).map(
      (th) => th.getAttribute("align") ?? ""
    );

    const trs = tableEl.querySelectorAll("tr");
    for (const tr of trs) {
      // 跳过表头行
      if (tr.querySelector("th")) continue;
      const tds = tr.querySelectorAll("td");
      if (tds.length > 0) {
        table.rows.push(Array.from(tds).map((td) => td.innerHTML));
      }
    }

    tables.push(table);
    // 替换为占位标记
    const idx = tables.length - 1;
    const placeholder = document.createComment(`TABLE_${idx}`);
    tableEl.replaceWith(placeholder);
  }

  return { tables, cleanHtml: temp.innerHTML };
}

// ── 列宽计算（Canvas measureText，零 DOM 布局干扰） ──

interface TableLayout {
  colWidths: number[];
  totalW: number;
}

function calcLayout(table: ParsedTable, availableWidth: number): TableLayout {
  const n = table.headers.length;

  // Canvas 测量纯文本宽度（零布局干扰）
  const canvas = document.createElement("canvas");
  const ctx = canvas.getContext("2d")!;
  // 与渲染字体对齐：font-size = --text-md(14px, src/styles/tokens.css) ×
  // .fakeTable font-size: 0.9em(src/components/content/Markdown.module.css) = 12.6px；
  // font-family = --font-ui("Microsoft YaHei", sans-serif, tokens.css) 第一个具体字体名；
  // font-weight = 600（.fakeTh 半粗体表头——常规字重测量会让粗体表头欠量溢出）
  ctx.font = '600 12.6px "Microsoft YaHei"';

  const PAD_H = 16; // padding-left + padding-right (8+8)
  const BORDER_H = 2; // border-left + border-right (1+1)
  // 安全余量：Canvas 测量与浏览器渲染存在亚像素/hinting 差异，
  // 不加余量时短列（列名=内容宽度）余量不足 1px，缩放时渲染取整
  // 路径变化会让文本临界溢出（实测：列名 2 字内容 1 字的窄列）。
  // 实测 1.25 DPI 下 12.6px 雅黑两字渲染宽比测量宽 +2.8px，
  // 4px 余量覆盖至 1.5 DPI；每列 +1px 的代价可忽略。
  const SAFE_MARGIN = 4;

  const headerW: number[] = [];
  for (const th of table.headers) {
    const text = stripHtml(th);
    headerW.push(Math.ceil(ctx.measureText(text).width) + PAD_H + BORDER_H + SAFE_MARGIN);
  }

  const cellMax = new Array(n).fill(0);
  for (const row of table.rows) {
    for (let i = 0; i < n; i++) {
      const text = stripHtml(row[i] ?? "");
      const w = Math.ceil(ctx.measureText(text).width) + PAD_H + BORDER_H + SAFE_MARGIN;
      cellMax[i] = Math.max(cellMax[i], w);
    }
  }

  const demands: number[] = [];
  for (let i = 0; i < n; i++) {
    // 列需求 = max(内容宽, 表头宽)：表头是渲染需求的一部分，
    // 只取内容宽会让「内容 < 表头」的列（如单位列 1 字 vs 表头 2 字）
    // 在全按需档被分到小于表头需求的宽度，表头文本溢出被相邻列盖住。
    demands.push(Math.max(cellMax[i], headerW[i]));
  }
  const colWidths = allocateCols(demands, headerW, availableWidth);
  return { colWidths, totalW: colWidths.reduce((s, w) => s + w, 0) };
}

/**
 * 列宽分配（总预算制，纯函数）。
 *
 * @param demands — 每列需求宽（单元格最大文本测量宽，不含上限钳制）
 * @param mins — 每列最小宽（表头宽，保证表头完整显示）
 * @param budget — 容器可用宽度（≤0 视为未测量，全部按需）
 *
 * 规则：总需求 ≤ 预算 → 全部按需；最小宽之和 ≥ 预算（极端窄窗）→
 * 全压到最小宽（溢出由 overflow-x 滚动兜底）；其余 → 按各列可压缩
 * 空间占比削减超预算部分。
 */
export function allocateCols(
  demands: number[],
  mins: number[],
  budget: number,
): number[] {
  const totalDemand = demands.reduce((s, w) => s + w, 0);
  const totalMin = mins.reduce((s, w) => s + w, 0);

  if (budget <= 0 || totalDemand <= budget) return demands.map((w) => w);
  if (totalMin >= budget) return mins.map((w) => w);

  const need = totalDemand - budget;
  const compressible = demands.map((w, i) => Math.max(0, w - mins[i]));
  const totalCompressible = compressible.reduce((s, c) => s + c, 0);

  const cols: number[] = [];
  for (let i = 0; i < demands.length; i++) {
    const cut =
      totalCompressible > 0 ? (need * compressible[i]) / totalCompressible : 0;
    cols.push(Math.max(mins[i], demands[i] - cut));
  }
  return cols;
}

function stripHtml(html: string): string {
  return html.replace(/<[^>]+>/g, "");
}

// ── 表格测量缓存（设计二 · 转换层） ──
//
// calcLayout 的 Canvas 测量成本随表格规模线性增长，流式期间每 33ms 一次
// content 变化都会触发重测。这里按「表格内容 + 容器宽」双键缓存：
// - 相同表格内容 + 相同 containerW → 复用列宽（同一引用，reconcile 签名稳定）
// - 容器宽变化（窗口缩放）→ 重测；缩放回原宽 → 命中缓存
// - 流式表格逐次增长 → 每次新 key miss（预期，正在生长的表格本就该重测）

const TABLE_LAYOUT_CACHE_MAX = 64;
const tableLayoutCache = new Map<string, TableLayout[]>();

/**
 * 计算（或复用）一组表格的列宽布局。
 * 命中缓存返回同一引用；未命中测量后写入缓存，超容量淘汰最久未用（Map 首项）。
 */
export function getTableLayouts(
  tables: ParsedTable[],
  containerW: number
): TableLayout[] {
  const key = `${JSON.stringify(tables)}|${containerW}`;
  const hit = tableLayoutCache.get(key);
  if (hit) {
    // LRU 提升：删除后重插，保持 Map 迭代顺序 = 最近使用序
    tableLayoutCache.delete(key);
    tableLayoutCache.set(key, hit);
    return hit;
  }

  const layouts = tables.map((t) => calcLayout(t, containerW));
  tableLayoutCache.set(key, layouts);
  if (tableLayoutCache.size > TABLE_LAYOUT_CACHE_MAX) {
    const oldest = tableLayoutCache.keys().next().value;
    if (oldest !== undefined) tableLayoutCache.delete(oldest);
  }
  return layouts;
}

// ── 顶层节点 reconcile（DOM 层） ──
//
// 首次挂载整体 innerHTML；后续更新把新 HTML 解析为顶层节点数组，与 root 现有
// 子节点逐个按下标比较签名：
// - 签名相同（元素 = outerHTML，文本 = `#text:内容`）→ 保留现有节点
//   （DOM 节点身份不变，用户在其内的原生文字选区不被打断）
// - 签名不同 → 替换；新内容短 → 裁掉多余；新内容长 → 追加。
//
// 用 outerHTML 而非 innerHTML 比较的原因：即使某节点被就地副作用改写
// （本项目无 mermaid/工具栏改写，假表格为静态 HTML），也只替换真正变化的部分。
export function reconcileTopLevelChildren(root: HTMLElement, html: string): void {
  // 需要一个脱离文档树的容器解析新 HTML，逐个顶层节点比较
  const template = document.createElement("template");
  template.innerHTML = html;
  const nextNodes = Array.from(template.content.childNodes);
  const prevNodes = Array.from(root.childNodes);

  const nodeSignature = (node: ChildNode): string => {
    if (node instanceof Element) return node.outerHTML;
    return `#text:${node.textContent ?? ""}`;
  };

  const prevSignatures = prevNodes.map(nodeSignature);
  const length = Math.max(prevNodes.length, nextNodes.length);

  for (let i = 0; i < length; i += 1) {
    const prev = prevNodes[i] ?? null;
    const next = nextNodes[i] ?? null;

    if (next === null) {
      // 新内容比旧的短（流式追加不会发生，兜底防御）：裁掉多余旧节点
      prev?.remove();
      continue;
    }
    if (prev === null) {
      // 新内容比旧的长：追加新节点
      root.appendChild(next);
      continue;
    }
    if (prevSignatures[i] === nodeSignature(next)) {
      // 该顶层块的源 HTML 没变——保留现有节点，不做任何替换
      continue;
    }
    root.replaceChild(next, prev);
  }
}

// ── 组件 ──

/**
 * Markdown 渲染组件。
 *
 * React.memo 浅比较 content（字符串原始值按内容比较）：闭合段（content 不再
 * 变化）直接跳过渲染，仅最后一段流式 reconcile。
 */
export const Markdown = memo(function Markdown({ content }: Props) {
  // 容器宽度活测量：窗口缩放 → 布局变化 → 根宽度变化 → 重算表格列宽
  const rootRef = useRef<HTMLDivElement | null>(null);
  const [containerW, setContainerW] = useState(0);

  useEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const update = () => setContainerW(el.clientWidth);
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Markdown 锚点是 innerHTML 写进来的普通 DOM，不是 React 子节点，所以在
  // 根容器上挂原生事件。click 覆盖左键/Ctrl/Shift/键盘，auxclick 覆盖中键。
  useEffect(() => {
    const root = rootRef.current;
    if (!root) return;

    const onLinkEvent = (event: MouseEvent) => {
      if (event.defaultPrevented) return;
      if (event.type === "auxclick" && event.button !== 1) return;
      if (event.type === "click" && event.button !== 0) return;

      const target = event.target;
      const anchor =
        target instanceof Element ? target.closest("a[href]") : null;
      if (!anchor) return;

      const href = (anchor.getAttribute("href") ?? "").trim();
      // 页内锚点保留默认行为；其余一律不交给 webview 导航。
      if (href.startsWith("#")) return;

      event.preventDefault();
      if (!href) return;

      if (!/^https?:\/\//i.test(href)) {
        showToast("仅支持在浏览器打开 http/https 链接", { kind: "warning" });
        return;
      }

      void invoke("open_url", { url: href }).catch((error: unknown) => {
        console.error("[Markdown] open_url 失败:", error, href);
        showToast(`无法打开链接：${String(error)}`, { kind: "warning" });
      });
    };

    root.addEventListener("click", onLinkEvent);
    root.addEventListener("auxclick", onLinkEvent);
    return () => {
      root.removeEventListener("click", onLinkEvent);
      root.removeEventListener("auxclick", onLinkEvent);
    };
  }, []);

  // R-38 旧路径兜底：若仍有带 data-remote-image 的 img（旧 HTML/未来策略），
  // 失败时退回 alt。当前 hardenRemoteImages 已把远程图替换为占位。
  // error 不冒泡，在根容器捕获阶段委托；本 effect 先于 innerHTML 写入注册。
  useLayoutEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    const onImageError = (event: Event) => {
      const target = event.target;
      if (!(target instanceof HTMLImageElement)) return;
      if (!target.hasAttribute("data-remote-image")) return;
      const alt = target.getAttribute("alt")?.trim();
      const fallback = document.createElement("span");
      fallback.className = styles.imageAlt;
      fallback.textContent = alt ? `图片未加载 · ${alt}` : "图片未加载";
      target.replaceWith(fallback);
    };
    root.addEventListener("error", onImageError, true);
    return () => root.removeEventListener("error", onImageError, true);
  }, []);

  // 第一步：marked → DOMPurify 消毒 → 远程图片硬化 → 表格提取。只依赖 content，
  // 窗口缩放不重跑（现状是 content/containerW 单 useMemo，缩放也全量重算）。
  const parsed = useMemo(() => {
    const raw = marked.parse(content) as string;
    const sanitized = sanitizeMarkdownHtml(raw);
    return parseTables(hardenRemoteImages(sanitized));
  }, [content]);

  // 第二步：列宽（getTableLayouts 缓存）+ 假表格重建。依赖表格内容 + 容器宽。
  const html = useMemo(() => {
    const { tables, cleanHtml } = parsed;
    const layouts = getTableLayouts(tables, containerW);

    // GFM 对齐标记（:--- 语法）→ 每列 text-align；只认 left/center/right，
    // 其他值忽略（消毒层放行 align 属性后此处做值域防御）
    const SAFE_ALIGNS = new Set(["left", "center", "right"]);
    const alignStyle = (table: ParsedTable, i: number): string => {
      const a = table.aligns[i] ?? "";
      return a && SAFE_ALIGNS.has(a) ? `text-align:${a};` : "";
    };

    let result = cleanHtml;
    for (let t = 0; t < tables.length; t++) {
      const table = tables[t];
      const layout = layouts[t];
      const n = table.headers.length;

      let div = `<div class="${styles.tableWrap}"><div class="${styles.fakeTable}" style="width:${layout.totalW}px">`;
      div += `<div class="${styles.fakeRow}">`;
      for (let i = 0; i < n; i++) {
        div += `<div class="${styles.fakeTh}" style="width:${layout.colWidths[i]}px;${alignStyle(table, i)}">${table.headers[i]}</div>`;
      }
      div += `</div>`;
      for (const row of table.rows) {
        div += `<div class="${styles.fakeRow}">`;
        for (let i = 0; i < n; i++) {
          const w = layout.colWidths[i];
          div += `<div class="${styles.fakeTd}" style="width:${w}px;min-width:${w}px;max-width:${w}px;${alignStyle(table, i)}">${row[i] ?? ""}</div>`;
        }
        div += `</div>`;
      }
      div += `</div></div>`;

      result = result.replace(`<!--TABLE_${t}-->`, div);
    }

    return result;
  }, [parsed, containerW]);

  // DOM 写入：首次挂载整体 innerHTML；后续更新顶层节点 reconcile。
  // useLayoutEffect 在 DOM 提交后同步执行，无空帧闪烁。
  useLayoutEffect(() => {
    const root = rootRef.current;
    if (!root) return;
    if (root.childNodes.length === 0) {
      root.innerHTML = html;
    } else {
      reconcileTopLevelChildren(root, html);
    }
  }, [html]);

  return <div ref={rootRef} className={styles.root} />;
});
