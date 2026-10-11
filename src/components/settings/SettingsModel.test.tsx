// @vitest-environment jsdom
// SettingsModel · 模型页（mask 契约 + 思考强度 + 失焦 patch）
//
// 覆盖：
//   - API Key mask：掩码常态显示；编辑失焦提交；未编辑失焦不上送（不误清）
//   - 👁 瞬态查看：reveal_api_key → 明文 readOnly；复原不提交
//   - 供应商/API 类型切换 → 即时 patch（llm 段）
//   - 思考强度：声明集（deepseek v4 [off,low,high,max] 默认 high 高亮）/
//     推断集（无声明 [off,medium,high] 默认 medium）/ 非 reasoning 隐藏
//   - 选择档位 → patch llm.thinking_level

import { describe, expect, it, beforeEach, afterEach, vi } from "vitest";
import React, { act, useState } from "react";
import { createRoot } from "react-dom/client";
import { SettingsModel, modelListCache } from "./SettingsModel";
import { useToastStore } from "../../stores/useToastStore";
import type { LlmConfig, LlmPatch } from "../../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;
// 带签名的 mock（类型即 (patch, label?) => void，可断言调用）
let onPatch = vi.fn((_p: LlmPatch, _l?: string) => {});

const PROVIDERS = [
  { id: "deepseek", displayName: "DeepSeek", apiType: "openai-completions", baseUrl: "https://api.deepseek.com/v1", compat: {} },
  { id: "openai", displayName: "OpenAI", apiType: "openai-completions", baseUrl: "https://api.openai.com/v1", compat: {} },
];

const V4_FLASH: Record<string, unknown> = {
  id: "deepseek-v4-flash",
  displayName: "DeepSeek V4 Flash",
  contextLength: 1000000,
  supportsReasoning: true,
  supportsVision: false,
  compat: {},
  thinkingLevels: ["off", "low", "high", "max"],
  defaultThinkingLevel: "high",
  enriched: true,
};

function maskedLlm(over: Partial<LlmConfig> = {}): LlmConfig {
  return {
    provider: "deepseek",
    api_type: "openai-completions",
    model: "",
    api_key: "********",
    base_url: null,
    headers: null,
    max_tokens: 4096,
    thinking_level: null,
    ...over,
  };
}

function installRoutes(list_models: unknown[] = [V4_FLASH]): void {
  __invokeMock.mockImplementation((cmd: string) => {
    if (cmd === "list_providers") return Promise.resolve(PROVIDERS);
    if (cmd === "list_models") return Promise.resolve(list_models);
    if (cmd === "reveal_api_key") return Promise.resolve("sk-real-secret");
    return Promise.resolve(undefined);
  });
}

async function flush(): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
}

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
});

function renderModel(llm: LlmConfig, busy = false): void {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(
      React.createElement(SettingsModel, { llm, busy, onPatch })
    );
  });
}

/** 测试用 llm 段合并（模拟 Panel mergeSettings 对 active 视角的更新：
 *  家字段 patch 合并进 providers[llm.provider]，active/thinking 顶层处理） */
function mergeLlm(prev: LlmConfig, p: LlmPatch): LlmConfig {
  const next = { ...prev };
  if (p.active_provider !== undefined) next.provider = p.active_provider;
  if (p.thinking_level !== undefined)
    next.thinking_level = p.thinking_level === "" ? null : p.thinking_level;
  if (p.providers && prev.provider) {
    const pp = p.providers[prev.provider];
    if (pp) {
      if (pp.api_type !== undefined) next.api_type = pp.api_type;
      if (pp.model !== undefined) next.model = pp.model;
      // 与 Panel mergeSettings 同口径：明文写入在本地归一为掩码
      if (pp.api_key !== undefined)
        next.api_key = pp.api_key === "" ? "" : "********";
      if (pp.base_url !== undefined)
        next.base_url = pp.base_url === "" ? null : pp.base_url;
    }
  }
  return next;
}

/** 带 state 的 Harness：受控回显（模拟 Panel 乐观合并） */
function Harness({ llm, busy = false }: { llm: LlmConfig; busy?: boolean }) {
  const [state, setState] = useState(llm);
  return React.createElement(SettingsModel, {
    llm: state,
    busy,
    onPatch: (p: LlmPatch, label?: string) => {
      onPatch(p, label);
      setState((prev) => mergeLlm(prev, p));
    },
  });
}

function renderHarness(llm: LlmConfig, busy = false): void {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(React.createElement(Harness, { llm, busy }));
  });
}

function keyInput(): HTMLInputElement {
  // type 随 reveal 在 password/text 间切换——用 placeholder 稳定定位
  return container!.querySelector<HTMLInputElement>(
    'input[placeholder="sk-..."]'
  )!;
}

/** 原生 setter + input 事件（React 受控） */
function typeInto(el: HTMLInputElement | HTMLTextAreaElement, value: string): void {
  const proto =
    el instanceof HTMLTextAreaElement
      ? window.HTMLTextAreaElement.prototype
      : window.HTMLInputElement.prototype;
  const setter = Object.getOwnPropertyDescriptor(proto, "value")!.set!;
  act(() => {
    setter.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function blur(el: HTMLElement): void {
  act(() => {
    el.dispatchEvent(new FocusEvent("focusout", { bubbles: true }));
  });
}

/** 获取模型列表 → 出现模型行（行文本 = displayName） */
async function fetchAndSelectModel(displayName: string): Promise<void> {
  // 点「获取模型列表」
  const fetchBtn = Array.from(container!.querySelectorAll("button")).find(
    (b) => b.textContent === "获取模型列表"
  );
  if (fetchBtn) {
    act(() => fetchBtn.dispatchEvent(new MouseEvent("click", { bubbles: true })));
    await flush();
  }
  // 点模型行（displayName；css module 类名含 modelItem 子串）
  const row = Array.from(container!.querySelectorAll("div")).find(
    (d) =>
      typeof d.className === "string" &&
      d.className.includes("modelItem") &&
      (d.textContent ?? "").includes(displayName)
  );
  expect(row).toBeTruthy();
  act(() => row!.dispatchEvent(new MouseEvent("click", { bubbles: true })));
  await flush();
}

beforeEach(() => {
  useToastStore.setState({ toasts: [] });
  __invokeMock.mockClear();
  // 清模块级模型列表缓存（防跨用例污染：切供应商 effect 会读缓存跳 fetch）
  for (const k of Object.keys(modelListCache)) delete modelListCache[k];
  onPatch = vi.fn((_p: LlmPatch, _l?: string) => {});
  installRoutes();
});

describe("SettingsModel · API Key mask 契约", () => {
  it("掩码常态显示；编辑失焦提交；未编辑失焦不上送（不误清）", async () => {
    renderModel(maskedLlm());
    await flush();
    const input = keyInput();
    expect(input.value).toBe("********");

    // 未编辑：聚焦直接失焦 → 无 api_key patch（防误清）
    act(() => input.dispatchEvent(new FocusEvent("focusin", { bubbles: true })));
    await flush();
    // 聚焦不得清空已填内容（2026-10-11 实测 bug：一点就没了，像把密钥删了）
    expect(keyInput().value).toBe("********");
    blur(input);
    await flush();
    expect(onPatch).not.toHaveBeenCalled();

    // 编辑：输入新 key → 失焦提交
    typeInto(input, "sk-new-key");
    await flush();
    blur(input);
    await flush();
    expect(onPatch).toHaveBeenCalledWith(
      { providers: { deepseek: { api_key: "sk-new-key" } } },
      "API Key"
    );
  });

  it("👁 瞬态查看：reveal 明文 readOnly；复原不提交", async () => {
    renderModel(maskedLlm());
    await flush();
    // 点 👁（keyToggle）
    const toggle = container!.querySelector("button[title='显示']") as HTMLButtonElement;
    act(() => toggle.dispatchEvent(new MouseEvent("click", { bubbles: true })));
    await flush();
    const input = keyInput();
    expect(input.value).toBe("sk-real-secret");
    expect(input.readOnly).toBe(true);
    // 修复断言：reveal 后 type 必须切 text（否则密码框按明文长度遮挡成星号）
    expect(input.type).toBe("text");
    // blur 复原（readOnly 失焦触发 onBlur → revealed null）——不产生 patch
    blur(input);
    await flush();
    expect(keyInput().value).toBe("********");
    expect(keyInput().type).toBe("password");
    expect(onPatch).not.toHaveBeenCalled();
  });

  it("👁 可用性：已配 key（掩码）可用；编辑中不可用", async () => {
    renderModel(maskedLlm());
    await flush();
    const toggle = container!.querySelector<HTMLButtonElement>("button[title='显示']");
    expect(toggle?.disabled).toBe(false);

    // 编辑中不可查看（否则"显示旧值"会盖住正在输入的新值，失焦还会把它吞掉）
    typeInto(keyInput(), "sk-x");
    await flush();
    const editing = container!.querySelector<HTMLButtonElement>(
      "button[title='编辑中不可查看']"
    );
    expect(editing?.disabled).toBe(true);
  });

  it("👁 未配 key 时禁用（title 表达原因）", async () => {
    renderModel(maskedLlm({ api_key: "" }));
    await flush();
    const toggle = container!.querySelector<HTMLButtonElement>("button[title='暂无密钥']");
    expect(toggle?.disabled).toBe(true);
  });

  it("保存后回掩码：输入 key 失焦 → 显示掩码且 👁 恢复可用（面板归一）", async () => {
    renderHarness(maskedLlm({ api_key: "" }));
    await flush();
    typeInto(keyInput(), "sk-new");
    await flush();
    blur(keyInput());
    await flush();
    // 明文不常驻面板态 → 回掩码；👁 因"有密钥"而可用
    expect(keyInput().value).toBe("********");
    const toggle = container!.querySelector<HTMLButtonElement>("button[title='显示']");
    expect(toggle?.disabled).toBe(false);
  });
});

describe("SettingsModel · 思考强度", () => {
  it("声明集模型：可见集 [off,low,high,max]、默认档 high 高亮、选择即 patch", async () => {
    renderHarness(maskedLlm({ model: "" }));
    await flush();
    // 尚无模型选择 → 行不显示
    expect(container!.textContent).not.toContain("思考强度");

    await fetchAndSelectModel("DeepSeek V4 Flash");

    // 调试断言：模型行点击应已产生 model patch（Harness 受控回显）
    expect(onPatch).toHaveBeenCalledWith(
      expect.objectContaining({
        providers: expect.objectContaining({
          deepseek: expect.objectContaining({ model: "deepseek-v4-flash" }),
        }),
      }),
      "模型"
    );

    // 思考强度行：dropdown 同款控件；trigger 显示当前档（默认 high → 高）
    expect(container!.textContent).toContain("思考强度");
    const trigger = Array.from(container!.querySelectorAll("button")).find(
      (b) => (b.textContent ?? "").startsWith("高")
    )!;
    expect(trigger).toBeTruthy();
    act(() =>
      trigger.dispatchEvent(new MouseEvent("click", { bubbles: true }))
    );
    await flush();

    // 可见集 = 声明集 [off,low,high,max]（展示名含默认标注）
    const menuTexts = Array.from(container!.querySelectorAll("div"))
      .map((d) => (d.textContent ?? "").trim())
      .filter((t) => ["关闭", "低", "高（默认）", "极致"].includes(t));
    expect(menuTexts).toEqual(["关闭", "低", "高（默认）", "极致"]);

    // 选「低」→ patch thinking_level
    const lowItem = Array.from(container!.querySelectorAll("div")).find(
      (d) => (d.textContent ?? "").trim() === "低"
    )!;
    act(() =>
      lowItem.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }))
    );
    await flush();
    expect(onPatch).toHaveBeenCalledWith({ thinking_level: "low" }, "思考强度");
  });

  it("无声明 reasoning 模型：推断集 [off,medium,high]、默认 medium", async () => {
    installRoutes([
      {
        id: "some-reasoning-model",
        displayName: "推理模型 X",
        contextLength: 128000,
        supportsReasoning: true,
        supportsVision: false,
        compat: {},
        enriched: true,
      },
    ]);
    renderHarness(maskedLlm({ model: "" }));
    await flush();
    await fetchAndSelectModel("推理模型 X");

    expect(container!.textContent).toContain("思考强度");
    const trigger = Array.from(container!.querySelectorAll("button")).find(
      (b) => (b.textContent ?? "").startsWith("中")
    )!;
    expect(trigger).toBeTruthy(); // 推断默认 medium → 中
    act(() =>
      trigger.dispatchEvent(new MouseEvent("click", { bubbles: true }))
    );
    await flush();

    const menuTexts = Array.from(container!.querySelectorAll("div"))
      .map((d) => (d.textContent ?? "").trim())
      .filter((t) => ["关闭", "中（默认）", "高"].includes(t));
    expect(menuTexts).toEqual(["关闭", "中（默认）", "高"]);
  });

  it("非 reasoning 模型：思考强度行隐藏", async () => {
    installRoutes([
      {
        id: "deepseek-chat",
        displayName: "DeepSeek Chat",
        contextLength: 128000,
        supportsReasoning: false,
        supportsVision: false,
        compat: {},
        enriched: true,
      },
    ]);
    renderHarness(maskedLlm({ model: "" }));
    await flush();
    await fetchAndSelectModel("DeepSeek Chat");

    expect(container!.textContent).not.toContain("思考强度");
  });
});

describe("SettingsModel · 供应商切换（分区：只 patch active）", () => {
  it("切供应商 → active_provider patch（不清 key；配置分区保留）", async () => {
    renderModel(maskedLlm());
    await flush();
    const trigger = Array.from(container!.querySelectorAll("button")).find(
      (b) => (b.textContent ?? "").includes("DeepSeek")
    )!;
    act(() => trigger.dispatchEvent(new MouseEvent("click", { bubbles: true })));
    await flush();
    const item = Array.from(container!.querySelectorAll("div")).find(
      (d) => d.textContent === "OpenAI"
    )!;
    act(() => item.dispatchEvent(new MouseEvent("mousedown", { bubbles: true })));
    await flush();

    const patch = onPatch.mock.calls[0]?.[0] as LlmPatch;
    expect(patch).toEqual({ active_provider: "openai" });
    expect(patch.providers).toBeUndefined(); // 不触碰其它家（含 key）
  });

  it("思考强度 patch 为段顶层（跨供应商全局）", async () => {
    renderHarness(maskedLlm({ model: "" }));
    await flush();
    await fetchAndSelectModel("DeepSeek V4 Flash");
    const trigger = Array.from(container!.querySelectorAll("button")).find(
      (b) => (b.textContent ?? "").startsWith("高")
    )!;
    act(() =>
      trigger.dispatchEvent(new MouseEvent("click", { bubbles: true }))
    );
    await flush();
    const lowItem = Array.from(container!.querySelectorAll("div")).find(
      (d) => (d.textContent ?? "").trim() === "低"
    )!;
    act(() =>
      lowItem.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }))
    );
    await flush();
    expect(onPatch).toHaveBeenCalledWith({ thinking_level: "low" }, "思考强度");
  });
});

describe("SettingsModel · 自动拉取模型列表（方案 b）", () => {
  function listModelsCalls(): Array<{ provider: string | undefined }> {
    return __invokeMock.mock.calls
      .filter((c) => c[0] === "list_models")
      .map((c) => ({ provider: (c[1] as { provider?: string }).provider }));
  }

  it("已配 key（掩码）且无缓存 → 挂载自动拉取并显示列表（无需手动点击）", async () => {
    renderModel(maskedLlm({ api_key: "********" }));
    await flush();
    const calls = listModelsCalls();
    expect(calls.length).toBeGreaterThanOrEqual(1);
    expect(calls[0].provider).toBe("deepseek");
    // 自动拉成功 → loaded：模型行已显示（无「获取模型列表」按钮）
    expect(container!.textContent).toContain("DeepSeek V4 Flash");
    expect(
      Array.from(container!.querySelectorAll("button")).some(
        (b) => b.textContent === "获取模型列表"
      )
    ).toBe(false);
  });

  it("未配 key → 不自动拉，保留手动入口", async () => {
    renderModel(maskedLlm({ api_key: "" }));
    await flush();
    expect(listModelsCalls()).toHaveLength(0);
    expect(
      Array.from(container!.querySelectorAll("button")).some(
        (b) => b.textContent === "获取模型列表"
      )
    ).toBe(true);
  });

  it("自动拉失败 → 静默回 idle（无 toast、手动按钮保留）", async () => {
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_providers") return Promise.resolve(PROVIDERS);
      if (cmd === "reveal_api_key") return Promise.resolve("sk-real-secret");
      if (cmd === "list_models") return Promise.reject("网络错误");
      return Promise.resolve(undefined);
    });
    renderModel(maskedLlm());
    await flush();
    const toasts = useToastStore.getState().toasts;
    expect(toasts).toHaveLength(0); // 静默
    expect(
      Array.from(container!.querySelectorAll("button")).some(
        (b) => b.textContent === "获取模型列表"
      )
    ).toBe(true);
  });

  it("切换供应商 → 该家无缓存时自动拉取", async () => {
    renderHarness(maskedLlm());
    await flush();
    expect(listModelsCalls().length).toBeGreaterThanOrEqual(1); // deepseek 自动拉
    // 切到 openai（Harness mergeLlm 只改 active；key 沿用掩码 → openai 自动拉）
    const trigger = Array.from(container!.querySelectorAll("button")).find(
      (b) => (b.textContent ?? "").includes("DeepSeek")
    )!;
    act(() => trigger.dispatchEvent(new MouseEvent("click", { bubbles: true })));
    await flush();
    const openaiItem = Array.from(container!.querySelectorAll("div")).find(
      (d) => d.textContent === "OpenAI"
    )!;
    act(() =>
      openaiItem.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }))
    );
    await flush();
    const calls = listModelsCalls();
    expect(calls.some((c) => c.provider === "openai")).toBe(true);
  });
});

describe("SettingsModel · 未收录标记（U 徽章）", () => {
  const V41_UNKNOWN: Record<string, unknown> = {
    id: "deepseek-flash",
    displayName: "deepseek-flash",
    contextLength: 0,
    supportsReasoning: false,
    supportsVision: false,
    compat: {},
    thinkingLevels: [],
    defaultThinkingLevel: null,
    enriched: false,
  };

  it("未收录模型显示 U 徽章（弱化）；已收录模型不显示", async () => {
    installRoutes([V4_FLASH, V41_UNKNOWN]);
    renderModel(maskedLlm());
    await flush();
    // 已配 mask key → 自动拉取，列表已出现
    expect(container!.textContent).toContain("DeepSeek V4 Flash");
    expect(container!.textContent).toContain("deepseek-flash");

    const unknownBadges = container!.querySelectorAll('[title^="尚未收录"]');
    expect(unknownBadges).toHaveLength(1);
    expect(unknownBadges[0].textContent).toBe("U");

    // 归属：U 徽章只在未收录行内（已收录行无）
    const rows = Array.from(container!.querySelectorAll("div")).filter(
      (d) =>
        typeof d.className === "string" && d.className.includes("modelItem")
    );
    const v4Row = rows.find((d) =>
      (d.textContent ?? "").includes("DeepSeek V4 Flash")
    )!;
    const unknownRow = rows.find((d) =>
      (d.textContent ?? "").includes("deepseek-flash")
    )!;
    expect(v4Row.querySelector('[title^="尚未收录"]')).toBeNull();
    expect(unknownRow.querySelector('[title^="尚未收录"]')).toBeTruthy();
  });

  it("未收录模型仍可选（无能力信息不阻断选用）", async () => {
    installRoutes([V41_UNKNOWN]);
    renderHarness(maskedLlm({ model: "" }));
    await flush();
    await fetchAndSelectModel("deepseek-flash");
    expect(onPatch).toHaveBeenCalledWith(
      expect.objectContaining({
        providers: expect.objectContaining({
          deepseek: expect.objectContaining({ model: "deepseek-flash" }),
        }),
      }),
      "模型"
    );
  });
});
