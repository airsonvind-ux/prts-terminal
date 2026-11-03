import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { showToast } from "../../stores/useToastStore";
import { MemoryViewFrame } from "./memoryViewShared";
import type { MemoryExperienceItem } from "../../types";
import styles from "./SettingsMemory.module.css";

// ═══════════════════════════════════════════════════════
// 记忆库（视图②）
//
// 按日期编排的完整经历条目。列表密度高，所以：
//  - 顶部一行读数（条目 / 覆盖天数 / 已收纳），先进来说清"有多少"；
//  - 日期头带当天条数，找某一天时不用数字数；
//  - 条目默认只露标题，点开看摘要——一次只展开一条，避免整页撑成文章。
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

  const absorbed = (items ?? []).filter((it) => it.absorbed).length;

  return (
    <MemoryViewFrame title="记忆库" onBack={onBack}>
      {items === null ? (
        <span className={styles.emptyHint}>加载中…</span>
      ) : items.length === 0 ? (
        <span className={styles.emptyHint}>（空）</span>
      ) : (
        <>
          <div className={styles.libraryStats}>
            <span className={styles.libraryStat}>
              <b>{items.length}</b> 条
            </span>
            <span className={styles.libraryStat}>
              <b>{groups.length}</b> 天
            </span>
            <span className={styles.libraryStat}>
              已收纳 <b>{absorbed}</b>
            </span>
          </div>

          {groups.map(([date, list]) => (
            <div key={date} className={styles.dateGroup}>
              <div className={styles.dateLabel}>
                {date}
                <span className={styles.dateCount}>{list.length} 条</span>
              </div>
              {list.map((item) => {
                const open = expanded === item.id;
                return (
                  <div
                    key={item.id}
                    className={`${styles.entry} ${open ? styles.entryOpen : ""}`}
                  >
                    <button
                      className={styles.entryHead}
                      type="button"
                      aria-expanded={open}
                      onClick={() => setExpanded(open ? null : item.id)}
                    >
                      <span
                        className={`${styles.entryChevron} ${open ? styles.entryChevronOpen : ""}`}
                        aria-hidden="true"
                      >
                        ›
                      </span>
                      <span className={styles.entryTitle}>{item.title}</span>
                      {item.absorbed ? (
                        <span className={styles.entryMark}>已收纳</span>
                      ) : item.sealed ? (
                        <span className={styles.entryMark}>已封存</span>
                      ) : null}
                    </button>
                    {open && (
                      <div className={styles.entryBody}>{item.summary}</div>
                    )}
                  </div>
                );
              })}
            </div>
          ))}
        </>
      )}
    </MemoryViewFrame>
  );
}
