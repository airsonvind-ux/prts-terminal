#!/usr/bin/env node
/**
 * Prepare a frozen data release directory from a database and its manifest.
 *
 * The output is a directory that can be uploaded as a GitHub/Gitee release
 * asset set. It does not upload anything and does not run the data pipeline.
 *
 * Usage:
 *   node scripts/prepare-data-release.mjs \
 *     --db src-tauri/resources/arknights.db \
 *     --manifest src-tauri/resources/DATA_MANIFEST.json \
 *     --data-version 2026.10.25 \
 *     --out-dir release-staging/data-2026.10.25
 */

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(SCRIPT_DIR, "..");
const DATA_VERSION_RE = /^\d{4}\.\d{2}\.\d{2}(?:\.\d+)?$/;

function fail(message) {
  throw new Error(message);
}

export function sha256File(filePath) {
  return crypto.createHash("sha256").update(fs.readFileSync(filePath)).digest("hex").toUpperCase();
}

export function normalizeDataVersion(value) {
  return String(value ?? "").trim().replace(/^v/i, "");
}

export function validateDataVersion(value) {
  const version = normalizeDataVersion(value);
  if (!DATA_VERSION_RE.test(version)) {
    fail(`数据版本格式无效：${JSON.stringify(value)}，应为 YYYY.MM.DD[.N]`);
  }
  return version;
}

/**
 * Validate a data manifest against the actual database file.
 *
 * @param {{
 *   manifest: object,
 *   databasePath: string,
 *   expectedVersion?: string|null,
 *   requireContract?: boolean,
 * }} options
 * @returns {{dataVersion: string, sha256: string, schemaVersion: number, passed: number|null, ignored: number|null}}
 */
export function validateDataArtifact({
  manifest,
  databasePath,
  expectedVersion = null,
  requireContract = true,
}) {
  if (!manifest || typeof manifest !== "object") fail("DATA_MANIFEST.json 不是对象");
  if (!fs.existsSync(databasePath) || !fs.statSync(databasePath).isFile()) {
    fail(`数据库不存在：${databasePath}`);
  }

  const dataVersion = validateDataVersion(manifest.dataVersion);
  if (expectedVersion && dataVersion !== validateDataVersion(expectedVersion)) {
    fail(`manifest 数据版本不一致：${dataVersion} != ${normalizeDataVersion(expectedVersion)}`);
  }

  const expectedSha = String(manifest?.database?.sha256 ?? "").trim().toUpperCase();
  if (!expectedSha) fail("manifest 缺少 database.sha256");
  const actualSha = sha256File(databasePath);
  if (actualSha !== expectedSha) {
    fail(`数据库 SHA-256 与 manifest 不一致：${actualSha} != ${expectedSha}`);
  }

  const schemaVersion = Number(manifest.schemaVersion);
  if (!Number.isInteger(schemaVersion) || schemaVersion < 1) {
    fail(`manifest schemaVersion 无效：${JSON.stringify(manifest.schemaVersion)}`);
  }

  const contract = manifest.dataContract;
  if (requireContract) {
    if (!contract || contract.status !== "passed") {
      fail(`数据契约未通过：${JSON.stringify(contract?.status ?? null)}`);
    }
    const passed = Number(contract.passed);
    const ignored = Number(contract.ignored);
    if (!Number.isInteger(passed) || passed < 0 || !Number.isInteger(ignored) || ignored < 0) {
      fail("manifest dataContract.passed/ignored 无效");
    }
  }

  return {
    dataVersion,
    sha256: actualSha,
    schemaVersion,
    passed: contract ? Number(contract.passed) : null,
    ignored: contract ? Number(contract.ignored) : null,
    contract: contract
      ? { passed: Number(contract.passed), ignored: Number(contract.ignored) }
      : null,
  };
}

/**
 * Build a frozen data release directory.
 *
 * @param {{
 *   databasePath: string,
 *   manifestPath: string,
 *   dataVersion?: string|null,
 *   outDir?: string|null,
 *   notesFile?: string|null,
 *   force?: boolean,
 *   requireContract?: boolean,
 * }} options
 * @returns {object}
 */
export function prepareDataRelease(options) {
  const databasePath = path.resolve(options.databasePath);
  const manifestPath = path.resolve(options.manifestPath);
  if (!fs.existsSync(manifestPath) || !fs.statSync(manifestPath).isFile()) {
    fail(`manifest 不存在：${manifestPath}`);
  }

  let manifest;
  try {
    manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
  } catch (error) {
    fail(`manifest 无法解析：${error?.message ?? error}`);
  }

  const version = validateDataVersion(options.dataVersion ?? manifest.dataVersion);
  const checked = validateDataArtifact({
    manifest,
    databasePath,
    expectedVersion: version,
    requireContract: options.requireContract !== false,
  });

  const outDir = path.resolve(options.outDir ?? path.join(ROOT, "release-staging", `data-${version}`));
  if (fs.existsSync(outDir)) {
    const entries = fs.readdirSync(outDir);
    if (entries.length > 0 && !options.force) {
      fail(`目标目录已存在且非空：${outDir}；需要覆盖时加 --force`);
    }
    if (options.force) fs.rmSync(outDir, { recursive: true, force: true });
  }
  fs.mkdirSync(outDir, { recursive: true });

  const stagedDb = path.join(outDir, "arknights.db");
  const stagedManifest = path.join(outDir, "DATA_MANIFEST.json");
  fs.copyFileSync(databasePath, stagedDb);
  fs.copyFileSync(manifestPath, stagedManifest);

  const manifestSha = sha256File(stagedManifest);
  const checksumsPath = path.join(outDir, "SHA256SUMS.txt");
  fs.writeFileSync(
    checksumsPath,
    `${checked.sha256} *arknights.db\n${manifestSha} *DATA_MANIFEST.json\n`,
    "utf8",
  );

  let notesPath = null;
  if (options.notesFile) {
    const notes = fs.readFileSync(path.resolve(options.notesFile), "utf8").trim();
    if (!notes) fail("release notes 为空");
    notesPath = path.join(outDir, "release-notes.md");
    fs.writeFileSync(notesPath, `${notes}\n`, "utf8");
  }

  return {
    dataVersion: version,
    database: stagedDb,
    manifest: stagedManifest,
    checksums: checksumsPath,
    notes: notesPath,
    sha256: checked.sha256,
    manifestSha256: manifestSha,
    schemaVersion: checked.schemaVersion,
    contract: { passed: checked.passed, ignored: checked.ignored },
  };
}

function parseArgs(argv) {
  const args = {
    db: null,
    manifest: null,
    dataVersion: null,
    outDir: null,
    notesFile: null,
    force: false,
    json: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--db") args.db = argv[++i];
    else if (arg === "--manifest") args.manifest = argv[++i];
    else if (arg === "--data-version") args.dataVersion = argv[++i];
    else if (arg === "--out-dir") args.outDir = argv[++i];
    else if (arg === "--notes-file") args.notesFile = argv[++i];
    else if (arg === "--force") args.force = true;
    else if (arg === "--json") args.json = true;
    else if (arg === "--help" || arg === "-h") args.help = true;
    else fail(`未知参数：${arg}`);
  }
  return args;
}

function printHelp() {
  console.log(`Usage: node scripts/prepare-data-release.mjs --db <file> --manifest <file> [options]

Options:
  --data-version <version>   Expected data version (default: manifest.dataVersion)
  --out-dir <dir>            Output directory (default: release-staging/data-<version>)
  --notes-file <file>        Optional release notes
  --force                    Overwrite a non-empty output directory
  --json                     Print a JSON summary
  --help                     Show this help`);
}

export function run(argv = process.argv.slice(2)) {
  const args = parseArgs(argv);
  if (args.help) {
    printHelp();
    return 0;
  }
  if (!args.db) fail("缺少 --db");
  if (!args.manifest) fail("缺少 --manifest");
  const result = prepareDataRelease({
    databasePath: args.db,
    manifestPath: args.manifest,
    dataVersion: args.dataVersion,
    outDir: args.outDir,
    notesFile: args.notesFile,
    force: args.force,
  });
  if (args.json) {
    console.log(JSON.stringify(result, null, 2));
  } else {
    console.log(`数据发布包已生成：${path.dirname(result.database)}`);
    console.log(`  dataVersion：${result.dataVersion}`);
    console.log(`  DB SHA-256：${result.sha256}`);
    console.log(`  契约：${result.contract.passed ?? "?"} passed / ${result.contract.ignored ?? "?"} ignored`);
  }
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    process.exitCode = run();
  } catch (error) {
    console.error(`数据发布包生成失败：${error?.message ?? error}`);
    process.exitCode = 1;
  }
}
