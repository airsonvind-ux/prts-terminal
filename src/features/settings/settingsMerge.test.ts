// settingsMerge · 本地乐观合并与后端 per-field 语义的一致性
//
// 这里存在的意义：每新增一个设置字段，后端 patch、TS 类型、本地合并三处都要改。
// 漏掉合并那一处，表现为「状态行显示新值、输入框还是旧值」——后端已保存，本地态
// 没跟上。所以用一条覆盖 MAA 全字段的用例把这类漏项钉住。

import { describe, expect, it } from "vitest";
import { mergeSettings } from "./settingsMerge";
import type { Settings } from "../../types";

function base(): Settings {
  return {
    llm: { active_provider: "p", providers: {}, thinking_level: null },
    exec: { mode: "auto" },
    maa: {
      server: "official",
      auto_detect: true,
      screenshot_extras: true,
      touch_extras: false,
      emulator: "",
      maa_path: "",
      emulator_path: "",
      adb_path: "",
      address: "",
    },
    workspace: { workspace_root: "" },
    window: { always_on_top: true },
  } as unknown as Settings;
}

describe("mergeSettings · MAA 段", () => {
  it("每个字段都能被 patch 覆盖（含 maa_path）", () => {
    const next = mergeSettings(base(), {
      maa: {
        server: "bilibili",
        auto_detect: false,
        screenshot_extras: false,
        touch_extras: true,
        emulator: "MuMu 12",
        maa_path: "D:\\MAA",
        emulator_path: "D:\\mumu",
        adb_path: "D:\\mumu\\adb.exe",
        address: "127.0.0.1:16384",
      },
    });
    expect(next.maa).toEqual({
      server: "bilibili",
      auto_detect: false,
      screenshot_extras: false,
      touch_extras: true,
      emulator: "MuMu 12",
      maa_path: "D:\\MAA",
      emulator_path: "D:\\mumu",
      adb_path: "D:\\mumu\\adb.exe",
      address: "127.0.0.1:16384",
    });
  });

  it("缺席字段不覆盖（maa_path 不被顺手清空）", () => {
    const cur = base();
    cur.maa.maa_path = "F:\\MAA";
    const next = mergeSettings(cur, { maa: { adb_path: "D:\\adb.exe" } });
    expect(next.maa.maa_path).toBe("F:\\MAA");
    expect(next.maa.adb_path).toBe("D:\\adb.exe");
  });

  it("空串是有效值（= 回到自动检测）", () => {
    const cur = base();
    cur.maa.maa_path = "F:\\MAA";
    const next = mergeSettings(cur, { maa: { maa_path: "" } });
    expect(next.maa.maa_path).toBe("");
  });
});
