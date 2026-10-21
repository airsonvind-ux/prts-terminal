// 通用「文件 / 链接点击打开」组件
//
// 复用面：设置页技能行点击打开源 md；工具卡片文件链接（读/写/编辑工具的结果，
// 知识库条目 md、Skill 源 md）与 web_fetch 的 URL 链接。后端负责给出目标
// 绝对路径 / URL（前端不拼路径）。
//
// 可见反馈契约：点击**必有可见结果**——成功弹 info toast（系统应用窗口 / 浏览器
// + 提示，防 opener 后台静默）；失败弹 warning toast（含后端错误文案），console
// 留痕；不允许静默无反应。path 空 = 不可点。
//
// 命令契约：kind=file → invoke open_path（后端校验存在，无白名单）；
//           kind=url  → invoke open_url（仅 http/https，系统浏览器打开）。

import { useState, type CSSProperties, type KeyboardEvent, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { showToast } from "../../stores/useToastStore";
import styles from "./FileLink.module.css";

export interface FileLinkProps {
  /** 目标：kind=file 为文件绝对路径；kind=url 为完整 URL（空 = 不可点） */
  path?: string | null;
  /** 链接类型（默认 file）：file → open_path；url → open_url（系统浏览器） */
  linkKind?: "file" | "url";
  /** 打开成功的提示文案（默认 file「已打开文件」/ url「已在浏览器打开」） */
  successHint?: string;
  className?: string;
  /** 盒布局由调用方供给时用（如工作区树行缩进） */
  style?: CSSProperties;
  title?: string;
  children: ReactNode;
}

export function FileLink({
  path,
  linkKind = "file",
  successHint,
  className,
  style,
  title,
  children,
}: FileLinkProps) {
  const [opening, setOpening] = useState(false);
  const clickable = Boolean(path) && !opening;
  const isUrl = linkKind === "url";

  const open = () => {
    if (!clickable || !path) return;
    setOpening(true);
    const command = isUrl ? "open_url" : "open_path";
    invoke(command, isUrl ? { url: path } : { path })
      .then(() => {
        // 打开成功：系统应用窗口 / 浏览器 + 轻提示（若系统静默则 toast 兜底可见）
        const hint = successHint ?? (isUrl ? "已在浏览器打开" : "已打开文件");
        showToast(`${hint}（${path}）`, { kind: "info" });
      })
      .catch((e: unknown) => {
        const msg = String(e);
        console.error(`[FileLink] ${command} 失败:`, msg, path);
        showToast(
          isUrl
            ? `无法打开链接：${msg}`
            : `无法打开文件：${msg}。若未弹出应用，请在系统中为 .md 设置默认打开方式`,
          { kind: "warning" }
        );
      })
      .finally(() => setOpening(false));
  };

  const onKeyDown = (e: KeyboardEvent<HTMLSpanElement>) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      open();
    }
  };

  return (
    <span
      role={clickable ? "button" : undefined}
      tabIndex={clickable ? 0 : undefined}
      className={[
        clickable ? styles.linkClickable : "",
        opening ? styles.linkOpening : "",
        className ?? "",
      ]
        .filter(Boolean)
        .join(" ")}
      title={title}
      style={style}
      onClick={open}
      onKeyDown={onKeyDown}
    >
      {children}
    </span>
  );
}
