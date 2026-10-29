// 消息列表的滚动所有权：跟底意图、几何事实、缩放保护与锚定校正。
//
// 写入点只有四类：items 变化钉底、容器尺寸变化重钉、窗口缩放保护、输入框
// 测量接管。新增任何自动滚动前，先确认它属于哪一类。

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import type { SessionEvent } from "../../types";
import type { DisplayItem } from "./messageItems";
import { registerMessageScrollHost } from "./messageScroll";

/** 钉底收敛窗口（ms）：items 变化后在这段时间内逐帧压到底，
 * 覆盖输入区/上下文环/大段文本布局随后落定的情况。 */
const PIN_SETTLE_MS = 400;

/** 贴底判定阈值（px）：离底距离不超过它就当成“贴底”。 */
const STICK_PX = 5;

/** 窗口缩放保护窗（ms）：resize 打头后这段时间内不钉底。 */
const WINDOW_RESIZE_GUARD_MS = 160;

/** 缩放锚校正量：把锚行送回原来的偏移处所需的 scrollTop 增量。
 *  纯函数，供单测直接验证（真实布局行为在浏览器探针里验证）。 */
export function anchorDelta(rowTop: number, listTop: number, offset: number): number {
  return rowTop - listTop - offset;
}

/** 读当前界面缩放基准（--face-size）。
 *  useResize 逐帧写 documentElement 的内联变量，是整个界面缩放的唯一源；
 *  窗口尺寸本身在这里靠不住——CSS 变量先变、窗口后变，中间隔着一两帧。 */
function readFaceSize(): string {
  if (typeof document === "undefined") return "";
  return document.documentElement.style.getPropertyValue("--face-size");
}

interface Options {
  items: DisplayItem[];
  turns: SessionEvent[][];
  currentEvents: SessionEvent[];
  conversationId?: string | null;
}

export function useMessageScroll({
  items,
  turns,
  currentEvents,
  conversationId,
}: Options) {
  const listRef: RefObject<HTMLDivElement | null> = useRef(null);

  /*
   * 跟底意图（ref）：true = 用户已离开底部，停止自动钉底。
   *
   * 用 useRef 而非 useState：滚动事件回调里不需要触发重渲染；它只在自动钉底
   * 逻辑（useEffect）里被读。
   *
   * 与下面的 moreBelow 是两件事：这里记「意图」（还跟不跟底），moreBelow 记
   * 「事实」（视口下方此刻还有没有内容）。
   */
  const userScrolledUpRef = useRef(false);

  /* 展开卡片后的阅读锁：即使用户当前在底部，也不再让 handleScroll
   * 自动恢复跟底；否则一次 scroll 事件就会把 following 改回 true。 */
  const readingHoldRef = useRef(false);

  /* 下方还有内容（几何事实）：按钮的唯一判据。 */
  const [moreBelow, setMoreBelow] = useState(false);

  /* 跟底意图的可渲染镜像：卡头只在用户离开底部、进入卡片内容时
   * 才允许 sticky；跟底期间保持普通流，避免和逐帧钉底抢滚动。 */
  const [following, setFollowing] = useState(true);

  /** 跟底意图的唯一写入口（ref 热路径读；镜像供布局选择） */
  const setStick = useCallback((stick: boolean) => {
    userScrolledUpRef.current = !stick;
    setFollowing((prev) => (prev === stick ? prev : stick));
  }, []);

  /* 观测记录：事件到达时 scrollHeight 已变 ⇒ 这次滚动是内容尺寸变化引起的
   * 钳位/锚定（浏览器行为），不是用户。 */
  const observedScrollHeightRef = useRef<number | null>(null);

  /* 钉底原语：直接写 scrollTop = scrollHeight（浏览器自行钳到最大）。 */
  const pinNow = useCallback(() => {
    const el = listRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, []);

  /** 回到最新（“回到底部”按钮）：恢复跟随 + 瞬时落底。 */
  const scrollToBottomNow = useCallback(() => {
    readingHoldRef.current = false;
    setStick(true);
    pinNow();
    setMoreBelow(false);
  }, [pinNow, setStick]);

  /** 用户展开卡片：停止自动跟底，把当前阅读位置固定住。
   *  需要回到最新时仍可用“回到底部”按钮恢复。 */
  const stopFollowing = useCallback(() => {
    readingHoldRef.current = true;
    setStick(false);
  }, [setStick]);

  /* 窗口缩放保护窗。 */
  const windowResizingRef = useRef(false);
  const resizeGuardTimerRef = useRef(0);

  /* 几何事实的读入口：量一次「离底距离」并写回 moreBelow。 */
  const refreshBelow = useCallback(() => {
    const el = listRef.current;
    if (!el) return;
    const next = el.scrollHeight - el.scrollTop - el.clientHeight > STICK_PX;
    if (next && windowResizingRef.current) return;
    setMoreBelow((prev) => (prev === next ? prev : next));
  }, []);

  /** 最近一次已知视口尺寸（由 ResizeObserver 回调维护） */
  const lastViewportRef = useRef({ w: 0, h: 0 });
  /** 最近一次已知界面缩放基准（--face-size） */
  const lastFaceSizeRef = useRef("");
  /** 缩放期间的视野锚：视野顶端那一行 + 它距列表顶的偏移 */
  const anchorRef = useRef<{ row: Element; offset: number } | null>(null);

  /** 记锚（缩放开始时只记一次） */
  const captureAnchor = useCallback(() => {
    const el = listRef.current;
    if (!el) return;
    const listTop = el.getBoundingClientRect().top;
    for (const row of Array.from(el.children)) {
      const r = row.getBoundingClientRect();
      if (r.bottom > listTop) {
        anchorRef.current = { row, offset: r.top - listTop };
        return;
      }
    }
    anchorRef.current = null;
  }, []);

  /** 把锚行送回原来的偏移处（缩放中每次尺寸变化后调）。 */
  const restoreAnchor = useCallback(() => {
    const el = listRef.current;
    const a = anchorRef.current;
    if (!el || !a || !a.row.isConnected) {
      anchorRef.current = null;
      return;
    }
    const listTop = el.getBoundingClientRect().top;
    const rowTop = a.row.getBoundingClientRect().top;
    const delta = anchorDelta(rowTop, listTop, a.offset);
    if (Math.abs(delta) > 0.5) {
      el.scrollTop = el.scrollTop + delta;
    }
  }, []);

  /** 打开（并顺延）保护窗；窗结束即释放锚 */
  const armResizeGuard = useCallback(() => {
    windowResizingRef.current = true;
    window.clearTimeout(resizeGuardTimerRef.current);
    resizeGuardTimerRef.current = window.setTimeout(() => {
      windowResizingRef.current = false;
      anchorRef.current = null;
      // 窗口/CSS 变量可能异步落定：用当前值收口，避免旧的视口快照一直
      // 被当成“仍在缩放”，把后续流式钉底全部挡掉。
      lastViewportRef.current = { w: window.innerWidth, h: window.innerHeight };
      lastFaceSizeRef.current = readFaceSize();
      // 缩放结束、重排已定：把几何事实重判一次（缩放期间 refreshBelow 被挂起）
      refreshBelow();
      if (!userScrolledUpRef.current) {
        pinNow();
        setMoreBelow(false);
      }
    }, WINDOW_RESIZE_GUARD_MS);
  }, [pinNow, refreshBelow]);

  useEffect(() => {
    window.addEventListener("resize", armResizeGuard);
    return () => {
      window.removeEventListener("resize", armResizeGuard);
      window.clearTimeout(resizeGuardTimerRef.current);
    };
  }, [armResizeGuard]);

  /* 提供给输入框的“稳定测量窗口”：测量期间接管滚动行为。 */
  useEffect(() => {
    registerMessageScrollHost({
      measure: (fn) => {
        const el = listRef.current;
        if (!el) return fn();
        const atBottom =
          el.scrollHeight - el.scrollTop - el.clientHeight <= STICK_PX;
        const saved = el.scrollTop;
        try {
          return fn();
        } finally {
          el.scrollTop = atBottom ? el.scrollHeight : saved;
        }
      },
    });
    return () => registerMessageScrollHost(null);
  }, []);

  /*
   * 用户发送新消息时强制重置滚底标志。
   *
   * 判断依据：新轮次归档（turns 增长），或进行中轮次的第一条事件
   * （合成 user/message）出现。无论用户之前在哪个历史位置，发送新消息意味
   * 着"我想看刚才发的和接下来的回复"。
   */
  const prevTurnCountRef = useRef(turns.length);
  useEffect(() => {
    const turnsGrew = turns.length > prevTurnCountRef.current;
    const last = currentEvents[currentEvents.length - 1];
    if (turnsGrew || last?.type === "user/message") {
      readingHoldRef.current = false;
      setStick(true);
    }
    prevTurnCountRef.current = turns.length;
  }, [turns, currentEvents, setStick]);

  /*
   * 滚动事件处理。
   *
   * 用户向上滚动（离底 > 5px）→ 停止自动跟底；滑回距底 ≤ 5px → 恢复跟底。
   * 判定顺序（结构不变量）：先给“回到贴底”让路，再谈屏蔽。
   */
  const handleScroll = useCallback(() => {
    const el = listRef.current;
    if (!el) return;
    // 几何事实先对齐：下面的屏蔽只针对「意图」的推断，不该拦事实
    refreshBelow();
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    if (distance <= STICK_PX) {
      if (!readingHoldRef.current) setStick(true);
      return;
    }
    if (windowResizingRef.current) return;
    // 内容尺寸刚变过 ⇒ 钳位/锚定引起的滚动（浏览器行为），不算用户意图
    const observed = observedScrollHeightRef.current;
    observedScrollHeightRef.current = el.scrollHeight;
    if (observed !== null && Math.abs(el.scrollHeight - observed) > 0.5) return;
    // 缩放基准/视口尺寸变了 ⇒ 缩放引起的重排
    const face = readFaceSize();
    const faceChanged =
      lastFaceSizeRef.current !== "" && lastFaceSizeRef.current !== face;
    const vp = lastViewportRef.current;
    const viewportChanged =
      vp.w !== 0 && (vp.w !== window.innerWidth || vp.h !== window.innerHeight);
    if (faceChanged || viewportChanged) {
      armResizeGuard();
      return;
    }
    setStick(false);
  }, [armResizeGuard, refreshBelow, setStick]);

  /*
   * 用户意图（手势）：向上滚轮 → 立即停止跟随。
   *
   * 前提：可滚范围真实存在。“离开底部”的引用对象是“还没看到的更早内容”——
   * 内容整体放得下时（欢迎页、短对话），上滑不移动任何像素。
   */
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      if (e.deltaY >= 0) {
        refreshBelow();
        // 下滚是明确的“回到最新”手势，解除阅读锁。
        if (readingHoldRef.current) {
          readingHoldRef.current = false;
          const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
          if (distance <= STICK_PX) setStick(true);
        }
        return;
      }
      if (el.scrollHeight - el.clientHeight <= STICK_PX) return;
      setStick(false);
      setMoreBelow(true);
    };
    el.addEventListener("wheel", onWheel, { passive: true });
    return () => el.removeEventListener("wheel", onWheel);
  }, [refreshBelow, setStick]);

  /*
   * 切换对话：重置滑底标志。
   *
   * 本副作用必须声明在钉底副作用之前：React 按声明顺序执行，否则钉底会先读到
   * 尚未重置的标志位而直接跳过。
   */
  useEffect(() => {
    readingHoldRef.current = false;
    setStick(true);
  }, [conversationId, setStick]);

  /*
   * 自动滚底（钉底）。
   *
   * 刻意不做“一次性”：切换对话/恢复时，输入区、上下文环、大段文本的布局会在
   * 随后几帧才落定，只钉一次会差一截。这里在 items 变化后跟一段收敛窗口，
   * 期间逐帧把滚动位置压到底（用户一上滑即停）。
   *
   * 必须用 useLayoutEffect：流式输出时 React 提交后浏览器会先按旧 scrollTop
   * 画一帧；若等到 useEffect 再钉底，画面会“先被新内容推下去、再被拉回来”，
   * 高频 chunk 下就是可见的上下抖动。布局阶段先写 scrollTop，用户看不到中间帧。
   */
  useLayoutEffect(() => {
    if (userScrolledUpRef.current) return;
    let raf = 0;
    const deadline = performance.now() + PIN_SETTLE_MS;
    const tick = () => {
      if (userScrolledUpRef.current) return;
      if (windowResizingRef.current) return;
      // 这里不再用 lastViewportRef 与当前视口比较：ResizeObserver 记录的是
      // 最后一帧观察值，窗口在 Tauri 里异步落定后可能永远差 1px，导致
      // 每次流式更新都提前 return，用户回到底部也保持不住。
      pinNow();
      if (performance.now() < deadline) raf = requestAnimationFrame(tick);
    };
    tick();
    // 首次钉底后：下方必然无内容——直接写死，不必读几何
    setMoreBelow(false);
    return () => cancelAnimationFrame(raf);
  }, [items, conversationId, pinNow]);

  /*
   * 对话区自身尺寸变化时保持沉底。
   *
   * 输入框随内容变长展开（高度是动画）会逐帧压缩对话区高度：此时 items 没变，
   * 上面的钉底副作用不会触发，最后一屏内容就被输入框挤出视口。这里盯列表自身
   * 尺寸，变化即重钉；用户已上滑（翻历史）时不动。
   */
  useEffect(() => {
    const el = listRef.current;
    if (!el || typeof ResizeObserver !== "function") return;
    const ro = new ResizeObserver(() => {
      const w = window.innerWidth;
      const h = window.innerHeight;
      const face = readFaceSize();
      const prev = lastViewportRef.current;
      const viewportChanged = prev.w !== 0 && (prev.w !== w || prev.h !== h);
      const faceChanged =
        lastFaceSizeRef.current !== "" && lastFaceSizeRef.current !== face;
      lastViewportRef.current = { w, h };
      lastFaceSizeRef.current = face;
      observedScrollHeightRef.current = el.scrollHeight;

      if (viewportChanged || faceChanged) {
        // UI 在缩放：记锚（一次缩放会话只记一次，窗结束释放）。
        if (!anchorRef.current) captureAnchor();
        armResizeGuard();
      }
      if (windowResizingRef.current) {
        restoreAnchor();
        return;
      }
      // 列表自身盒子变了（输入框展开/回落）⇒ 可见高度变 ⇒ 几何事实重判
      refreshBelow();
      if (userScrolledUpRef.current) return;
      pinNow();
      setMoreBelow(false);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [pinNow, armResizeGuard, captureAnchor, refreshBelow, restoreAnchor]);

  /*
   * 内容尺寸观测（2026-10-08 补）。
   *
   * 内容在滚动容器内部变高/变矮时不会产生滚动事件；ResizeObserver 观测的是
   * 容器自身，items 也没变。用 MutationObserver 补这个信号。
   */
  useEffect(() => {
    const el = listRef.current;
    if (!el || typeof MutationObserver !== "function") return;
    const mo = new MutationObserver(() => refreshBelow());
    mo.observe(el, { childList: true, subtree: true, characterData: true });
    return () => mo.disconnect();
  }, [refreshBelow]);

  return { listRef, moreBelow, following, scrollToBottomNow, stopFollowing, handleScroll };
}
