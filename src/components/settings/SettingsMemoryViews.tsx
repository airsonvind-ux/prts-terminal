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

import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { showToast } from "../../stores/useToastStore";
import { SectionNote } from "./ui/SettingsSection";
import type {
  MemoryDetail,
  MemoryExperienceItem,
  MemoryGrowthPage,
  MemoryRecentItem,
  MemorySetLongtermResult,
} from "../../types";
import styles from "./SettingsMemory.module.css";

/** 当前打开的二级视图（null＝主列表） */
export type MemView =
  | { kind: "growth"; name: string }
  | { kind: "growthNew" }
  | { kind: "compiled" }
  | { kind: "library" }
  | null;

/** 「关于博士」页名（固定键；删除＝复位） */
const PROFILE_NAME = "关于博士";

/** 「先前」（长期记忆）字数上限：与后端硬闸同源（orchestrator::memory::MAX_LONGTERM_CHARS） */
const LONGTERM_MAX_CHARS = 600;

/** Unicode 码点数（对齐后端 chars().count()；不用 String.length） */
function charCount(s: string): number {
  return [...s].length;
}

/** 标签文本 → 数组（、/逗号分隔） */
function parseTags(text: string): string[] {
  return text
    .split(/[、,，]/)
    .map((t) => t.trim())
    .filter((t) => t.length > 0);
}

/** unix 毫秒 → YYYY-MM-DD（本地时区） */
function fmtDate(ts: number): string {
  const d = new Date(ts);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/** 视图框架：返回 + 标题 + 内容 */
export function MemoryViewFrame({
  title,
  onBack,
  children,
}: {
  title: string;
  onBack: () => void;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    // 打开视图时把视野滚到视图顶端（内容区是滚动容器；无该 API 的环境跳过）
    ref.current?.scrollIntoView?.({ block: "start" });
  }, []);
  return (
    <div className={styles.view} ref={ref}>
      <div className={styles.viewHeader}>
        <button className={styles.backBtn} type="button" onClick={onBack}>
          ← 返回
        </button>
        <span className={styles.viewTitle}>{title}</span>
      </div>
      <div className={styles.viewBody}>{children}</div>
    </div>
  );
}

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
  const bodyRef = useRef<HTMLTextAreaElement>(null);

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
              onClick={() => void del()}
              disabled={busy}
            >
              {name === PROFILE_NAME ? "复位" : "删除"}
            </button>
          </div>
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
            {charCount(preEdit)} 字 / 上限 {LONGTERM_MAX_CHARS} 字（超出会被拒绝）。
            保存后对新对话生效，正在进行的对话靠提醒看到新版。
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
        这里是与提示词同源的两节注入内容；「近期」只读，长期记忆可编辑。
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
