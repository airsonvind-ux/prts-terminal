import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import {
  collectReleaseAssets,
  githubReleaseCommand,
  publishRelease,
} from "../publish-release.mjs";

const tempDirs = [];

function tempDir() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "prts-publish-"));
  tempDirs.push(dir);
  return dir;
}

function createStaging(version = "0.1.0") {
  const root = tempDir();
  const name = `PRTS.Terminal_${version}_x64-setup.exe`;
  fs.writeFileSync(path.join(root, name), "installer", "utf8");
  fs.writeFileSync(path.join(root, `${name}.sig`), "signature", "utf8");
  fs.writeFileSync(path.join(root, "SHA256SUMS.txt"), "ABC *installer\n", "utf8");
  fs.writeFileSync(path.join(root, "release-notes.md"), "notes\n", "utf8");
  for (const provider of ["gitee", "github"]) {
    fs.mkdirSync(path.join(root, provider), { recursive: true });
    fs.writeFileSync(
      path.join(root, provider, "latest.json"),
      JSON.stringify({ version, platforms: { "windows-x86_64": {} } }),
      "utf8",
    );
  }
  return { root, name };
}

afterEach(() => {
  for (const dir of tempDirs.splice(0)) {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

describe("collectReleaseAssets", () => {
  it("collects installer, signature, checksums and manifests", () => {
    const { root, name } = createStaging();
    const assets = collectReleaseAssets(root);
    expect(path.basename(assets.installer)).toBe(name);
    expect(path.basename(assets.signature)).toBe(`${name}.sig`);
    expect(assets.manifests.github).toContain("github");
    expect(assets.manifests.gitee).toContain("gitee");
  });

  it("rejects a staging directory without exactly one installer", () => {
    const root = tempDir();
    expect(() => collectReleaseAssets(root)).toThrow(/只有一个 \.exe/);
  });
});

describe("githubReleaseCommand", () => {
  it("includes assets and notes", () => {
    const result = githubReleaseCommand({
      tag: "v0.1.0",
      repo: "owner/repo",
      assets: ["installer.exe", "installer.exe.sig"],
      notes: "release-notes.md",
    });
    expect(result.command).toBe("gh");
    expect(result.args).toEqual([
      "release",
      "create",
      "v0.1.0",
      "--repo",
      "owner/repo",
      "--title",
      "v0.1.0",
      "--notes-file",
      "release-notes.md",
      "installer.exe",
      "installer.exe.sig",
    ]);
  });
});

describe("publishRelease dry-run", () => {
  it("does not call gh or Gitee when dry-run is enabled", async () => {
    const { root } = createStaging();
    const result = await publishRelease({
      stagingDir: root,
      version: "0.1.0",
      githubRepo: "owner/github-release",
      giteeRepo: "owner/gitee-release",
      dryRun: true,
    });
    expect(result.tag).toBe("v0.1.0");
    expect(result.results).toHaveLength(2);
    expect(result.results.every((item) => item.dryRun)).toBe(true);
  });
});
