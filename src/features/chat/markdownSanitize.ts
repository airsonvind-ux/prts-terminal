// src/components/content/markdownSanitize.ts
// Markdown HTML 消毒（DOMPurify 白名单配置 · 模块 F 批 2-A+B 设计四）
//
// 管线位置：marked 输出 → sanitizeMarkdownHtml → parseTables（表格提取/假表格重建）
// → reconcile 渲染。
//
// 关键边界：
// - 消毒必须在表格提取之前：parseTables 提取的 th/td innerHTML 片段因此已受覆盖；
// - 假表格重建阶段注入的内联 style（宽度）与 CSS modules 类名发生在消毒之后，
//   不受白名单影响——所以用户内容中的 style 属性可以放心整体剔除。
// - 本模块独立于 Markdown.tsx：白名单配置可单测，Markdown.tsx 只依赖薄函数。

import DOMPurify from "dompurify";
import type { Config } from "dompurify";

/**
 * DOMPurify 白名单配置（基于 PRTS 实际渲染域）。
 *
 * 渲染域 = marked(gfm+breaks) 的全部输出形态：段落/行内/列表/引用/代码块/
 * 标题/分割线/链接/图片 + 任务列表（input[type=checkbox]）+ 表格
 * （原生 table 标签在 parseTables 阶段被提取替换为 div 假表格）。
 *
 * 定案（主对话确认）：
 * - data: URI 禁止（含图片的 data URI）——DOM 兜底拦截（见 sanitizeMarkdownHtml）
 * - style 属性剔除（FORBID_ATTR: ["style", ...]）——假表格宽度是重建注入，
 *   在消毒之后，不受影响
 * - svg/math 整棵移除（无渲染支持）
 */
export const SANITIZE_CONFIG: Config = {
  // 默认 HTML 配置已覆盖 marked gfm 全部输出标签；显式声明以防默认漂移
  ALLOWED_TAGS: [
    "p",
    "br",
    "hr",
    "a",
    "img",
    "strong",
    "b",
    "em",
    "i",
    "del",
    "s",
    "u",
    "code",
    "span",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "blockquote",
    "pre",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "th",
    "td",
    // 任务列表 checkbox（marked gfm 输出 input[type=checkbox]）
    "input",
  ],
  ALLOWED_ATTR: [
    "href",
    "title",
    "class",
    "id",
    // img
    "src",
    "alt",
    "width",
    "height",
    // 任务列表 input（仅此处出现）
    "type",
    "checked",
    "disabled",
    // GFM 表格对齐标记（marked 输出 th/td align 属性，parseTables 消费）
    "align",
  ],
  // 内联 style 一律剔除：假表格的宽度样式是重建阶段注入（消毒之后），
  // 用户内容中的 style 无渲染消费点（CSS modules 覆盖全部视觉样式）。
  // on* 事件属性 DOMPurify 默认剥离，显式列出作为文档性防御。
  FORBID_ATTR: [
    "style",
    "target",
    "onerror",
    "onclick",
    "onload",
    "onmouseover",
    "onmouseout",
  ],
  FORBID_TAGS: [
    "script",
    "style",
    "iframe",
    "object",
    "embed",
    "form",
    "button",
    "textarea",
    "select",
    "link",
    "meta",
    "base",
    "svg",
    "math",
  ],
  // 非法标签剥壳保留文本（表格提取前的安全默认：恶意标签的内容仍可展示为纯文本）
  KEEP_CONTENT: true,
  // URL 协议白名单：只放行显式安全协议与相对/锚点，拒绝 javascript:/vbscript: 等。
  // 注意：不能用 DOMPurify 默认的 ALLOWED_URI_REGEXP 形态（其中 `[a-z+.\-]+(?:[^a-z+]|$)`
  // 分支会匹配 `javascript:` 这类协议名前缀并放行——已实测确认）。
  // data: 协议由 sanitizeMarkdownHtml 的 DOM 兜底剥离（见下），正则无需覆盖。
  ALLOWED_URI_REGEXP:
    /^(?:https?:\/\/|mailto:|tel:|#|\/|\.\/|\.\.\/|[^a-z])/i,
  // 注：DOMPurify 3.x 默认 DATA_URI_TAGS 含 img/audio/video 等，且配置面只有
  // ADD_DATA_URI_TAGS（追加）无法覆盖移除——data: 由 sanitizeMarkdownHtml 内
  // DOM 兜底拦截（见该函数注释）。
  // 严格 ALLOWED_URI_REGEXP 会被 DOMPurify 用于检查所有属性（不只 href/src）：
  // `type="checkbox"` 等普通值不匹配协议白名单 → 属性被误杀（已实测）。
  // 将非 URL 属性加入 URI 安全集合跳过该检查（默认集合已含 title/class/id/alt）；
  // href/src 仍走严格正则 → javascript:/vbscript: 被拒。
  // align 值（left/center/right）非 URL，同样需要跳过，否则被误杀。
  ADD_URI_SAFE_ATTR: ["type", "checked", "disabled", "width", "height", "align"],
};

/**
 * 消毒 marked 输出的 HTML（同步，浏览器环境）。
 *
 * DOMPurify 的 sanitize 之后做 DOM 兜底：默认 DATA_URI_TAGS 含 img/audio/video
 * 等（3.x 只有 ADD_DATA_URI_TAGS 可追加、无覆盖），sanitize 会按标签放行
 * src/href 的 data: URI——此处遍历所有元素，剥离以 data: 开头的 src/href。
 * 该兜底发生在消毒之后、表格提取之前，假表格重建的宽度 style 不受影响。
 */
export function sanitizeMarkdownHtml(html: string): string {
  const clean = DOMPurify.sanitize(html, SANITIZE_CONFIG);

  const temp = document.createElement("div");
  temp.innerHTML = clean;
  const URI_ATTRS = ["src", "href", "xlink:href"] as const;
  for (const el of temp.querySelectorAll("*")) {
    for (const name of URI_ATTRS) {
      const value = el.getAttribute(name);
      if (value && value.trim().toLowerCase().startsWith("data:")) {
        el.removeAttribute(name);
      }
    }
  }
  return temp.innerHTML;
}

/**
 * R-38 最小硬化：远程图片保留直连，但不带 Referer、屏幕外不预取、
 * 解码不阻塞渲染。只改远程 http/https（含协议相对 //）图片；本地 asset、
 * 相对路径与 data: 不动。
 *
 * 这是风险接受下的缓解，不是代理：盲 SSRF 面仍存在，见 decisions.md D-012。
 */
const REMOTE_IMAGE_SRC = /^(?:https?:)?\/\//i;

export function hardenRemoteImages(html: string): string {
  const temp = document.createElement("div");
  temp.innerHTML = html;
  for (const img of temp.querySelectorAll("img")) {
    const src = img.getAttribute("src")?.trim() ?? "";
    if (!REMOTE_IMAGE_SRC.test(src)) continue;
    img.setAttribute("loading", "lazy");
    img.setAttribute("decoding", "async");
    img.setAttribute("referrerpolicy", "no-referrer");
    img.setAttribute("data-remote-image", "");
  }
  return temp.innerHTML;
}
