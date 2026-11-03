// 设置页 · "技能"子页
//
// 两个节：「安装技能」（支持形态与生效时机的说明 + 两枚安装入口）、
// 「已安装技能」（内置 / 用户差异说明 + 清单）。
// 清单与扩展页同构：ListRow 一行一个技能，主行可点开源 md（FileLink 负责
// open_path 与成败 toast），右侧槽位是来源标签与两步行内确认的移除按钮。

import { useState } from "react";
import type { SkillEntry, SkillPreview } from "../../types";
import {
  SettingsSection,
  SectionHint,
  SectionWarning,
} from "./ui/SettingsSection";
import { ListRow } from "./ui/ListRow";
import { Button } from "./ui/Button";
import { FileLink } from "../../shared/ui/FileLink";
import styles from "./SettingsSkills.module.css";

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export interface SettingsSkillsProps {
  /** 技能清单（Panel 挂载时拉取一次，安装/移除后刷新）；null = 加载中/失败 */
  skills: SkillEntry[] | null;
  /** 对话处理中：仍可安装（本轮技能清单已冻结，改动从下一轮生效） */
  busy?: boolean;
  /** 安装 .md 文件（Panel 负责选择器、命令与刷新） */
  onInstallFile: () => void;
  /** 安装技能文件夹（根级含 SKILL.md；Panel 负责选择器、命令与刷新） */
  onInstallFolder: () => void;
  /** 已预览到脚本、等待用户确认的安装（null = 无待确认） */
  pendingInstall: SkillPreview | null;
  /** 正在提交确认安装 */
  installing: boolean;
  onConfirmInstall: () => void;
  onCancelInstall: () => void;
  /** 移除用户技能（Panel 负责命令与刷新；此处只做行内确认） */
  onRemove: (entry: SkillEntry) => void;
}

export function SettingsSkills({
  skills,
  busy = false,
  onInstallFile,
  onInstallFolder,
  pendingInstall,
  installing,
  onConfirmInstall,
  onCancelInstall,
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
        <SectionHint>
          支持 .md 文件，或包含 SKILL.md 的技能文件夹；安装后即可使用
        </SectionHint>
        <div className={styles.actions}>
          <Button onClick={onInstallFile}>安装 .md 文件</Button>
          <Button onClick={onInstallFolder}>安装技能文件夹</Button>
        </div>
        {pendingInstall && (
          <SectionWarning>
            <div className={styles.warningTitle}>
              这个技能包包含可执行/脚本文件
            </div>
            <div className={styles.warningText}>
              安装后仍可使用；执行包内脚本时，即使处于自动化模式也会先弹出确认。
            </div>
            <div className={styles.scriptList}>
              {pendingInstall.scripts.map((file) => (
                <div key={file.path} className={styles.scriptRow}>
                  <span className={styles.scriptPath} title={file.path}>
                    {file.path}
                  </span>
                  <span className={styles.scriptSize}>
                    {formatBytes(file.sizeBytes)}
                  </span>
                </div>
              ))}
            </div>
            <div className={styles.actions}>
              <Button disabled={installing} onClick={onCancelInstall}>
                取消
              </Button>
              <Button
                variant="primary"
                disabled={installing}
                onClick={onConfirmInstall}
              >
                {installing ? "安装中…" : "仍要安装"}
              </Button>
            </div>
          </SectionWarning>
        )}
      </SettingsSection>

      <SettingsSection
        title={skills ? `已安装技能（${skills.length}）` : "已安装技能"}
      >
        <SectionHint>
          内置技能由应用提供、不可移除；自己安装的技能可移除，不影响原始文件
        </SectionHint>
        {skills === null ? (
          <span className={styles.emptyHint}>技能清单加载中…</span>
        ) : skills.length === 0 ? (
          <span className={styles.emptyHint}>无技能</span>
        ) : (
          <div className={styles.skillList}>
            {skills.map((s) => {
              const isBuiltin = s.source === "builtin";
              const sourceLabel = isBuiltin
                ? "内置"
                : s.trust === "untrusted"
                  ? "需确认"
                  : "用户";
              return (
                <ListRow
                  key={s.name}
                  hoverable
                  title={
                    <FileLink
                      className={styles.skillLink}
                      path={s.filePath}
                      successHint={`已打开「${s.name}」`}
                      title={s.description}
                    >
                      <span className={styles.skillName}>{s.name}</span>
                    </FileLink>
                  }
                  subtitle={s.description}
                  titleAttr={s.description}
                  trailing={
                    <>
                      <span
                        className={styles.sourceTag}
                        title={
                          s.trust === "untrusted"
                            ? "执行包内脚本时会再次确认"
                            : undefined
                        }
                      >
                        {sourceLabel}
                      </span>
                      {!isBuiltin &&
                        (armed === s.name ? (
                          <>
                            <Button
                              variant="danger"
                              onClick={() => {
                                setArmed(null);
                                onRemove(s);
                              }}
                            >
                              确认移除
                            </Button>
                            <Button onClick={() => setArmed(null)}>取消</Button>
                          </>
                        ) : (
                          <Button onClick={() => setArmed(s.name)}>
                            移除
                          </Button>
                        ))}
                    </>
                  }
                />
              );
            })}
          </div>
        )}
      </SettingsSection>
    </div>
  );
}
