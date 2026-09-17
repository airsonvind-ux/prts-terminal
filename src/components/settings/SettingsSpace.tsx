// 设置页 · "空间"子页（草案 v2 + 任务 B/C/D 定稿形态）
//
// 工作空间节：路径**只读显示**（未设置显示「未设置」）+ 「选择目录」按钮 →
// 系统目录选择器（choose_workspace_directory 后端命令 + dialog）→ 选定即存
// （onWorkspaceRootChange 上抛 → Panel 漏斗 patch workspace.workspace_root）。
// hint 明示修改后重启生效（后端装配期消费一次，无热切换）。
//
// 技能节：容器内行式只读列表（整体成块 + 行间分隔线 + 悬停高亮）；行可点击
// → FileLink 通用链接打开源 md（open_path 校验存在后放行）。

import type { SkillEntry } from "../../types";
import { SettingsSection } from "./ui/SettingsSection";
import { FileLink } from "../ui/FileLink";
import styles from "./SettingsSpace.module.css";

export interface SettingsSpaceProps {
  /** 当前生效 workspace 根（空串 = 未设置，装配回退 data_dir） */
  workspaceRoot: string;
  /** 技能清单（Panel 挂载 list_skills 一次）；null = 加载中/失败 */
  skills: SkillEntry[] | null;
  /** 「选择目录」按钮上抛（Panel：busy 守卫 → 目录选择器 → 选中 patch 保存） */
  onChooseWorkspace: () => void;
}

export function SettingsSpace({
  workspaceRoot,
  skills,
  onChooseWorkspace,
}: SettingsSpaceProps) {
  return (
    <div className={styles.container}>
      <SettingsSection title="工作空间">
        {/* 多行排布：label / 路径全文独立行（长路径可换行不挤压）/ 按钮行 / hint */}
        <div className={styles.wsLabel}>路径</div>
        <div
          className={workspaceRoot ? styles.wsValue : styles.wsValueEmpty}
          title={workspaceRoot || undefined}
        >
          {workspaceRoot || "未设置"}
        </div>
        <div className={styles.wsActions}>
          <button className={styles.pickButton} onClick={onChooseWorkspace}>
            选择目录
          </button>
        </div>
        <p className={styles.wsHint}>
          文件与命令操作的工作根目录，修改后重启生效
        </p>
      </SettingsSection>

      <SettingsSection title="技能">
        {skills === null ? (
          <span className={styles.emptyHint}>技能清单加载中…</span>
        ) : skills.length === 0 ? (
          <span className={styles.emptyHint}>无技能</span>
        ) : (
          <div className={styles.skillList}>
            {skills.map((s) => (
              <FileLink
                key={s.name}
                path={s.filePath}
                className={styles.skillRow}
                successHint={`已打开「${s.name}」`}
                title={s.description}
              >
                <span className={styles.skillName}>{s.name}</span>
                <span className={styles.skillDesc}>{s.description}</span>
              </FileLink>
            ))}
          </div>
        )}
      </SettingsSection>
    </div>
  );
}
