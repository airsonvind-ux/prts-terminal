import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { showToast } from "../../stores/useToastStore";
import { SectionNote } from "./ui/SettingsSection";
import {
  MemoryViewFrame,
  PROFILE_NAME,
  parseTags,
  fmtDate,
} from "./memoryViewShared";
import type { MemoryDetail, MemoryGrowthPage } from "../../types";
import { useConfirmFocus } from "./useConfirmFocus";
import styles from "./SettingsMemory.module.css";

// ═══════════════════════════════════════════════════════
// 成长页阅读 / 编辑
// ═══════════════════════════════════════════════════════

export function GrowthView({
  name,
  busy,
  onBack,
  onSaved,
}: {
  /** 现有页名；缺省＝新建 */
  name?: string;
  busy: boolean;
  onBack: () => void;
  onSaved: () => void;
}) {
  const isNew = !name;
  const [page, setPage] = useState<MemoryGrowthPage | null>(null);
  const [loading, setLoading] = useState(!isNew);
  const [mode, setMode] = useState<"read" | "edit">(isNew ? "edit" : "read");
  const [draft, setDraft] = useState({
    name: "",
    brief: "",
    body: "",
    tagsText: "",
  });
  const [saving, setSaving] = useState(false);
  const [confirmDel, setConfirmDel] = useState(false);
  const bodyRef = useRef<HTMLTextAreaElement>(null);
  const { modalRef, handleKeyDown } = useConfirmFocus(confirmDel, () =>
    setConfirmDel(false)
  );

  // 正文编辑框随内容自增（与阅读态展示框同尺寸；CSS 已去原生缩放柄）
  useEffect(() => {
    const el = bodyRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight + 2}px`;
  }, [draft.body, mode]);

  const fillDraft = useCallback((p: MemoryGrowthPage) => {
    setDraft({
      name: p.name,
      brief: p.brief,
      body: p.body,
      tagsText: p.tags.join("、"),
    });
  }, []);

  const loadPage = useCallback(async () => {
    if (!name) return;
    setLoading(true);
    try {
      const d = await invoke<MemoryDetail>("memory_get", { kind: "growth", name });
      const p = d.page ?? null;
      setPage(p);
      if (p) fillDraft(p);
    } catch (e) {
      showToast(`读取失败：${String(e)}`, { kind: "warning" });
    } finally {
      setLoading(false);
    }
  }, [name, fillDraft]);

  useEffect(() => {
    void loadPage();
  }, [loadPage]);

  const save = async () => {
    const nm = draft.name.trim();
    if (!nm) {
      showToast("页名不能为空", { kind: "warning" });
      return;
    }
    setSaving(true);
    try {
      const tags = parseTags(draft.tagsText);
      if (isNew) {
        await invoke("memory_create", {
          name: nm,
          brief: draft.brief,
          body: draft.body,
          tags,
        });
        showToast(`已新建「${nm}」`, { kind: "info", dedupeKey: "memory-save" });
        onSaved();
        onBack();
      } else {
        const changed = await invoke<boolean>("memory_update", {
          name: nm,
          brief: draft.brief,
          body: draft.body,
          tags,
        });
        if (changed) {
          showToast(`已保存「${nm}」`, { kind: "info", dedupeKey: "memory-save" });
          onSaved();
          await loadPage();
        } else {
          showToast("无改动，未写入", { kind: "info", dedupeKey: "memory-save" });
        }
        setMode("read");
      }
    } catch (e) {
      showToast(`保存失败：${String(e)}`, { kind: "warning" });
    } finally {
      setSaving(false);
    }
  };

  const del = async () => {
    if (!name) return;
    setConfirmDel(false);
    try {
      await invoke("memory_delete", { name });
      showToast(
        name === PROFILE_NAME ? "「关于博士」已复位" : `已删除「${name}」`,
        { kind: "info", dedupeKey: "memory-del" }
      );
      onSaved();
      onBack();
    } catch (e) {
      showToast(`删除失败：${String(e)}`, { kind: "warning" });
    }
  };

  const cancelEdit = () => {
    if (isNew) {
      onBack();
      return;
    }
    if (page) fillDraft(page);
    setMode("read");
  };

  if (loading) {
    return (
      <MemoryViewFrame title={name ?? "新建专题"} onBack={onBack}>
        <span className={styles.emptyHint}>加载中…</span>
      </MemoryViewFrame>
    );
  }
  if (!isNew && !page) {
    return (
      <MemoryViewFrame title={name ?? ""} onBack={onBack}>
        <span className={styles.emptyHint}>未找到「{name}」</span>
      </MemoryViewFrame>
    );
  }

  return (
    <MemoryViewFrame title={isNew ? "新建专题" : (page?.name ?? "")} onBack={onBack}>
      {mode === "read" && page ? (
        <>
          <div className={styles.viewMeta}>{fmtDate(page.updatedAt)} 更新</div>
          {page.brief && <div className={styles.viewBrief}>{page.brief}</div>}
          <div className={styles.viewText}>
            {page.body.trim() ? page.body : "（尚无内容）"}
          </div>
          {page.tags.length > 0 && (
            <div className={styles.tagRow}>
              {page.tags.map((t) => (
                <span className={styles.tag} key={t}>
                  {t}
                </span>
              ))}
            </div>
          )}
          <div className={styles.actions}>
            <button
              className={`${styles.btn} ${styles.btnPrimary}`}
              onClick={() => setMode("edit")}
              disabled={busy}
            >
              编辑
            </button>
            <button
              className={`${styles.btn} ${styles.btnDanger}`}
              onClick={() => setConfirmDel(true)}
              disabled={busy}
            >
              {name === PROFILE_NAME ? "复位" : "删除"}
            </button>
          </div>

          {confirmDel && (
            <div className={styles.backdrop} onClick={() => setConfirmDel(false)}>
              <div
                ref={modalRef}
                className={styles.modal}
                role="dialog"
                aria-modal="true"
                aria-labelledby="memory-delete-title"
                tabIndex={-1}
                onClick={(e) => e.stopPropagation()}
                onKeyDown={handleKeyDown}
              >
                <div className={styles.modalHeader}>
                  <span id="memory-delete-title">
                    {name === PROFILE_NAME ? "复位「关于博士」" : "删除专题"}
                  </span>
                </div>
                <div className={styles.modalBody}>
                  <SectionNote>
                    {name === PROFILE_NAME
                      ? "复位将清空该页的全部内容（页面保留）；此操作不可撤销。"
                      : `将删除「${name}」及其全部内容；此操作不可撤销。`}
                  </SectionNote>
                </div>
                <div className={styles.modalFooter}>
                  <button
                    type="button"
                    className={styles.btn}
                    data-confirm-cancel
                    onClick={() => setConfirmDel(false)}
                  >
                    取消
                  </button>
                  <button
                    className={`${styles.btn} ${styles.btnDanger}`}
                    onClick={() => void del()}
                    disabled={busy}
                  >
                    {name === PROFILE_NAME ? "确认复位" : "确认删除"}
                  </button>
                </div>
              </div>
            </div>
          )}
        </>
      ) : (
        <div className={styles.form}>
          <label className={styles.fieldLabel}>页名</label>
          <input
            className={styles.input}
            value={draft.name}
            disabled={!isNew}
            placeholder="如：轴-推演 / 博士偏好"
            onChange={(e) => setDraft({ ...draft, name: e.target.value })}
          />
          <label className={styles.fieldLabel}>简述</label>
          <input
            className={styles.input}
            value={draft.brief}
            placeholder="一句话简述（进主题目录）"
            onChange={(e) => setDraft({ ...draft, brief: e.target.value })}
          />
          <label className={styles.fieldLabel}>正文</label>
          <textarea
            className={styles.textarea}
            ref={bodyRef}
            value={draft.body}
            onChange={(e) => setDraft({ ...draft, body: e.target.value })}
          />
          <label className={styles.fieldLabel}>标签</label>
          <input
            className={styles.input}
            value={draft.tagsText}
            placeholder="用、或逗号分隔，可留空"
            onChange={(e) => setDraft({ ...draft, tagsText: e.target.value })}
          />
          <div className={styles.formMeta}>创建 / 更新时间由系统记录。</div>
          <div className={styles.actions}>
            <button
              className={`${styles.btn} ${styles.btnPrimary}`}
              onClick={() => void save()}
              disabled={busy || saving}
            >
              {saving ? "保存中…" : "保存"}
            </button>
            <button className={styles.btn} onClick={cancelEdit}>
              取消
            </button>
          </div>
        </div>
      )}
    </MemoryViewFrame>
  );
}
