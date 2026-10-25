// 设置页主容器的状态、挂载拉取与即时保存漏斗。
//
// 组件只保留 tab 路由与 JSX；这里集中处理：
//   1. get_settings / get_app_meta / list_skills 一次拉取
//   2. updateSettings 单漏斗：busy 守卫 → 乐观合并 → 串行保存 → 失败摘除 + toast
//   3. 工作区、技能、MAA 等页面的操作入口
//   4. 上次所在页的读写与引导通知

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  ApiType,
  AppMeta,
  ConversationConfigPatch,
  ConversationModelConfig,
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
import { useConversationStore } from "../../stores/useConversationStore";
import { SETTINGS_TAB_GUIDE } from "../guide/guideContent";
import { mergeSettings } from "./settingsMerge";
import type { SettingsTabKey } from "./settingsTabs";

interface PendingSettingsSave {
  patch: SettingsPatch;
  label: string | undefined;
  save: (p: SettingsPatch) => Promise<unknown>;
}

export function useSettingsPanel() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);
  // 当前 UI 的乐观视图；由 confirmed + 待提交队列重算，不直接作为回滚基准。
  const settingsRef = useRef<Settings | null>(null);
  // 最后一次确认已落盘的设置；失败时从这里重算，避免旧快照覆盖后一次成功。
  const confirmedRef = useRef<Settings | null>(null);
  // 已乐观更新、等待串行提交的 patch 队列。
  const pendingSavesRef = useRef<PendingSettingsSave[]>([]);
  const flushingSavesRef = useRef(false);
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
  // 当前对话 / 新对话草稿：设置页模型配置的唯一归属
  const activeConv = useConversationStore((s) =>
    s.activeId ? s.conversations.find((c) => c.id === s.activeId) : undefined
  );
  const draftConfig = useConversationStore((s) => s.draftConfig);
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

  // 设置页模型配置的三态：当前对话 → 新对话草稿 → 首次启动种子。
  // 供应商凭据/BaseURL 仍从全局 providers 取；模型身份不读全局 active。
  const effectiveModelConfig = useMemo<ConversationModelConfig | null>(() => {
    if (!settings) return null;
    if (activeConv?.provider && activeConv.model) {
      return {
        provider: activeConv.provider,
        model: activeConv.model,
        thinkingLevel: activeConv.thinkingLevel ?? null,
      };
    }
    if (draftConfig?.provider && draftConfig.model) return draftConfig;
    const seedProvider = settings.llm.active_provider;
    const seedPc = settings.llm.providers[seedProvider];
    if (seedProvider && seedPc?.model) {
      return {
        provider: seedProvider,
        model: seedPc.model,
        thinkingLevel: settings.llm.thinking_level ?? null,
      };
    }
    return null;
  }, [settings, activeConv?.provider, activeConv?.model, activeConv?.thinkingLevel, draftConfig]);

  // 模型页受控视角：会话身份 + 该供应商的全局连接配置
  const activeLlmConfig = useMemo(() => {
    if (!settings) return null;
    const provider =
      effectiveModelConfig?.provider ?? settings.llm.active_provider;
    const pc = settings.llm.providers[provider];
    return {
      provider,
      api_type: pc?.api_type ?? ("openai-completions" as ApiType),
      model: effectiveModelConfig?.model ?? pc?.model ?? "",
      api_key: pc?.api_key ?? "",
      base_url: pc?.base_url ?? null,
      headers: pc?.headers ?? null,
      max_tokens: pc?.max_tokens ?? 4096,
      thinking_level: effectiveModelConfig?.thinkingLevel ?? null,
    } as Settings["llm"]["providers"][string] & {
      provider: string;
      thinking_level: string | null;
    };
  }, [settings, effectiveModelConfig]);

  /** 模型显示名/窗口变化后补写会话展示字段（请求身份已先落）。 */
  const refreshConversationModelDisplay = useCallback(
    (conversationId: string, provider: string, model: string) => {
      invoke<{
        displayName: string;
        contextLength: number;
      } | null>("get_local_model_info", { provider, modelId: model })
        .then((info) => {
          useConversationStore
            .getState()
            .updateConversationModel(
              conversationId,
              info?.displayName ?? model,
              info?.contextLength ?? 0
            );
        })
        .catch(() => {
          useConversationStore
            .getState()
            .updateConversationModel(conversationId, model, 0);
        });
    },
    []
  );

  /** 设置页写模型身份：已有对话落会话；新对话落草稿（首条消息固化）。 */
  const updateConversationConfig = useCallback(
    (patch: ConversationConfigPatch) => {
      const currentSettings = settingsRef.current;
      if (!currentSettings) return;
      const seedProvider = currentSettings.llm.active_provider;
      const seedPc = currentSettings.llm.providers[seedProvider];
      const base: ConversationModelConfig = effectiveModelConfig ?? {
        provider: seedProvider,
        model: seedPc?.model ?? "",
        thinkingLevel: currentSettings.llm.thinking_level ?? null,
      };
      const provider = patch.provider ?? base.provider;
      const providerChanged = provider !== base.provider;
      const savedModel =
        settingsRef.current?.llm.providers[provider]?.model ?? "";
      const model =
        patch.model ??
        (providerChanged ? savedModel : base.model);
      const next: ConversationModelConfig = {
        provider,
        model,
        thinkingLevel:
          patch.thinkingLevel !== undefined
            ? patch.thinkingLevel
            : providerChanged
              ? null
              : base.thinkingLevel,
      };
      const store = useConversationStore.getState();
      const currentId = store.activeId;
      if (currentId) {
        // 窗口变小：先把当前用量与目标窗口比一次；超出则要求先压缩，
        // 不静默截断，也不等下一轮请求失败。
        const currentConv = store.conversations.find((c) => c.id === currentId);
        const used =
          currentConv?.contextTokens?.contextInputTokens ??
          currentConv?.contextTokens?.inputTokens ??
          0;
        if (
          patch.contextLength &&
          patch.contextLength > 0 &&
          used > patch.contextLength
        ) {
          showToast("当前上下文超过新模型窗口，请先压缩后再切换", {
            kind: "warning",
            dedupeKey: "model-window-too-small",
          });
          return;
        }
        store.updateConversationConfig(
          currentId,
          next,
          patch.modelName,
          patch.contextLength
        );
        if (
          model.length > 0 &&
          !patch.modelName &&
          (providerChanged || model !== base.model)
        ) {
          refreshConversationModelDisplay(currentId, provider, model);
        }
        // 持久化：首轮尚未落盘时会失败（会话文件未建），随 append_turn 的
        // metaSnapshot 补上；失败静默，不阻断设置页。
        invoke("update_conversation_meta", {
          conversationId: currentId,
          provider: next.provider,
          model: next.model,
          thinkingLevel: next.thinkingLevel ?? "",
        }).catch(() => {});
      } else {
        store.setDraftConfig(next);
      }
    },
    [effectiveModelConfig, refreshConversationModelDisplay]
  );

  // 挂载：get_settings + get_app_meta + list_skills 各一次（子页切换不重复拉取）
  useEffect(() => {
    let alive = true;
    invoke<Settings>("get_settings")
      .then((s) => {
        if (!alive) return;
        confirmedRef.current = s;
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
  // 乐观更新先落 UI → 串行提交（默认 set_settings patch；带窗口副作用的
  // 专用命令由调用方传 save 实参，如 set_always_on_top）→ 失败只摘除该 patch
  // 并重算 UI，避免晚到的失败回滚覆盖后一次成功。
  const recomputeOptimistic = useCallback(() => {
    const base = confirmedRef.current;
    if (!base) return;
    let next = base;
    for (const item of pendingSavesRef.current) {
      next = mergeSettings(next, item.patch);
    }
    settingsRef.current = next;
    setSettings(next);
  }, []);

  const flushSaves = useCallback(async () => {
    if (flushingSavesRef.current) return;
    flushingSavesRef.current = true;
    try {
      while (pendingSavesRef.current.length > 0) {
        const item = pendingSavesRef.current[0];
        try {
          await item.save(item.patch);
          const base = confirmedRef.current;
          if (base) confirmedRef.current = mergeSettings(base, item.patch);
        } catch (err: unknown) {
          showToast(`保存失败（${item.label ?? "设置"}）：${String(err)}`, {
            kind: "warning",
          });
        } finally {
          // 无论成功或失败都只摘除当前项；后提交的 patch 仍在队列中，
          // 重算后会保留它们，不会退回更早的整份快照。
          pendingSavesRef.current = pendingSavesRef.current.slice(1);
          recomputeOptimistic();
        }
      }
    } finally {
      flushingSavesRef.current = false;
    }
  }, [recomputeOptimistic]);

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
      if (!settingsRef.current || !confirmedRef.current) return;
      pendingSavesRef.current.push({ patch, label, save });
      recomputeOptimistic();
      void flushSaves();
    },
    [flushSaves, recomputeOptimistic]
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
    updateConversationConfig,
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
