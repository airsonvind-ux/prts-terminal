// 设置页主容器 · 子页路由（app/model/safety/space/skills/memory/about）
//              + 即时保存单漏斗
//
// 职责：
//   1. 挂载时 invoke("get_settings") 一次 → settings state（全部子页的受控数据源）
//   2. updateSettings 单漏斗：busy 守卫（单点）→ 乐观合并 state → invoke
//      set_settings(per-field patch) → 失败回滚 + toast（label 区分文案）。
//      文本字段由子页控件 blur 时触发（即时保存 = 变更即写；卸载不再批保存）
//   3. 五 tab active-only 路由：仅当前 tab 渲染（首挂取数据、切走卸载；
//      settings 数据在 Panel 层常驻——切回不重复 get_settings）
//   4. 记忆上次所在页：切页即写 useUIStore.settingsInitialTab；打开时读该值。
//      ContextIndicator 跳模型页 = 覆盖一次（置 "model" 再打开）
//
// M3-B：busy（任一对话处理中）时本漏斗拒绝一切变更 → toast + 不更新 state →
// 受控控件自动回弹（后端另有 set_settings busy 闸双保险）。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { SettingsAbout } from "./SettingsAbout";
import { SettingsApp } from "./SettingsApp";
import { SettingsExtension } from "./SettingsExtension";
import { SettingsModel } from "./SettingsModel";
import { SettingsSafety } from "./SettingsSafety";
import { SettingsSkills } from "./SettingsSkills";
import { SettingsSpace } from "./SettingsSpace";
import { SettingsMemory } from "./SettingsMemory";
import { SettingsUsage } from "./SettingsUsage";
import type { MaaStatus } from "../../types";
import { useUIStore } from "../../stores/useUIStore";
import { useGuideStore } from "../../stores/useGuideStore";
import { SETTINGS_TAB_GUIDE } from "../guide/guideContent";
import {
  hasPendingApproval,
  isProcessingBusy,
  useProcessingStore,
} from "../../stores/useProcessingStore";
import { showToast } from "../../stores/useToastStore";
import type {
  ApiType,
  AppMeta,
  ExecutionMode,
  Settings,
  SettingsPatch,
  SkillEntry,
} from "../../types";
import styles from "./SettingsPanel.module.css";

const TABS = [
  { key: "app", label: "应用" },
  { key: "model", label: "模型" },
  { key: "safety", label: "安全" },
  { key: "space", label: "空间" },
  { key: "skills", label: "技能" },
  { key: "memory", label: "记忆" },
  { key: "usage", label: "用量" },
  { key: "extension", label: "扩展" },
  { key: "about", label: "关于" },
] as const;

export type SettingsTabKey = (typeof TABS)[number]["key"];

/// api_key 掩码占位（与后端 MASKED_SECRET 一致；merge 时掩码不覆盖现值）
export const MASKED_SECRET = "********";

/**
 * patch → 新 settings 的纯合并（测试直接调用）。
 *
 * 与后端 set_settings 的 per-field 语义对齐：缺席字段不覆盖；llm 段 = v2
 * 分区：active_provider / providers.{家} 深层合并（新家懒建；家内字段缺席不
 * 覆盖、空串清除、api_key 掩码占位不覆盖、明文写入在本地归一为掩码）
 * / thinking_level 全局（空串清除）。
 */
export function mergeSettings(cur: Settings, patch: SettingsPatch): Settings {
  const next: Settings = { ...cur, workspace: { ...cur.workspace }, window: { ...cur.window } };
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
        // api_key === MASKED_SECRET 表达，👁 可用性靠它（曾因本地持明文而
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

export function SettingsPanel() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  // 权威引用：乐观合并 / 失败回滚的基准（无卸载批保存——ref 不再承担保存职责）
  const settingsRef = useRef<Settings | null>(null);
  // 应用只读元信息（关于页展示；空间页批 C2 复用同一份 props）
  const [appMeta, setAppMeta] = useState<AppMeta | null>(null);
  // 技能清单（技能页；null = 加载中/失败）
  const [skills, setSkills] = useState<SkillEntry[] | null>(null);
  const [activeTab, setActiveTab] = useState<SettingsTabKey>(
    () => useUIStore.getState().settingsInitialTab
  );
  // busy 订阅（M3-B 全局锁口径）：驱动漏斗守卫 + 模型页控件禁用
  const settingsBusy = useProcessingStore(isProcessingBusy);
  // 引导键集是否已从后端拉到（未拉到不弹——宁可不弹，也不误弹）
  const guideHydrated = useGuideStore((s) => s.hydrated);

  // 引导：进入某页且该页说明没看过 → 出通知卡（每页一次；切页换掉旧的那张）
  useEffect(() => {
    if (!guideHydrated) return;
    const key = SETTINGS_TAB_GUIDE[activeTab];
    if (key) useGuideStore.getState().enqueueNotice(key);
  }, [activeTab, guideHydrated]);

  // ── 待审批：不留面板 ──
  // 审批确认卡在消息流里，被遮罩盖住就点不到——这正是原先“处理中一律不能进设置”
  // 要防的那件事（会一直到 300s 超时）。现在只针对这一种情况收窄：有待审批时
  // 面板自动收起并把原因说出来，其余处理中情况一律可进、按项锁。
  const pendingApproval = useProcessingStore(hasPendingApproval);
  useEffect(() => {
    if (!pendingApproval) return;
    showToast("有新命令待确认", {
      kind: "info",
      dedupeKey: "approval-blocking",
    });
    useUIStore.getState().setOverlay(null);
  }, [pendingApproval]);

  // active 供应商视角配置（模型页受控）：providers[active] 拼 LlmConfig
  // （provider = active 名 + 段顶层 thinking_level），缺条目 = 空视角
  const activeLlmConfig = useMemo(() => {
    if (!settings) return null;
    const active = settings.llm.active_provider;
    const pc = settings.llm.providers[active];
    return {
      provider: active,
      api_type: pc?.api_type ?? ("openai-completions" as ApiType),
      model: pc?.model ?? "",
      api_key: pc?.api_key ?? "",
      base_url: pc?.base_url ?? null,
      headers: pc?.headers ?? null,
      max_tokens: pc?.max_tokens ?? 4096,
      thinking_level: settings.llm.thinking_level ?? null,
    } as Settings["llm"]["providers"][string] & {
      provider: string;
      thinking_level: string | null;
    };
  }, [settings]);

  // ── 挂载：get_settings + get_app_meta 各一次（子页切换不重复拉取） ──

  useEffect(() => {
    let alive = true;
    invoke<Settings>("get_settings")
      .then((s) => {
        if (!alive) return;
        settingsRef.current = s;
        setSettings(s);
      })
      .catch((e) => {
        if (!alive) return;
        console.error("[settings] get_settings 失败", e);
        setLoadFailed(true);
      });
    invoke<AppMeta>("get_app_meta")
      .then((m) => {
        if (alive) setAppMeta(m);
      })
      .catch((e) => {
        if (alive) console.error("[settings] get_app_meta 失败", e);
      });
    invoke<SkillEntry[]>("list_skills")
      .then((s) => {
        if (alive) setSkills(s);
      })
      .catch((e) => {
        if (alive) console.error("[settings] list_skills 失败", e);
      });
    return () => {
      alive = false;
    };
  }, []);

  // ── 即时保存单漏斗（busy 守卫单点收敛） ──
  // 乐观更新先落 UI → 执行保存（默认 set_settings patch；带窗口副作用的
  // 专用命令由调用方传 save 实参，如 set_always_on_top）→ 失败回滚 + 可见
  // toast（保存失败前端可见——设置页扩展定稿约束）。

  const commit = useCallback(
    (
      patch: SettingsPatch,
      label: string | undefined,
      save: (p: SettingsPatch) => Promise<unknown>,
      /** 纯窗口类改动（置顶）：不受“对话处理中”限制，与后端同一口径 */
      windowOnly = false
    ) => {
      // 即时查 busy（getState 快照，避免为一次性读订阅重渲染）
      if (!windowOnly && isProcessingBusy(useProcessingStore.getState())) {
        showToast(`对话处理中不能修改${label ?? "设置"}`, {
          kind: "warning",
          dedupeKey: `settings-busy-${label ?? ""}`,
        });
        return;
      }
      const prev = settingsRef.current;
      if (!prev) return;
      const next = mergeSettings(prev, patch);
      settingsRef.current = next;
      setSettings(next);
      save(patch).catch((err: unknown) => {
        // 回滚到 prev + 用户可见失败（后端已拒 / 写盘失败）
        settingsRef.current = prev;
        setSettings(prev);
        showToast(`保存失败（${label ?? "设置"}）：${String(err)}`, {
          kind: "warning",
        });
      });
    },
    []
  );

  const updateSettings = useCallback(
    (patch: SettingsPatch, label?: string) =>
      commit(patch, label, (p) => invoke("set_settings", { patch: p })),
    [commit]
  );

  /** 置顶开关：带窗口副作用的专用命令（应用窗口 + 落盘由后端原子完成）。
   *  纯窗口属性，与对话运行态无关——不受 busy 限制（windowOnly）。 */
  const handleSetAlwaysOnTop = useCallback(
    (enabled: boolean) =>
      commit(
        { window: { always_on_top: enabled } },
        "窗口置顶",
        (p) =>
          invoke("set_always_on_top", {
            enabled: p.window?.always_on_top ?? enabled,
          }),
        true
      ),
    [commit]
  );

  /** 空间页「选择目录」：busy 守卫 → 系统目录选择器 → 选定即存（patch） */
  const handleChooseWorkspace = useCallback(async () => {
    if (isProcessingBusy(useProcessingStore.getState())) {
      showToast("对话处理中不能修改路径", {
        kind: "warning",
        dedupeKey: "settings-busy-路径",
      });
      return;
    }
    const current = settingsRef.current?.workspace.workspace_root ?? "";
    try {
      const picked = await invoke<string | null>("choose_workspace_directory", {
        current: current || null,
      });
      if (picked) {
        // 选定即存：走漏斗（内部再 guard busy——选择器打开期间对话若开始，
        // 此处补拦 + 回滚显示）
        updateSettings(
          { workspace: { workspace_root: picked } },
          "路径"
        );
      }
    } catch (e) {
      showToast(`选择目录失败：${String(e)}`, { kind: "warning" });
    }
  }, [updateSettings]);

  /** 空间页「恢复默认」：busy 守卫 → 写空串（= 回到默认工作区） */
  const handleResetWorkspace = useCallback(() => {
    if (isProcessingBusy(useProcessingStore.getState())) {
      showToast("对话处理中不能修改路径", {
        kind: "warning",
        dedupeKey: "settings-busy-路径",
      });
      return;
    }
    updateSettings({ workspace: { workspace_root: "" } }, "路径");
  }, [updateSettings]);

  /** 扩展页「测试连接」：状态留在本层——子页切走会卸载，留在子页就丢了。
   *  计数而不是布尔：并发保护回「正在连接中」的那次没接手连接，
   *  不能在 finally 里把真正在跑的那次的「进行中」抹掉。 */
  const [maaInFlight, setMaaInFlight] = useState(0);
  const handleTestConnection = useCallback(async (): Promise<MaaStatus | null> => {
    setMaaInFlight((n) => n + 1);
    try {
      return await invoke<MaaStatus>("maa_test_connection");
    } catch (e) {
      const msg = String(e);
      if (msg.includes("正在连接中")) {
        showToast("上一次连接还在进行中", { kind: "info" });
      } else {
        showToast(`连接失败：${msg}`, { kind: "warning" });
      }
      return null;
    } finally {
      setMaaInFlight((n) => Math.max(0, n - 1));
    }
  }, []);

  /** 扩展页「选择」模拟器安装目录：busy 守卫 → 系统目录选择器 → 选定即存 */
  const handleChooseEmulatorDir = useCallback(async () => {
    if (isProcessingBusy(useProcessingStore.getState())) {
      showToast("对话处理中不能修改路径", {
        kind: "warning",
        dedupeKey: "settings-busy-路径",
      });
      return;
    }
    const current = settingsRef.current?.maa.emulator_path ?? "";
    try {
      const picked = await invoke<string | null>("pick_emulator_directory", {
        current: current || null,
      });
      if (picked) {
        updateSettings({ maa: { emulator_path: picked } }, "安装路径");
      }
    } catch (e) {
      showToast(`选择目录失败：${String(e)}`, { kind: "warning" });
    }
  }, [updateSettings]);

  /** 扩展页「选择」adb 可执行文件：同上，文件选择器 */
  const handleChooseAdbFile = useCallback(async () => {
    if (isProcessingBusy(useProcessingStore.getState())) {
      showToast("对话处理中不能修改路径", {
        kind: "warning",
        dedupeKey: "settings-busy-路径",
      });
      return;
    }
    try {
      const picked = await invoke<string | null>("pick_adb_file");
      if (picked) {
        updateSettings({ maa: { adb_path: picked } }, "ADB 路径");
      }
    } catch (e) {
      showToast(`选择文件失败：${String(e)}`, { kind: "warning" });
    }
  }, [updateSettings]);

  /** 技能页：重新拉取清单（安装 / 移除后刷新；挂载时已拉一次） */
  const refreshSkills = useCallback(() => {
    invoke<SkillEntry[]>("list_skills")
      .then(setSkills)
      .catch((e) => console.error("[settings] list_skills 失败", e));
  }, []);

  /** 技能页「安装」：选择器（文件 / 文件夹）→ install_skill → 刷新 + 可见反馈 */
  const handleInstallSkill = useCallback(
    async (kind: "file" | "folder") => {
      try {
        const picked = await invoke<string | null>(
          kind === "file" ? "pick_skill_file" : "pick_skill_folder"
        );
        if (!picked) return; // 用户取消
        const entry = await invoke<SkillEntry>("install_skill", {
          sourcePath: picked,
        });
        showToast(`已安装技能「${entry.name}」`, { kind: "info" });
        refreshSkills();
      } catch (e) {
        showToast(`安装失败：${String(e)}`, { kind: "warning" });
      }
    },
    [refreshSkills]
  );

  /** 技能页「移除」：行内两步确认后调用（内置技能不走到这里） */
  const handleRemoveSkill = useCallback(
    async (entry: SkillEntry) => {
      try {
        await invoke<string>("remove_skill", { filePath: entry.filePath });
        showToast(`已移除技能「${entry.name}」`, { kind: "info" });
        refreshSkills();
      } catch (e) {
        showToast(`移除失败：${String(e)}`, { kind: "warning" });
      }
    },
    [refreshSkills]
  );

  const switchTab = (key: SettingsTabKey) => {
    setActiveTab(key);
    // 记忆上次所在页：切页即写（打开时读初始值）
    useUIStore.getState().setSettingsInitialTab(key);
  };

  // ── 加载 / 失败态 ──

  if (loadFailed) {
    return (
      <div className={styles.loading}>
        <span className={styles.loadingText}>设置加载失败，请重启后重试</span>
      </div>
    );
  }
  if (!settings) {
    return (
      <div className={styles.loading}>
        <span className={styles.loadingText}>加载中…</span>
      </div>
    );
  }

  const activeLabel = TABS.find((t) => t.key === activeTab)?.label ?? "";

  return (
    <div className={styles.container} data-guide="settings-panel">
      {/* ── 左侧导航（9 项，全 2 字名；侧栏不扩张） ── */}
      <nav className={styles.sidebar}>
        <div className={styles.sidebarTitle}>设置</div>
        {TABS.map((tab) => (
          <div
            key={tab.key}
            data-tab={tab.key}
            className={`${styles.navItem} ${
              activeTab === tab.key ? styles.navItemActive : ""
            }`}
            onClick={() => switchTab(tab.key)}
          >
            {tab.label}
          </div>
        ))}
      </nav>

      {/* ── 右侧内容（active-only：仅当前页渲染） ── */}
      <div className={styles.content}>
        <div className={styles.contentTitle}>{activeLabel}</div>
        <div className={styles.contentBody}>
          {activeTab === "model" && (
            <SettingsModel
              llm={activeLlmConfig!}
              busy={settingsBusy}
              onPatch={(patch, label) => updateSettings({ llm: patch }, label)}
            />
          )}
          {activeTab === "safety" && (
            <SettingsSafety
              mode={settings.exec.mode}
              busy={settingsBusy}
              onModeChange={(mode: ExecutionMode) =>
                updateSettings({ exec: { mode } }, "命令确认")
              }
            />
          )}
          {activeTab === "space" && (
            <SettingsSpace
              workspaceRoot={
                settings.workspace.workspace_root ||
                appMeta?.defaultWorkspaceRoot ||
                ""
              }
              isDefault={!settings.workspace.workspace_root}
              busy={settingsBusy}
              onChooseWorkspace={handleChooseWorkspace}
              onResetWorkspace={handleResetWorkspace}
            />
          )}
          {activeTab === "skills" && (
            <SettingsSkills
              skills={skills}
              busy={settingsBusy}
              onInstallFile={() => handleInstallSkill("file")}
              onInstallFolder={() => handleInstallSkill("folder")}
              onRemove={handleRemoveSkill}
            />
          )}
          {activeTab === "memory" && <SettingsMemory busy={settingsBusy} />}
          {activeTab === "usage" && <SettingsUsage />}
          {activeTab === "extension" && (
            <SettingsExtension
              maa={settings.maa}
              busy={settingsBusy}
              testing={maaInFlight > 0}
              onTest={handleTestConnection}
              onPatch={(patch, label) => updateSettings({ maa: patch }, label)}
              onChooseEmulatorDir={handleChooseEmulatorDir}
              onChooseAdbFile={handleChooseAdbFile}
            />
          )}
          {activeTab === "app" && (
            <SettingsApp
              alwaysOnTop={settings.window.always_on_top}
              onSetAlwaysOnTop={handleSetAlwaysOnTop}
            />
          )}
          {activeTab === "about" && <SettingsAbout meta={appMeta} />}
        </div>
      </div>
    </div>
  );
}
