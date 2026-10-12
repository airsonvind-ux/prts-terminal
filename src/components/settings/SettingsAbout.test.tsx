// @vitest-environment jsdom
// SettingsAbout · 关于页（使用说明入口 + 许可证折叠块）
//
// 覆盖：
//   - 默认折叠：不请求 read_license
//   - 展开：请求一次并显示全文；再折叠再展开命中缓存，不重复请求
//   - 读取失败：就地显示错误，不弹窗

import { describe, expect, it, beforeEach, afterEach } from "vitest";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { SettingsAbout } from "./SettingsAbout";
import type { AppMeta } from "../../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

const LICENSE_PLACEHOLDER = "Apache License\n\nVersion 2.0, January 2004\n";

const META: AppMeta = {
  version: "0.1.0",
  dataDir: "C:/tmp/.prts-dev",
  defaultWorkspaceRoot: "C:/tmp/PRTS-WorkSpace",
  dbPath: "C:/tmp/.prts-dev/arknights.db",
  knowledgeDir: "C:/tmp/resources/knowledge",
  skillsDir: "C:/tmp/resources/skills",
  userGuidePath: "C:/tmp/resources/docs/使用说明.md",
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
  const btn = Array.from(container.querySelectorAll("button")).find((b) =>
    b.textContent?.includes("许可证")
  );
  if (!btn) throw new Error("未找到许可证折叠行");
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
    if (cmd === "read_license") return Promise.resolve(LICENSE_PLACEHOLDER);
    return Promise.resolve(undefined);
  });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("SettingsAbout · 许可证块", () => {
  it("默认折叠：不请求 read_license，页面上没有全文", async () => {
    renderAbout();
    await flush();
    expect(readLicenseCalls()).toBe(0);
    expect(container.querySelector("pre")).toBeNull();
    expect(licenseToggle().getAttribute("aria-expanded")).toBe("false");
  });

  it("展开：请求一次并显示全文；再折叠再展开命中缓存，不重复请求", async () => {
    renderAbout();
    await flush();

    await click(licenseToggle());
    expect(readLicenseCalls()).toBe(1);
    expect(container.querySelector("pre")?.textContent).toContain(
      "Apache License"
    );
    expect(licenseToggle().getAttribute("aria-expanded")).toBe("true");

    // 折叠再展开：文本已缓存，不应再次请求
    await click(licenseToggle());
    expect(container.querySelector("pre")).toBeNull();
    await click(licenseToggle());
    expect(readLicenseCalls()).toBe(1);
    expect(container.querySelector("pre")?.textContent).toContain(
      "Apache License"
    );
  });

  it("读取失败：就地显示错误，不弹窗", async () => {
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
