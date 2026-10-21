// src/components/content/MessageList.tsx
// 消息列表 · 核心展示组件（架构升级 · 模块 1：投影缓存渲染）
//
// 职责：将 turns（已归档轮次的 SessionEvent 序列）+ currentEvents
//       （进行中轮次的实时投影）按时间线交错排列为统一的展示项列表，
//       渲染消息气泡和工具事件卡片。
//       智能滚动：流式输出期间尊重用户手动滚动位置。
//       右键菜单：拦截浏览器默认菜单，仅保留复制功能。
//
// 消息气泡/事件卡片全部由投影函数派生（src/projection.ts），
// 不再维护独立的 messages 数组。

import { memo, useMemo, useRef, useEffect, useCallback, useState } from "react";
import type { SessionEvent } from "../../types";
import type { CompactionMeta } from "../../types";
import { buildArchivedItems, buildLiveItems } from "./messageItems";
import type { DisplayItem } from "./messageItems";
import { AttachmentStrip } from "./AttachmentStrip";
import { EventBadge } from "./EventBadge";
import { Markdown } from "./Markdown";
import { WelcomeMark } from "./WelcomeMark";
import { registerMessageScrollHost } from "./messageScroll";
import styles from "./MessageList.module.css";

/**
 * 压缩点提示卡（模块 D · 上下文衰减）。
 *
 * 本地条目，非 LLM 消息：turns 全量保留（回看自由），本卡标记「从这一位置起
 * 之前的对话已压缩归档」——压缩点之后为模型当前可见的保留对话。
 * 展开可读压缩摘要（LLM checkpoint 正文）。
 */
function CompactionNotice({
  foldedCount,
  summary,
}: {
  foldedCount: number;
  summary: string;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className={styles.compactionCard} data-compaction-card>
      <button
        type="button"
        className={styles.compactionHeader}
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        <span>上下文已压缩 · {foldedCount} 轮并入摘要</span>
        <span className={styles.compactionChevron} aria-hidden>
          {open ? "▾" : "▸"}
        </span>
      </button>
      <div className={styles.compactionDesc}>
        被压缩的对话已并入上方摘要，不再进入模型上下文（原历史保留，仅供回看）；
        从下一条消息起为当前对话。
      </div>
      {open && (
        <pre className={styles.compactionSummary} data-compaction-summary>
          {summary}
        </pre>
      )}
    </div>
  );
}

/** 首部连续错误轮的折叠记录（不删除数据，只是不再占据对话首屏）。 */
function PreflightNotice({
  count,
  errors,
}: {
  count: number;
  errors: string[];
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className={styles.preflightCard} data-preflight-card>
      <button
        type="button"
        className={styles.preflightHeader}
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        <span>启动阶段失败 {count} 次 · 已恢复</span>
        <span className={styles.compactionChevron} aria-hidden>
          {open ? "▾" : "▸"}
        </span>
      </button>
      {open && (
        <div className={styles.preflightErrors}>
          {errors.map((message, index) => (
            <div key={index}>{message}</div>
          ))}
        </div>
      )}
    </div>
  );
}

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

// ── 单行渲染 ──

/**
 * 一行展示项（消息气泡 / 事件卡 / pending / 压缩卡）。
 *
 * memo 的前提是归档 item 引用跨刷新稳定（buildArchivedItems 按 turns 缓存 +
 * 单轮投影缓存）：流式刷新时归档行整段跳过重渲染，只有当前轮的行会更新。
 */
const MessageRow = memo(function MessageRow({
  item,
  onCancelPending,
}: {
  item: DisplayItem;
  onCancelPending?: (entryId: string) => void;
}) {
  switch (item.type) {
    case "welcome":
      return (
        <div className={styles.welcome}>
          <WelcomeMark />
        </div>
      );
    case "events":
      return (
        <EventBadge
          events={item.events}
          interactive={item.interactive}
          showApprovalControls={false}
          className={styles.row}
        />
      );
    case "message":
      if (item.role === "assistant") {
        return (
          <div
            data-message-area
            className={`${styles.bubble} ${styles.assistant} ${styles.row}`}
          >
            {item.segments.map((seg, segIdx) => (
              <div
                key={segIdx}
                className={seg.transition ? styles.streaming : undefined}
              >
                <Markdown content={seg.content} />
                {item.isStreaming && segIdx === item.segments.length - 1 && (
                  <span className={styles.cursor} />
                )}
              </div>
            ))}
          </div>
        );
      }
      // M3-B：轮中止标记条（灰/斜体小字，独立样式——不走 error 分支；
      // 「正在中止…」与「该轮已中止」同一套样式，前者是过程、后者是结果）
      if (item.role === "aborted" || item.role === "aborting") {
        return (
          <div
            data-message-area
            className={`${styles.bubble} ${styles.aborted} ${styles.row}`}
          >
            {item.content}
          </div>
        );
      }
      return (
        <div
          data-message-area
          className={`${styles.bubble} ${styles[item.role]} ${styles.row}`}
        >
          {item.role === "user" && item.attachments && item.attachments.length > 0 && (
            <AttachmentStrip files={item.attachments} variant="message" />
          )}
          <Markdown content={item.content} />
        </div>
      );
    case "pending":
      return (
        <button
          type="button"
          className={`${styles.steerPending} ${styles.row}`}
          title="点击取消"
          data-steer-pending
          onClick={() => onCancelPending?.(item.id)}
        >
          {item.text}
        </button>
      );
    case "preflight":
      return <PreflightNotice count={item.count} errors={item.errors} />;
    case "compaction":
      return (
        <CompactionNotice
          foldedCount={item.foldedCount}
          summary={item.summary}
        />
      );
  }
});

// ── 组件 ──

interface Props {
  turns: SessionEvent[][];
  currentEvents: SessionEvent[];
  /** 当前没有对话（草稿态 / 首次使用）→ 对话区呈现欢迎字标 */
  showWelcome: boolean;
  /** 中止已发出、轮未收尾（2026-10-08）：轮尾追加一条「正在中止…」 */
  aborting?: boolean;
  /** 压缩记录（模块 D；undefined = 未压缩过）。决定压缩点提示卡位置与摘要 */
  compaction?: CompactionMeta;
  /** 当前对话的插话 pending（待注入气泡；跨轮留存，undefined/空 = 无） */
  pending?: readonly { id: string; text: string }[];
  /** pending 气泡点击取消（cancel_steer 成功移除后消失） */
  onCancelPending?: (entryId: string) => void;
  /** 当前对话 id：变化即视为切换对话（重置滑底标志，保证新对话能自动沉底） */
  conversationId?: string | null;
}

export function MessageList({ turns, currentEvents, showWelcome, compaction, pending, onCancelPending, conversationId, aborting = false }: Props) {
  const listRef = useRef<HTMLDivElement>(null);

  /*
   * 跟底意图（ref）：true = 用户已离开底部，停止自动钉底。
   *
   * 用 useRef 而非 useState：滚动事件回调里不需要触发重渲染；它只在自动钉底
   * 逻辑（useEffect）里被读。
   *
   * 与下面的 moreBelow 是两件事：这里记「意图」（还跟不跟底），moreBelow 记
   * 「事实」（视口下方此刻还有没有内容）。2026-10-08 之前只有一个 atBottom
   * 同时回答两个问题，而只有意图那半有维护机制——内容在滚动容器内部变矮不产生
   * 任何事件（见「内容尺寸观测」），于是卡片收起后按钮永久占位（实测缺口）。
   */
  const userScrolledUpRef = useRef(false);

  /*
   * 下方还有内容（几何事实）：按钮的唯一判据。
   *
   * 只在刷新点读一次「离底距离」，结论变了才 setState。刷新点覆盖所有可能改变
   * 几何的地方：滚动事件、内容尺寸变化、滚轮、列表自身尺寸变化、缩放窗结束。
   *
   * 为什么按钮只认事实：按钮的语义是「回到底部」，只有下方真有内容（离底距离
   * > STICK_PX）时它才可点得出结果。此前它由意图闩锁驱动，于是出现过「内容
   * 缩回、按钮仍在、点击也无事可做」的假阳性（2026-10-08 实测）。事实为真 ⇒
   * 按钮一定有意义，这条由构造保证。
   */
  const [moreBelow, setMoreBelow] = useState(false);

  /** 跟底意图的唯一写入口（ref，热路径读；不触发重渲染） */
  const setStick = useCallback((stick: boolean) => {
    userScrolledUpRef.current = !stick;
  }, []);

  /* 滚动写入白名单 / 观测记录（照 Hanako 的两条通用判据）
   *   observedScrollHeightRef —— 最近观测的 scrollHeight：事件到达时它已变 ⇒
   *   这次滚动是内容尺寸变化引起的钳位/锚定（浏览器行为），也不是用户。
   *   （曾另有一条“写入白名单”：记下自己写过的落点、滚动事件与之相符就忽略。
   *    它会在“用户之后滚到同一位置”时永远吞掉判定，已删——见 handleScroll。）*/
  const observedScrollHeightRef = useRef<number | null>(null);

  /*
   * ── 滚动所有权（谁可以写列表的滚动位置）──
   *
   * 全部写入点就下面四处；新增任何“自动滚动”前，先回答它属于哪一条：
   *   1. items 变化（新消息/归档/切对话）→ 钉底（带 PIN_SETTLE_MS 收敛窗；
   *      用户已上滑则不钉）；
   *   2. 输入框尺寸回落/展开等“内容区被挤压” → ResizeObserver 重钉底；
   *   3. 窗口缩放 → 不写（保护窗由 resize 事件与视口尺寸变化共同开启；
   *      滚动事件同样忽略，意图改由滚轮手势判定；见下）；
   *   4. 输入框布局测量（messageScroll.ts）→ 测量期间接管，贴底续贴底、
   *      否则原位。
   * 用户上滑（离底 > STICK_PX）立即停自动钉底；滑回底部恢复。
   *
   * 第 5 条（2026-10-08 补）：内容尺寸变化 → 只更新「下方还有没有内容」这个
   * 事实（按钮判据），不动滚动位置；见下面的内容观测副作用。
   */

  /* 钉底原语：直接写 scrollTop = scrollHeight（浏览器自行钳到最大）。 */
  const pinNow = useCallback(() => {
    const el = listRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, []);

  /** 回到最新（“回到底部”按钮）：恢复跟随 + 瞬时落底。
   *  落底后「下方还有内容」必然为假——直接写死，省一次布局读。 */
  const scrollToBottomNow = useCallback(() => {
    setStick(true);
    pinNow();
    setMoreBelow(false);
  }, [pinNow, setStick]);

  /*
   * 窗口缩放保护窗。
   *
   * 缩放是用户在调整视图，不是内容变化——若在缩放中钉底，对话内容会随缩放
   * 手势上下滚动（2026-09 实测反馈）。
   *
   * 触发来源有两个，任取其一：
   *   1. window resize 事件（通常可用）；
   *   2. **视口尺寸变化观察**（ResizeObserver 回调里比 innerWidth/innerHeight）
   *      ——Tauri/WebView2 下 resize 事件的到达时机不可靠，只靠事件会出现
   *      “保护窗从未打开、缩放中照旧钉底”；尺寸比较不依赖事件也不依赖顺序。
   */
  const windowResizingRef = useRef(false);
  const resizeGuardTimerRef = useRef(0);

  /*
   * 几何事实的读入口：量一次「离底距离」并写回 moreBelow（只在结论变化时更新）。
   *
   * 判定顺序与 handleScroll 同构：先给「已经在底部」让路，再谈屏蔽。
   * 缩放保护窗期间只挡「有内容在下方」的判定（重排会改距离，此刻读到的大不
   * 可信）；「已回到贴底」（距离 ≤ STICK_PX）不需要前提，任何时候都成立——
   * 屏蔽它就会出现「滚到底按钮也不消失」的老毛病（2026-09 教训）。
   */
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

  /** 把锚行送回原来的偏移处（缩放中每次尺寸变化后调）。
   *  界面整体随 --face-size 缩放：窗口一变，每行都要重排、整篇高度随之变，
   *  而滚动位置是像素值——不校正的话，视野里的内容会整体挪位。 */
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
      // 缩放结束、重排已定：把几何事实重判一次（缩放期间 refreshBelow 被挂起）
      refreshBelow();
    }, WINDOW_RESIZE_GUARD_MS);
  }, [refreshBelow]);

  useEffect(() => {
    window.addEventListener("resize", armResizeGuard);
    return () => {
      window.removeEventListener("resize", armResizeGuard);
      window.clearTimeout(resizeGuardTimerRef.current);
    };
  }, [armResizeGuard]);

  /* 提供给输入框的“稳定测量窗口”：测量期间接管滚动行为（见 messageScroll.ts）。
   * 贴底则测量后仍贴底；否则原位不动——避免输入框复位测量把滚动位置钳下去。 */
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

  // ── 展示项派生 ──
  //
  // 归档部分只在 turns / compaction / showWelcome 变化时重算（内部再按轮缓存
  // 投影），流式刷新的每次 currentEvents 变更不再重算整段历史；进行中部分
  // 每次刷新重算，但规模限于当前轮。
  const archivedItems = useMemo(
    () => buildArchivedItems(turns, showWelcome, compaction),
    [turns, showWelcome, compaction]
  );
  const liveItems = useMemo(
    () => buildLiveItems(currentEvents, pending, aborting),
    [currentEvents, pending, aborting]
  );
  // 无进行中内容时直接复用归档数组（引用稳定 → 不触发多余的滚底副作用）
  const items = useMemo(
    () =>
      showWelcome
        ? archivedItems
        : liveItems.length > 0
          ? [...archivedItems, ...liveItems]
          : archivedItems,
    [archivedItems, liveItems, showWelcome]
  );

  /*
   * 用户发送新消息时强制重置滚底标志。
   *
   * 判断依据：新轮次归档（turns 增长），或进行中轮次的
   * 第一条事件（合成 user/message）出现。无论用户之前在哪个
   * 历史位置，发送新消息意味着"我想看刚才发的和接下来的回复"，
   * 此时自动滚底不应该被之前的 scrollUp 标志阻塞。
   */
  const prevTurnCountRef = useRef(turns.length);
  useEffect(() => {
    const turnsGrew = turns.length > prevTurnCountRef.current;
    const last = currentEvents[currentEvents.length - 1];
    if (turnsGrew || last?.type === "user/message") {
      setStick(true);
    }
    prevTurnCountRef.current = turns.length;
  }, [turns, currentEvents, setStick]);

  /*
   * 滚动事件处理。
   *
   * 用户向上滚动（离底 > 5px）→ 停止自动跟底；滑回距底 ≤ 5px → 恢复跟底。
   * 判据用“离底距离”而非“位置是否变小”：浏览器因布局变化钳位滚动位置时，
   * 落点就是当时的最大滚动量，即仍然贴底——不该被当成用户上滑。
   *
   * 缩放期间直接忽略：重排会让离底距离变大，那不是用户意图（意图由滚轮手势
   * 单独判定，见下）。这里除了保护窗，还额外比一次缩放基准（--face-size）与
   * 视口尺寸——scroll 事件先于 ResizeObserver 回调到达，保护窗可能还没打开。
   */
  const handleScroll = useCallback(() => {
    const el = listRef.current;
    if (!el) return;
    // 几何事实先对齐：下面的屏蔽只针对「意图」的推断，不该拦事实
    refreshBelow();
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    /*
     * 判定顺序（结构不变量）：先给“回到贴底”让路，再谈屏蔽。
     *
     * “已经在底部”是唯一不需要前提的判据（重排/钳位/自己写的回声都
     * 不可能把距离变成 0）——所以它放在最前面，不受下面任何提前返回影响。
     * 后面的屏蔽逻辑只能拦住“离开贴底”的判定，拦不住恢复。
     *
     * 之前的顺序相反（先屏蔽、后判定），于是保护窗开着、或基准记录过期时，
     * 连“滚到底”也会被吞掉：表现为滚到底按钮不消失、只有点击才恢复，
     * 而且成不成要看滚动事件落在保护窗内还是窗外（不稳定）——2026-09 实测。
     */
    if (distance <= STICK_PX) {
      setStick(true);
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
   * 缩放保护窗期间滚动事件不可信（重排会改距离），但手势本身可信——这两条
   * 合起来才能既“缩放不动内容”，又不错过用户在缩放中真的上滑了。
   *
   * 前提：可滚范围真实存在。“离开底部”的引用对象是“还没看到的更早内容”——
   * 内容整体放得下时（欢迎页、短对话），上滑不移动任何像素，翻标志只会凭空
   * 点亮“回到最新”（2026-09 实测）。所以先量几何，无可滚范围直接忽略。
   */
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      if (e.deltaY >= 0) {
        // 下滚：位置由 scroll 事件回报，这里只顺手重判一次事实（滚到底、或
        // 内容已放得下时按钮该消失，而那种情形恰好不会有滚动事件）。
        refreshBelow();
        return;
      }
      // 上滑：溢出守卫已证明有可滚范围，「离开底部」即意味着下方还有内容。
      // 缩放保护窗期间滚动事件不可信，这条手势通道必须独立成立（2026-09）。
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
   * 上一段对话“上滑过”的状态会随 ref 带到下一段，导致新对话完全不自动沉底。
   * 本副作用必须声明在钉底副作用之前：React 按声明顺序执行，否则钉底会先读到
   * 尚未重置的标志位而直接跳过。
   */
  useEffect(() => {
    setStick(true);
  }, [conversationId, setStick]);

  /*
   * 自动滚底（钉底）。
   *
   * 刻意不做“一次性”：切换对话/恢复时，输入区、上下文环、大段文本的布局会在
   * 随后几帧才落定，只钉一次会差一截——表现为“比较接近于底部但不在底部”。
   * 这里在 items 变化后跟一段收敛窗口，期间逐帧把滚动位置压到底（用户一上滑
   * 即停）。
   */
  useEffect(() => {
    if (userScrolledUpRef.current) return;
    let raf = 0;
    const deadline = performance.now() + PIN_SETTLE_MS;
    const tick = () => {
      if (userScrolledUpRef.current) return;
      if (windowResizingRef.current) return;
      const vp = lastViewportRef.current;
      if (vp.w !== 0 && (vp.w !== window.innerWidth || vp.h !== window.innerHeight)) {
        return;
      }
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
   *
   * 但“尺寸变化”有两种来源：输入框（该钉）与窗口缩放（不该钉）。判据是本次
   * 回调里视口尺寸是否变了——变了就是窗口在缩放，开保护窗并跳过。
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
        // 不能用“保护窗是否已开”来判定——滚动事件（通常来自我们自己刚写过的
        // 位置）可能先一步开窗，那样就永远记不上锚。
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
   * 缺口：内容在滚动容器内部变高/变矮时，一个事件都不会来——scroll 只在滚动
   * 位置变化时触发（卡片收起时 scrollTop 本就停在 0，不需要钳位），上面的
   * ResizeObserver 观测的是容器自身（盒子由外层 flex 定死，内容变化不动它），
   * items 也没变（卡片展开/收起是 EventBadge 的局部 state）。于是「下方还有
   * 没有内容」无人维护：按钮挂在屏幕上、点了也无事可做（实测缺口）。
   *
   * 用 MutationObserver 补这个信号：本应用里内容高度变化都来自 DOM 变更
   *（流式文本、卡片展开/收起、Markdown 迟到渲染）。已知盲区：纯样式引起的
   * 尺寸变化（图片/字体迟到排版）不产生 DOM 变更——这条路径当前无受害面
   *（附件是定尺寸缩略图，字体随包本地加载），滚动/钉底/缩放窗结束时的重判
   * 也仍会兜住。
   */
  useEffect(() => {
    const el = listRef.current;
    if (!el || typeof MutationObserver !== "function") return;
    const mo = new MutationObserver(() => refreshBelow());
    mo.observe(el, { childList: true, subtree: true, characterData: true });
    return () => mo.disconnect();
  }, [refreshBelow]);

  return (
    <div className={styles.listArea}>
      <div className={styles.list} ref={listRef} onScroll={handleScroll} data-message-list>
        {items.map((item, i) => (
          // 回调只传给 pending 行：归档行拿到恒为 undefined 的稳定属性，
          // 不会因为 ChatView 里 handleCancelSteer 换引用而整段重渲染
          <MessageRow
            key={i}
            item={item}
            onCancelPending={item.type === "pending" ? onCancelPending : undefined}
          />
        ))}
      </div>
      {/* 回到底部（外观与位置照 Hanako：圆形、水平居中、浮在输入区上方）。
          判据是几何事实 moreBelow（视口下方还有内容）：按钮只在点得出结果时
          出现；欢迎页（showWelcome）无对话，即便被压出溢出（极小窗口）也不出现。 */}
      {!showWelcome && moreBelow && (
        <button
          type="button"
          className={styles.scrollFab}
          aria-label="回到最新"
          title="回到最新"
          onClick={scrollToBottomNow}
        >
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <polyline points="6 9 12 15 18 9" />
          </svg>
        </button>
      )}
    </div>
  );
}
