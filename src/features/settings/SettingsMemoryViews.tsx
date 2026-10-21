// 设置页 · 记忆子页的二级视图（2026-09 结构定稿）
//
// 三套视图：
//  - GrowthView   成长页阅读 / 编辑（新建走同一表单的空白态）
//  - CompiledView 「当下记忆」：近期（只读）＋ 先前（唯一可编辑处）
//  - LibraryView  「记忆库」：全部经历条目，按日期编排（新在前）
//
// 形态：设置内容区内的替换视图（顶部「← 返回」＋标题）。高度天然等于内容区，
// 滚动交给内容区——不用整窗浮层（旧版浮层固定在窗口上，会出现高过整个界面的
// 内容页）。打开时把视野滚到视图顶端。

import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { showToast } from "../../stores/useToastStore";
import { SectionNote } from "./ui/SettingsSection";
import {
  MemoryViewFrame,
  LONGTERM_MAX_CHARS,
  charCount,
} from "./memoryViewShared";

export type { MemView } from "./memoryViewShared";
export { GrowthView } from "./SettingsMemoryGrowth";
import type {
  MemoryExperienceItem,
  MemoryRecentItem,
  MemorySetLongtermResult,
} from "../../types";
import styles from "./SettingsMemory.module.css";

// ═══════════════════════════════════════════════════════
// 当下记忆（视图①）
// ═══════════════════════════════════════════════════════

export function CompiledView({
  recent,
  previous,
  busy,
  onBack,
  onSaved,
}: {
  recent: MemoryRecentItem[];
  previous: string;
  busy: boolean;
  onBack: () => void;
  onSaved: () => void;
}) {
  const [preEdit, setPreEdit] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const preRef = useRef<HTMLTextAreaElement>(null);

  // 编辑框随内容自增（与阅读态展示框同尺寸；CSS 已去原生缩放柄）
  useEffect(() => {
    const el = preRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight + 2}px`;
  }, [preEdit]);

  const savePre = async () => {
    if (preEdit === null) return;
    setSaving(true);
    try {
      const r = await invoke<MemorySetLongtermResult>("memory_set_longterm", {
        text: preEdit,
      });
      if (r.changed) {
        showToast(`长期记忆已重写（版本 ${r.version}，${r.chars} 字）`, {
          kind: "info",
          dedupeKey: "memory-pre-save",
        });
        onSaved();
      } else {
        showToast("无改动，未重写", { kind: "info", dedupeKey: "memory-pre-save" });
      }
      setPreEdit(null);
    } catch (e) {
      showToast(`保存失败：${String(e)}`, { kind: "warning" });
    } finally {
      setSaving(false);
    }
  };

  return (
    <MemoryViewFrame title="当下记忆" onBack={onBack}>
      <div className={styles.viewCaption}>近期</div>
      {recent.length === 0 ? (
        <span className={styles.emptyHint}>（暂无）</span>
      ) : (
        <div className={styles.recentList}>
          {recent.map((r, i) => (
            <div key={`${r.date}-${i}`} className={styles.recentItem}>
              <div className={styles.recentDate}>{r.date}</div>
              <div>
                {r.title}：{r.summary}
              </div>
            </div>
          ))}
        </div>
      )}

      <div className={styles.viewCaption}>先前</div>
      {preEdit === null ? (
        <>
          {previous.trim() ? (
            <div className={styles.viewText}>{previous}</div>
          ) : (
            <span className={styles.emptyHint}>（暂无）</span>
          )}
          <div className={styles.actions}>
            <button
              className={styles.btn}
              onClick={() => setPreEdit(previous)}
              disabled={busy || saving}
            >
              编辑
            </button>
          </div>
        </>
      ) : (
        <>
          <textarea
            className={styles.textarea}
            ref={preRef}
            value={preEdit}
            onChange={(e) => setPreEdit(e.target.value)}
            data-pre-edit
          />
          <div className={styles.formMeta}>
            {charCount(preEdit)} 字 / 上限 {LONGTERM_MAX_CHARS} 字（超出会被拒绝）。保存后对新对话生效。
          </div>
          <div className={styles.actions}>
            <button
              className={`${styles.btn} ${styles.btnPrimary}`}
              onClick={() => void savePre()}
              disabled={busy || saving}
            >
              {saving ? "保存中…" : "保存"}
            </button>
            <button className={styles.btn} onClick={() => setPreEdit(null)}>
              取消
            </button>
          </div>
        </>
      )}

      <SectionNote>
        这里展示的是 PRTS 当前记住的内容：「近期」只读；「先前」（长期梗概）可编辑。
      </SectionNote>
    </MemoryViewFrame>
  );
}

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
