// @vitest-environment jsdom
// SettingsExtension · 扩展页（MAA 接入）
// 覆盖：三节齐备、状态行口径、自动检测开/关的置灰与解锁、运行中锁定、
//       无增强通道的退化、变更上抛。

import { describe, expect, it, afterEach, vi } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import type { MaaSettings, MaaStatus } from "../../types";
import { SettingsExtension } from "./SettingsExtension";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let currentStatus: MaaStatus;

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn((cmd: string) =>
    cmd === "maa_status" || cmd === "maa_test_connection"
      ? Promise.resolve(currentStatus)
      : Promise.resolve(null)
  ),
}));

const MAA: MaaSettings = {
  server: "official",
  auto_detect: true,
  screenshot_extras: true,
  touch_extras: false,
  emulator: "",
  emulator_path: "",
  adb_path: "",
  address: "",
};

function status(over: Partial<MaaStatus> = {}): MaaStatus {
  return {
    maa_installed: true,
    bridge_available: true,
    connected: true,
    running: false,
    stale: false,
    last_error: null,
    emulator: "MuMu 12",
    address: "127.0.0.1:16384",
    extras: true,
    screencap: "MumuExtras",
    emulators: ["MuMu 12", "雷电 LDPlayer"],
    resolved: [
      {
        emulator: "MuMu 12",
        config: "MuMuEmulator12",
        adb_path: "D:\\mumu\\shell\\adb.exe",
        address: "127.0.0.1:16384",
        extras: true,
        root: "D:\\mumu",
      },
    ],
    ...over,
  };
}

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
});

async function render(
  maa: MaaSettings = MAA,
  busy = false,
  onPatch: (patch: unknown, label?: string) => void = () => {},
  onChooseEmulatorDir: () => void = () => {},
  onChooseAdbFile: () => void = () => {},
  onTest: () => Promise<MaaStatus | null> = async () => currentStatus,
  testing = false
): Promise<void> {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  await act(async () => {
    root.render(
      React.createElement(SettingsExtension, {
        maa,
        busy,
        testing,
        onTest,
        onPatch,
        onChooseEmulatorDir,
        onChooseAdbFile,
      })
    );
  });
}

const disabledControls = () =>
  Array.from(
    container!.querySelectorAll<HTMLElement>(
      "input:disabled, select:disabled, button:disabled"
    )
  );

describe("SettingsExtension（扩展页）", () => {
  it("三节齐备：账号 / 连接 / 增强，且不泄露技术细节", async () => {
    currentStatus = status();
    await render();
    const text = container!.textContent ?? "";
    expect(text).toContain("账号");
    expect(text).toContain("连接");
    expect(text).toContain("增强");
    expect(text).toContain("官服");
    expect(text).toContain("B 服");
    expect(text).toContain("自动检测");
    expect(text).toContain("安装路径");
    expect(text).toContain("测试连接");
    // 不泄露技术细节
    expect(text).not.toContain("截图增强也依赖它");
    expect(text).not.toContain("PRTS 不会自动识别");
  });

  it("测试进行中：状态行让位给「正在连接…」，且整页锁住（不能中途改设置）", async () => {
    currentStatus = status();
    await render(MAA, false, () => {}, () => {}, () => {}, undefined, true);
    const text = container!.textContent ?? "";
    expect(text).toContain("正在连接…");
    expect(text).toContain("连接中…");
    expect(text).not.toContain("已连接：MuMu 12");
    // 测试期间改设置会拆桥、还会卡在管理器锁上——整页必须只读
    expect(disabledControls().length).toBeGreaterThanOrEqual(4);
  });

  it("已连接时状态行是这页的关注中心：连的是谁 + 增强到底有没有生效", async () => {
    currentStatus = status();
    await render();
    expect(container!.textContent).toContain(
      "已连接：MuMu 12 · 截图增强已生效（MumuExtras）"
    );
    // 地址是技术细节，不进状态行
    expect(container!.textContent).not.toContain("已连接：MuMu 12 · 127.0.0.1:16384");
  });

  it("增强开着但实际降级 → 如实说走的是普通通道", async () => {
    currentStatus = status({ screencap: "RawByNc" });
    await render();
    expect(container!.textContent).toContain(
      "截图增强已启用，当前走普通通道（RawByNc）"
    );
  });

  it("自动检测开着：连接节四项置灰；关掉后原地解锁（一行都不增减）", async () => {
    currentStatus = status();
    await render();
    // 自动检测开：模拟器下拉 + 安装路径 + 连接地址 + ADB 路径 都不可改
    expect(disabledControls().length).toBeGreaterThanOrEqual(4);
    expect(container!.textContent).toContain("自动识别；关掉后可自己指定");
    expect(container!.textContent).toContain("当前：MuMu 12");

    act(() => root.unmount());
    container.remove();
    await render({ ...MAA, auto_detect: false });
    // 自动检测关：没有一个控件是禁用的（解锁是原地发生，不靠增减行）
    expect(disabledControls().length).toBe(0);
  });

  it("未连接时把原因摆出来（不吞错）", async () => {
    currentStatus = status({
      connected: false,
      emulator: null,
      address: null,
      extras: false,
      last_error: "没有运行中的模拟器（已装：MuMu 12）。请先启动模拟器。",
    });
    await render();
    expect(container!.textContent).toContain("没有运行中的模拟器");
  });

  it("设置改过 → 明说「设置已更改，下次连接生效」", async () => {
    currentStatus = status({
      connected: false,
      emulator: null,
      address: null,
      extras: false,
      stale: true,
    });
    await render();
    expect(container!.textContent).toContain("设置已更改，下次连接生效");
  });

  it("任务运行中：整页锁定 + 出示警示", async () => {
    currentStatus = status({ running: true });
    await render();
    expect(container!.textContent).toContain("MAA 正在运行，暂不可改");
    // 两个服务器单选 + 自动检测 + 模拟器下拉 + 安装路径 + 地址 + ADB + 两个开关
    expect(disabledControls().length).toBeGreaterThanOrEqual(4);
  });

  it("对话框处理中也锁（busy）", async () => {
    currentStatus = status();
    await render(MAA, true);
    expect(disabledControls().length).toBeGreaterThanOrEqual(4);
  });

  it("没有增强通道的模拟器：本节只留一句说明", async () => {
    currentStatus = status({
      resolved: [
        {
          emulator: "BlueStacks",
          config: "BlueStacks",
          adb_path: "D:\\bs\\HD-Adb.exe",
          address: "127.0.0.1:5555",
          extras: false,
          root: "D:\\bs",
        },
      ],
    });
    await render();
    const text = container!.textContent ?? "";
    expect(text).toContain("这台模拟器不支持增强");
    expect(text).not.toContain("触控增强");
  });

  it("点服务器 / 改开关 → 上抛 patch（受控不回显）", async () => {
    currentStatus = status();
    const onPatch = vi.fn();
    await render(MAA, false, onPatch);

    const radios = Array.from(
      container!.querySelectorAll<HTMLInputElement>(
        'input[type="radio"][name="maaServer"]'
      )
    );
    act(() => {
      radios[1].dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(onPatch).toHaveBeenCalledWith({ server: "bilibili" }, "服务器");
    expect(radios[0].checked, "受控：外部没更新就还是官服").toBe(true);

    const checks = Array.from(
      container!.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')
    );
    act(() => {
      checks[0].dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(onPatch).toHaveBeenCalledWith({ auto_detect: false }, "自动检测");
  });
});
