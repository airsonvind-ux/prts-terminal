// 设置页 · "技能"子页
//
// 两个节：「安装技能」（支持形态与生效时机的说明 + 两枚安装入口）、
// 「已安装技能」（内置 / 用户差异说明 + 清单）。
// 清单主区经 FileLink 打开源 md（open_path + 成败 toast）；
// 移除只对用户技能开放，行内两步确认避免静默删除。

import { useState } from "react";
import type { SkillEntry } from "../../types";
import { SettingsSection, SectionWarning } from "./ui/SettingsSection";
import { FileLink } from "../ui/FileLink";
import styles from "./SettingsSkills.module.css";

export interface SettingsSkillsProps {
  /** 技能清单（Panel 挂载时拉取一次，安装/移除后刷新）；null = 加载中/失败 */
  skills: SkillEntry[] | null;
  /** 对话处理中：仍可安装（本轮技能清单已冻结，改动从下一轮生效） */
  busy?: boolean;
  /** 安装 .md 文件（Panel 负责选择器、命令与刷新） */
  onInstallFile: () => void;
  /** 安装技能文件夹（根级含 SKILL.md；Panel 负责选择器、命令与刷新） */
  onInstallFolder: () => void;
  /** 移除用户技能（Panel 负责命令与刷新；此处只做行内确认） */
  onRemove: (entry: SkillEntry) => void;
}

export function SettingsSkills({
  skills,
  busy = false,
  onInstallFile,
  onInstallFolder,
  onRemove,
}: SettingsSkillsProps) {
  // 待确认移除的技能名（null = 无在确认态）
  const [armed, setArmed] = useState<string | null>(null);

  return (
    <div className={styles.container}>
      <SettingsSection title="安装技能">
        {busy && (
          <SectionWarning>
            对话处理中仍可安装；技能改动从下一轮生效，当前对话不受影响
          </SectionWarning>
        )}
        <p className={styles.sectionHint}>
          支持 .md 文件，或包含 SKILL.md 的技能文件夹；安装后即可使用
        </p>
        <div className={styles.actions}>
          <button className={styles.actionButton} onClick={onInstallFile}>
            安装 .md 文件
          </button>
          <button className={styles.actionButton} onClick={onInstallFolder}>
            安装技能文件夹
          </button>
        </div>
      </SettingsSection>

      <SettingsSection
        title={skills ? `已安装技能（${skills.length}）` : "已安装技能"}
      >
        <p className={styles.sectionHint}>
          内置技能由应用提供、不可移除；自己安装的技能可移除，不影响原始文件
        </p>
        {skills === null ? (
          <span className={styles.emptyHint}>技能清单加载中…</span>
        ) : skills.length === 0 ? (
          <span className={styles.emptyHint}>无技能</span>
        ) : (
          <div className={styles.skillList}>
            {skills.map((s) => {
              const isBuiltin = s.source === "builtin";
              return (
                <div key={s.name} className={styles.skillRow}>
                  <FileLink
                    path={s.filePath}
                    className={styles.skillMain}
                    successHint={`已打开「${s.name}」`}
                    title={s.description}
                  >
                    <span className={styles.skillName}>{s.name}</span>
                    <span className={styles.skillDesc}>{s.description}</span>
                  </FileLink>
                  <div className={styles.skillSide}>
                    <span className={styles.sourceTag}>
                      {isBuiltin ? "内置" : "用户"}
                    </span>
                    {!isBuiltin &&
                      (armed === s.name ? (
                        <>
                          <button
                            className={styles.actionButton}
                            onClick={() => {
                              setArmed(null);
                              onRemove(s);
                            }}
                          >
                            确认移除
                          </button>
                          <button
                            className={styles.actionButton}
                            onClick={() => setArmed(null)}
                          >
                            取消
                          </button>
                        </>
                      ) : (
                        <button
                          className={styles.actionButton}
                          onClick={() => setArmed(s.name)}
                        >
                          移除
                        </button>
                      ))}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </SettingsSection>
    </div>
  );
}
