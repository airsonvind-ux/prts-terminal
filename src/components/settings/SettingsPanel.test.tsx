// @vitest-environment jsdom
// SettingsPanel · 五子页路由 + 即时保存单漏斗测试
//
// 覆盖（设置页扩展批 B / C1）：
//   - 七个导航项（app/model/safety/space/skills/memory/about）与默认页（记忆 settingsInitialTab）
//   - Safety 页即时保存：radio 变更 → 一次 set_settings patch invoke（无卸载批保存）
//   - busy 守卫：对话处理中变更被拒（toast + 受控回弹，无 invoke）
//   - 保存失败：回滚 + toast（保存失败前端可见）
//   - 记忆上次所在页：切页写 useUIStore.settingsInitialTab
//   - 应用页：置顶开关走 set_always_on_top 专用命令 + 失败回滚；退出按钮存在
//   - 关于页：get_app_meta 版本与路径展示
//   - mergeSettings 纯函数：per-field 合并 / 空串清除 / mask 不覆盖 / 缺席不覆盖

import { describe, expect, it, beforeEach, afterEach } from "vitest";
import React, { act } from "react";
import { createRoot } from "react-dom/client";
import { SettingsPanel, mergeSettings, MASKED_SECRET } from "./SettingsPanel";
import { useConversationStore } from "../../stores/useConversationStore";
import { useProcessingStore } from "../../stores/useProcessingStore";
import { useToastStore } from "../../stores/useToastStore";
import { useUIStore } from "../../stores/useUIStore";
import type { Settings } from "../../types";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT =
  true;

let container: HTMLDivElement;
let root: ReturnType<typeof createRoot>;

const NAV_KEYS = ["app", "model", "safety", "space", "skills", "memory", "about"] as const;

function defaultSettings(): Settings {
  return {
    schema_version: 2,
    llm: {
      active_provider: "",
      providers: {},
      thinking_level: null,
    },
    exec: { mode: "ask" },
    workspace: { workspace_root: "" },
    window: { always_on_top: true },
  };
}

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
});

function resetStores(): void {
  useProcessingStore.setState({ processing: {} });
  useConversationStore.setState({ conversations: [], activeId: null });
  useToastStore.setState({ toasts: [] });
  useUIStore.setState({
    overlay: null,
    errorMsg: null,
    hiddenMode: null,
    settingsInitialTab: "app",
  });
}

async function flush(): Promise<void> {
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
}

function renderPanel(): void {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root.render(React.createElement(SettingsPanel));
  });
}

function clickTab(key: string): void {
  const el = container!.querySelector<HTMLElement>(`[data-tab="${key}"]`)!;
  act(() => {
    el.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

function clickRadio(index: number): void {
  act(() => {
    const inputs = container!.querySelectorAll<HTMLInputElement>(
      'input[type="radio"][name="execMode"]'
    );
    inputs[index].dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

function radioChecked(index: number): boolean {
  const inputs = container!.querySelectorAll<HTMLInputElement>(
    'input[type="radio"][name="execMode"]'
  );
  return inputs[index].checked;
}

function setSettingsCalls(): Array<{ patch: unknown }> {
  // invoke("set_settings", { patch }) → mock 调用参数 = { patch }
  return __invokeMock.mock.calls
    .filter((c) => c[0] === "set_settings")
    .map((c) => c[1] as { patch: unknown });
}

function seedBusy(): void {
  useProcessingStore.setState({
    processing: {
      busy_sid: {
        conversationId: "busy-conv",
        currentEvents: [
          { type: "user/message", sessionId: "busy_sid", content: "x" },
        ],
        agentPhase: "streaming",
        mood: "平静",
        moodIntensity: 0,
      },
    },
  });
}

beforeEach(() => {
  resetStores();
  __invokeMock.mockClear();
  // 还原默认路由（个别用例用 mockImplementation 覆盖后，避免泄漏到后续用例）
  __invokeMock.mockImplementation(
    (globalThis as { __defaultInvokeImpl?: (cmd: string) => Promise<unknown> })
      .__defaultInvokeImpl!
  );
});

describe("SettingsPanel · 子页路由", () => {
  it("挂载后渲染七个导航项，默认停 model 页", async () => {
    renderPanel();
    await flush();
    for (const key of NAV_KEYS) {
      expect(container!.querySelector(`[data-tab="${key}"]`)).toBeTruthy();
    }
    // 内容区标题 = 当前页（model）
    expect(container!.textContent).toContain("模型");
  });

  it("切到安全页：两选项静态并存（ask 默认选中）", async () => {
    renderPanel();
    await flush();
    clickTab("safety");
    expect(container!.textContent).toContain("操作前询问");
    expect(container!.textContent).toContain("自动化");
    expect(radioChecked(0)).toBe(true); // 操作前询问
    expect(radioChecked(1)).toBe(false);
  });

  it("active-only：非当前页不渲染（切页后 safety 内容卸载）", async () => {
    renderPanel();
    await flush();
    clickTab("safety");
    expect(container!.textContent).toContain("操作前询问");
    clickTab("about");
    expect(container!.textContent).not.toContain("操作前询问");
  });

  it("记忆上次所在页：切页写 settingsInitialTab；重挂载停记忆页", async () => {
    renderPanel();
    await flush();
    clickTab("safety");
    expect(useUIStore.getState().settingsInitialTab).toBe("safety");

    // 卸载重挂 → 初始页 = 记忆的 safety
    act(() => root.unmount());
    renderPanel();
    await flush();
    expect(container!.textContent).toContain("操作前询问");
  });
});

describe("SettingsPanel · 即时保存单漏斗（Safety 接线）", () => {
  it("radio 变更 → 一次 set_settings patch（{exec:{mode}}）+ 乐观更新", async () => {
    renderPanel();
    await flush();
    clickTab("safety");
    clickRadio(1); // 自动化
    await flush();

    const calls = setSettingsCalls();
    expect(calls).toHaveLength(1);
    expect(calls[0].patch).toEqual({ exec: { mode: "auto" } });
    expect(radioChecked(1)).toBe(true);
  });

  it("卸载无延迟保存：变更后卸载面板不再产生 set_settings 调用", async () => {
    renderPanel();
    await flush();
    clickTab("safety");
    clickRadio(1);
    await flush();
    const before = setSettingsCalls().length;
    expect(before).toBe(1);

    act(() => root.unmount());
    await flush();
    expect(setSettingsCalls().length).toBe(before); // 无批保存追加
  });

  it("busy：对话处理中变更被拒（toast + 回弹，无 invoke）", async () => {
    renderPanel();
    await flush();
    clickTab("safety");
    seedBusy();
    clickRadio(1); // busy 下点自动化
    await flush();

    expect(setSettingsCalls().length).toBe(0);
    expect(radioChecked(0)).toBe(true); // 受控回弹到 ask
    const toasts = useToastStore.getState().toasts;
    expect(
      toasts.some((t) => t.message.includes("对话处理中不能修改命令确认"))
    ).toBe(true);
  });

  it("保存失败：回滚 + toast（前端可见）", async () => {
    // 本用例内 set_settings 恒失败
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_settings") return Promise.resolve(defaultSettings());
      if (cmd === "set_settings") return Promise.reject("写盘失败");
      return Promise.resolve(undefined);
    });
    renderPanel();
    await flush();
    clickTab("safety");
    clickRadio(1);
    await flush();

    expect(radioChecked(0)).toBe(true); // 回滚到 ask
    const toasts = useToastStore.getState().toasts;
    expect(toasts.some((t) => t.message.includes("保存失败"))).toBe(true);
  });
});

describe("SettingsPanel · 应用页（置顶开关 + 退出）", () => {
  function topCheckbox(): HTMLInputElement {
    return container!.querySelector<HTMLInputElement>(
      'input[type="checkbox"]'
    )!;
  }

  function toggleTopCheckbox(): void {
    // 受控 checkbox：React 由 click 驱动 onChange
    act(() => {
      topCheckbox().dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
  }

  it("应用页渲染：置顶开关 + 退出按钮（草案 v2：无静态参数展示）", async () => {
    renderPanel();
    await flush();
    clickTab("app");
    expect(container!.textContent).toContain("置顶显示");
    expect(container!.textContent).toContain("开启后窗口始终浮于其他窗口之上");
    expect(container!.textContent).toContain("关闭终端");
    // 静态参数展示已删除（草案 v2）
    expect(container!.textContent).not.toContain("600 × 600");
    expect(container!.textContent).not.toContain("透明");
    expect(topCheckbox().checked).toBe(true); // 默认置顶
  });

  it("切换置顶 → set_always_on_top 专用命令 + 乐观更新", async () => {
    renderPanel();
    await flush();
    clickTab("app");
    toggleTopCheckbox();
    await flush();

    const calls = __invokeMock.mock.calls.filter(
      (c) => c[0] === "set_always_on_top"
    );
    expect(calls).toHaveLength(1);
    expect((calls[0][1] as { enabled: boolean }).enabled).toBe(false);
    expect(topCheckbox().checked).toBe(false); // 乐观已更新
    // 不走 set_settings（专用命令原子完成窗口 + 落盘）
    expect(setSettingsCalls().length).toBe(0);
  });

  it("置顶失败：回滚 + toast", async () => {
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_settings") return Promise.resolve(defaultSettings());
      if (cmd === "get_app_meta")
        return Promise.resolve({
          version: "0.1.0",
          data_dir: "C:/fake/data",
          workspace_root: "C:/fake/ws",
          db_path: "C:/fake/db",
          knowledge_dir: "C:/fake/kb",
          skills_dir: "C:/fake/skills",
        });
      if (cmd === "set_always_on_top") return Promise.reject("窗口操作失败");
      return Promise.resolve(undefined);
    });
    renderPanel();
    await flush();
    clickTab("app");
    toggleTopCheckbox();
    await flush();

    expect(topCheckbox().checked).toBe(true); // 回滚到置顶
    const toasts = useToastStore.getState().toasts;
    expect(
      toasts.some((t) => t.message.includes("保存失败（窗口置顶）"))
    ).toBe(true);
  });
});

describe("SettingsPanel · 关于页（极简：名称 + 版本）", () => {
  it("显示应用名与版本（草案 v2：无技术信息/路径）", async () => {
    renderPanel();
    await flush();
    clickTab("about");
    expect(container!.textContent).toContain("PRTS 终端助手");
    expect(container!.textContent).toContain("版本 0.1.0");
    // 已删除内容断言
    expect(container!.textContent).not.toContain("数据目录");
    expect(container!.textContent).not.toContain("框架");
  });
});

describe("SettingsPanel · 空间页（路径只读 + 目录选择 + 目录结构树）", () => {
  it("渲染：路径只读值 + hint + 目录结构节（树 + 搜索框 + 根一层拉起）", async () => {
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_settings") {
        const s = defaultSettings();
        s.workspace.workspace_root = "F:/ws";
        return Promise.resolve(s);
      }
      if (cmd === "list_workspace_dir") return Promise.resolve([]);
      return Promise.resolve(undefined);
    });
    renderPanel();
    await flush();
    clickTab("space");
    expect(container!.textContent).toContain("工作空间");
    expect(container!.textContent).toContain("F:/ws");
    expect(container!.textContent).not.toContain("未设置");
    expect(container!.textContent).toContain("修改后重启生效");
    // 目录结构节：树容器 + 搜索框在位
    expect(container!.querySelector("[data-workspace-tree]")).toBeTruthy();
    expect(
      container!.querySelector('input[placeholder="搜索工作空间"]')
    ).toBeTruthy();
    // 根一层用当前工作区根拉起（前端不拼路径）
    const listCalls = __invokeMock.mock.calls.filter(
      (c) => c[0] === "list_workspace_dir"
    );
    expect(listCalls).toHaveLength(1);
    expect((listCalls[0][1] as { path: string }).path).toBe("F:/ws");
  });

  it("未设置时路径显示「未设置」（任务 B）", async () => {
    renderPanel();
    await flush();
    clickTab("space");
    expect(container!.textContent).toContain("未设置");
  });

  it("选择目录：按钮 → choose_workspace_directory → 选定即存 patch", async () => {
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_settings") return Promise.resolve(defaultSettings());
      if (cmd === "choose_workspace_directory")
        return Promise.resolve("F:/picked-ws");
      return Promise.resolve(undefined);
    });
    renderPanel();
    await flush();
    clickTab("space");

    const pickerCalls = () =>
      __invokeMock.mock.calls.filter(
        (c) => c[0] === "choose_workspace_directory"
      );
    expect(pickerCalls()).toHaveLength(0);

    const btn = Array.from(container!.querySelectorAll("button")).find(
      (b) => b.textContent === "选择目录"
    )!;
    act(() => {
      btn.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();

    expect(pickerCalls()).toHaveLength(1);
    expect((pickerCalls()[0][1] as { current: string | null }).current).toBeNull();
    const calls = setSettingsCalls();
    expect(calls).toHaveLength(1);
    expect(calls[0].patch).toEqual({
      workspace: { workspace_root: "F:/picked-ws" },
    });
    // 乐观更新已显示新路径
    expect(container!.textContent).toContain("F:/picked-ws");
  });

  it("在资源管理器中打开 → open_workspace_in_explorer + 可见反馈", async () => {
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_settings") {
        const s = defaultSettings();
        s.workspace.workspace_root = "F:/ws";
        return Promise.resolve(s);
      }
      if (cmd === "list_workspace_dir") return Promise.resolve([]);
      return Promise.resolve(undefined);
    });
    renderPanel();
    await flush();
    clickTab("space");

    const btn = Array.from(container!.querySelectorAll("button")).find(
      (b) => b.textContent === "在资源管理器中打开"
    )!;
    expect(btn).toBeTruthy();
    act(() => {
      btn.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();

    expect(
      __invokeMock.mock.calls.filter(
        (c) => c[0] === "open_workspace_in_explorer"
      )
    ).toHaveLength(1);
    const toasts = useToastStore.getState().toasts;
    expect(
      toasts.some((t) => t.message.includes("已在文件管理器打开工作目录"))
    ).toBe(true);
  });
});

describe("SettingsPanel · 技能页（由空间页技能节拆出）", () => {
  const SKILL_FIXTURE = [
    // 字段按后端真实契约（SkillEntry camelCase：filePath）
    { name: "数值计算", description: "数值类任务推演流程标准。", filePath: "F:/skills/数值计算.md" },
    { name: "行动推演", description: "寻路与行动模拟。", filePath: "F:/skills/行动推演.md" },
  ];

  it("渲染：技能容器列表（名称 + 描述）", async () => {
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_settings") return Promise.resolve(defaultSettings());
      if (cmd === "list_skills") return Promise.resolve(SKILL_FIXTURE);
      return Promise.resolve(undefined);
    });
    renderPanel();
    await flush();
    clickTab("skills");
    expect(container!.textContent).toContain("数值计算");
    expect(container!.textContent).toContain("行动推演");
    // 技能页不再承载工作空间内容（拆分后职责单一）
    expect(container!.textContent).not.toContain("修改后重启生效");
  });

  it("技能行点击 → open_path 打开源 md + 成功可见反馈（info toast）", async () => {
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_settings") return Promise.resolve(defaultSettings());
      if (cmd === "list_skills") return Promise.resolve(SKILL_FIXTURE);
      return Promise.resolve(undefined);
    });
    renderPanel();
    await flush();
    clickTab("skills");

    const calcRow = Array.from(container!.querySelectorAll("span")).find(
      (s) => (s.textContent ?? "").includes("数值计算")
        && (s.textContent ?? "").includes("数值类任务")
    )!;
    act(() => {
      calcRow.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();

    const calls = __invokeMock.mock.calls.filter((c) => c[0] === "open_path");
    expect(calls).toHaveLength(1);
    expect((calls[0][1] as { path: string }).path).toBe("F:/skills/数值计算.md");
    // 成功必有可见反馈（点击无静默）
    const toasts = useToastStore.getState().toasts;
    expect(toasts.some((t) => t.message.includes("已打开「数值计算」"))).toBe(true);
  });

  it("技能行点击失败 → warning toast（不静默）", async () => {
    __invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_settings") return Promise.resolve(defaultSettings());
      if (cmd === "list_skills") return Promise.resolve(SKILL_FIXTURE);
      if (cmd === "open_path")
        return Promise.reject("文件不存在：F:/skills/x.md");
      return Promise.resolve(undefined);
    });
    renderPanel();
    await flush();
    clickTab("skills");

    const calcRow = Array.from(container!.querySelectorAll("span")).find(
      (s) => (s.textContent ?? "").includes("数值计算")
        && (s.textContent ?? "").includes("数值类任务")
    )!;
    act(() => {
      calcRow.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await flush();

    const toasts = useToastStore.getState().toasts;
    expect(
      toasts.some((t) => t.kind === "warning" && t.message.includes("无法打开文件"))
    ).toBe(true);
  });
});

describe("mergeSettings · 纯函数", () => {
  const base = defaultSettings();

  it("per-field 合并：缺席不覆盖", () => {
    const next = mergeSettings(base, { exec: { mode: "auto" } });
    expect(next.exec.mode).toBe("auto");
    expect(next.llm).toEqual(base.llm);
    const next2 = mergeSettings(next, { exec: { mode: "ask" } });
    expect(next2.exec.mode).toBe("ask");
  });

  it("家字段空串清除 + 顶层 thinking 清除", () => {
    const withVals = mergeSettings(base, {
      llm: {
        active_provider: "deepseek",
        providers: { deepseek: { model: "m1", base_url: "https://x" } },
        thinking_level: "high",
      },
    });
    expect(withVals.llm.providers.deepseek.base_url).toBe("https://x");
    const cleared = mergeSettings(withVals, {
      llm: {
        providers: { deepseek: { base_url: "" } },
        thinking_level: "",
      },
    });
    expect(cleared.llm.providers.deepseek.base_url).toBeNull();
    expect(cleared.llm.thinking_level).toBeNull();
    expect(cleared.llm.providers.deepseek.model).toBe("m1"); // 未触及
  });

  it("api_key 掩码占位不覆盖该家现值；他家不受影响", () => {
    const seeded = mergeSettings(base, {
      llm: {
        providers: { deepseek: { api_key: "sk-real" } },
      },
    });
    const masked = mergeSettings(seeded, {
      llm: { providers: { deepseek: { api_key: MASKED_SECRET } } },
    });
    expect(masked.llm.providers.deepseek.api_key).toBe("sk-real");
  });

  it("window / workspace 段合并", () => {
    const next = mergeSettings(base, {
      window: { always_on_top: false },
      workspace: { workspace_root: "F:/ws" },
    });
    expect(next.window.always_on_top).toBe(false);
    expect(next.workspace.workspace_root).toBe("F:/ws");
  });

  it("多供应商切换保留（核心验收）：切 active 不动其它家配置", () => {
    const seeded = mergeSettings(base, {
      llm: {
        active_provider: "deepseek",
        providers: {
          deepseek: { model: "deepseek-v4-flash", api_key: "sk-ds" },
          openai: { model: "gpt-5", api_key: "sk-oa" },
        },
      },
    });
    const switched = mergeSettings(seeded, {
      llm: { active_provider: "openai" },
    });
    expect(switched.llm.active_provider).toBe("openai");
    // 目标家回填可用（Panel 视角）
    expect(switched.llm.providers.openai.model).toBe("gpt-5");
    expect(switched.llm.providers.openai.api_key).toBe("sk-oa");
    // deepseek 完整保留
    expect(switched.llm.providers.deepseek.model).toBe("deepseek-v4-flash");
    expect(switched.llm.providers.deepseek.api_key).toBe("sk-ds");
  });

  it("家字段 patch 懒建新家条目", () => {
    const next = mergeSettings(base, {
      llm: {
        active_provider: "kimi",
        providers: { kimi: { api_key: "sk-kimi", model: "kimi-k2.7-code" } },
      },
    });
    expect(next.llm.providers.kimi.api_key).toBe("sk-kimi");
    expect(next.llm.providers.kimi.api_type).toBe("openai-completions");
  });
});
