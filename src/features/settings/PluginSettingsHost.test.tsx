// @vitest-environment jsdom
// 扩展页（列表 / 详情 / 安装）的交互契约（UI 规格草案 2 §六）。
//
// 覆盖：
//   - 行点击进详情，Esc 回列表（且 preventDefault，面板不被一起关掉）
//   - 点开关只切开关，不进详情；对话处理中「关闭」方向置灰
//   - 内置插件没有卸载按钮；外部插件卸载要两步确认
//   - 能力超过两条折叠成「能力（N）」
//   - 停用/异常时工具数显示「—」，异常行副行显示原因

import { describe, expect, it, beforeEach, afterEach, vi } from "vitest";
import { act } from "react";
import { createRoot } from "react-dom/client";
import type { PluginList, PluginPreview } from "../../types";
import { PluginSettingsHost } from "./PluginSettingsHost";
import type { MaaSettingsProps } from "./MaaSettings";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

const MAA: MaaSettingsProps["maa"] = {
  server: "official",
  auto_detect: true,
  screenshot_extras: true,
  touch_extras: false,
  emulator: "",
  emulator_path: "",
  adb_path: "",
  address: "",
};

const MAA_PROPS: MaaSettingsProps = {
  maa: MAA,
  busy: false,
  testing: false,
  onTest: async () => null,
  onPatch: () => {},
  onChooseEmulatorDir: () => {},
  onChooseAdbFile: () => {},
};

/** 详情页里会挂上 MAA 设置页，它一进来就问 maa_status；给个空台账形状即可。 */
const MAA_STATUS = {
  maa_installed: true,
  bridge_available: true,
  connected: false,
  running: false,
  stale: false,
  last_error: null,
  emulator: null,
  address: null,
  extras: false,
  screencap: null,
  emulators: [],
  resolved: [],
};

function listFixture(): PluginList {
  return {
    plugins: [
      {
        id: "maa",
        name: "MAA 自动化",
        version: "0.1.1",
        kind: "builtin",
        state: "loaded",
        reason: null,
        toolCount: 1,
        hasSettingsPage: true,
        capabilities: ["maa.bridge"],
      },
      {
        id: "demo",
        name: "演示插件",
        version: "0.3.0",
        kind: "process",
        state: "disabled",
        reason: null,
        toolCount: null,
        hasSettingsPage: false,
        capabilities: ["fs.plugin", "query.call", "maa.bridge"],
      },
    ],
    unregistered: [],
  };
}

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

async function renderHost(turnBusy = false) {
  await act(async () => {
    root.render(
      <PluginSettingsHost maaProps={MAA_PROPS} turnBusy={turnBusy} />,
    );
  });
  // list_plugins 是挂载后的异步调用，等它落地
  await act(async () => {
    await Promise.resolve();
  });
}

function rows(): HTMLElement[] {
  return Array.from(
    container.querySelectorAll<HTMLElement>('div[role="button"]'),
  );
}

function rowNamed(title: string): HTMLElement {
  const row = rows().find((el) => el.textContent?.includes(title));
  if (!row) throw new Error(`找不到行：${title}`);
  return row;
}

function buttonNamed(text: string): HTMLButtonElement | undefined {
  return Array.from(container.querySelectorAll("button")).find((el) =>
    el.textContent?.includes(text),
  );
}

function title(): string {
  return container.querySelector("header span")?.textContent ?? "";
}

function toolRowValue(): string {
  const terms = Array.from(container.querySelectorAll("dt"));
  const tool = terms.find((el) => el.textContent === "工具");
  return tool?.nextElementSibling?.textContent ?? "";
}

beforeEach(() => {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);

  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "list_plugins") return listFixture();
    if (cmd === "maa_status") return MAA_STATUS;
    if (cmd === "set_plugin_enabled") return listFixture();
    if (cmd === "uninstall_plugin") return listFixture();
    throw new Error(`没有这个命令：${cmd}`);
  });
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("PluginSettingsHost（扩展页）", () => {
  it("行点击进详情，Esc 回列表且阻止面板关闭", async () => {
    await renderHost();
    expect(title()).toBe("扩展");

    await act(async () => rowNamed("MAA 自动化").click());
    expect(title()).toBe("MAA 自动化");

    const host = container.firstElementChild as HTMLElement;
    let allowed = true;
    await act(async () => {
      const event = new KeyboardEvent("keydown", {
        key: "Escape",
        bubbles: true,
        cancelable: true,
      });
      allowed = host.dispatchEvent(event);
    });

    // dispatchEvent 返回 false = preventDefault 已调用（OverlayContainer 靠它判断）
    expect(allowed).toBe(false);
    expect(title()).toBe("扩展");
  });

  it("点开关只切开关，不进入详情", async () => {
    await renderHost();
    const checkbox = rowNamed("MAA 自动化").querySelector(
      'input[type="checkbox"]',
    ) as HTMLInputElement;

    await act(async () => checkbox.click());

    expect(title()).toBe("扩展");
    expect(invokeMock).toHaveBeenCalledWith("set_plugin_enabled", {
      id: "maa",
      enabled: false,
    });
  });

  it("对话处理中「关闭」方向被闸住，打开方向照常", async () => {
    await renderHost(true);

    const loaded = rowNamed("MAA 自动化").querySelector(
      'input[type="checkbox"]',
    ) as HTMLInputElement;
    expect(loaded.disabled).toBe(true);

    const disabled = rowNamed("演示插件").querySelector(
      'input[type="checkbox"]',
    ) as HTMLInputElement;
    expect(disabled.disabled).toBe(false);
  });

  it("内置插件没有卸载；外部插件卸载要两步确认", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "list_plugins") return listFixture();
      if (cmd === "maa_status") return MAA_STATUS;
      if (cmd === "uninstall_plugin") return listFixture();
      throw new Error(`没有这个命令：${cmd}`);
    });
    await renderHost();

    await act(async () => rowNamed("MAA 自动化").click());
    expect(buttonNamed("卸载")).toBeUndefined();

    await act(async () => buttonNamed("返回")?.click());
    expect(title()).toBe("扩展");

    await act(async () => rowNamed("演示插件").click());
    await act(async () => buttonNamed("卸载")?.click());

    expect(buttonNamed("确认卸载")).toBeDefined();
    await act(async () => buttonNamed("确认卸载")?.click());

    expect(invokeMock).toHaveBeenCalledWith("uninstall_plugin", {
      id: "demo",
      deleteData: false,
    });
  });

  it("能力超过两条折叠成「能力（N）」", async () => {
    await renderHost();
    await act(async () => rowNamed("演示插件").click());

    expect(container.textContent).toContain("读写自己的数据目录");
    expect(container.textContent).toContain("查询方舟数据（只读）");
    expect(container.textContent).not.toContain("启动 / 关闭 MAA 桥进程");
    expect(buttonNamed("能力（3）")).toBeDefined();

    await act(async () => buttonNamed("能力（3）")?.click());
    expect(container.textContent).toContain("启动 / 关闭 MAA 桥进程");
  });

  it("停用插件的工具数显示破折号，异常行的副行是原因", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd !== "list_plugins") return listFixture();
      return {
        plugins: [
          {
            ...listFixture().plugins[1],
            state: "failed",
            reason: "这个文件夹里没有 plugin.json",
            capabilities: [],
          },
        ],
        unregistered: [],
      } satisfies PluginList;
    });

    await renderHost();
    expect(container.textContent).toContain("这个文件夹里没有 plugin.json");
    expect(rows()).toHaveLength(1);

    await act(async () => rows()[0]!.click());
    expect(toolRowValue()).toBe("—");
  });

  it("安装预览确认后回到列表", async () => {
    const preview: PluginPreview = {
      token: "tok-1",
      name: "演示插件",
      id: "demo",
      version: "0.3.0",
      author: null,
      kind: "process",
      capabilities: ["fs.plugin"],
      fileCount: 3,
      totalBytes: 2048,
    };
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "list_plugins") return listFixture();
      if (cmd === "preview_plugin_install") return preview;
      if (cmd === "confirm_plugin_install") return listFixture();
      throw new Error(`没有这个命令：${cmd}`);
    });

    await renderHost();
    await act(async () => buttonNamed("＋")?.click());
    expect(title()).toBe("安装插件");

    // 选择文件夹 → 后端返回预览
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "pick_plugin_folder") return "C:/tmp/demo";
      if (cmd === "preview_plugin_install") return preview;
      if (cmd === "confirm_plugin_install") return listFixture();
      if (cmd === "list_plugins") return listFixture();
      throw new Error(`没有这个命令：${cmd}`);
    });
    await act(async () => buttonNamed("选择文件夹")?.click());
    await act(async () => {
      await Promise.resolve();
    });

    expect(container.textContent).toContain("装到 plugins/demo/");

    await act(async () => buttonNamed("安装")?.click());
    await act(async () => {
      await Promise.resolve();
    });

    expect(invokeMock).toHaveBeenCalledWith("confirm_plugin_install", {
      token: "tok-1",
    });
    expect(title()).toBe("扩展");
  });
});
