// @vitest-environment jsdom
// src/components/content/markdownSanitize.test.ts
// DOMPurify 白名单配置测试（模块 F · 批 2-A+B 设计四）
//
// jsdom 环境：DOMPurify 需要 DOM。
// 覆盖：
//   1. marked 正常输出（表格/代码块/链接/列表/任务列表）消毒后结构完整
//   2. script / iframe / style 标签清除（KEEP_CONTENT 保留文本）
//   3. onerror 等事件属性剥离
//   4. javascript: URI 被拒（href 剥离）
//   5. data: URI 被拒（DATA_URI_TAGS: []）
//   6. 用户内容中的 style 属性剔除（假表格宽度是重建注入，在消毒之后）
//   7. 表格单元格内 HTML 已消毒（消毒在表格提取之前 → th/td innerHTML 受覆盖）

import { describe, expect, it } from "vitest";
import { marked } from "marked";
import { sanitizeMarkdownHtml } from "./markdownSanitize";

marked.setOptions({ breaks: true, gfm: true });

// ═══════════════════════════════════════════════
// 正常渲染域：结构完整
// ═══════════════════════════════════════════════

describe("sanitizeMarkdownHtml · 正常渲染域结构完整", () => {
  it("marked 完整输出（标题/段落/列表/引用/代码块/表格/链接）消毒后标签保留", () => {
    const md = [
      "# 标题",
      "",
      "段落 **加粗** 与 *斜体* 和 `行内代码`。",
      "",
      "- 列表项一",
      "- 列表项二",
      "",
      "> 引用",
      "",
      "```ts",
      "const x = 1;",
      "```",
      "",
      "| 名称 | 数值 |",
      "| --- | --- |",
      "| 源石虫 | 300 |",
      "",
      "[链接](https://example.com)",
    ].join("\n");

    const html = sanitizeMarkdownHtml(marked.parse(md) as string);

    expect(html).toContain("<h1>标题</h1>");
    expect(html).toContain("<strong>加粗</strong>");
    expect(html).toContain("<em>斜体</em>");
    expect(html).toContain("<code>行内代码</code>");
    expect(html).toContain("<ul>");
    expect(html).toContain("<blockquote>");
    expect(html).toContain("<pre>");
    expect(html).toContain("<table>");
    expect(html).toContain("<td>300</td>");
    // 链接 href 保留（http 协议白名单）
    expect(html).toContain('<a href="https://example.com">链接</a>');
  });

  it("任务列表 input[type=checkbox] 的 checked/disabled 属性保留", () => {
    const html = sanitizeMarkdownHtml(
      marked.parse("- [x] 已完成\n- [ ] 未完成") as string
    );
    // 属性顺序 = jsdom 序列化顺序（marked 原始顺序：checked/disabled/type）
    expect(html).toContain('<input checked="" disabled="" type="checkbox">');
    expect(html).toContain('<input disabled="" type="checkbox">');
  });
});

// ═══════════════════════════════════════════════
// 恶意注入：清除/无害化
// ═══════════════════════════════════════════════

describe("sanitizeMarkdownHtml · 恶意注入拦截", () => {
  it("script 标签整棵清除（内容一并丢弃，KEEP_CONTENT 对 FORBID_CONTENTS 无效）", () => {
    const html = sanitizeMarkdownHtml(
      "<p>前文</p><script>alert(1)</script><p>后文</p>"
    );
    expect(html).not.toContain("script");
    expect(html).toContain("<p>前文</p>");
    expect(html).toContain("<p>后文</p>");
    // script 在 DEFAULT_FORBID_CONTENTS 中：整棵删除，内容不保留（比剥壳更严格）
    expect(html).not.toContain("alert(1)");
  });

  it("iframe / style 标签整棵清除", () => {
    const html = sanitizeMarkdownHtml(
      "<p>x</p><iframe src=\"https://evil.example\"></iframe><style>body{display:none}</style>"
    );
    expect(html).not.toContain("iframe");
    expect(html).not.toContain("<style");
    expect(html).toContain("<p>x</p>");
  });

  it("onerror 等事件属性剥离", () => {
    const html = sanitizeMarkdownHtml(
      '<img src="https://example.com/a.png" onerror="alert(1)" onload="steal()">'
    );
    expect(html).not.toContain("onerror");
    expect(html).not.toContain("onload");
    // src 保留（http 白名单内）
    expect(html).toContain('src="https://example.com/a.png"');
  });

  it("javascript: URI 被拒，href 剥离为纯文本链接", () => {
    const html = sanitizeMarkdownHtml(
      '<a href="javascript:alert(1)">点我</a>'
    );
    expect(html).not.toContain("javascript:");
    expect(html).not.toContain("href=");
    // KEEP_CONTENT：链接文本保留
    expect(html).toContain("点我");
  });

  it("data: URI 被拒（DATA_URI_TAGS 为空数组，含 img data:image）", () => {
    const html = sanitizeMarkdownHtml(
      '<img src="data:image/png;base64,AAAA"><a href="data:text/html,<script>alert(1)</script>">d</a>'
    );
    expect(html).not.toContain("data:");
    expect(html).not.toContain("href=");
  });

  it("用户内容中的 style 属性剔除（FORBID_ATTR）", () => {
    const html = sanitizeMarkdownHtml(
      '<p style="color:red;background:url(https://evil.example/x)">内容</p><span style="font-size:99px">s</span>'
    );
    expect(html).not.toContain("style=");
    expect(html).toContain("<p>内容</p>");
    expect(html).toContain("<span>s</span>");
  });

  it("svg / math 整棵移除", () => {
    const html = sanitizeMarkdownHtml(
      '<p>a</p><svg><path d="M0 0"/></svg><math><mi>x</mi></math><p>b</p>'
    );
    expect(html).not.toContain("svg");
    expect(html).not.toContain("math");
    expect(html).toContain("<p>a</p>");
    expect(html).toContain("<p>b</p>");
  });
});

// ═══════════════════════════════════════════════
// 表格单元格覆盖（消毒在表格提取之前）
// ═══════════════════════════════════════════════

describe("sanitizeMarkdownHtml · 表格单元格内 HTML 已消毒", () => {
  it("th/td innerHTML 中的恶意属性被剥离（管线前提：消毒在 parseTables 前）", () => {
    const html = sanitizeMarkdownHtml(
      "<table><thead><tr><th onclick=\"alert(1)\">表头</th></tr></thead>" +
        "<tbody><tr><td><img src=\"x\" onerror=\"alert(1)\">数据</td></tr></tbody></table>"
    );
    expect(html).not.toContain("onclick");
    expect(html).not.toContain("onerror");
    expect(html).toContain("<th>表头</th>");
    expect(html).toContain("<td>");
    expect(html).toContain("数据");
  });

  it("表格内 style 属性同样被剔除（用户内容），不影响假表格重建的宽度样式", () => {
    const html = sanitizeMarkdownHtml(
      '<table><tr><td style="width:9999px">内容</td></tr></table>'
    );
    expect(html).not.toContain("style=");
    expect(html).toContain("<td>内容</td>");
  });
});
