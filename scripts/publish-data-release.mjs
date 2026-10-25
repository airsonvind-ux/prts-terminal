#!/usr/bin/env node
/**
 * Publish a prepared data release directory to GitHub.
 *
 * The data release is a build input for the app release workflow. It is not a
 * user-facing installer and it does not contain source code.
 *
 * Usage:
 *   node scripts/publish-data-release.mjs \
 *     --repo owner/prts-terminal-release \
 *     --data-version 2026.10.25 \
 *     --dir release-staging/data-2026.10.25 \
 *     --dry-run
 */

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

function fail(message) {
  throw new Error(message);
}

function normalizeDataVersion(value) {
  return String(value ?? "").trim().replace(/^v/i, "");
}

export function collectDataAssets(dir) {
  const root = path.resolve(dir);
  if (!fs.existsSync(root) || !fs.statSync(root).isDirectory()) {
    fail(`数据发布目录不存在：${root}`);
  }
  const assets = ["arknights.db", "DATA_MANIFEST.json", "SHA256SUMS.txt"].map((name) => {
    const file = path.join(root, name);
    if (!fs.existsSync(file) || !fs.statSync(file).isFile()) {
      fail(`数据发布文件不存在：${file}`);
    }
    return file;
  });
  const notes = path.join(root, "release-notes.md");
  if (fs.existsSync(notes)) assets.push(notes);
  return assets;
}

export function dataReleaseCommand({ repo, dataVersion, assets, notes }) {
  const tag = `data-${normalizeDataVersion(dataVersion)}`;
  const args = ["release", "create", tag, "--repo", repo, "--title", tag];
  if (notes) args.push("--notes-file", notes);
  else args.push("--notes", `PRTS data snapshot ${normalizeDataVersion(dataVersion)}`);
  for (const asset of assets) args.push(asset);
  return { command: "gh", args, tag };
}

export function publishDataRelease({ repo, dataVersion, dir, token, dryRun }) {
  const version = normalizeDataVersion(dataVersion);
  if (!version) fail("缺少 data-version");
  if (!repo || !repo.includes("/")) fail(`发布仓必须是 owner/repo 形式：${repo}`);
  const assets = collectDataAssets(dir);
  const notes = assets.find((file) => file.endsWith("release-notes.md")) ?? null;
  const create = dataReleaseCommand({ repo, dataVersion: version, assets, notes });
  if (dryRun) {
    return { dryRun: true, tag: create.tag, command: create.command, args: create.args };
  }
  const env = { ...process.env, GH_TOKEN: token ?? process.env.GH_TOKEN ?? "" };
  const exists = spawnSync("gh", ["release", "view", create.tag, "--repo", repo], {
    encoding: "utf8",
    env,
  }).status === 0;
  if (!exists) {
    const result = spawnSync(create.command, create.args, { encoding: "utf8", env });
    if (result.status !== 0) {
      fail(`创建数据 Release 失败：${result.stderr || result.stdout || result.status}`);
    }
    return { created: true, tag: create.tag, assets: assets.length };
  }
  const upload = spawnSync(
    "gh",
    ["release", "upload", create.tag, "--repo", repo, "--clobber", ...assets],
    { encoding: "utf8", env },
  );
  if (upload.status !== 0) {
    fail(`上传数据 Release 失败：${upload.stderr || upload.stdout || upload.status}`);
  }
  return { uploaded: assets.length, tag: create.tag };
}

function parseArgs(argv) {
  const args = {
    repo: null,
    dataVersion: null,
    dir: null,
    token: null,
    dryRun: false,
    json: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--repo") args.repo = argv[++i];
    else if (arg === "--data-version") args.dataVersion = argv[++i];
    else if (arg === "--dir") args.dir = argv[++i];
    else if (arg === "--token") args.token = argv[++i];
    else if (arg === "--dry-run") args.dryRun = true;
    else if (arg === "--json") args.json = true;
    else if (arg === "--help" || arg === "-h") args.help = true;
    else fail(`未知参数：${arg}`);
  }
  return args;
}

function printHelp() {
  console.log(`Usage: node scripts/publish-data-release.mjs --repo <owner/repo> --data-version <version> --dir <dir> [options]

Options:
  --token <token>   GitHub token (default: GH_TOKEN)
  --dry-run         Print the planned gh command, make no remote writes
  --json            Print a JSON summary
  --help            Show this help`);
}

export function run(argv = process.argv.slice(2)) {
  const args = parseArgs(argv);
  if (args.help) {
    printHelp();
    return 0;
  }
  const result = publishDataRelease({
    repo: args.repo,
    dataVersion: args.dataVersion,
    dir: args.dir,
    token: args.token ?? process.env.GH_TOKEN ?? null,
    dryRun: args.dryRun,
  });
  if (args.json) console.log(JSON.stringify(result, null, 2));
  else console.log(JSON.stringify(result));
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    process.exitCode = run();
  } catch (error) {
    console.error(`数据 Release 发布失败：${error?.message ?? error}`);
    process.exitCode = 1;
  }
}
