#!/usr/bin/env node
/**
 * Verify a Tauri updater minisign signature without the Tauri CLI.
 *
 * Tauri's updater artifact signature is a base64-encoded minisign signature
 * file. The public key in `tauri.conf.json` is a base64-encoded minisign
 * public key file. This script parses both formats, checks the key id, verifies
 * the installer signature and the trusted-comment signature, and optionally
 * checks the expected version/file name.
 *
 * Usage:
 *   node scripts/verify-update-signature.mjs \
 *     --installer <installer.exe> \
 *     --signature <installer.exe.sig> \
 *     --config src-tauri/tauri.conf.json \
 *     [--version 0.1.0] [--file <installer.exe>] [--json]
 */

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");
const PUBLIC_KEY_BYTES = 42;
const SIGNATURE_BYTES = 74;
const GLOBAL_SIGNATURE_BYTES = 64;
const TRUSTED_COMMENT_PREFIX = "trusted comment: ";

function fail(message) {
  throw new Error(message);
}

function readTextInput(input, label) {
  if (typeof input !== "string" || input.length === 0) {
    fail(`${label} 为空`);
  }
  try {
    if (fs.existsSync(input) && fs.statSync(input).isFile()) {
      return fs.readFileSync(input, "utf8");
    }
  } catch {
    // 长 base64 字符串可能不是合法路径；继续按内联文本处理。
  }
  return input;
}

function decodeMinisignText(input, label) {
  const text = readTextInput(input, label);
  if (text.includes("untrusted comment:")) return text;
  const decoded = Buffer.from(text.trim(), "base64").toString("utf8");
  if (!decoded.includes("untrusted comment:")) {
    fail(`${label} 既不是 minisign 文本，也不是 base64 编码的 minisign 文本`);
  }
  return decoded;
}

function parsePublicKeyBytes(bytes) {
  if (bytes.length !== PUBLIC_KEY_BYTES) {
    fail(`minisign 公钥长度应为 ${PUBLIC_KEY_BYTES} 字节，实际 ${bytes.length}`);
  }
  const algorithm = bytes.subarray(0, 2).toString("ascii");
  if (algorithm !== "Ed" && algorithm !== "ED") {
    fail(`不支持的 minisign 公钥算法：${JSON.stringify(algorithm)}`);
  }
  return {
    algorithm,
    keyId: Buffer.from(bytes.subarray(2, 10)),
    key: Buffer.from(bytes.subarray(10, 42)),
  };
}

/**
 * Parse a minisign public key from:
 * - a minisign public-key file path,
 * - the minisign public-key text itself,
 * - a base64-encoded minisign public-key file (as used by tauri.conf.json),
 * - a raw base64-encoded 42-byte minisign public key.
 *
 * @param {string} input
 * @returns {{algorithm: string, keyId: Buffer, key: Buffer}}
 */
export function parseMinisignPublicKey(input) {
  const text = readTextInput(input, "minisign 公钥");
  if (!text.includes("untrusted comment:")) {
    const decoded = Buffer.from(text.trim(), "base64");
    if (decoded.length === PUBLIC_KEY_BYTES) {
      return parsePublicKeyBytes(decoded);
    }
    const decodedText = decoded.toString("utf8");
    if (!decodedText.includes("untrusted comment:")) {
      fail("minisign 公钥格式无法识别");
    }
    return parseMinisignPublicKeyText(decodedText);
  }
  return parseMinisignPublicKeyText(text);
}

function parseMinisignPublicKeyText(text) {
  const lines = text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  if (lines.length < 2 || !lines[0].startsWith("untrusted comment:")) {
    fail("minisign 公钥文本缺少 untrusted comment 或公钥行");
  }
  return parsePublicKeyBytes(Buffer.from(lines[1], "base64"));
}

/**
 * Parse a minisign signature from either a .sig file path, the signature text,
 * or its base64-encoded representation.
 *
 * @param {string} input
 * @returns {{
 *   algorithm: string,
 *   keyId: Buffer,
 *   signature: Buffer,
 *   globalSignature: Buffer,
 *   trustedComment: string,
 *   prehashed: boolean,
 * }}
 */
export function parseMinisignSignature(input) {
  const text = decodeMinisignText(input, "minisign 签名");
  const lines = text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean);
  if (lines.length < 4) {
    fail("minisign 签名至少需要 4 行");
  }
  if (!lines[0].startsWith("untrusted comment:")) {
    fail("minisign 签名缺少 untrusted comment");
  }
  const signatureBytes = Buffer.from(lines[1], "base64");
  if (signatureBytes.length !== SIGNATURE_BYTES) {
    fail(`minisign 签名长度应为 ${SIGNATURE_BYTES} 字节，实际 ${signatureBytes.length}`);
  }
  if (!lines[2].startsWith(TRUSTED_COMMENT_PREFIX)) {
    fail("minisign 签名缺少 trusted comment");
  }
  const globalSignature = Buffer.from(lines[3], "base64");
  if (globalSignature.length !== GLOBAL_SIGNATURE_BYTES) {
    fail(`minisign 全局签名长度应为 ${GLOBAL_SIGNATURE_BYTES} 字节，实际 ${globalSignature.length}`);
  }
  const algorithm = signatureBytes.subarray(0, 2).toString("ascii");
  if (algorithm !== "Ed" && algorithm !== "ED") {
    fail(`不支持的 minisign 签名算法：${JSON.stringify(algorithm)}`);
  }
  return {
    algorithm,
    keyId: Buffer.from(signatureBytes.subarray(2, 10)),
    signature: Buffer.from(signatureBytes.subarray(10, 74)),
    globalSignature,
    trustedComment: lines[2].slice(TRUSTED_COMMENT_PREFIX.length),
    prehashed: algorithm === "ED",
  };
}

/**
 * Split Tauri's trusted comment into its tab-separated fields.
 *
 * Example:
 *   timestamp:1792503381	file:PRTS Terminal_0.1.0_x64-setup.exe	version:0.1.0
 *
 * @param {string} trustedComment
 * @returns {Record<string, string>}
 */
export function parseTrustedComment(trustedComment) {
  const fields = {};
  for (const part of trustedComment.split("\t")) {
    const separator = part.indexOf(":");
    if (separator <= 0) continue;
    const key = part.slice(0, separator).trim();
    const value = part.slice(separator + 1).trim();
    if (key) fields[key] = value;
  }
  return fields;
}

function normalizeVersion(value) {
  return String(value ?? "").trim().replace(/^v/i, "");
}

function publicKeyObject(rawKey) {
  return crypto.createPublicKey({
    key: Buffer.concat([ED25519_SPKI_PREFIX, rawKey]),
    format: "der",
    type: "spki",
  });
}

/**
 * Verify an installer against its Tauri updater .sig file.
 *
 * @param {{
 *   installerPath: string,
 *   signature: string,
 *   publicKey: string,
 *   expectedVersion?: string,
 *   expectedFile?: string,
 *   allowLegacy?: boolean,
 * }} options
 * @returns {{
 *   algorithm: string,
 *   keyId: string,
 *   trustedComment: string,
 *   fields: Record<string, string>,
 *   prehashed: boolean,
 * }}
 */
export function verifyUpdateSignature({
  installerPath,
  signature,
  publicKey,
  expectedVersion,
  expectedFile,
  allowLegacy = true,
}) {
  if (!installerPath || !fs.existsSync(installerPath)) {
    fail(`安装包不存在：${installerPath ?? "(未提供)"}`);
  }
  const installer = fs.readFileSync(installerPath);
  const parsedKey = parseMinisignPublicKey(publicKey);
  const parsedSignature = parseMinisignSignature(signature);

  if (!parsedKey.keyId.equals(parsedSignature.keyId)) {
    fail(
      `签名 key id ${parsedSignature.keyId.toString("hex")} 与公钥 key id ${parsedKey.keyId.toString("hex")} 不一致`,
    );
  }
  if (!parsedSignature.prehashed && !allowLegacy) {
    fail("签名是 legacy 模式，但当前不允许 legacy 签名");
  }

  const signedData = parsedSignature.prehashed
    ? crypto.createHash("blake2b512").update(installer).digest()
    : installer;
  const keyObject = publicKeyObject(parsedKey.key);
  if (!crypto.verify(null, signedData, keyObject, parsedSignature.signature)) {
    fail("安装包签名校验失败");
  }

  const trustedCommentBytes = Buffer.from(parsedSignature.trustedComment, "utf8");
  const globalData = Buffer.concat([parsedSignature.signature, trustedCommentBytes]);
  if (!crypto.verify(null, globalData, keyObject, parsedSignature.globalSignature)) {
    fail("trusted comment 签名校验失败");
  }

  const fields = parseTrustedComment(parsedSignature.trustedComment);
  if (expectedVersion !== undefined) {
    const actualVersion = normalizeVersion(fields.version);
    const wantedVersion = normalizeVersion(expectedVersion);
    if (actualVersion !== wantedVersion) {
      fail(`签名版本 ${actualVersion || "(缺失)"} 与期望版本 ${wantedVersion} 不一致`);
    }
  }
  if (expectedFile !== undefined) {
    const actualFile = fields.file ? path.basename(fields.file.replace(/\\/g, "/")) : "";
    const wantedFile = path.basename(expectedFile);
    if (actualFile !== wantedFile) {
      fail(`签名文件名 ${actualFile || "(缺失)"} 与期望文件名 ${wantedFile} 不一致`);
    }
  }

  return {
    algorithm: parsedSignature.algorithm,
    keyId: parsedKey.keyId.toString("hex"),
    trustedComment: parsedSignature.trustedComment,
    fields,
    prehashed: parsedSignature.prehashed,
  };
}

function readUpdaterPubkey(configPath) {
  const config = JSON.parse(fs.readFileSync(configPath, "utf8"));
  const pubkey = config?.plugins?.updater?.pubkey;
  if (typeof pubkey !== "string" || pubkey.length === 0) {
    fail(`${configPath} 没有 plugins.updater.pubkey`);
  }
  return pubkey;
}

function parseArgs(argv) {
  const args = {
    installer: null,
    signature: null,
    publicKey: null,
    config: null,
    version: null,
    file: null,
    json: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--installer") args.installer = argv[++i];
    else if (arg === "--signature") args.signature = argv[++i];
    else if (arg === "--pubkey") args.publicKey = argv[++i];
    else if (arg === "--config") args.config = argv[++i];
    else if (arg === "--version") args.version = argv[++i];
    else if (arg === "--file") args.file = argv[++i];
    else if (arg === "--json") args.json = true;
    else if (arg === "--help" || arg === "-h") args.help = true;
    else fail(`未知参数：${arg}`);
  }
  return args;
}

function printHelp() {
  console.log(`Usage: node scripts/verify-update-signature.mjs --installer <file> --signature <file> [options]

Options:
  --config <path>    Read plugins.updater.pubkey from a Tauri config
  --pubkey <value>   Minisign public key file/text, or its base64 form
  --version <value>  Require the signature's trusted-comment version
  --file <name>      Require the signature's trusted-comment file name
  --json             Print the verification result as JSON
  --help             Show this help`);
}

export function run(argv = process.argv.slice(2)) {
  const args = parseArgs(argv);
  if (args.help) {
    printHelp();
    return 0;
  }
  if (!args.installer) fail("缺少 --installer");
  if (!args.signature) fail("缺少 --signature");
  if (!args.publicKey && !args.config) fail("需要 --pubkey 或 --config");
  const publicKey = args.publicKey ?? readUpdaterPubkey(args.config);
  const result = verifyUpdateSignature({
    installerPath: args.installer,
    signature: args.signature,
    publicKey,
    expectedVersion: args.version ?? undefined,
    expectedFile: args.file ?? path.basename(args.installer),
  });
  if (args.json) {
    console.log(JSON.stringify(result, null, 2));
  } else {
    console.log(`签名校验通过：${path.basename(args.installer)}`);
    console.log(`  key id: ${result.keyId}`);
    console.log(`  模式: ${result.prehashed ? "prehashed" : "legacy"}`);
    console.log(`  trusted comment: ${result.trustedComment}`);
  }
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    process.exitCode = run();
  } catch (error) {
    console.error(`签名校验失败：${error?.message ?? error}`);
    process.exitCode = 1;
  }
}
