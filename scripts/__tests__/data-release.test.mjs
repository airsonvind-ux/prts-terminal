import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import {
  prepareDataRelease,
  validateDataArtifact,
  validateDataVersion,
} from "../prepare-data-release.mjs";
import { buildDataReleaseAssetUrl, fetchDataRelease } from "../fetch-data-release.mjs";
import {
  collectDataAssets,
  dataReleaseCommand,
  publishDataRelease,
} from "../publish-data-release.mjs";

const tempDirs = [];

function tempDir() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "prts-data-release-"));
  tempDirs.push(dir);
  return dir;
}

function sha256(data) {
  return crypto.createHash("sha256").update(data).digest("hex").toUpperCase();
}

function createDataFixture(version = "2026.10.25") {
  const root = tempDir();
  const db = Buffer.from("sqlite fixture\n");
  const dbPath = path.join(root, "arknights.db");
  fs.writeFileSync(dbPath, db);
  const manifestPath = path.join(root, "DATA_MANIFEST.json");
  const manifest = {
    appVersion: "0.1.0",
    dataVersion: version,
    generatedAt: "2026-10-25T00:00:00Z",
    sourceCommit: "fe7d8d0",
    schemaVersion: 1,
    database: {
      file: "arknights.db",
      bytes: db.length,
      sha256: sha256(db),
    },
    dataContract: {
      status: "passed",
      passed: 1326,
      ignored: 39,
    },
  };
  fs.writeFileSync(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, "utf8");
  return { root, db, dbPath, manifestPath, manifest };
}

afterEach(() => {
  for (const dir of tempDirs.splice(0)) {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

describe("data version", () => {
  it("accepts date versions and rejects malformed values", () => {
    expect(validateDataVersion("2026.10.25")).toBe("2026.10.25");
    expect(validateDataVersion("v2026.10.25.2")).toBe("2026.10.25.2");
    expect(() => validateDataVersion("2026-10-25")).toThrow(/格式无效/);
  });
});

describe("data manifest validation", () => {
  it("accepts a matching manifest and database", () => {
    const fixture = createDataFixture();
    const result = validateDataArtifact({
      manifest: fixture.manifest,
      databasePath: fixture.dbPath,
      expectedVersion: "2026.10.25",
    });
    expect(result.dataVersion).toBe("2026.10.25");
    expect(result.sha256).toBe(fixture.manifest.database.sha256);
    expect(result.contract).toEqual({ passed: 1326, ignored: 39 });
  });

  it("rejects a database hash mismatch and a non-passing contract", () => {
    const fixture = createDataFixture();
    expect(() =>
      validateDataArtifact({
        manifest: {
          ...fixture.manifest,
          database: { ...fixture.manifest.database, sha256: "0".repeat(64) },
        },
        databasePath: fixture.dbPath,
      }),
    ).toThrow(/SHA-256/);

    expect(() =>
      validateDataArtifact({
        manifest: {
          ...fixture.manifest,
          dataContract: { status: "pending", passed: 0, ignored: 0 },
        },
        databasePath: fixture.dbPath,
      }),
    ).toThrow(/数据契约未通过/);
  });
});

describe("prepareDataRelease", () => {
  it("stages database, manifest and checksums", () => {
    const fixture = createDataFixture();
    const outDir = tempDir();
    const notes = path.join(fixture.root, "release-notes.md");
    fs.writeFileSync(notes, "data release notes\n", "utf8");

    const result = prepareDataRelease({
      databasePath: fixture.dbPath,
      manifestPath: fixture.manifestPath,
      dataVersion: "2026.10.25",
      outDir,
      notesFile: notes,
    });

    expect(result.dataVersion).toBe("2026.10.25");
    expect(fs.existsSync(path.join(outDir, "arknights.db"))).toBe(true);
    expect(fs.existsSync(path.join(outDir, "DATA_MANIFEST.json"))).toBe(true);
    expect(fs.readFileSync(result.checksums, "utf8")).toBe(
      `${fixture.manifest.database.sha256} *arknights.db\n${result.manifestSha256} *DATA_MANIFEST.json\n`,
    );
    expect(fs.readFileSync(path.join(outDir, "release-notes.md"), "utf8")).toBe(
      "data release notes\n",
    );
  });

  it("refuses a non-empty output directory without force", () => {
    const fixture = createDataFixture();
    const outDir = tempDir();
    fs.writeFileSync(path.join(outDir, "old.txt"), "old", "utf8");
    expect(() =>
      prepareDataRelease({
        databasePath: fixture.dbPath,
        manifestPath: fixture.manifestPath,
        outDir,
      }),
    ).toThrow(/已存在且非空/);
  });
});

describe("publishDataRelease", () => {
  it("collects the required data assets and builds a gh release command", () => {
    const fixture = createDataFixture();
    const dir = tempDir();
    for (const name of ["arknights.db", "DATA_MANIFEST.json", "SHA256SUMS.txt"]) {
      fs.copyFileSync(path.join(name === "arknights.db" ? fixture.dbPath : fixture.manifestPath), path.join(dir, name));
    }
    const assets = collectDataAssets(dir);
    expect(assets.map((file) => path.basename(file))).toEqual([
      "arknights.db",
      "DATA_MANIFEST.json",
      "SHA256SUMS.txt",
    ]);
    const command = dataReleaseCommand({
      repo: "owner/prts-terminal-release",
      dataVersion: "2026.10.25",
      assets,
    });
    expect(command.tag).toBe("data-2026.10.25");
    expect(command.args).toContain("data-2026.10.25");
  });

  it("does not call gh in dry-run", () => {
    const fixture = createDataFixture();
    const dir = tempDir();
    fs.copyFileSync(fixture.dbPath, path.join(dir, "arknights.db"));
    fs.copyFileSync(fixture.manifestPath, path.join(dir, "DATA_MANIFEST.json"));
    fs.writeFileSync(path.join(dir, "SHA256SUMS.txt"), "sum\n", "utf8");
    const result = publishDataRelease({
      repo: "owner/prts-terminal-release",
      dataVersion: "2026.10.25",
      dir,
      dryRun: true,
    });
    expect(result.dryRun).toBe(true);
    expect(result.tag).toBe("data-2026.10.25");
  });
});

describe("fetchDataRelease", () => {
  it("builds the versioned GitHub asset URL", () => {
    expect(
      buildDataReleaseAssetUrl({
        repo: "owner/prts-terminal-release",
        dataVersion: "2026.10.25",
        asset: "arknights.db",
      }),
    ).toBe(
      "https://github.com/owner/prts-terminal-release/releases/download/data-2026.10.25/arknights.db",
    );
  });

  it("returns a plan without network when dry-run is set", async () => {
    const result = await fetchDataRelease({
      repo: "owner/prts-terminal-release",
      dataVersion: "2026.10.25",
      outDir: tempDir(),
      dryRun: true,
    });
    expect(result.dryRun).toBe(true);
    expect(result.databaseUrl).toContain("/data-2026.10.25/arknights.db");
    expect(result.manifestUrl).toContain("/data-2026.10.25/DATA_MANIFEST.json");
  });

  it("rejects an invalid repository before any network call", async () => {
    await expect(
      fetchDataRelease({
        repo: "not-a-repo",
        dataVersion: "2026.10.25",
        outDir: tempDir(),
        dryRun: true,
      }),
    ).rejects.toThrow(/owner\/repo/);
  });
});
