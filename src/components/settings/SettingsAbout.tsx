// 设置页 · "关于"子页（草案 v2 定稿形态）
//
// 极简：应用名 + 版本 + 使用说明入口。
// version 与 userGuidePath 都来自 get_app_meta（静态事实，面板挂载时拉一次）。
// 使用说明不做应用内阅读：交给系统的默认应用打开随包的 md。

import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AppMeta } from "../../types";
import { showToast } from "../../stores/useToastStore";
import styles from "./SettingsAbout.module.css";

export interface SettingsAboutProps {
  /** Panel 挂载时 get_app_meta 一次；null = 加载中/失败 */
  meta: AppMeta | null;
}

export function SettingsAbout({ meta }: SettingsAboutProps) {
  // ── 许可证全文：默认折叠；展开时才读随包文件，成功即缓存，失败可重试 ──
  const [licenseOpen, setLicenseOpen] = useState(false);
  const [licenseText, setLicenseText] = useState<string | null>(null);
  const [licenseError, setLicenseError] = useState("");

  const toggleLicense = () => {
    const next = !licenseOpen;
    setLicenseOpen(next);
    if (next && licenseText === null) {
      setLicenseError("");
      invoke<string>("read_license")
        .then((t) => setLicenseText(t))
        .catch((e: unknown) => setLicenseError(String(e)));
    }
  };

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

      {/* 许可证：末尾一个可展开块，默认折叠。正文来自随包 LICENSE——
          不在前端硬编码（Hanako 那份“许可证全文”实际是硬编码的短声明）。 */}
      <div className={styles.licenseBlock}>
        <button
          className={styles.licenseToggle}
          onClick={toggleLicense}
          aria-expanded={licenseOpen}
        >
          <span className={styles.licenseArrow} aria-hidden="true">
            {licenseOpen ? "▾" : "▸"}
          </span>
          许可证
        </button>
        {licenseOpen &&
          (licenseError ? (
            <span className={styles.licenseError}>{licenseError}</span>
          ) : licenseText === null ? (
            <span className={styles.licenseLoading}>读取中…</span>
          ) : (
            <pre className={styles.licenseText}>{licenseText}</pre>
          ))}
      </div>
    </div>
  );
}
