#!/usr/bin/env node
// 手册门禁：随包手册是给外部开发者读的，不能漏出仓库内部细节；
// 能力表必须与宿主实现一致（否则文档会与代码各说各话）。
//
// 检查三件事：
//   1. 手册里不出现仓库内部路径与工具名（规格 §四 的禁止词）；
//   2. 每篇都有标题、没有空文件；
//   3. 04-能力.md 的标记块与 plugin_check --dump-capabilities 渲染结果一致。

import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DOCS = join(ROOT, "src-tauri", "content", "docs", "plugin-dev");

/// 这些词出现在手册里，说明写成了内部笔记。
const FORBIDDEN = ["src/", "cargo", "host::internal", "plugin-tmp"];

const problems = [];

function checkForbidden() {
  for (const name of readdirSync(DOCS).filter((entry) => entry.endsWith(".md"))) {
    const path = join(DOCS, name);
    const text = readFileSync(path, "utf8");
    if (text.trim().length === 0) problems.push(`${name}: 文件是空的`);
    if (!text.startsWith("# ")) problems.push(`${name}: 缺少一级标题`);

    const lines = text.split(/\r?\n/);
    lines.forEach((line, index) => {
      for (const word of FORBIDDEN) {
        if (line.includes(word)) {
          problems.push(`${name}:${index + 1} 出现禁止词「${word}」：${line.trim()}`);
        }
      }
    });
  }
}

function checkCapabilityBlock() {
  try {
    execFileSync(process.execPath, [join(ROOT, "scripts", "gen-capability-doc.mjs"), "--check"], {
      stdio: "inherit",
      windowsHide: true,
    });
  } catch {
    problems.push("04-能力.md 的能力表与实现不一致");
  }
}

checkForbidden();
checkCapabilityBlock();

if (problems.length > 0) {
  console.error("手册门禁未通过：");
  for (const problem of problems) console.error(`  - ${problem}`);
  process.exit(1);
}
console.log("手册门禁通过");
