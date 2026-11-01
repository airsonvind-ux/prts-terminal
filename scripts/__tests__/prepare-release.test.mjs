import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { createMinisignFixture } from "./minisign-fixture.mjs";
import {
  buildLatestJson,
  buildReleaseAssetUrl,
  prepareRelease,
  sha256File,
} from "../prepare-release.mjs";

const tempDirs = [];

function tempDir() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "prts-release-"));
  tempDirs.push(dir);
  return dir;
}

function createProject(version, pubkey) {
  const root = tempDir();
  fs.writeFileSync(path.join(root, "package.json"), JSON.stringify({ version }));
  fs.mkdirSync(path.join(root, "src-tauri"), { recursive: true });
  fs.writeFileSync(
    path.join(root, "src-tauri", "tauri.conf.json"),
    JSON.stringify({ version, plugins: { updater: { pubkey } } }),
  );
  fs.writeFileSync(
    path.join(root, "src-tauri", "Cargo.toml"),
    `[package]\nname = "prts-test"\nversion = "${version}"\n`,
  );
  return root;
}

function writeReleaseFixture({ version = "9.9.9", filename = `PRTS Terminal_${version}_x64-setup.exe` } = {}) {
  const fixture = createMinisignFixture({ version, filename });
  const root = createProject(version, fixture.tauriPubkey);
  const installer = path.join(tempDir(), filename);
  fs.writeFileSync(installer, fixture.data);
  const signature = `${installer}.sig`;
  fs.writeFileSync(signature, fixture.signatureBase64, "utf8");
  const notesFile = path.join(tempDir(), "release-notes.md");
  fs.writeFileSync(notesFile, "# Test release\n\nNotes body.\n", "utf8");
  return { fixture, root, installer, signature, notesFile, version };
}

afterEach(() => {
  for (const dir of tempDirs.splice(0)) {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

describe("release asset URLs", () => {
  it("gitee 保留空格编码，github 用规范化后的点号资产名", () => {
    expect(
      buildReleaseAssetUrl({
        provider: "gitee",
        repo: "owner/prts-terminal-release",
        version: "0.1.0",
        filename: "PRTS Terminal_0.1.0_x64-setup.exe",
      }),
    ).toBe(
      "https://gitee.com/owner/prts-terminal-release/releases/download/v0.1.0/PRTS%20Terminal_0.1.0_x64-setup.exe",
    );
    // GitHub 上传时把资产名里的空白规范化成点，下载 URL 必须跟着用点号，否则 404
    expect(
      buildReleaseAssetUrl({
        provider: "github",
        repo: "owner/prts-terminal-release",
        version: "v0.1.0",
        filename: "PRTS Terminal_0.1.0_x64-setup.exe",
      }),
    ).toBe(
      "https://github.com/owner/prts-terminal-release/releases/download/v0.1.0/PRTS.Terminal_0.1.0_x64-setup.exe",
    );
  });

  it("uses an explicit URL as-is and returns null without a repo", () => {
    expect(
      buildReleaseAssetUrl({
        provider: "gitee",
        url: "https://example.test/installer.exe",
        version: "0.1.0",
        filename: "installer.exe",
      }),
    ).toBe("https://example.test/installer.exe");
    expect(
      buildReleaseAssetUrl({
        provider: "gitee",
        version: "0.1.0",
        filename: "installer.exe",
      }),
    ).toBeNull();
  });
});

describe("latest.json shape", () => {
  it("uses the Tauri updater platform shape", () => {
    expect(
      buildLatestJson({
        version: "0.1.0",
        notes: "notes",
        pubDate: "2026-01-02T03:04:05.000Z",
        platform: "windows-x86_64",
        signature: "sig",
        url: "https://example.test/installer.exe",
      }),
    ).toEqual({
      version: "0.1.0",
      notes: "notes",
      pub_date: "2026-01-02T03:04:05.000Z",
      platforms: {
        "windows-x86_64": {
          signature: "sig",
          url: "https://example.test/installer.exe",
        },
      },
    });
  });
});

describe("prepareRelease", () => {
  it("stages installer, signature, checksums and both manifests", () => {
    const { fixture, root, installer, signature, notesFile, version } = writeReleaseFixture();
    const outDir = tempDir();
    const result = prepareRelease({
      root,
      installer,
      signature,
      notesFile,
      giteeRepo: "owner/prts-terminal-release",
      githubRepo: "owner/prts-terminal-release",
      outDir,
      config: path.join(root, "src-tauri", "tauri.conf.json"),
      pubDate: "2026-01-02T03:04:05.000Z",
    });

    const expectedSha = crypto.createHash("sha256").update(fixture.data).digest("hex").toUpperCase();
    expect(result.version).toBe(version);
    expect(result.sha256).toBe(expectedSha);
    expect(result.manifests.map((item) => item.provider)).toEqual(["gitee", "github"]);
    expect(fs.existsSync(result.installer)).toBe(true);
    expect(fs.existsSync(result.signature)).toBe(true);
    expect(fs.readFileSync(result.checksums, "utf8")).toBe(
      `${expectedSha} *${path.basename(installer)}\n`,
    );

    const gitee = JSON.parse(fs.readFileSync(result.manifests[0].path, "utf8"));
    expect(gitee.version).toBe(version);
    expect(gitee.platforms["windows-x86_64"].signature).toBe(fixture.signatureBase64.trim());
    expect(gitee.platforms["windows-x86_64"].url).toBe(
      `https://gitee.com/owner/prts-terminal-release/releases/download/v${version}/${encodeURIComponent(path.basename(installer))}`,
    );
  });

  it("refuses to overwrite a non-empty staging directory without force", () => {
    const { root, installer, signature, notesFile } = writeReleaseFixture();
    const outDir = tempDir();
    const options = {
      root,
      installer,
      signature,
      notesFile,
      giteeRepo: "owner/prts-terminal-release",
      outDir,
      config: path.join(root, "src-tauri", "tauri.conf.json"),
    };
    prepareRelease(options);
    expect(() => prepareRelease(options)).toThrow(/目标目录已存在且非空/);
    expect(() => prepareRelease({ ...options, force: true })).not.toThrow();
  });

  it("requires at least one release repository or URL", () => {
    const { root, installer, signature } = writeReleaseFixture();
    expect(() =>
      prepareRelease({
        root,
        installer,
        signature,
        outDir: tempDir(),
        config: path.join(root, "src-tauri", "tauri.conf.json"),
      }),
    ).toThrow(/至少需要/);
  });

  it("rejects an installer name that does not contain the version", () => {
    const fixture = createMinisignFixture({ version: "9.9.9", filename: "installer.exe" });
    const root = createProject("9.9.9", fixture.tauriPubkey);
    const installer = path.join(tempDir(), "installer.exe");
    fs.writeFileSync(installer, fixture.data);
    const signature = `${installer}.sig`;
    fs.writeFileSync(signature, fixture.signatureBase64, "utf8");
    expect(() =>
      prepareRelease({
        root,
        installer,
        signature,
        giteeRepo: "owner/prts-terminal-release",
        outDir: tempDir(),
        config: path.join(root, "src-tauri", "tauri.conf.json"),
      }),
    ).toThrow(/安装包文件名不包含版本号/);
  });
});

describe("sha256File", () => {
  it("returns the uppercase SHA-256 of a file", () => {
    const dir = tempDir();
    const file = path.join(dir, "abc.txt");
    fs.writeFileSync(file, "abc");
    expect(sha256File(file)).toBe(
      "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
    );
  });
});
