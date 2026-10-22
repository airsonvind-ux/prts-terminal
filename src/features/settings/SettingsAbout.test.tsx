// @vitest-environment jsdom
// SettingsAbout: app identity + license sections.
import { describe, expect, it, beforeEach, afterEach } from "vitest";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { SettingsAbout } from "./SettingsAbout";
import type { AppMeta } from "../../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

interface LicenseSection {
  id: string;
  title: string;
  license: string;
  text: string;
}

const LICENSE_SECTIONS: LicenseSection[] = [
  {
    id: "core",
    title: "核心许可",
    license: "Apache License 2.0",
    text: "Apache License\n\nVersion 2.0, January 2004\n",
  },
  {
    id: "bridge",
    title: "Bridge 许可",
    license: "AGPL-3.0-or-later",
    text: "GNU AFFERO GENERAL PUBLIC LICENSE\n",
  },
  {
    id: "third-party",
    title: "第三方声明",
    license: "组件与随包内容",
    text: "Third-Party Notices\n",
  },
];

const META: AppMeta = {
  version: "0.1.0",
  dataDir: "C:/tmp/.prts-dev",
  defaultWorkspaceRoot: "C:/tmp/PRTS-WorkSpace",
  dbPath: "C:/tmp/.prts-dev/arknights.db",
  dbMeta: {
    dataVersion: "2026.10.18",
    generatedAt: "2026-10-22T05:11:04Z",
    sourceCommit: "bee3f83",
    schemaVersion: 1,
  },
  knowledgeDir: "C:/tmp/resources/knowledge",
  skillsDir: "C:/tmp/resources/skills",
  userGuidePath: "C:/tmp/resources/docs/guide.md",
};

function renderAbout(meta: AppMeta | null = META): void {
  act(() => {
    root.render(<SettingsAbout meta={meta} />);
  });
}

async function flush(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
  });
}

function licenseToggle(): HTMLButtonElement {
  const btn = container.querySelector(
    'button[aria-label="查看许可证"], button[aria-label="收起许可证"]'
  );
  if (!btn) throw new Error("license toggle not found");
  return btn as HTMLButtonElement;
}

function sectionToggle(title: string): HTMLButtonElement {
  const btn = container.querySelector(
    `button[aria-label="展开${title}"], button[aria-label="收起${title}"]`
  );
  if (!btn) throw new Error(`license section toggle not found: ${title}`);
  return btn as HTMLButtonElement;
}

function click(el: Element): Promise<void> {
  act(() => el.dispatchEvent(new MouseEvent("click", { bubbles: true })));
  return flush();
}

function readLicenseCalls(): number {
  return __invokeMock.mock.calls.filter((c) => c[0] === "read_license").length;
}

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  __invokeMock.mockReset();
  __invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "read_license") return Promise.resolve(LICENSE_SECTIONS);
    if (cmd === "read_data_notice") {
      return Promise.resolve("数据与内容声明\n\n非官方数据工件。");
    }
    return Promise.resolve(undefined);
  });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("SettingsAbout · app identity", () => {
  it("shows icon, name and version", async () => {
    renderAbout();
    await flush();

    const icon = container.querySelector("img");
    expect(icon).not.toBeNull();
    expect(icon?.getAttribute("src")).toMatch(/^data:image\/svg\+xml/);
    expect(container.textContent).toContain("PRTS 终端助手");
    expect(container.textContent).toContain("版本 0.1.0");
  });
});

describe("SettingsAbout · data", () => {
  it("shows the data date and loads the notice once", async () => {
    renderAbout();
    await flush();

    expect(container.textContent).toContain("数据更新至");
    expect(container.textContent).toContain("2026.10.18");
    expect(container.querySelector("pre")).toBeNull();

    const toggle = container.querySelector(
      'button[aria-label="查看数据与内容声明"]'
    ) as HTMLButtonElement;
    await click(toggle);
    expect(container.textContent).toContain("非官方数据工件");
    expect(
      __invokeMock.mock.calls.filter((c) => c[0] === "read_data_notice")
    ).toHaveLength(1);

    await click(toggle);
    expect(container.querySelector("pre")).toBeNull();
    await click(toggle);
    expect(
      __invokeMock.mock.calls.filter((c) => c[0] === "read_data_notice")
    ).toHaveLength(1);
  });
});

describe("SettingsAbout · license sections", () => {
  it("is collapsed by default and does not request the license", async () => {
    renderAbout();
    await flush();

    expect(readLicenseCalls()).toBe(0);
    expect(container.querySelector("pre")).toBeNull();
    expect(licenseToggle().getAttribute("aria-expanded")).toBe("false");
  });

  it("loads once, then expands each document independently", async () => {
    renderAbout();
    await flush();

    await click(licenseToggle());
    expect(readLicenseCalls()).toBe(1);
    expect(container.textContent).toContain("核心许可");
    expect(container.textContent).toContain("Bridge 许可");
    expect(container.textContent).toContain("第三方声明");
    expect(container.querySelector("pre")).toBeNull();

    await click(sectionToggle("核心许可"));
    expect(container.querySelector("pre")?.textContent).toContain(
      "Apache License"
    );
    expect(sectionToggle("核心许可").getAttribute("aria-expanded")).toBe(
      "true"
    );

    await click(sectionToggle("核心许可"));
    expect(container.querySelector("pre")).toBeNull();

    // Collapse and reopen the outer block: cached sections, no second request.
    await click(licenseToggle());
    expect(container.querySelector("pre")).toBeNull();
    await click(licenseToggle());
    expect(readLicenseCalls()).toBe(1);
    expect(sectionToggle("Bridge 许可")).toBeTruthy();
  });

  it("shows a read error in place without a pre block", async () => {
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "read_license") {
        return Promise.reject("读取许可证文件失败（C:/tmp/LICENSE）：拒绝访问");
      }
      return Promise.resolve(undefined);
    });
    renderAbout();
    await flush();

    await click(licenseToggle());
    expect(container.textContent).toContain("读取许可证文件失败");
    expect(container.querySelector("pre")).toBeNull();
  });
});
