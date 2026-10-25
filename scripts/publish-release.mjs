#!/usr/bin/env node
/**
 * Publish an already-prepared release staging directory.
 *
 * GitHub uses the `gh` CLI. Gitee uses its v5 release API. Both providers
 * support a dry-run that only prints the planned operations.
 *
 * The script never builds, signs, or edits source files.
 *
 * Usage:
 *   node scripts/publish-release.mjs \
 *     --staging-dir release-staging/0.1.0 \
 *     --version 0.1.0 \
 *     --github-repo owner/prts-terminal-release \
 *     --gitee-repo owner/prts-terminal-release \
 *     --dry-run
 */

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(SCRIPT_DIR, "..");
const GITEE_API = "https://gitee.com/api/v5";

function fail(message) {
  throw new Error(message);
}

function normalizeVersion(value) {
  return String(value ?? "").trim().replace(/^v/i, "");
}

function requireFile(filePath, label) {
  if (!fs.existsSync(filePath) || !fs.statSync(filePath).isFile()) {
    fail(`${label}不存在：${filePath}`);
  }
  return filePath;
}

/**
 * Collect the files that belong to an app release.
 *
 * @param {string} stagingDir
 * @returns {{installer: string, signature: string, checksums: string, notes: string|null, manifests: Record<string,string>}}
 */
export function collectReleaseAssets(stagingDir) {
  const dir = path.resolve(stagingDir);
  if (!fs.existsSync(dir) || !fs.statSync(dir).isDirectory()) {
    fail(`发布暂存目录不存在：${dir}`);
  }
  const entries = fs.readdirSync(dir);
  const installers = entries.filter((name) => name.toLowerCase().endsWith(".exe"));
  if (installers.length !== 1) {
    fail(`发布暂存目录应只有一个 .exe，实际 ${installers.length} 个`);
  }
  const installer = path.join(dir, installers[0]);
  const signature = requireFile(`${installer}.sig`, "签名文件");
  const checksums = requireFile(path.join(dir, "SHA256SUMS.txt"), "校验和文件");
  const notesCandidate = path.join(dir, "release-notes.md");
  const notes = fs.existsSync(notesCandidate) ? notesCandidate : null;
  const manifests = {};
  for (const provider of ["gitee", "github"]) {
    const manifest = path.join(dir, provider, "latest.json");
    if (fs.existsSync(manifest)) manifests[provider] = manifest;
  }
  return { installer, signature, checksums, notes, manifests };
}

export function githubReleaseCommand({ tag, repo, assets, notes }) {
  const args = ["release", "create", tag, "--repo", repo, "--title", tag];
  if (notes) args.push("--notes-file", notes);
  else args.push("--notes", `PRTS Terminal ${tag}`);
  for (const asset of assets) args.push(asset);
  return { command: "gh", args };
}

export function giteeReleasePayload({ tag, notes }) {
  return {
    tag_name: tag,
    name: tag,
    body: notes ? fs.readFileSync(notes, "utf8").trim() : `PRTS Terminal ${tag}`,
    prerelease: false,
  };
}

async function giteeRequest(pathname, { method = "GET", token, json, form } = {}) {
  const url = new URL(`${GITEE_API}${pathname}`);
  if (token) url.searchParams.set("access_token", token);
  const init = { method, headers: {} };
  if (json) {
    init.headers["Content-Type"] = "application/json";
    init.body = JSON.stringify(json);
  }
  if (form) init.body = form;
  const response = await fetch(url, init);
  const text = await response.text();
  let body = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = text;
  }
  if (!response.ok) {
    fail(`Gitee API ${method} ${pathname} 失败：${response.status} ${response.statusText} ${text}`);
  }
  return body;
}

async function ensureGiteeRelease({ repo, tag, notes, token }) {
  const [owner, name] = repo.split("/");
  if (!owner || !name) fail(`Gitee 发布仓必须是 owner/repo 形式：${repo}`);
  try {
    return await giteeRequest(`/repos/${owner}/${name}/releases/tags/${encodeURIComponent(tag)}`, {
      token,
    });
  } catch (error) {
    if (!String(error?.message ?? error).includes("404")) throw error;
  }
  return giteeRequest(`/repos/${owner}/${name}/releases`, {
    method: "POST",
    token,
    json: giteeReleasePayload({ tag, notes }),
  });
}

async function uploadGiteeAssets({ repo, releaseId, assets, token }) {
  const [owner, name] = repo.split("/");
  const uploaded = [];
  for (const asset of assets) {
    const form = new FormData();
    form.append("access_token", token);
    form.append("file", new Blob([fs.readFileSync(asset)]), path.basename(asset));
    const result = await giteeRequest(
      `/repos/${owner}/${name}/releases/${releaseId}/attach_files`,
      { method: "POST", token, form },
    );
    uploaded.push({ asset, result });
  }
  return uploaded;
}

function runGh(args, { token }) {
  const result = spawnSync("gh", args, {
    encoding: "utf8",
    env: { ...process.env, GH_TOKEN: token ?? process.env.GH_TOKEN ?? "" },
  });
  if (result.status !== 0) {
    fail(`gh ${args.join(" ")} 失败：${result.stderr || result.stdout || result.status}`);
  }
  return result.stdout;
}

async function publishGithub({ repo, tag, assets, notes, token, dryRun }) {
  const args = ["release", "view", tag, "--repo", repo];
  if (dryRun) {
    const create = githubReleaseCommand({ tag, repo, assets, notes });
    return [{ provider: "github", dryRun: true, command: create.command, args: create.args }];
  }
  const exists = spawnSync("gh", args, {
    encoding: "utf8",
    env: { ...process.env, GH_TOKEN: token ?? process.env.GH_TOKEN ?? "" },
  }).status === 0;
  if (!exists) {
    const create = githubReleaseCommand({ tag, repo, assets, notes });
    runGh(create.args, { token });
    return [{ provider: "github", created: true, tag }];
  }
  runGh(["release", "upload", tag, "--repo", repo, "--clobber", ...assets], { token });
  return [{ provider: "github", uploaded: assets.length, tag }];
}

async function publishGitee({ repo, tag, assets, notes, token, dryRun }) {
  if (dryRun) {
    return [{ provider: "gitee", dryRun: true, tag, assets }];
  }
  if (!token) fail("缺少 GITEE_TOKEN");
  const release = await ensureGiteeRelease({ repo, tag, notes, token });
  const releaseId = release?.id;
  if (!releaseId) fail("Gitee release 响应缺少 id");
  const uploaded = await uploadGiteeAssets({ repo, releaseId, assets, token });
  return [{ provider: "gitee", releaseId, uploaded: uploaded.length, tag }];
}

/**
 * Publish a staged release to GitHub and/or Gitee.
 *
 * @param {{
 *   stagingDir: string,
 *   version?: string|null,
 *   tag?: string|null,
 *   githubRepo?: string|null,
 *   giteeRepo?: string|null,
 *   githubToken?: string|null,
 *   giteeToken?: string|null,
 *   dryRun?: boolean,
 * }} options
 * @returns {Promise<object>}
 */
export async function publishRelease(options) {
  const assets = collectReleaseAssets(options.stagingDir);
  const version = normalizeVersion(options.version ?? path.basename(path.resolve(options.stagingDir)));
  if (!version) fail("无法确定版本号");
  const tag = options.tag ?? `v${version}`;
  const results = [];

  if (options.githubRepo) {
    if (!assets.manifests.github) fail("缺少 github/latest.json");
    results.push(
      ...(await publishGithub({
        repo: options.githubRepo,
        tag,
        assets: [assets.installer, assets.signature, assets.checksums, assets.notes, assets.manifests.github].filter(Boolean),
        notes: assets.notes,
        token: options.githubToken,
        dryRun: Boolean(options.dryRun),
      })),
    );
  }
  if (options.giteeRepo) {
    if (!assets.manifests.gitee) fail("缺少 gitee/latest.json");
    results.push(
      ...(await publishGitee({
        repo: options.giteeRepo,
        tag,
        assets: [assets.installer, assets.signature, assets.checksums, assets.notes, assets.manifests.gitee].filter(Boolean),
        notes: assets.notes,
        token: options.giteeToken,
        dryRun: Boolean(options.dryRun),
      })),
    );
  }
  if (results.length === 0) fail("至少需要 --github-repo 或 --gitee-repo 之一");
  return { version, tag, results };
}

function parseArgs(argv) {
  const args = {
    stagingDir: null,
    version: null,
    tag: null,
    githubRepo: null,
    giteeRepo: null,
    githubToken: null,
    giteeToken: null,
    dryRun: false,
    json: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--staging-dir") args.stagingDir = argv[++i];
    else if (arg === "--version") args.version = argv[++i];
    else if (arg === "--tag") args.tag = argv[++i];
    else if (arg === "--github-repo") args.githubRepo = argv[++i];
    else if (arg === "--gitee-repo") args.giteeRepo = argv[++i];
    else if (arg === "--github-token") args.githubToken = argv[++i];
    else if (arg === "--gitee-token") args.giteeToken = argv[++i];
    else if (arg === "--dry-run") args.dryRun = true;
    else if (arg === "--json") args.json = true;
    else if (arg === "--help" || arg === "-h") args.help = true;
    else fail(`未知参数：${arg}`);
  }
  return args;
}

function printHelp() {
  console.log(`Usage: node scripts/publish-release.mjs --staging-dir <dir> [options]

Options:
  --version <version>        Release version (default: staging directory name)
  --tag <tag>                Release tag (default: v<version>)
  --github-repo <owner/repo> GitHub release repository
  --gitee-repo <owner/repo>  Gitee release repository
  --github-token <token>     GitHub token (default: GH_TOKEN)
  --gitee-token <token>      Gitee token (default: GITEE_TOKEN)
  --dry-run                  Print planned operations, make no remote writes
  --json                     Print a JSON summary
  --help                     Show this help`);
}

export async function run(argv = process.argv.slice(2)) {
  const args = parseArgs(argv);
  if (args.help) {
    printHelp();
    return 0;
  }
  if (!args.stagingDir) fail("缺少 --staging-dir");
  const result = await publishRelease({
    stagingDir: args.stagingDir,
    version: args.version,
    tag: args.tag,
    githubRepo: args.githubRepo,
    giteeRepo: args.giteeRepo,
    githubToken: args.githubToken ?? process.env.GH_TOKEN ?? null,
    giteeToken: args.giteeToken ?? process.env.GITEE_TOKEN ?? null,
    dryRun: args.dryRun,
  });
  if (args.json) {
    console.log(JSON.stringify(result, null, 2));
  } else {
    console.log(`发布计划：${result.tag}`);
    for (const item of result.results) {
      console.log(`  ${item.provider}: ${JSON.stringify(item)}`);
    }
  }
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    process.exitCode = await run();
  } catch (error) {
    console.error(`发布失败：${error?.message ?? error}`);
    process.exitCode = 1;
  }
}
