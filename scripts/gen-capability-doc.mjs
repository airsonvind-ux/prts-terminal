#!/usr/bin/env node
// 把宿主的能力表渲染进手册《04 能力》的标记块。
//
// 数据来源是 plugin_check --dump-capabilities（与宿主共用同一份 HostServiceInfo），
// 这里只做 JSON → Markdown，不猜任何字段。
//
// 用法：
//   node scripts/gen-capability-doc.mjs          # 写入
//   node scripts/gen-capability-doc.mjs --check  # 只比对，有差异退出 1

import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const DOC = join(ROOT, "src-tauri", "content", "docs", "plugin-dev", "04-能力.md");
const BEGIN = "<!-- BEGIN:capabilities -->";
const END = "<!-- END:capabilities -->";

/** 找校验器：显式环境变量优先，其次 release、debug。 */
function locateValidator() {
  const candidates = [
    process.env.PRTS_PLUGIN_CHECK,
    join(ROOT, "src-tauri", "target", "release", exeName("plugin_check")),
    join(ROOT, "src-tauri", "target", "debug", exeName("plugin_check")),
  ].filter(Boolean);
  for (const candidate of candidates) {
    if (existsSync(candidate)) return candidate;
  }
  throw new Error(
    "找不到 plugin_check：先跑 `cargo build --bin plugin_check`，或用 PRTS_PLUGIN_CHECK 指定路径",
  );
}

function exeName(base) {
  return process.platform === "win32" ? `${base}.exe` : base;
}

function dumpCapabilities(validator) {
  const raw = execFileSync(validator, ["--dump-capabilities"], {
    encoding: "utf8",
    windowsHide: true,
  });
  const parsed = JSON.parse(raw);
  if (!Array.isArray(parsed)) throw new Error("能力表不是数组");
  for (const entry of parsed) {
    for (const field of ["key", "summary", "params", "returns"]) {
      if (typeof entry[field] !== "string" || entry[field].trim() === "") {
        throw new Error(`能力 ${entry.key ?? "?"} 缺少 ${field}`);
      }
    }
  }
  return parsed;
}

/** 表格单元格里的竖线会把列撑开，转义掉；换行压成空格。 */
function cell(text) {
  return text.replace(/\|/g, "\\|").replace(/\r?\n/g, " ").trim();
}

function render(entries) {
  const rows = entries.map(
    (entry) =>
      `| \`${cell(entry.key)}\` | ${cell(entry.summary)} | \`${cell(entry.params)}\` | \`${cell(entry.returns)}\` |`,
  );
  return ["| 能力 | 用途 | 参数 | 返回 |", "|---|---|---|---|", ...rows].join("\n");
}

function main() {
  const check = process.argv.includes("--check");
  const entries = dumpCapabilities(locateValidator());
  const block = render(entries);

  if (!existsSync(DOC)) throw new Error(`手册不存在：${DOC}`);
  const text = readFileSync(DOC, "utf8");
  const start = text.indexOf(BEGIN);
  const end = text.indexOf(END);
  if (start === -1 || end === -1 || end < start) {
    throw new Error("04-能力.md 里找不到能力表标记块");
  }

  const next = `${text.slice(0, start + BEGIN.length)}\n${block}\n${text.slice(end)}`;
  if (next === text) {
    console.log("能力表已是最新");
    return;
  }
  if (check) {
    console.error("能力表与实现不一致：跑 `node scripts/gen-capability-doc.mjs` 重新生成");
    process.exit(1);
  }
  writeFileSync(DOC, next, "utf8");
  console.log(`已更新 ${entries.length} 条能力`);
}

main();
