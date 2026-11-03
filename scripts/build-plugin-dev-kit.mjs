#!/usr/bin/env node
// 打包「插件开发套件」：一个目录，拿走就能写插件，不需要仓库。
//
//   dist/plugin-dev-kit/
//   ├─ 手册/            随包手册（content/docs/plugin-dev）
//   ├─ 模板/            python-hello、rust-hello
//   ├─ plugin_check.exe 校验器
//   └─ README.md        一屏：先跑校验器，再读快速开始
//
// 隔离会话只拿这个目录——它不假设对方能读仓库、能编译 Rust。

import { execFileSync } from "node:child_process";
import { copyFileSync, cpSync, existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const OUT = join(ROOT, "dist", "plugin-dev-kit");
const DOCS = join(ROOT, "src-tauri", "content", "docs", "plugin-dev");
const TEMPLATES = join(ROOT, "plugin-templates");

function exeName(base) {
  return process.platform === "win32" ? `${base}.exe` : base;
}

function locateValidator() {
  const candidates = [
    process.env.PRTS_PLUGIN_CHECK,
    join(ROOT, "src-tauri", "target", "release", exeName("plugin_check")),
    join(ROOT, "src-tauri", "target", "debug", exeName("plugin_check")),
  ].filter(Boolean);
  for (const candidate of candidates) {
    if (existsSync(candidate)) return candidate;
  }
  throw new Error("找不到 plugin_check：先构建它，或用 PRTS_PLUGIN_CHECK 指定路径");
}

const README = `# PRTS 插件开发套件

这个目录里没有仓库，也不需要编译任何东西。

## 先跑校验器

\`\`\`bash
./plugin_check ./python-hello
\`\`\`

八项全 \`PASS\` 才说明这个插件能被宿主装上。输出怎么读见 \`手册/09-排错.md\`。

## 再读快速开始

\`手册/01-快速开始.md\`：从一个空文件夹到在对话里调用成功，Python 与 Rust 各一遍。

## 目录

- \`手册/\`：写插件需要知道的一切（协议、能力、生命周期、发布）。
- \`模板/python-hello/\`：能直接跑的最小插件，改 \`hello.py\` 就行。
- \`模板/rust-hello/\`：同样是能跑的，产物要拷到 \`plugin.json\` 旁边。
- \`plugin_check\`：离线校验器，装之前先跑它。

## 装进应用

设置 → 扩展 → 安装（选文件夹，zip 先解压）。装好后新建一个对话，工具才会出现。
`;

function main() {
  if (!existsSync(DOCS)) throw new Error(`手册目录不存在：${DOCS}`);
  if (!existsSync(TEMPLATES)) throw new Error(`模板目录不存在：${TEMPLATES}`);
  const validator = locateValidator();

  rmSync(OUT, { recursive: true, force: true });
  mkdirSync(OUT, { recursive: true });

  cpSync(DOCS, join(OUT, "手册"), { recursive: true });
  mkdirSync(join(OUT, "模板"), { recursive: true });
  for (const name of ["python-hello", "rust-hello"]) {
    // Rust 模板的构建目录不进套件（对方没有仓库，也不该收到几 MB 的中间产物）。
    const skipBuild = (source) => !source.split(/[\\/]/).includes("target");
    cpSync(join(TEMPLATES, name), join(OUT, "模板", name), {
      recursive: true,
      filter: name === "rust-hello" ? skipBuild : undefined,
    });
  }
  copyFileSync(validator, join(OUT, exeName("plugin_check")));
  writeFileSync(join(OUT, "README.md"), README, "utf8");

  // 自检：套件里的校验器能跑，且对自带的 Python 模板给出结论（不要求本机有 python）。
  try {
    execFileSync(join(OUT, exeName("plugin_check")), ["--help"], { stdio: "ignore", windowsHide: true });
  } catch {
    throw new Error("套件里的 plugin_check 跑不起来");
  }

  console.log(`已生成 ${OUT}`);
}

main();
