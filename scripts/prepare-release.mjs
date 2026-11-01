#!/usr/bin/env node
/**
 * Build a local release staging directory from an already-built installer.
 *
 * This script never signs, uploads, or rebuilds. It verifies the Tauri updater
 * signature, hashes the installer, and writes the two updater manifests
 * (`gitee/latest.json` and/or `github/latest.json`) plus `SHA256SUMS.txt`.
 *
 * Usage:
 *   node scripts/prepare-release.mjs \
 *     --installer <installer.exe> \
 *     --gitee-repo <owner/repo> \
 *     [--github-repo <owner/repo>] \
 *     [--version 0.1.0] [--notes-file release-notes.md] \
 *     [--out-dir release-staging] [--force]
 *
 * The repository URLs are explicit on purpose. Do not bake a GitHub account
 * into the script while the source/release repositories are still undecided.
 */

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { verifyUpdateSignature } from "./verify-update-signature.mjs";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(SCRIPT_DIR, "..");
const DEFAULT_CONFIG = path.join(ROOT, "src-tauri", "tauri.conf.json");
const DEFAULT_OUT_DIR = path.join(ROOT, "release-staging");

function fail(message) {
  throw new Error(message);
}

export function normalizeVersion(value) {
  return String(value ?? "").trim().replace(/^v/i, "");
}

function readJson(filePath) {
  return JSON.parse(fs.readFileSync(filePath, "utf8"));
}

/**
 * Read the three version sources that must agree.
 *
 * @param {string} [root]
 * @returns {{packageVersion: string, tauriVersion: string, cargoVersion: string}}
 */
export function readProjectVersions(root = ROOT) {
  const packageJson = readJson(path.join(root, "package.json"));
  const tauriConfig = readJson(path.join(root, "src-tauri", "tauri.conf.json"));
  const cargoToml = fs.readFileSync(path.join(root, "src-tauri", "Cargo.toml"), "utf8");
  const cargoMatch = cargoToml.match(/^version\s*=\s*"([^"]+)"/m);
  return {
    packageVersion: String(packageJson.version ?? ""),
    tauriVersion: String(tauriConfig.version ?? ""),
    cargoVersion: cargoMatch ? cargoMatch[1] : "",
  };
}

function assertProjectVersion(root, version) {
  const versions = readProjectVersions(root);
  const wanted = normalizeVersion(version);
  const entries = [
    ["package.json", versions.packageVersion],
    ["src-tauri/tauri.conf.json", versions.tauriVersion],
    ["src-tauri/Cargo.toml", versions.cargoVersion],
  ];
  const mismatches = entries.filter(([, value]) => normalizeVersion(value) !== wanted);
  if (mismatches.length > 0) {
    const detail = entries.map(([name, value]) => `  ${name}: ${value || "(缺失)"}`).join("\n");
    fail(`版本不一致，期望 ${wanted}：\n${detail}`);
  }
  return versions;
}

export function sha256File(filePath) {
  return crypto.createHash("sha256").update(fs.readFileSync(filePath)).digest("hex").toUpperCase();
}

function encodeAssetName(name) {
  return encodeURIComponent(name);
}

/**
 * GitHub 在上传时会把资产名里的空白字符规范化成点（`PRTS Terminal_x.exe` →
 * `PRTS.Terminal_x.exe`），下载 URL 必须用规范化后的名字，否则 404。
 * 0.1.0 与 0.1.1 都因为这一项手动改过清单，固化为代码。
 */
function normalizeGithubAssetName(name) {
  return String(name ?? "").replace(/\s+/g, ".");
}

/**
 * @param {{
 *   provider: "gitee"|"github",
 *   repo?: string|null,
 *   url?: string|null,
 *   version: string,
 *   filename: string,
 * }} options
 * @returns {string|null}
 */
export function buildReleaseAssetUrl({ provider, repo, url, version, filename }) {
  if (url) return url;
  if (!repo) return null;
  if (!repo.includes("/")) {
    fail(`${provider} 仓库必须是 owner/repo 形式，实际为 ${JSON.stringify(repo)}`);
  }
  const host = provider === "gitee" ? "https://gitee.com" : "https://github.com";
  const tag = `v${normalizeVersion(version)}`;
  const assetName = provider === "github" ? normalizeGithubAssetName(filename) : filename;
  return `${host}/${repo.replace(/^\/+|\/+$/g, "")}/releases/download/${tag}/${encodeAssetName(assetName)}`;
}

/**
 * @param {{
 *   version: string,
 *   notes: string,
 *   pubDate: string,
 *   platform: string,
 *   signature: string,
 *   url: string,
 * }} options
 * @returns {object}
 */
export function buildLatestJson({ version, notes, pubDate, platform, signature, url }) {
  return {
    version: normalizeVersion(version),
    notes,
    pub_date: pubDate,
    platforms: {
      [platform]: {
        signature,
        url,
      },
    },
  };
}

function readUpdaterPubkey(configPath) {
  const config = readJson(configPath);
  const pubkey = config?.plugins?.updater?.pubkey;
  if (typeof pubkey !== "string" || pubkey.length === 0) {
    fail(`${configPath} 没有 plugins.updater.pubkey`);
  }
  return pubkey;
}

function ensureEmptyOrForced(targetDir, force) {
  if (!fs.existsSync(targetDir)) return;
  const entries = fs.readdirSync(targetDir);
  if (entries.length === 0) return;
  if (!force) {
    fail(`目标目录已存在且非空：${targetDir}；需要覆盖时加 --force`);
  }
  // --force 是显式覆盖：先清空版本目录，避免上一次的 gitee/github
  // 清单或旧安装包残留下来被误上传。
  fs.rmSync(targetDir, { recursive: true, force: true });
}

function writeJson(filePath, value) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, `${JSON.stringify(value, null, 2)}\n`, "utf8");
}

/**
 * @param {{
 *   installer: string,
 *   signature?: string|null,
 *   version?: string|null,
 *   notesFile?: string|null,
 *   pubDate?: string|null,
 *   platform?: string,
 *   giteeRepo?: string|null,
 *   giteeUrl?: string|null,
 *   githubRepo?: string|null,
 *   githubUrl?: string|null,
 *   outDir?: string|null,
 *   config?: string|null,
 *   pubkey?: string|null,
 *   force?: boolean,
 *   root?: string,
 * }} options
 * @returns {object}
 */
export function prepareRelease(options) {
  const root = path.resolve(options.root ?? ROOT);
  const installerPath = path.resolve(options.installer);
  if (!fs.existsSync(installerPath) || !fs.statSync(installerPath).isFile()) {
    fail(`安装包不存在：${installerPath}`);
  }
  const installerName = path.basename(installerPath);
  const version = normalizeVersion(options.version ?? readProjectVersions(root).packageVersion);
  if (!version) fail("无法确定版本号");
  assertProjectVersion(root, version);
  if (!installerName.includes(version)) {
    fail(`安装包文件名不包含版本号 ${version}：${installerName}`);
  }

  const signaturePath = path.resolve(options.signature ?? `${installerPath}.sig`);
  if (!fs.existsSync(signaturePath) || !fs.statSync(signaturePath).isFile()) {
    fail(`签名文件不存在：${signaturePath}`);
  }
  const configPath = path.resolve(options.config ?? DEFAULT_CONFIG);
  const publicKey = options.pubkey ?? readUpdaterPubkey(configPath);
  const verification = verifyUpdateSignature({
    installerPath,
    signature: signaturePath,
    publicKey,
    expectedVersion: version,
    expectedFile: installerName,
  });

  const giteeUrl = buildReleaseAssetUrl({
    provider: "gitee",
    repo: options.giteeRepo,
    url: options.giteeUrl,
    version,
    filename: installerName,
  });
  const githubUrl = buildReleaseAssetUrl({
    provider: "github",
    repo: options.githubRepo,
    url: options.githubUrl,
    version,
    filename: installerName,
  });
  if (!giteeUrl && !githubUrl) {
    fail("至少需要 --gitee-repo/--gitee-url 或 --github-repo/--github-url 之一");
  }

  const notes = options.notesFile
    ? fs.readFileSync(path.resolve(options.notesFile), "utf8").trim()
    : `PRTS Terminal ${version}`;
  if (!notes) fail("release notes 为空");
  const pubDate = options.pubDate ?? new Date().toISOString();
  if (Number.isNaN(Date.parse(pubDate))) {
    fail(`pub_date 不是有效日期：${pubDate}`);
  }
  const platform = options.platform ?? "windows-x86_64";
  const signature = fs.readFileSync(signaturePath, "utf8").trim();
  if (!signature) fail("签名文件为空");

  const outRoot = path.resolve(options.outDir ?? DEFAULT_OUT_DIR);
  const targetDir = path.join(outRoot, version);
  ensureEmptyOrForced(targetDir, Boolean(options.force));
  fs.mkdirSync(targetDir, { recursive: true });

  const stagedInstaller = path.join(targetDir, installerName);
  const stagedSignature = path.join(targetDir, path.basename(signaturePath));
  fs.copyFileSync(installerPath, stagedInstaller);
  fs.copyFileSync(signaturePath, stagedSignature);

  const sha256 = sha256File(installerPath);
  const checksumsPath = path.join(targetDir, "SHA256SUMS.txt");
  fs.writeFileSync(checksumsPath, `${sha256} *${installerName}\n`, "utf8");
  const stagedNotesPath = path.join(targetDir, "release-notes.md");
  fs.writeFileSync(stagedNotesPath, `${notes}\n`, "utf8");

  const manifests = [];
  for (const [provider, url] of [
    ["gitee", giteeUrl],
    ["github", githubUrl],
  ]) {
    if (!url) continue;
    const manifest = buildLatestJson({
      version,
      notes,
      pubDate,
      platform,
      signature,
      url,
    });
    const manifestPath = path.join(targetDir, provider, "latest.json");
    writeJson(manifestPath, manifest);
    manifests.push({ provider, path: manifestPath, url });
  }

  return {
    version,
    installer: stagedInstaller,
    signature: stagedSignature,
    sha256,
    checksums: checksumsPath,
    notes: stagedNotesPath,
    manifests,
    verification,
  };
}

function parseArgs(argv) {
  const args = {
    installer: null,
    signature: null,
    version: null,
    notesFile: null,
    pubDate: null,
    platform: "windows-x86_64",
    giteeRepo: null,
    giteeUrl: null,
    githubRepo: null,
    githubUrl: null,
    outDir: null,
    config: null,
    pubkey: null,
    force: false,
    json: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--installer") args.installer = argv[++i];
    else if (arg === "--signature") args.signature = argv[++i];
    else if (arg === "--version") args.version = argv[++i];
    else if (arg === "--notes-file") args.notesFile = argv[++i];
    else if (arg === "--pub-date") args.pubDate = argv[++i];
    else if (arg === "--platform") args.platform = argv[++i];
    else if (arg === "--gitee-repo") args.giteeRepo = argv[++i];
    else if (arg === "--gitee-url") args.giteeUrl = argv[++i];
    else if (arg === "--github-repo") args.githubRepo = argv[++i];
    else if (arg === "--github-url") args.githubUrl = argv[++i];
    else if (arg === "--out-dir") args.outDir = argv[++i];
    else if (arg === "--config") args.config = argv[++i];
    else if (arg === "--pubkey") args.pubkey = argv[++i];
    else if (arg === "--force") args.force = true;
    else if (arg === "--json") args.json = true;
    else if (arg === "--help" || arg === "-h") args.help = true;
    else fail(`未知参数：${arg}`);
  }
  return args;
}

function printHelp() {
  console.log(`Usage: node scripts/prepare-release.mjs --installer <file> [options]

Options:
  --gitee-repo <owner/repo>   Gitee release repository
  --github-repo <owner/repo>  GitHub release repository
  --gitee-url <url>           Explicit Gitee installer URL (overrides repo)
  --github-url <url>          Explicit GitHub installer URL (overrides repo)
  --signature <file>          Signature path (default: <installer>.sig)
  --version <version>         Expected version (default: package.json)
  --notes-file <file>         Release notes (default: PRTS Terminal <version>)
  --pub-date <iso>            Manifest pub_date (default: now)
  --platform <key>            Platform key (default: windows-x86_64)
  --out-dir <dir>             Staging root (default: release-staging)
  --config <path>             Tauri config with plugins.updater.pubkey
  --pubkey <value>            Override the updater public key
  --force                     Overwrite a non-empty staging directory
  --json                      Print a JSON summary
  --help                      Show this help`);
}

export function run(argv = process.argv.slice(2)) {
  const args = parseArgs(argv);
  if (args.help) {
    printHelp();
    return 0;
  }
  if (!args.installer) fail("缺少 --installer");
  const result = prepareRelease(args);
  if (args.json) {
    console.log(JSON.stringify(result, null, 2));
  } else {
    console.log(`发布暂存已生成：${path.dirname(result.installer)}`);
    console.log(`  版本：${result.version}`);
    console.log(`  SHA-256：${result.sha256}`);
    console.log(`  签名：${result.verification.keyId} / ${result.verification.prehashed ? "prehashed" : "legacy"}`);
    for (const manifest of result.manifests) {
      console.log(`  ${manifest.provider}：${manifest.path}`);
    }
  }
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    process.exitCode = run();
  } catch (error) {
    console.error(`发布暂存生成失败：${error?.message ?? error}`);
    process.exitCode = 1;
  }
}
