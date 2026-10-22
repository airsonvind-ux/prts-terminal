// 设置页主容器的状态、挂载拉取与即时保存漏斗。
//
// 组件只保留 tab 路由与 JSX；这里集中处理：
//   1. get_settings / get_app_meta / list_skills 一次拉取
//   2. updateSettings 单漏斗：busy 守卫 → 乐观合并 → invoke → 失败回滚 + toast
//   3. 工作区、技能、MAA 等页面的操作入口
//   4. 上次所在页的读写与引导通知

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  ApiType,
  AppMeta,
  MaaStatus,
  Settings,
  SettingsPatch,
  SkillEntry,
  SkillPreview,
} from "../../types";
import {
  isProcessingBusy,
  useProcessingStore,
} from "../../stores/useProcessingStore";
import { showToast } from "../../stores/useToastStore";
import { useUIStore } from "../../stores/useUIStore";
import { useGuideStore } from "../../stores/useGuideStore";
import { SETTINGS_TAB_GUIDE } from "../guide/guideContent";
import { mergeSettings } from "./settingsMerge";
import type { SettingsTabKey } from "./settingsTabs";

export function useSettingsPanel() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  // 权威引用：乐观合并 / 失败回滚的基准（无卸载批保存——ref 不再承担保存职责）
  const settingsRef = useRef<Settings | null>(null);
  // 应用只读元信息（关于页展示；空间页批 C2 复用同一份 props）
  const [appMeta, setAppMeta] = useState<AppMeta | null>(null);
  // 技能清单（技能页；null = 加载中/失败）
  const [skills, setSkills] = useState<SkillEntry[] | null>(null);
  // 待确认的含脚本技能（选择器已选、预览已有脚本清单；null = 无待确认）
  const [pendingSkillInstall, setPendingSkillInstall] = useState<{
    sourcePath: string;
    preview: SkillPreview;
  } | null>(null);
  const [installingSkill, setInstallingSkill] = useState(false);
  const [activeTab, setActiveTab] = useState<SettingsTabKey>(
    () => useUIStore.getState().settingsInitialTab
  );
  // busy 订阅（M3-B 全局锁口径）：驱动漏斗守卫 + 模型页控件禁用
  const settingsBusy = useProcessingStore(isProcessingBusy);
  // 引导键集是否已从后端拉到（未拉到不弹——宁可不弹，也不误弹）
  const guideHydrated = useGuideStore((s) => s.hydrated);

  // 引导：进入某页且该页说明没看过 → 出通知卡（每页一次；切页换掉旧的那张）。
  // 必须等面板就绪（settings 到位、容器 data-guide 已渲染）。
  const panelReady = settings !== null;
  useEffect(() => {
    if (!guideHydrated || !panelReady) return;
    const key = SETTINGS_TAB_GUIDE[activeTab];
    if (key) useGuideStore.getState().enqueueNotice(key);
  }, [activeTab, guideHydrated, panelReady]);

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

  // 挂载：get_settings + get_app_meta + list_skills 各一次（子页切换不重复拉取）
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
        updateSettings({ workspace: { workspace_root: picked } }, "路径");
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

  /** 技能页「安装」：选择器 → 只读预览 → 无脚本直接装 / 有脚本先确认 */
  const handleInstallSkill = useCallback(
    async (kind: "file" | "folder") => {
      try {
        const picked = await invoke<string | null>(
          kind === "file" ? "pick_skill_file" : "pick_skill_folder"
        );
        if (!picked) return; // 用户取消
        setPendingSkillInstall(null); // 新选择覆盖旧的待确认安装
        const preview = await invoke<SkillPreview>("preview_skill_source", {
          sourcePath: picked,
        });
        if (preview.scripts.length > 0) {
          setPendingSkillInstall({ sourcePath: picked, preview });
          return;
        }
        const entry = await invoke<SkillEntry>("install_skill", {
          sourcePath: picked,
          confirmScripts: false,
        });
        showToast(`已安装技能「${entry.name}」`, { kind: "info" });
        refreshSkills();
      } catch (e) {
        showToast(`安装失败：${String(e)}`, { kind: "warning" });
      }
    },
    [refreshSkills]
  );

  /** 含脚本技能：用户已在警告块确认，带 confirmScripts=true 重试安装 */
  const handleConfirmSkillInstall = useCallback(async () => {
    if (!pendingSkillInstall || installingSkill) return;
    setInstallingSkill(true);
    try {
      const entry = await invoke<SkillEntry>("install_skill", {
        sourcePath: pendingSkillInstall.sourcePath,
        confirmScripts: true,
      });
      setPendingSkillInstall(null);
      showToast(`已安装技能「${entry.name}」`, { kind: "info" });
      refreshSkills();
    } catch (e) {
      showToast(`安装失败：${String(e)}`, { kind: "warning" });
    } finally {
      setInstallingSkill(false);
    }
  }, [installingSkill, pendingSkillInstall, refreshSkills]);

  const handleCancelSkillInstall = useCallback(() => {
    if (!installingSkill) setPendingSkillInstall(null);
  }, [installingSkill]);

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

  return {
    settings,
    loadFailed,
    appMeta,
    skills,
    activeTab,
    settingsBusy,
    activeLlmConfig,
    updateSettings,
    handleSetAlwaysOnTop,
    handleChooseWorkspace,
    handleResetWorkspace,
    maaInFlight,
    handleTestConnection,
    handleChooseEmulatorDir,
    handleChooseAdbFile,
    handleInstallSkill,
    pendingSkillInstall,
    installingSkill,
    handleConfirmSkillInstall,
    handleCancelSkillInstall,
    handleRemoveSkill,
    switchTab,
  };
}
