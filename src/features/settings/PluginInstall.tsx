// 设置页 · 安装插件（二级视图，UI 规格草案 2 §三）。
//
// 不做嵌套 modal：设置面板本身已经是 aria-modal，再弹一层会让 Esc 的归属变糊。
// 选择文件夹后先预览（后端把目录拷进 staging 并校验十条），确认才原子落位。

import type { PluginPreview } from "../../types";
import { Button } from "./ui/Button";
import { capabilityLabel, formatBytes, kindLabel } from "./pluginText";
import styles from "./SettingsPlugins.module.css";

interface PluginInstallProps {
  preview: PluginPreview | null;
  busy: boolean;
  onPick: () => void;
  onCancel: () => void;
  onConfirm: () => void;
}

export function PluginInstall({
  preview,
  busy,
  onPick,
  onCancel,
  onConfirm,
}: PluginInstallProps) {
  if (!preview) {
    return (
      <div className={styles.install}>
        <p className={styles.sectionHint}>
          选一个插件文件夹。压缩包请先解压——这里只认目录。
        </p>
        <div className={styles.installActions}>
          <Button variant="primary" disabled={busy} onClick={onPick}>
            选择文件夹
          </Button>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.install}>
      <dl className={styles.facts}>
        <dt>名称</dt>
        <dd>{preview.name}</dd>

        <dt>标识</dt>
        <dd>{preview.id}</dd>

        <dt>版本</dt>
        <dd>
          {preview.version} · {kindLabel(preview.kind)}
        </dd>

        {preview.author && (
          <>
            <dt>作者</dt>
            <dd>{preview.author}</dd>
          </>
        )}

        <dt>能力</dt>
        <dd>
          {preview.capabilities.length === 0
            ? "—"
            : preview.capabilities.map((key) => (
                <span key={key} className={styles.capability}>
                  {capabilityLabel(key)}
                </span>
              ))}
        </dd>

        <dt>内容</dt>
        <dd>
          {preview.fileCount} 个文件 · {formatBytes(preview.totalBytes)}
        </dd>
      </dl>

      <p className={styles.installTarget}>
        装到 plugins/{preview.id}/
      </p>

      <div className={styles.installActions}>
        <Button variant="primary" disabled={busy} onClick={onConfirm}>
          安装
        </Button>
        <Button disabled={busy} onClick={onCancel}>
          取消
        </Button>
      </div>
    </div>
  );
}
