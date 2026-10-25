import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { createMinisignFixture } from "./minisign-fixture.mjs";
import {
  parseMinisignPublicKey,
  parseTrustedComment,
  verifyUpdateSignature,
} from "../verify-update-signature.mjs";

const tempDirs = [];

function tempDir() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "prts-signature-"));
  tempDirs.push(dir);
  return dir;
}

function writeFixture(fixture, filename = fixture.filename) {
  const dir = tempDir();
  const installerPath = path.join(dir, filename);
  const signaturePath = `${installerPath}.sig`;
  fs.writeFileSync(installerPath, fixture.data);
  fs.writeFileSync(signaturePath, fixture.signatureBase64, "utf8");
  return { dir, installerPath, signaturePath };
}

afterEach(() => {
  for (const dir of tempDirs.splice(0)) {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

describe("minisign public key parsing", () => {
  it("parses a base64-encoded minisign public-key file", () => {
    const fixture = createMinisignFixture();
    const parsed = parseMinisignPublicKey(fixture.tauriPubkey);
    expect(parsed.algorithm).toBe("ED");
    expect(parsed.keyId.toString("hex")).toBe(fixture.keyId.toString("hex"));
    expect(parsed.key).toHaveLength(32);
  });

  it("parses minisign public-key text and a raw 42-byte base64 key", () => {
    const fixture = createMinisignFixture();
    const fromText = parseMinisignPublicKey(fixture.publicKeyText);
    expect(fromText.keyId.toString("hex")).toBe(fixture.keyId.toString("hex"));

    const raw = Buffer.from(fixture.publicKeyText.split("\n")[1], "base64").toString("base64");
    const fromRaw = parseMinisignPublicKey(raw);
    expect(fromRaw.keyId.toString("hex")).toBe(fixture.keyId.toString("hex"));
  });
});

describe("Tauri updater signature verification", () => {
  it("verifies a prehashed signature and trusted-comment fields", () => {
    const fixture = createMinisignFixture();
    const paths = writeFixture(fixture);
    const result = verifyUpdateSignature({
      installerPath: paths.installerPath,
      signature: paths.signaturePath,
      publicKey: fixture.tauriPubkey,
      expectedVersion: fixture.version,
      expectedFile: fixture.filename,
    });
    expect(result.prehashed).toBe(true);
    expect(result.keyId).toBe(fixture.keyId.toString("hex"));
    expect(result.fields.version).toBe(fixture.version);
    expect(result.fields.file).toBe(fixture.filename);
  });

  it("rejects a tampered installer", () => {
    const fixture = createMinisignFixture();
    const paths = writeFixture(fixture);
    const tampered = Buffer.from(fixture.data);
    tampered[0] ^= 0xff;
    fs.writeFileSync(paths.installerPath, tampered);
    expect(() =>
      verifyUpdateSignature({
        installerPath: paths.installerPath,
        signature: paths.signaturePath,
        publicKey: fixture.tauriPubkey,
      }),
    ).toThrow(/安装包签名校验失败/);
  });

  it("rejects a version mismatch", () => {
    const fixture = createMinisignFixture({ version: "1.2.3" });
    const paths = writeFixture(fixture);
    expect(() =>
      verifyUpdateSignature({
        installerPath: paths.installerPath,
        signature: paths.signaturePath,
        publicKey: fixture.tauriPubkey,
        expectedVersion: "1.2.4",
      }),
    ).toThrow(/签名版本 1\.2\.3 与期望版本 1\.2\.4 不一致/);
  });

  it("rejects a filename mismatch", () => {
    const fixture = createMinisignFixture({ filename: "installer.exe" });
    const paths = writeFixture(fixture);
    expect(() =>
      verifyUpdateSignature({
        installerPath: paths.installerPath,
        signature: paths.signaturePath,
        publicKey: fixture.tauriPubkey,
        expectedFile: "other.exe",
      }),
    ).toThrow(/签名文件名 installer\.exe 与期望文件名 other\.exe 不一致/);
  });

  it("rejects a key-id mismatch", () => {
    const signed = createMinisignFixture();
    const otherKey = createMinisignFixture();
    const paths = writeFixture(signed);
    expect(() =>
      verifyUpdateSignature({
        installerPath: paths.installerPath,
        signature: paths.signaturePath,
        publicKey: otherKey.tauriPubkey,
      }),
    ).toThrow(/key id .* 与公钥 key id .* 不一致/);
  });

  it("supports legacy signatures only when explicitly allowed", () => {
    const fixture = createMinisignFixture({ prehashed: false });
    const paths = writeFixture(fixture);
    expect(() =>
      verifyUpdateSignature({
        installerPath: paths.installerPath,
        signature: paths.signaturePath,
        publicKey: fixture.tauriPubkey,
      }),
    ).not.toThrow();
    expect(() =>
      verifyUpdateSignature({
        installerPath: paths.installerPath,
        signature: paths.signaturePath,
        publicKey: fixture.tauriPubkey,
        allowLegacy: false,
      }),
    ).toThrow(/legacy/);
  });

  it("parses trusted-comment fields without treating spaces in filenames as separators", () => {
    expect(
      parseTrustedComment(
        "timestamp:1700000000\tfile:PRTS Terminal_0.1.0_x64-setup.exe\tversion:0.1.0",
      ),
    ).toEqual({
      timestamp: "1700000000",
      file: "PRTS Terminal_0.1.0_x64-setup.exe",
      version: "0.1.0",
    });
  });
});
