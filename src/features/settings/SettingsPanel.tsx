// 设置页主容器 · 子页路由（app/model/safety/space/skills/memory/about）
//              + 即时保存单漏斗
//
// 状态、挂载拉取与保存逻辑在 useSettingsPanel；本文件只保留 tab 路由与 JSX。

import {
  useEffect,
  useRef,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { SettingsAbout } from "./SettingsAbout";
import { SettingsApp } from "./SettingsApp";
import { SettingsExtension } from "./SettingsExtension";
import { SettingsModel } from "./SettingsModel";
import { SettingsSafety } from "./SettingsSafety";
import { SettingsSkills } from "./SettingsSkills";
import { SettingsSpace } from "./SettingsSpace";
import { SettingsMemory } from "./SettingsMemory";
import { SettingsUsage } from "./SettingsUsage";
import { useSettingsPanel } from "./useSettingsPanel";
import { TABS } from "./settingsTabs";
import type { ExecutionMode } from "../../types";
import styles from "./SettingsPanel.module.css";

export { MASKED_SECRET, mergeSettings } from "./settingsMerge";

export function SettingsPanel() {
  const {
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
  } = useSettingsPanel();

  const activeTabRef = useRef<HTMLButtonElement>(null);

  // 覆盖层先渲染对话框根、设置数据随后到位：若焦点还停在根上，
  // 数据到位后把它交给当前页签，避免键盘用户面对一个没有可见焦点的面板。
  useEffect(() => {
    if (!settings) return;
    const dialog = document.querySelector<HTMLElement>('[role="dialog"]');
    if (dialog && document.activeElement === dialog) {
      activeTabRef.current?.focus();
    }
  }, [settings]);

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

  const handleTabKeyDown = (
    event: ReactKeyboardEvent<HTMLButtonElement>,
    index: number
  ) => {
    let nextIndex: number;
    if (event.key === "ArrowDown") {
      nextIndex = Math.min(TABS.length - 1, index + 1);
    } else if (event.key === "ArrowUp") {
      nextIndex = Math.max(0, index - 1);
    } else if (event.key === "Home") {
      nextIndex = 0;
    } else if (event.key === "End") {
      nextIndex = TABS.length - 1;
    } else {
      return;
    }

    event.preventDefault();
    const next = TABS[nextIndex];
    if (!next) return;
    const tabs = event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>(
      '[role="tab"]'
    );
    tabs?.[nextIndex]?.focus();
    switchTab(next.key);
  };

  return (
    <div className={styles.container}>
      {/* ── 左侧导航（9 项，全 2 字名；侧栏不扩张） ── */}
      <div className={styles.sidebar}>
        <div className={styles.sidebarTitle}>设置</div>
        <nav
          className={styles.tabList}
          role="tablist"
          aria-label="设置页签"
          aria-orientation="vertical"
        >
          {TABS.map((tab, index) => (
            <button
              key={tab.key}
              ref={activeTab === tab.key ? activeTabRef : undefined}
              type="button"
              id={`settings-tab-${tab.key}`}
              role="tab"
              data-focus-inset
              data-tab={tab.key}
              aria-selected={activeTab === tab.key}
              aria-controls="settings-panel"
              tabIndex={activeTab === tab.key ? 0 : -1}
              data-overlay-autofocus={
                activeTab === tab.key ? "" : undefined
              }
              className={`${styles.navItem} ${
                activeTab === tab.key ? styles.navItemActive : ""
              }`}
              onClick={() => switchTab(tab.key)}
              onKeyDown={(event) => handleTabKeyDown(event, index)}
            >
              {tab.label}
            </button>
          ))}
        </nav>
      </div>

      {/* ── 右侧内容（active-only：仅当前页渲染） ── */}
      <div className={styles.content}>
        <div className={styles.contentTitle}>{activeLabel}</div>
        <div
          className={styles.contentBody}
          data-guide="settings-body"
          id="settings-panel"
          role="tabpanel"
          aria-labelledby={`settings-tab-${activeTab}`}
          data-focus-inset
          tabIndex={0}
        >
          {activeTab === "model" && (
            <SettingsModel
              llm={activeLlmConfig!}
              busy={settingsBusy}
              onPatch={(patch, label) => updateSettings({ llm: patch }, label)}
              onConfigPatch={updateConversationConfig}
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
              pendingInstall={pendingSkillInstall?.preview ?? null}
              installing={installingSkill}
              onConfirmInstall={handleConfirmSkillInstall}
              onCancelInstall={handleCancelSkillInstall}
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
