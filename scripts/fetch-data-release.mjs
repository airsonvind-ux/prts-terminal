#!/usr/bin/env node
/**
 * Download a frozen data release and validate it before a release build.
 *
 * The data release is expected to contain:
 *   arknights.db
 *   DATA_MANIFEST.json
 *
 * It is a build input, not source code. The source repository never contains
 * the database.
 *
 * Usage:
 *   node scripts/fetch-data-release.mjs \
 *     --repo owner/prts-terminal-release \
 *     --data-version 2026.10.25 \
 *     --out-dir src-tauri/resources
 */

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import {
  validateDataArtifact,
  validateDataVersion,
} from "./prepare-data-release.mjs";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(SCRIPT_DIR, "..");
const DEFAULT_OUT_DIR = path.join(ROOT, "src-tauri", "resources");

function fail(message) {
  throw new Error(message);
}

export function buildDataReleaseAssetUrl({ repo, dataVersion, asset }) {
  if (!repo || !repo.includes("/")) {
    fail(`数据发布仓必须是 owner/repo 形式，实际为 ${JSON.stringify(repo)}`);
  }
  const version = validateDataVersion(dataVersion);
  const encodedAsset = encodeURIComponent(asset);
  return `https://github.com/${repo.replace(/^\/+|\/+$/g, "")}/releases/download/data-${version}/${encodedAsset}`;
}

async function downloadFile({ url, destination, token }) {
  const headers = {
    "User-Agent": "prts-terminal-release",
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  const response = await fetch(url, { headers, redirect: "follow" });
  if (!response.ok) {
    fail(`下载失败：${response.status} ${response.statusText} ${url}`);
  }
  const data = Buffer.from(await response.arrayBuffer());
  fs.writeFileSync(destination, data);
  return data.length;
}

/**
 * Download and validate a data release into an output directory.
 *
 * @param {{
 *   repo: string,
 *   dataVersion: string,
 *   outDir?: string|null,
 *   expectedSha?: string|null,
 *   token?: string|null,
 *   force?: boolean,
 *   requireContract?: boolean,
 *   dryRun?: boolean,
 * }} options
 * @returns {Promise<object>}
 */
export async function fetchDataRelease(options) {
  const version = validateDataVersion(options.dataVersion);
  const dbUrl = buildDataReleaseAssetUrl({
    repo: options.repo,
    dataVersion: version,
    asset: "arknights.db",
  });
  const manifestUrl = buildDataReleaseAssetUrl({
    repo: options.repo,
    dataVersion: version,
    asset: "DATA_MANIFEST.json",
  });
  const outDir = path.resolve(options.outDir ?? DEFAULT_OUT_DIR);
  const dbTarget = path.join(outDir, "arknights.db");
  const manifestTarget = path.join(outDir, "DATA_MANIFEST.json");

  const plan = {
    dataVersion: version,
    repo: options.repo,
    databaseUrl: dbUrl,
    manifestUrl: manifestUrl,
    outDir,
  };
  if (options.dryRun) {
    return { ...plan, dryRun: true };
  }

  if (!options.force && (fs.existsSync(dbTarget) || fs.existsSync(manifestTarget))) {
    fail(`目标文件已存在：${outDir}；需要覆盖时加 --force`);
  }

  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "prts-data-release-"));
  const tempDb = path.join(tempDir, "arknights.db");
  const tempManifest = path.join(tempDir, "DATA_MANIFEST.json");
  try {
    await downloadFile({ url: dbUrl, destination: tempDb, token: options.token });
    await downloadFile({ url: manifestUrl, destination: tempManifest, token: options.token });

    let manifest;
    try {
      manifest = JSON.parse(fs.readFileSync(tempManifest, "utf8"));
    } catch (error) {
      fail(`下载的 DATA_MANIFEST.json 无法解析：${error?.message ?? error}`);
    }

    const checked = validateDataArtifact({
      manifest,
      databasePath: tempDb,
      expectedVersion: version,
      requireContract: options.requireContract !== false,
    });
    if (options.expectedSha) {
      const expected = String(options.expectedSha).trim().toUpperCase();
      if (checked.sha256 !== expected) {
        fail(`下载的数据库 SHA-256 与期望值不一致：${checked.sha256} != ${expected}`);
      }
    }

    fs.mkdirSync(outDir, { recursive: true });
    fs.copyFileSync(tempDb, dbTarget);
    fs.copyFileSync(tempManifest, manifestTarget);
    return {
      ...plan,
      database: dbTarget,
      manifest: manifestTarget,
      sha256: checked.sha256,
      schemaVersion: checked.schemaVersion,
      contract: { passed: checked.passed, ignored: checked.ignored },
      bytes: fs.statSync(dbTarget).size,
    };
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
}

function parseArgs(argv) {
  const args = {
    repo: null,
    dataVersion: null,
    outDir: null,
    expectedSha: null,
    token: null,
    force: false,
    dryRun: false,
    json: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--repo") args.repo = argv[++i];
    else if (arg === "--data-version") args.dataVersion = argv[++i];
    else if (arg === "--out-dir") args.outDir = argv[++i];
    else if (arg === "--expected-sha") args.expectedSha = argv[++i];
    else if (arg === "--token") args.token = argv[++i];
    else if (arg === "--force") args.force = true;
    else if (arg === "--dry-run") args.dryRun = true;
    else if (arg === "--json") args.json = true;
    else if (arg === "--help" || arg === "-h") args.help = true;
    else fail(`未知参数：${arg}`);
  }
  return args;
}

function printHelp() {
  console.log(`Usage: node scripts/fetch-data-release.mjs --repo <owner/repo> --data-version <version> [options]

Options:
  --out-dir <dir>       Target resources directory (default: src-tauri/resources)
  --expected-sha <sha>  Require the downloaded database to match this SHA-256
  --token <token>       Token for a private release repository
  --force               Overwrite existing database and manifest
  --dry-run             Print URLs and validate the plan without network
  --json                Print a JSON summary
  --help                Show this help`);
}

export async function run(argv = process.argv.slice(2)) {
  const args = parseArgs(argv);
  if (args.help) {
    printHelp();
    return 0;
  }
  if (!args.repo) fail("缺少 --repo");
  if (!args.dataVersion) fail("缺少 --data-version");
  const result = await fetchDataRelease({
    repo: args.repo,
    dataVersion: args.dataVersion,
    outDir: args.outDir,
    expectedSha: args.expectedSha,
    token: args.token ?? process.env.GITHUB_TOKEN ?? null,
    force: args.force,
    dryRun: args.dryRun,
  });
  if (args.json) {
    console.log(JSON.stringify(result, null, 2));
  } else if (result.dryRun) {
    console.log(`数据下载计划：${result.dataVersion}`);
    console.log(`  DB：${result.databaseUrl}`);
    console.log(`  manifest：${result.manifestUrl}`);
    console.log(`  输出：${result.outDir}`);
  } else {
    console.log(`数据已下载并校验：${result.database}`);
    console.log(`  dataVersion：${result.dataVersion}`);
    console.log(`  SHA-256：${result.sha256}`);
    console.log(`  契约：${result.contract.passed ?? "?"} passed / ${result.contract.ignored ?? "?"} ignored`);
  }
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    process.exitCode = await run();
  } catch (error) {
    console.error(`数据 Release 下载失败：${error?.message ?? error}`);
    process.exitCode = 1;
  }
}
