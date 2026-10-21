import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { showToast } from "../../stores/useToastStore";
import { SectionNote } from "./ui/SettingsSection";
import {
  MemoryViewFrame,
  LONGTERM_MAX_CHARS,
  charCount,
} from "./memoryViewShared";
import type { MemoryRecentItem, MemorySetLongtermResult } from "../../types";
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
