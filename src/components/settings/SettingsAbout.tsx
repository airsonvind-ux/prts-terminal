// 设置页 · "关于"子页（草案 v2 定稿形态）
//
// 极简：应用名 + 版本 + 使用说明入口。
// version 与 userGuidePath 都来自 get_app_meta（静态事实，面板挂载时拉一次）。
// 使用说明不做应用内阅读：交给系统的默认应用打开随包的 md。

import { invoke } from "@tauri-apps/api/core";
import type { AppMeta } from "../../types";
import { showToast } from "../../stores/useToastStore";
import styles from "./SettingsAbout.module.css";

export interface SettingsAboutProps {
  /** Panel 挂载时 get_app_meta 一次；null = 加载中/失败 */
  meta: AppMeta | null;
}

export function SettingsAbout({ meta }: SettingsAboutProps) {
  /** 用系统默认应用打开《使用说明》（可见反馈契约：成败都有 toast） */
  const openUserGuide = () => {
    if (!meta) return;
    invoke("open_path", { path: meta.userGuidePath })
      .then(() => {
        showToast("已用系统默认应用打开《使用说明》", { kind: "info" });
      })
      .catch((e: unknown) => {
        showToast(`打开《使用说明》失败：${String(e)}`, { kind: "warning" });
      });
  };

  return (
    <div className={styles.container}>
      <span className={styles.name}>PRTS 终端助手</span>
      <span className={styles.version}>
        {meta ? `版本 ${meta.version}` : "版本加载中…"}
      </span>
      <button
        className={styles.guideLink}
        onClick={openUserGuide}
        disabled={!meta}
        title="用系统默认应用打开《使用说明》"
        aria-label="用系统默认应用打开《使用说明》"
      >
        使用说明
      </button>
    </div>
  );
}
