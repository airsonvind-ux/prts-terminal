// 设置页 · "空间"子页（工作空间：路径 / 选择目录 / 资源管理器 / 目录树）
//
// 2026-09 拆分：技能节迁往「技能」页——工作空间与技能是不同来源的对象，
// 且目录树需要整列宽度。
//
// 工作空间节：路径**只读显示**（未设置显示「未设置」）+ 「选择目录」按钮 →
// 系统目录选择器（choose_workspace_directory）→ 选定即存；hint 明示修改后
// 重启生效（后端装配期消费一次，无热切换）。
// 「在资源管理器中打开」：目标固定为工作区根（后端 open_workspace_in_explorer，
// 目录不能走 open_path——那里只放行文件）。
// 「目录结构」：工作区树（按需逐层加载）+ 按名搜索（WorkspaceTree）。

import { invoke } from "@tauri-apps/api/core";
import { showToast } from "../../stores/useToastStore";
import { SettingsSection, SectionHint } from "./ui/SettingsSection";
import { WorkspaceTree } from "./WorkspaceTree";
import styles from "./SettingsSpace.module.css";

export interface SettingsSpaceProps {
  /** 当前生效 workspace 根（空串 = 未设置，装配回退 data_dir） */
  workspaceRoot: string;
  /** 对话处理中：工作空间路径暂不可改（目录浏览不受限） */
  busy?: boolean;
  /** 「选择目录」按钮上抛（Panel：busy 守卫 → 目录选择器 → 选中 patch 保存） */
  onChooseWorkspace: () => void;
}

export function SettingsSpace({
  workspaceRoot,
  onChooseWorkspace,
  busy = false,
}: SettingsSpaceProps) {
  /** 在系统文件管理器打开工作区根（可见反馈契约：成败都有 toast） */
  const openInExplorer = () => {
    invoke("open_workspace_in_explorer")
      .then(() => {
        showToast("已在文件管理器打开工作目录", { kind: "info" });
      })
      .catch((e: unknown) => {
        showToast(`打开文件管理器失败：${String(e)}`, { kind: "warning" });
      });
  };

  return (
    <div className={styles.container}>
      <SettingsSection title="工作空间">
        {busy && (
          <SectionHint>对话处理中，工作空间路径暂不可改</SectionHint>
        )}
        {/* 多行排布：label / 路径行（路径 + 选择目录）/ 打开行 / hint */}
        <div className={styles.wsLabel}>路径</div>
        <div className={styles.wsPathRow}>
          <div
            className={workspaceRoot ? styles.wsValue : styles.wsValueEmpty}
            title={workspaceRoot || undefined}
          >
            {workspaceRoot || "未设置"}
          </div>
          <button
            className={styles.actionButton}
            onClick={onChooseWorkspace}
            disabled={busy}
            title="选择一个目录作为工作空间根"
          >
            选择目录
          </button>
        </div>
        <div className={styles.wsActions}>
          <button
            className={styles.actionButton}
            onClick={openInExplorer}
            disabled={!workspaceRoot}
            title="在系统资源管理器中打开工作目录"
            aria-label="在系统资源管理器中打开工作目录"
          >
            在资源管理器中打开
          </button>
        </div>
        <p className={styles.wsHint}>
          文件与命令操作的工作根目录，修改后重启生效
        </p>
      </SettingsSection>

      <SettingsSection title="目录结构">
        <WorkspaceTree root={workspaceRoot} />
      </SettingsSection>
    </div>
  );
}
