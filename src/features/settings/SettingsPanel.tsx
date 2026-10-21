// 设置页主容器 · 子页路由（app/model/safety/space/skills/memory/about）
//              + 即时保存单漏斗
//
// 状态、挂载拉取与保存逻辑在 useSettingsPanel；本文件只保留 tab 路由与 JSX。

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
    updateSettings,
    handleSetAlwaysOnTop,
    handleChooseWorkspace,
    handleResetWorkspace,
    maaInFlight,
    handleTestConnection,
    handleChooseEmulatorDir,
    handleChooseAdbFile,
    handleInstallSkill,
    handleRemoveSkill,
    switchTab,
  } = useSettingsPanel();

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
    <div className={styles.container}>
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
        <div className={styles.contentBody} data-guide="settings-body">
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
