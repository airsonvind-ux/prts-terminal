import type { ModelInfo } from "../../types";
import styles from "./SettingsProvider.module.css";

export type ModelListStatus = "idle" | "loading" | "loaded" | "empty" | "error";

function formatContext(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${Math.round(n / 1_000)}K`;
  return String(n);
}

interface ModelListProps {
  status: ModelListStatus;
  models: ModelInfo[];
  selectedModel: string;
  error: string;
  onSelect: (id: string) => void;
  onFetch: () => void;
}

export function ModelListSection({
  status,
  models,
  selectedModel,
  error,
  onSelect,
  onFetch,
}: ModelListProps) {
  return (
    <div className={styles.modelSection}>
      {status === "idle" && (
        <button className={styles.fetchButton} onClick={onFetch}>
          获取模型列表
        </button>
      )}
      {status === "loading" && (
        <div className={styles.modelStatus}>
          <span className={styles.modelLoading}>正在获取模型列表…</span>
        </div>
      )}
      {status === "error" && (
        <div className={styles.modelStatus}>
          <span className={styles.modelError}>{error}</span>
          <button className={styles.retryButton} onClick={onFetch}>
            重试
          </button>
        </div>
      )}
      {status === "empty" && (
        <div className={styles.modelStatus}>
          <span className={styles.modelEmpty}>该 API Key 下未找到可用模型</span>
          <button className={styles.retryButton} onClick={onFetch}>
            重新获取
          </button>
        </div>
      )}
      {models.length > 0 && (
        <div className={styles.modelList}>
          {models.map((m) => (
            <div
              key={m.id}
              className={`${styles.modelItem} ${
                m.id === selectedModel ? styles.modelItemActive : ""
              }`}
              onClick={() => onSelect(m.id)}
            >
              <span className={styles.modelItemName}>
                <span className={styles.modelItemNameText} title={m.displayName}>
                  {m.displayName}
                </span>
                <span className={styles.modelItemBadges}>
                  {m.supportsReasoning && (
                    <span className={styles.modelBadge} title="支持推理">
                      R
                    </span>
                  )}
                  {m.supportsVision && (
                    <span className={styles.modelBadgeVision} title="支持图像输入">
                      V
                    </span>
                  )}
                  {!m.enriched && (
                    <span
                      className={styles.modelBadgeUnknown}
                      title="尚未收录该模型的能力信息（思考/视觉/上下文未知）；可以正常调用，思考强度不可设置"
                    >
                      U
                    </span>
                  )}
                </span>
              </span>
              <span className={styles.modelItemContext}>
                {m.contextLength > 0 ? formatContext(m.contextLength) : "—"}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
