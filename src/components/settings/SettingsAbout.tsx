// 设置页 · "关于"子页
//
// 三段式：应用身份 / 更新控制台 / 随包资源。
// - 应用：图标、名称、版本居中，作为页首落款，不承载操作。
// - 更新：一条状态线 + 按阶段替换的动作；错误就地显示，不弹窗。
// - 资源：使用说明与许可证，低频入口后置。
//
// version 与 userGuidePath 都来自 get_app_meta（静态事实，面板挂载时拉一次）。
// 使用说明不做应用内阅读：交给系统的默认应用打开随包的 md。
// Action Dock 负责当下提示；这里只负责手动检查、下载、安装和失败兜底。

import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AppMeta } from "../../types";
import { showToast } from "../../stores/useToastStore";
import { useUpdaterStore } from "../../stores/useUpdaterStore";
import { SettingsSection, SectionHint } from "./ui/SettingsSection";
import prtsIcon from "../../assets/prts-icon.svg";
import styles from "./SettingsAbout.module.css";

export interface SettingsAboutProps {
  /** Panel 挂载时 get_app_meta 一次；null = 加载中/失败 */
  meta: AppMeta | null;
}

export function SettingsAbout({ meta }: SettingsAboutProps) {
  const updater = useUpdaterStore((s) => s.status);
  const updaterAction = useUpdaterStore((s) => s.action);
  const updaterError = useUpdaterStore((s) => s.actionError);
  const blockers = useUpdaterStore((s) => s.blockers);

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

  const openDownload = () => {
    const url = updater?.manualDownloadUrl ?? updater?.available?.downloadUrl;
    if (!url) return;
    void invoke("open_url", { url }).catch((e: unknown) =>
      showToast(`打开下载页失败：${String(e)}`, { kind: "warning" })
    );
  };

  const updateLine = (() => {
    if (!updater) return "更新状态加载中…";
    switch (updater.phase) {
      case "dev":
        return "开发模式不检查更新";
      case "checking":
        return "正在检查更新…";
      case "upToDate":
        return "已是最新版本";
      case "available":
        return `发现新版本 ${updater.available?.version ?? ""}`;
      case "downloading": {
        const pct =
          updater.downloadedBytes !== null &&
          updater.totalBytes !== null &&
          updater.totalBytes > 0
            ? Math.round(
                (updater.downloadedBytes / updater.totalBytes) * 100
              )
            : null;
        return pct === null ? "正在下载更新…" : `正在下载更新 ${pct}%`;
      }
      case "downloaded":
        return "更新已下载，重启后生效";
      case "installing":
        return "正在启动安装器…";
      case "error":
        return updater.errorStage === "check"
          ? "检查更新失败"
          : updater.errorStage === "download"
            ? "下载更新失败"
            : updater.errorStage === "install"
              ? "安装没有完成"
              : "更新失败";
      default:
        return "尚未检查更新";
    }
  })();

  const installBlocked =
    blockers?.activeTurn === true || blockers?.maaRunning === true;
  const blockerText = blockers?.activeTurn
    ? "有对话正在处理，暂时不能重启安装"
    : blockers?.maaRunning
      ? "MAA 任务正在运行，暂时不能重启安装"
      : "";
  const canCheck =
    updater !== null &&
    (updater.phase === "error" ||
      updater.phase === "idle" ||
      updater.phase === "upToDate");
  const manualUrl =
    updater?.manualDownloadUrl ?? updater?.available?.downloadUrl;
  const showManualDownload =
    manualUrl !== undefined &&
    updater?.phase !== "downloaded" &&
    updater?.phase !== "downloading" &&
    updater?.phase !== "installing";

  return (
    <div className={styles.container}>
      <SettingsSection ariaLabel="应用" className={styles.identitySection}>
        <div className={styles.identity}>
          <img className={styles.appIcon} src={prtsIcon} alt="" />
          <span className={styles.appName}>PRTS 终端助手</span>
          <span className={styles.appVersion}>
            {meta ? `版本 ${meta.version}` : "版本加载中…"}
          </span>
        </div>
      </SettingsSection>

      <SettingsSection title="更新">
        <div className={styles.updateStatus}>{updateLine}</div>
        {(updaterError || updater?.error) && (
          <div className={styles.updateError}>
            {updaterError ?? updater?.error}
          </div>
        )}
        <div className={styles.updateActions}>
          {updater?.phase === "available" && (
            <button
              className={`${styles.updateBtn} ${styles.updatePrimary}`}
              disabled={updaterAction !== null}
              onClick={() => void useUpdaterStore.getState().download()}
            >
              {updaterAction === "download" ? "下载中…" : "下载更新"}
            </button>
          )}
          {updater?.phase === "downloaded" && (
            <button
              className={`${styles.updateBtn} ${styles.updatePrimary}`}
              disabled={updaterAction !== null || installBlocked}
              title={blockerText || undefined}
              onClick={() => void useUpdaterStore.getState().install()}
            >
              {updaterAction === "install" ? "启动中…" : "重启并安装"}
            </button>
          )}
          {canCheck && (
            <button
              className={styles.updateBtn}
              disabled={updaterAction !== null}
              onClick={() => void useUpdaterStore.getState().check()}
            >
              {updater?.phase === "error" ? "重试检查" : "检查更新"}
            </button>
          )}
          {showManualDownload && (
            <button className={styles.updateBtn} onClick={openDownload}>
              手动下载
            </button>
          )}
        </div>
        {updater?.phase === "downloaded" && installBlocked && (
          <div className={styles.updateBlocker}>{blockerText}</div>
        )}
        {updater?.phase !== "dev" && (
          <SectionHint>启动时静默检查；下载与安装都由你确认。</SectionHint>
        )}
      </SettingsSection>

      <SettingsSection title="资源">
        <div className={styles.resourceRow}>
          <div className={styles.resourceText}>
            <span className={styles.resourceLabel}>使用说明</span>
            <span className={styles.resourceHint}>
              随包文档，用系统默认应用打开
            </span>
          </div>
          <button
            className={styles.resourceBtn}
            onClick={openUserGuide}
            disabled={!meta}
            title="用系统默认应用打开《使用说明》"
            aria-label="打开使用说明"
          >
            打开
          </button>
        </div>

        <div className={styles.resourceRow}>
          <div className={styles.resourceText}>
            <span className={styles.resourceLabel}>许可证</span>
            <span className={styles.resourceHint}>Apache License 2.0</span>
          </div>
          <button
            className={styles.resourceBtn}
            onClick={toggleLicense}
            aria-expanded={licenseOpen}
            aria-label={licenseOpen ? "收起许可证" : "查看许可证"}
          >
            {licenseOpen ? "收起" : "查看"}
          </button>
        </div>

        {licenseOpen &&
          (licenseError ? (
            <div className={styles.licenseError}>{licenseError}</div>
          ) : licenseText === null ? (
            <div className={styles.licenseLoading}>读取中…</div>
          ) : (
            <pre className={styles.licenseText}>{licenseText}</pre>
          ))}
      </SettingsSection>
    </div>
  );
}
