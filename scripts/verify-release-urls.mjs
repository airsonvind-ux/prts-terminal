// @ts-check
/**
 * verify-release-urls —— 发布后线上可达性检查
 *
 * 检查什么（两个源各查一遍）：
 *   1. 清单可取回（Gitee raw/master，GitHub latest/download）
 *   2. 清单 version 与本地 package.json（或 --version）一致
 *   3. 清单里的安装包 URL 能 200（跟随重定向）
 *   4. 清单里的 signature 非空
 *
 * 为什么单独做成脚本：0.1.0 与 0.1.1 两次都在「GitHub 资产名空格被规范成点、
 * 清单里仍写 %20」上栽过。手工改一次产物不足以防止复发，必须每条发布都跑。
 *
 * 用法：
 *   node scripts/verify-release-urls.mjs
 *   node scripts/verify-release-urls.mjs --version 0.1.1
 *   node scripts/verify-release-urls.mjs --json
 *
 * 退出码：全部通过 0，任一失败 1。
 */

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, "..");

const SOURCES = [
  {
    id: "gitee",
    label: "Gitee 主源",
    manifest: "https://gitee.com/airsonvind0/prts-terminal-release/raw/master/latest.json",
  },
  {
    id: "github",
    label: "GitHub 兜底源",
    manifest:
      "https://github.com/airsonvind-ux/prts-terminal-release/releases/latest/download/latest.json",
  },
];

function readLocalVersion() {
  const pkg = JSON.parse(fs.readFileSync(path.join(ROOT, "package.json"), "utf8"));
  return String(pkg.version ?? "").trim();
}

function parseArgs(argv) {
  const args = { version: null, json: false };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === "--version") args.version = String(argv[(i += 1)] ?? "").trim();
    else if (a === "--json") args.json = true;
  }
  return args;
}

async function fetchJson(url) {
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  const text = await res.text();
  try {
    return JSON.parse(text);
  } catch {
    throw new Error("返回内容不是 JSON");
  }
}

async function headOk(url) {
  const res = await fetch(url, { method: "HEAD", redirect: "follow" });
  return { ok: res.ok, status: res.status };
}

async function checkSource(source, expectedVersion) {
  const result = { id: source.id, label: source.label, ok: false, checks: [] };
  const add = (name, ok, detail) => result.checks.push({ name, ok, detail });

  let manifest;
  try {
    manifest = await fetchJson(source.manifest);
    add("清单可取回", true, source.manifest);
  } catch (error) {
    add("清单可取回", false, String(error.message ?? error));
    return result;
  }

  const version = String(manifest.version ?? "").trim();
  add("版本一致", version === expectedVersion, `清单 ${version || "(空)"} / 期望 ${expectedVersion}`);

  const notes = String(manifest.notes ?? "");
  add("notes 非空", notes.trim().length > 0, `${notes.length} 字符`);

  const platforms = manifest.platforms ?? {};
  const keys = Object.keys(platforms);
  add("含 windows-x86_64", keys.includes("windows-x86_64"), keys.join(",") || "(无)");

  const entry = platforms["windows-x86_64"] ?? {};
  const signature = String(entry.signature ?? "");
  add("signature 非空", signature.length > 0, `${signature.length} 字符`);

  const url = String(entry.url ?? "");
  if (!url) {
    add("安装包 URL 可达", false, "清单未给出 url");
  } else {
    try {
      const { ok, status } = await headOk(url);
      add("安装包 URL 可达", ok, `HTTP ${status} ${url}`);
    } catch (error) {
      add("安装包 URL 可达", false, `${String(error.message ?? error)} ${url}`);
    }
  }

  result.ok = result.checks.every((c) => c.ok);
  return result;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const expected = args.version || readLocalVersion();
  if (!expected) {
    console.error("无法确定期望版本：package.json 里没有 version，也没传 --version");
    process.exit(1);
  }

  const results = [];
  for (const source of SOURCES) {
    results.push(await checkSource(source, expected));
  }

  if (args.json) {
    console.log(JSON.stringify({ expected, results }, null, 2));
  } else {
    console.log(`期望版本：${expected}`);
    for (const r of results) {
      console.log(`\n== ${r.label} ${r.ok ? "通过" : "未通过"}`);
      for (const c of r.checks) {
        console.log(`   ${c.ok ? "ok  " : "FAIL"}  ${c.name}  ${c.detail}`);
      }
    }
  }

  const failed = results.filter((r) => !r.ok);
  if (failed.length) {
    console.log(`\n结果：${failed.length} 个源未通过`);
    process.exit(1);
  }
  console.log("\n结果：两个源全部通过");
  return 0;
}

main().catch((error) => {
  console.error(String(error?.message ?? error));
  process.exit(1);
});
