// 设置页 · "关于"子页（草案 v2 定稿形态）
//
// 极简：应用名 + 版本两行（version = get_app_meta：CARGO_PKG_VERSION）。
// 无节标题、无技术信息、无路径列表。

import type { AppMeta } from "../../types";
import styles from "./SettingsAbout.module.css";

export interface SettingsAboutProps {
  /** Panel 挂载时 get_app_meta 一次；null = 加载中/失败 */
  meta: AppMeta | null;
}

export function SettingsAbout({ meta }: SettingsAboutProps) {
  return (
    <div className={styles.container}>
      <span className={styles.name}>PRTS 终端助手</span>
      <span className={styles.version}>
        {meta ? `版本 ${meta.version}` : "版本加载中…"}
      </span>
    </div>
  );
}
