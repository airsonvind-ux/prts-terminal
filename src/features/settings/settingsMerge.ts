// 设置 patch → 新 settings 的纯合并。
//
// 与后端 set_settings 的 per-field 语义对齐：缺席字段不覆盖；llm 段 = v2
// 分区：active_provider / providers.{家} 深层合并（新家懒建；家内字段缺席不
// 覆盖、空串清除、api_key 掩码占位不覆盖、明文写入在本地归一为掩码）
// / thinking_level 全局（空串清除）。

import type { ApiType, Settings, SettingsPatch } from "../../types";

/// api_key 掩码占位（与后端 MASKED_SECRET 一致；merge 时掩码不覆盖现值）
export const MASKED_SECRET = "********";

export function mergeSettings(cur: Settings, patch: SettingsPatch): Settings {
  const next: Settings = {
    ...cur,
    workspace: { ...cur.workspace },
    window: { ...cur.window },
  };
  if (patch.llm) {
    const p = patch.llm;
    const llm: Settings["llm"] = {
      ...cur.llm,
      providers: { ...cur.llm.providers },
    };
    if (p.active_provider !== undefined) llm.active_provider = p.active_provider;
    if (p.providers) {
      for (const [name, pp] of Object.entries(p.providers)) {
        const base = cur.llm.providers[name];
        // base 优先；无 base 用默认（显式展开避免 TS2783）
        const entry: Settings["llm"]["providers"][string] = {
          ...{
            api_type: "openai-completions" as ApiType,
            model: "",
            api_key: "",
            base_url: null,
            headers: null,
            max_tokens: 4096,
          },
          ...base,
        };
        if (pp.api_type !== undefined) entry.api_type = pp.api_type;
        if (pp.model !== undefined) entry.model = pp.model;
        // 掩码占位不覆盖；明文 / 空串都归一成「后端会回给我们的形态」：
        // 非空明文在本地只留掩码。原因有二——① 明文不常驻面板态（诚实镜像
        // get_settings 的掩码下发）；② 「已配 key」这一事实由
        // api_key === MASKED_SECRET 表达，复制按钮可用性靠它（曾因本地持明文而
        // 把按钮禁用，2026-10-11 实测 bug）。
        if (pp.api_key !== undefined && pp.api_key !== MASKED_SECRET) {
          entry.api_key = pp.api_key === "" ? "" : MASKED_SECRET;
        }
        if (pp.base_url !== undefined) {
          entry.base_url = pp.base_url === "" ? null : pp.base_url;
        }
        if (pp.headers !== undefined) {
          entry.headers = pp.headers === "" ? null : pp.headers;
        }
        if (pp.max_tokens !== undefined) entry.max_tokens = pp.max_tokens;
        llm.providers[name] = entry;
      }
    }
    if (p.thinking_level !== undefined) {
      llm.thinking_level = p.thinking_level === "" ? null : p.thinking_level;
    }
    next.llm = llm;
  }
  if (patch.exec && patch.exec.mode !== undefined) {
    next.exec = { mode: patch.exec.mode };
  }
  if (patch.workspace && patch.workspace.workspace_root !== undefined) {
    next.workspace.workspace_root = patch.workspace.workspace_root;
  }
  if (patch.window && patch.window.always_on_top !== undefined) {
    next.window.always_on_top = patch.window.always_on_top;
  }
  if (patch.maa) {
    const p = patch.maa;
    const maa: Settings["maa"] = { ...cur.maa };
    if (p.server !== undefined) maa.server = p.server;
    if (p.auto_detect !== undefined) maa.auto_detect = p.auto_detect;
    if (p.screenshot_extras !== undefined) maa.screenshot_extras = p.screenshot_extras;
    if (p.touch_extras !== undefined) maa.touch_extras = p.touch_extras;
    if (p.emulator !== undefined) maa.emulator = p.emulator;
    if (p.emulator_path !== undefined) maa.emulator_path = p.emulator_path;
    if (p.adb_path !== undefined) maa.adb_path = p.adb_path;
    if (p.address !== undefined) maa.address = p.address;
    next.maa = maa;
  }
  return next;
}
