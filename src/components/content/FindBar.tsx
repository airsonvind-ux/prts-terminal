// src/components/content/FindBar.tsx
// 会话内查找条（Ctrl+F · 设计定稿 2026-09-12）
//
// 行为：Ctrl+F 打开（已开时重新聚焦+全选）；Enter/F3/↓ 下一个、
// Shift 系上一个（循环）；Esc/× 关闭并清除高亮；输入即时匹配；
// 流式内容变化 300ms 防抖重算；首次定位 = 视口内或之下第一个命中
// （"就近向下"，非"跳最新"）。
//
// 高亮：CSS Custom Highlight API（不动 DOM）；Range 由 findInMessages 产出。

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { findMatches } from "../../findInMessages";
import { getPageSelection } from "../../contextMenu";
import styles from "./FindBar.module.css";

const RECOMPUTE_DEBOUNCE_MS = 300;
const HIGHLIGHT_NAME = "prts-find";
const HIGHLIGHT_ACTIVE_NAME = "prts-find-active";

/** 查找区根元素（MessageList 的滚动容器） */
function findRoot(): HTMLElement | null {
  return document.querySelector<HTMLElement>("[data-message-list]");
}

/** 首次定位：视口内或之下第一个命中；无则回绕到第一处 */
function pickInitialIndex(ranges: Range[], root: HTMLElement): number {
  const viewTop = root.getBoundingClientRect().top;
  for (let i = 0; i < ranges.length; i++) {
    if (ranges[i].getBoundingClientRect().bottom >= viewTop) return i;
  }
  return 0;
}

/** Highlight API 可用性（旧环境无；测试环境由 stub 提供） */
function highlightsAvailable(): boolean {
  return (
    typeof CSS !== "undefined" &&
    !!CSS.highlights &&
    typeof Highlight !== "undefined"
  );
}

export function FindBar() {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [matches, setMatches] = useState<Range[]>([]);
  const [activeIndex, setActiveIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const queryRef = useRef(query);
  queryRef.current = query;

  // ── 开关 ──

  const openBar = useCallback(() => {
    // 页面有选中文本时直接作为查找词（常见设计——选中即所指）
    const selection = getPageSelection();
    if (selection) setQuery(selection);
    setOpen(true);
    requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.select();
    });
  }, []);

  const closeBar = useCallback(() => {
    setOpen(false);
    setQuery("");
    setMatches([]);
    setActiveIndex(0);
  }, []);

  // 打开时给消息区加让位类（顶部 padding：避免浮层遮挡文首前几行的命中）
  useEffect(() => {
    if (!open) return;
    const list = document.querySelector("[data-message-list]");
    list?.classList.add("prts-find-open");
    return () => {
      list?.classList.remove("prts-find-open");
    };
  }, [open]);

  // ── 匹配（即时：打开与查询变化）──

  useEffect(() => {
    if (!open) return;
    const root = findRoot();
    if (!root) return;
    const ranges = findMatches(root, query);
    setMatches(ranges);
    setActiveIndex(ranges.length ? pickInitialIndex(ranges, root) : 0);
  }, [open, query]);

  // ── 匹配（流式内容变化 → 防抖重算，保持当前激活项）──

  useEffect(() => {
    if (!open) return;
    const root = findRoot();
    if (!root) return;
    let timer: number | undefined;
    const observer = new MutationObserver(() => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        const ranges = findMatches(root, queryRef.current);
        setMatches(ranges);
        setActiveIndex((prev) =>
          ranges.length === 0 ? 0 : Math.min(prev, ranges.length - 1)
        );
      }, RECOMPUTE_DEBOUNCE_MS);
    });
    observer.observe(root, {
      childList: true,
      subtree: true,
      characterData: true,
    });
    return () => {
      observer.disconnect();
      window.clearTimeout(timer);
    };
  }, [open]);

  // ── 高亮同步 ──

  useEffect(() => {
    if (!highlightsAvailable()) return;
    if (!open || matches.length === 0) {
      CSS.highlights.delete(HIGHLIGHT_NAME);
      CSS.highlights.delete(HIGHLIGHT_ACTIVE_NAME);
      return;
    }
    CSS.highlights.set(HIGHLIGHT_NAME, new Highlight(...matches));
    const active = matches[activeIndex];
    if (active) {
      CSS.highlights.set(HIGHLIGHT_ACTIVE_NAME, new Highlight(active));
    } else {
      CSS.highlights.delete(HIGHLIGHT_ACTIVE_NAME);
    }
  }, [open, matches, activeIndex]);

  // ── 卸载清理 ──

  useEffect(() => {
    return () => {
      if (!highlightsAvailable()) return;
      CSS.highlights.delete(HIGHLIGHT_NAME);
      CSS.highlights.delete(HIGHLIGHT_ACTIVE_NAME);
    };
  }, []);

  // ── 步进 / 跳转 ──

  const jumpTo = useCallback((index: number, ranges: Range[]) => {
    const el = ranges[index]?.startContainer.parentElement;
    el?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, []);

  const step = useCallback(
    (delta: 1 | -1) => {
      if (matches.length === 0) return;
      const next = (activeIndex + delta + matches.length) % matches.length;
      setActiveIndex(next);
      jumpTo(next, matches);
    },
    [matches, activeIndex, jumpTo]
  );

  // ── 全局键位（Ctrl+F / F3 / Esc）──

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") {
        e.preventDefault();
        openBar();
        return;
      }
      if (!open) return;
      if (e.key === "F3") {
        e.preventDefault();
        step(e.shiftKey ? -1 : 1);
      } else if (e.key === "Escape") {
        closeBar();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, openBar, closeBar, step]);

  // ── 输入框键位（Enter / Shift+Enter）──

  const onInputKeyDown = (e: ReactKeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      step(e.shiftKey ? -1 : 1);
    }
  };

  if (!open) return null;

  const count = matches.length;

  return (
    <div
      className={styles.findBar}
      role="search"
      aria-label="会话内查找"
      data-findbar=""
    >
      <svg
        className={styles.icon}
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        aria-hidden="true"
      >
        <circle cx="11" cy="11" r="7" />
        <path d="M21 21l-4.3-4.3" />
      </svg>
      <input
        ref={inputRef}
        className={styles.input}
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={onInputKeyDown}
        placeholder="查找"
        autoFocus
        aria-label="查找关键词"
      />
      <span className={styles.count}>
        {count === 0 ? "0/0" : `${activeIndex + 1}/${count}`}
      </span>
      <button
        className={styles.btn}
        onClick={() => step(-1)}
        disabled={count === 0}
        aria-label="上一个"
        title="上一个（Shift+Enter）"
      >
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.5"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <path d="M18 15l-6-6-6 6" />
        </svg>
      </button>
      <button
        className={styles.btn}
        onClick={() => step(1)}
        disabled={count === 0}
        aria-label="下一个"
        title="下一个（Enter）"
      >
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.5"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <path d="M6 9l6 6 6-6" />
        </svg>
      </button>
      <button
        className={styles.btn}
        onClick={closeBar}
        aria-label="关闭"
        title="关闭（Esc）"
      >
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.5"
          strokeLinecap="round"
          aria-hidden="true"
        >
          <path d="M6 6l12 12M18 6L6 18" />
        </svg>
      </button>
    </div>
  );
}
