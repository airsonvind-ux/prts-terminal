// src/components/content/AttachmentStrip.tsx
// 附件条 —— 输入区 / 消息区共用的横向附件展示。
//
// 形态（PRTS 小窗：限高一行、不换行、超出横向滚动）：
//   - 图片：小方缩略图（asset 协议读本地文件），角上删除标（仅输入区）
//   - 其他：窄文件名 chip（图标 + 截断名）
// 名字一律进 tooltip（空间紧）。
//
// 交互：**点击 chip 打开文件**（invoke open_path，系统默认程序）。
// 缩略图经 Tauri asset 协议读会话落点（scope 见 tauri.conf.json
// `security.assetProtocol`）；读取失败退化图标，不报错。

import { useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { showToast } from "../../stores/useToastStore";
import type { Attachment } from "../../types";
import styles from "./AttachmentStrip.module.css";

/** 把本地路径转为 asset URL（Tauri 之外的环境返回 null，退化图标）。 */
function assetUrl(path: string): string | null {
  try {
    return convertFileSrc(path);
  } catch {
    return null;
  }
}

/** 点击附件 → 用系统默认程序打开（失败 toast）。 */
function openAttachment(file: Attachment): void {
  void invoke("open_path", { path: file.path }).catch((err) =>
    showToast(String(err), { kind: "warning", dedupeKey: "open-attachment-failed" })
  );
}

function ClipIcon() {
  return (
    <svg
      width="12"
      height="12"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
    </svg>
  );
}

function RemoveButton({
  name,
  onRemove,
}: {
  name: string;
  onRemove: () => void;
}) {
  return (
    <button
      type="button"
      className={styles.remove}
      onClick={(e) => {
        // 阻止冒泡：移除不等于打开
        e.stopPropagation();
        onRemove();
      }}
      aria-label={`移除附件 ${name}`}
      title="移除"
    >
      <svg width="8" height="8" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round">
        <line x1="18" y1="6" x2="6" y2="18" />
        <line x1="6" y1="6" x2="18" y2="18" />
      </svg>
    </button>
  );
}

function AttachmentChip({
  file,
  onRemove,
}: {
  file: Attachment;
  onRemove?: () => void;
}) {
  const [failed, setFailed] = useState(false);
  const isImage = file.mimeType.startsWith("image/");
  const src = isImage && !failed ? assetUrl(file.path) : null;

  const openProps = {
    role: "button" as const,
    tabIndex: 0,
    title: file.name,
    onClick: () => openAttachment(file),
    onKeyDown: (e: React.KeyboardEvent) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        openAttachment(file);
      }
    },
  };

  if (src) {
    return (
      <span className={`${styles.imageChip} ${styles.clickable}`} {...openProps}>
        {/* 双层：底层整幅模糊（只在外缘带露出），上层锐利按外缘渐隐 */}
        <span className={styles.imageMedia}>
          <img
            className={styles.imageBlur}
            src={src}
            alt=""
            aria-hidden="true"
            onError={() => setFailed(true)}
          />
          <img
            className={styles.imageSharp}
            src={src}
            alt={file.name}
            onError={() => setFailed(true)}
          />
        </span>
        {onRemove && <RemoveButton name={file.name} onRemove={onRemove} />}
      </span>
    );
  }
  return (
    <span className={`${styles.fileChip} ${styles.clickable}`} {...openProps}>
      <span className={styles.icon}>
        <ClipIcon />
      </span>
      <span className={styles.fileName}>{file.name}</span>
      {onRemove && <RemoveButton name={file.name} onRemove={onRemove} />}
    </span>
  );
}

export function AttachmentStrip({
  files,
  onRemove,
  variant = "input",
}: {
  files: Attachment[];
  /** 传入 = 每个 chip 带删除标（输入区）；不传 = 只读展示（消息区） */
  onRemove?: (index: number) => void;
  variant?: "input" | "message";
}) {
  if (files.length === 0) return null;
  return (
    <div
      className={`${styles.strip} ${variant === "message" ? styles.message : styles.input}`}
    >
      {files.map((f, i) => (
        <AttachmentChip
          key={`${f.path}#${i}`}
          file={f}
          onRemove={onRemove ? () => onRemove(i) : undefined}
        />
      ))}
    </div>
  );
}
