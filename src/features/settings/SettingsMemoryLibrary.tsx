import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { showToast } from "../../stores/useToastStore";
import { MemoryViewFrame } from "./memoryViewShared";
import type { MemoryExperienceItem } from "../../types";
import styles from "./SettingsMemory.module.css";

// ═══════════════════════════════════════════════════════
// 记忆库（视图②）
// ═══════════════════════════════════════════════════════

export function LibraryView({ onBack }: { onBack: () => void }) {
  const [items, setItems] = useState<MemoryExperienceItem[] | null>(null);
  const [expanded, setExpanded] = useState<string | null>(null);

  useEffect(() => {
    void (async () => {
      try {
        setItems(await invoke<MemoryExperienceItem[]>("memory_experience_list"));
      } catch (e) {
        showToast(`读取失败：${String(e)}`, { kind: "warning" });
        setItems([]);
      }
    })();
  }, []);

  // 后端已按时间降序返回；按日期分组、组内保持原序（新在前）
  const groups: [string, MemoryExperienceItem[]][] = [];
  {
    const map = new Map<string, MemoryExperienceItem[]>();
    for (const it of items ?? []) {
      const arr = map.get(it.date);
      if (arr) arr.push(it);
      else map.set(it.date, [it]);
    }
    groups.push(...map.entries());
  }

  return (
    <MemoryViewFrame title="记忆库" onBack={onBack}>
      {items === null ? (
        <span className={styles.emptyHint}>加载中…</span>
      ) : items.length === 0 ? (
        <span className={styles.emptyHint}>（空）</span>
      ) : (
        groups.map(([date, list]) => (
          <div key={date} className={styles.dateGroup}>
            <div className={styles.dateLabel}>{date}</div>
            {list.map((item) => (
              <div key={item.id} className={styles.entry}>
                <button
                  className={styles.entryHead}
                  type="button"
                  onClick={() =>
                    setExpanded((cur) => (cur === item.id ? null : item.id))
                  }
                >
                  <span className={styles.entryTitle}>{item.title}</span>
                  {item.absorbed ? (
                    <span className={styles.entryMark}>已收纳</span>
                  ) : item.sealed ? (
                    <span className={styles.entryMark}>已封存</span>
                  ) : null}
                </button>
                {expanded === item.id && (
                  <div className={styles.entryBody}>{item.summary}</div>
                )}
              </div>
            ))}
          </div>
        ))
      )}
    </MemoryViewFrame>
  );
}
