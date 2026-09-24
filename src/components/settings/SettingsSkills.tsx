// 设置页 · "技能"子页
//
// 由原「空间」页的技能节拆出（2026-09 空间页拆分）：工作空间与技能是两个不同
// 来源的对象（一个在用户磁盘上、一个随应用分发），同页并列没有共同上下文；
// 目录树还需要整列宽度。故「空间」管工作空间，「技能」管技能清单。
//
// 只读展示：名称 + 描述，点击打开源 md（FileLink：open_path + 可见 toast）。

import type { SkillEntry } from "../../types";
import { SettingsSection } from "./ui/SettingsSection";
import { FileLink } from "../ui/FileLink";
import styles from "./SettingsSkills.module.css";

export interface SettingsSkillsProps {
  /** 技能清单（Panel 挂载 list_skills 一次）；null = 加载中/失败 */
  skills: SkillEntry[] | null;
}

export function SettingsSkills({ skills }: SettingsSkillsProps) {
  return (
    <div className={styles.container}>
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
