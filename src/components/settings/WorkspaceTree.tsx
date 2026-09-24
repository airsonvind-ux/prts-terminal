// 设置页 · 空间子页 · 工作区树 + 按名搜索
//
// 树：按需逐层加载（点开哪级读哪级），缩进固定、名称单行省略（悬停看全名）。
// **照实列出隐藏项**——树是"真实目录"，与 PRTS 自己的 list_dir 同口径；
// 降噪交给搜索（搜索跳过点开头项与依赖/构建目录，见后端 search_workspace_entries）。
// 这处口径不同是有意的：UI 与 PRTS 对同一目录的回答必须一致。
//
// 搜索：输入文字**不触发搜索、不碰树**；按回车进入结果态——结果就地替掉树
// （同一块区域，窄面板里不互相遮挡）；点击命中 → 退出搜索、回到树、展开祖先
// 目录链并选中该行；Esc 或清空输入退出。进入结果态后继续输入会实时重搜
// （180ms debounce + 版本号守卫丢乱序返回）。
//
// 为什么不用浮层下拉：设置面板只有一百多像素高，浮层会在窄而矮的框里遮住整棵树
// （2026-09 实测反馈）。为什么必须回车：树的消失只能是用户动作的结果，
// 逐字符触发会让“我只是在打字”变成“树自己没了”（同次反馈）。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { showToast } from "../../stores/useToastStore";
import { FileLink } from "../ui/FileLink";
import type { WorkspaceEntry, WorkspaceSearchHit } from "../../types";
import styles from "./WorkspaceTree.module.css";

/** 搜索防抖（与 Hanako 工作台同值） */
const SEARCH_DEBOUNCE_MS = 180;
/** 每级缩进（px） */
const INDENT_PX = 12;

export interface WorkspaceTreeProps {
  /** 工作区根（空串 = 未设置：不加载，显示提示） */
  root: string;
}

/** 可见行（展开态压平为平表，缩进由 depth 决定） */
interface TreeRow {
  entry: WorkspaceEntry;
  depth: number;
}

export function WorkspaceTree({ root }: WorkspaceTreeProps) {
  // 已加载子项：目录绝对路径 → 子项（根一层用 root 自身作键）
  const [childrenByPath, setChildrenByPath] = useState<
    Map<string, WorkspaceEntry[]>
  >(new Map());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [selected, setSelected] = useState<string | null>(null);
  const loadedRef = useRef<Set<string>>(new Set());
  const loadingRef = useRef<Set<string>>(new Set());
  const treeRef = useRef<HTMLDivElement>(null);

  // 搜索
  const [query, setQuery] = useState("");
  const [searchMode, setSearchMode] = useState(false);
  const [hits, setHits] = useState<WorkspaceSearchHit[]>([]);
  const [searchPending, setSearchPending] = useState(false);
  const searchVersionRef = useRef(0);

  /** 载入某目录一层（已加载 / 在途 → no-op） */
  const ensureLoaded = useCallback((dirPath: string) => {
    if (loadedRef.current.has(dirPath) || loadingRef.current.has(dirPath)) {
      return;
    }
    loadingRef.current.add(dirPath);
    invoke<WorkspaceEntry[]>("list_workspace_dir", { path: dirPath })
      .then((entries) => {
        loadedRef.current.add(dirPath);
        setChildrenByPath((prev) => new Map(prev).set(dirPath, entries));
      })
      .catch((e: unknown) => {
        showToast(`读取目录失败：${String(e)}`, { kind: "warning" });
      })
      .finally(() => {
        loadingRef.current.delete(dirPath);
      });
  }, []);

  // 工作区根变化：清空重建（展开态 / 选中态 / 加载缓存一并重置）
  useEffect(() => {
    setChildrenByPath(new Map());
    setExpanded(new Set());
    setSelected(null);
    setHits([]);
    setSearchPending(false);
    setSearchMode(false);
    setQuery("");
    loadedRef.current = new Set();
    loadingRef.current = new Set();
    if (root.trim()) ensureLoaded(root);
  }, [root, ensureLoaded]);

  /** 展开 / 收起目录（点击即确保有数据） */
  const toggleDir = useCallback(
    (entry: WorkspaceEntry) => {
      if (!entry.isDir) return;
      ensureLoaded(entry.path);
      setExpanded((prev) => {
        const next = new Set(prev);
        if (next.has(entry.path)) next.delete(entry.path);
        else next.add(entry.path);
        return next;
      });
    },
    [ensureLoaded]
  );

  /** 可见行：按展开态 DFS 压平 */
  const rows = useMemo(() => {
    const out: TreeRow[] = [];
    const walk = (dirPath: string, depth: number) => {
      const kids = childrenByPath.get(dirPath);
      if (!kids) return;
      for (const entry of kids) {
        out.push({ entry, depth });
        if (entry.isDir && expanded.has(entry.path)) walk(entry.path, depth + 1);
      }
    };
    walk(root, 0);
    return out;
  }, [childrenByPath, expanded, root]);

  // 搜索：仅“搜索态”下生效（回车进入）——防抖 + 版本号守卫（乱序返回丢弃）。
  // 只输文字不搜：树的消失必须是用户动作（回车）的结果，不能是逐字符的副作用。
  useEffect(() => {
    const trimmed = query.trim();
    if (!searchMode || !trimmed || !root.trim()) {
      setHits([]);
      setSearchPending(false);
      return;
    }
    const version = ++searchVersionRef.current;
    setSearchPending(true);
    const timer = setTimeout(() => {
      invoke<WorkspaceSearchHit[]>("search_workspace_files", { query: trimmed })
        .then((items) => {
          if (searchVersionRef.current !== version) return;
          setHits(Array.isArray(items) ? items : []);
          setSearchPending(false);
        })
        .catch((e: unknown) => {
          if (searchVersionRef.current !== version) return;
          console.error("[space] 工作区搜索失败", e);
          setHits([]);
          setSearchPending(false);
        });
    }, SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [query, searchMode, root]);

  /** 退出搜索：清空输入回到树 */
  const exitSearch = useCallback(() => {
    setSearchMode(false);
    setQuery("");
    setHits([]);
  }, []);

  /** 点击命中：退出搜索（回到树）+ 展开祖先链 + 选中该行 */
  const pickHit = useCallback(
    (hit: WorkspaceSearchHit) => {
      exitSearch();
      const toExpand = [...hit.ancestorDirs, ...(hit.isDir ? [hit.path] : [])];
      for (const dir of toExpand) ensureLoaded(dir);
      setExpanded((prev) => new Set([...prev, ...toExpand]));
      setSelected(hit.path);
    },
    [ensureLoaded, exitSearch]
  );

  // 选中行滚入视野（nearest：不打断用户当前位置）
  useEffect(() => {
    if (!selected) return;
    const nodes = treeRef.current?.querySelectorAll<HTMLElement>("[data-ws-path]");
    if (!nodes) return;
    const target = Array.from(nodes).find(
      (n) => n.getAttribute("data-ws-path") === selected
    );
    target?.scrollIntoView?.({ block: "nearest" });
  }, [selected, rows]);

  const disabled = !root.trim();
  /** 结果态：回车进入搜索后，结果就地替掉树（窄面板里不用浮层遮罩） */
  const showingResults = searchMode && query.trim() !== "";

  return (
    <div className={styles.wrap}>
      <input
        className={styles.searchInput}
        value={query}
        placeholder="搜索工作空间"
        aria-label="搜索工作空间"
        title="输入关键词后按回车搜索（Esc 退出）"
        disabled={disabled}
        onChange={(e) => {
          const next = e.target.value;
          setQuery(next);
          // 清空输入 = 结束搜索：树立即回来，不需要再按 Esc
          if (!next.trim()) setSearchMode(false);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            if (query.trim()) setSearchMode(true);
          } else if (e.key === "Escape") {
            exitSearch();
          }
        }}
      />

      <div className={styles.treeBox} ref={treeRef} data-workspace-tree>
        {disabled ? (
          <div className={styles.emptyHint}>未设置工作空间</div>
        ) : showingResults ? (
          <div className={styles.results} role="listbox" aria-label="搜索结果">
            {hits.length === 0 ? (
              <div className={styles.emptyHint}>
                {searchPending ? "搜索中…" : "无匹配"}
              </div>
            ) : (
              hits.map((hit) => (
                <button
                  key={hit.path}
                  type="button"
                  className={styles.resultRow}
                  onClick={() => pickHit(hit)}
                >
                  <span className={styles.resultName}>{hit.name}</span>
                  <span className={styles.resultPath}>
                    {hit.parentSubdir || "（根目录）"}
                  </span>
                </button>
              ))
            )}
          </div>
        ) : !childrenByPath.has(root) ? (
          <div className={styles.emptyHint}>读取中…</div>
        ) : rows.length === 0 ? (
          <div className={styles.emptyHint}>（空目录）</div>
        ) : (
          rows.map(({ entry, depth }) => {
            const isOpen = entry.isDir && expanded.has(entry.path);
            const isEmptyDir = isOpen && childrenByPath.get(entry.path)?.length === 0;
            return (
              <div key={entry.path} className={styles.rowWrap} data-ws-path={entry.path}>
                {entry.isDir ? (
                  <button
                    type="button"
                    className={`${styles.row}${selected === entry.path ? ` ${styles.rowSelected}` : ""}`}
                    style={{ paddingLeft: 4 + depth * INDENT_PX }}
                    title={entry.name}
                    onClick={() => toggleDir(entry)}
                  >
                    <span className={styles.caret}>{isOpen ? "▾" : "▸"}</span>
                    <span className={styles.name}>{entry.name}</span>
                  </button>
                ) : (
                  <FileLink
                    path={entry.path}
                    className={`${styles.row}${selected === entry.path ? ` ${styles.rowSelected}` : ""}`}
                    style={{ paddingLeft: 4 + depth * INDENT_PX }}
                    title={entry.name}
                    successHint="已打开文件"
                  >
                    <span className={styles.caretSpacer} />
                    <span className={styles.name}>{entry.name}</span>
                  </FileLink>
                )}
                {isEmptyDir && (
                  <div
                    className={styles.emptyRow}
                    style={{ paddingLeft: 4 + (depth + 1) * INDENT_PX }}
                  >
                    （空目录）
                  </div>
                )}
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
