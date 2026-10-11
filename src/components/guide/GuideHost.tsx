// src/components/guide/GuideHost.tsx
// 说明层宿主（应用内引导）· 一套机制、两种形态
//
// 与项目里的覆盖层（设置 / 历史）同一套做法：**挂在正面内部**（`.frontFace`，
// position:absolute）。这样压暗天然被正面的 overflow 与坐标体系管住，不会铺到
// 窗口的透明区；洞与卡片的坐标用局部值（offset*），立方体做情绪动作
// （WAAPI translateY / rotate）时仍严格对齐。
//
// 交互是 modal（与遮罩一致）：引导期间挡住后面的界面，点遮罩 = 下一步 / 知道了，
// 卡片内点击不冒泡，滚轮不外泄。**挡住交互是为了正确性**——锚点不会在引导脚下
// 跑掉。同时留两个随时退出的出口：卡片上的「跳过」、以及用户一动键盘。
//
// 展示哪一项由**当前表面**决定（对话 / 设置面板），不是一律看队首——队列里
// 属于别的表面的项原地等着，等它那个表面回来再出。
//
// 兜底是硬约束：每步进入量一次锚点，ResizeObserver 挂锚点 / 正面 / 根随尺寸重算；
// **量不到就整步跳过**（不猜位置、不留半张气泡）。

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  FACE_ANCHOR,
  GUIDE_ITEMS,
  NOTICE_ANCHOR,
  type GuideSurface,
} from "./guideContent";
import { pickActiveGuideKey, useGuideStore } from "../../stores/useGuideStore";
import { useUIStore } from "../../stores/useUIStore";
import styles from "./GuideHost.module.css";

interface Box {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** 会话焦点卡宽度（正面宽 400，268 留出余量） */
const FOCUS_CARD_W = 268;

/** 引导卡出现前的延迟（无动画可等的场合）：一帧两帧就够，别让页面内容被盖。 */
const GUIDE_DELAY_MS = 200;

/**
 * 带 waitFor 时的最低停顿：等动画本身已经是一段过渡式等待，只需给它一个落点，
 * 不再叠加固定延迟（Air 2026-10-11：衔接应当快，动画的收尾已经平缓）。
 */
const GUIDE_TAIL_FLOOR_MS = 120;

/** waitFor 取样间隔 / 稳定判定 / 上限（按取样次数计时，不读 performance.now，假定时器下也确定） */
const SETTLE_SAMPLE_MS = 60;
const SETTLE_QUIET_SAMPLES = 2;
const SETTLE_MAX_SAMPLES = 30;

/**
 * 等这个元素**有尺寸且尺寸稳定**（连续若干次取样一致），最多 SETTLE_MAX_SAMPLES 次。
 *
 * 返回 false = 一直没能量到尺寸（始终为 0）→ 调用方跳过整步。
 *
 * 为什么必须要求非零：标题的逐字展开**从空串开始**（宽度 0），那一刻量不到；
 * 若把"量不到"当成"没有锚点"而跳过，整条引导会凭空消失
 * （2026-10-11 实测：标题引导直接被跳过、跳到了进度环）。等它长出来再落位。
 * 元素不存在时返回 true——那种情况由入口检查负责跳过。
 */
async function anchorSettled(key: string): Promise<boolean> {
  if (!document.querySelector(`[data-guide="${key}"]`)) return true;
  let last = "";
  let same = 0;
  for (let i = 0; i < SETTLE_MAX_SAMPLES; i++) {
    const r = document
      .querySelector<HTMLElement>(`[data-guide="${key}"]`)
      ?.getBoundingClientRect();
    const w = Math.round(r?.width ?? 0);
    const h = Math.round(r?.height ?? 0);
    const sig = `${w}x${h}`;
    if (w > 0 && h > 0 && sig === last) {
      same += 1;
      if (same >= SETTLE_QUIET_SAMPLES) return true;
    } else {
      same = 0;
      last = sig;
    }
    await new Promise<void>((res) => {
      window.setTimeout(res, SETTLE_SAMPLE_MS);
    });
  }
  const last_ = document
    .querySelector<HTMLElement>(`[data-guide="${key}"]`)
    ?.getBoundingClientRect();
  return (last_?.width ?? 0) > 0 && (last_?.height ?? 0) > 0;
}

/** 通知卡与子页内容区边缘的间距 */
const NOTICE_INSET = 8;

/** 把元素在正面内的局部矩形算出来（正面的 padding box 为原点）。 */
function localBox(el: HTMLElement, face: HTMLElement): Box | null {
  const w = el.offsetWidth;
  const h = el.offsetHeight;
  if (w > 0 && h > 0) {
    // 首选布局值：offset* 不受外壳变换影响，立方体做情绪动作时仍严格对齐
    let left = 0;
    let top = 0;
    let node: HTMLElement | null = el;
    while (node && node !== face) {
      left += node.offsetLeft;
      top += node.offsetTop;
      node = node.offsetParent as HTMLElement | null;
    }
    if (node === face) return { left, top, width: w, height: h };
  }
  // 退化：视觉矩形相减（jsdom 等无布局引擎的场合走这条）
  const r = el.getBoundingClientRect();
  if (r.width <= 0 || r.height <= 0) return null;
  const f = face.getBoundingClientRect();
  return {
    left: r.left - f.left,
    top: r.top - f.top,
    width: r.width,
    height: r.height,
  };
}

/** 正面的局部尺寸（夹取卡片用）。 */
function faceBox(face: HTMLElement): { w: number; h: number } {
  if (face.offsetWidth > 0 && face.offsetHeight > 0) {
    return { w: face.offsetWidth, h: face.offsetHeight };
  }
  const r = face.getBoundingClientRect();
  return { w: r.width, h: r.height };
}

/** 气泡摆位：优先锚点下方，放不下翻到上方；水平居中并夹在正面内。 */
function place(
  anchor: Box,
  card: { w: number; h: number },
  face: { w: number; h: number }
): { left: number; top: number } {
  const GAP = 10;
  const EDGE = 8;
  const below = anchor.top + anchor.height + GAP;
  const above = anchor.top - GAP - card.h;
  const top =
    below + card.h <= face.h - EDGE
      ? below
      : above >= EDGE
        ? above
        : Math.max(EDGE, face.h - card.h - EDGE);
  let left = anchor.left + anchor.width / 2 - card.w / 2;
  left = Math.max(EDGE, Math.min(left, face.w - card.w - EDGE));
  return { left, top };
}

export function GuideHost() {
  const overlay = useUIStore((s) => s.overlay);
  // 当前表面：对话界面 / 设置面板。历史等其它面板：不展示任何引导。
  const surface: GuideSurface | null =
    overlay === null ? "chat" : overlay === "settings" ? "settings" : null;
  const activeKey = useGuideStore((s) => pickActiveGuideKey(s.queue, surface));
  const advance = useGuideStore((s) => s.advance);
  const skip = useGuideStore((s) => s.skip);
  const enqueue = useGuideStore((s) => s.enqueue);
  const exit = useGuideStore((s) => s.exit);
  const tourKeys = useGuideStore((s) => s.tourKeys);
  const item = activeKey !== null ? GUIDE_ITEMS[activeKey] : undefined;

  const rootRef = useRef<HTMLDivElement>(null);
  const cardRef = useRef<HTMLDivElement>(null);
  const [box, setBox] = useState<Box | null>(null);
  const [face, setFace] = useState<{ w: number; h: number } | null>(null);
  const [card, setCard] = useState<{ w: number; h: number } | null>(null);

  // 延后出现：量好位置先不显示（visibility: hidden 仍然参与布局），到点再露。
  // 带 waitFor 的（欢迎卡）：等那个元素的入场动画演完即可，只给一个 120ms 落点——
  // 动画本身就是过渡式等待，再叠加固定延迟就会显得"动画完了还不出"。
  // **同一个表面上的换步不重新等**：否则每点一次「下一步」整层都会消失一下再
  // 回来，用户看到的是闪一下（Air 2026-10-11）。
  const [shown, setShown] = useState(false);
  const shownSurfaceRef = useRef<GuideSurface | null>(null);
  useEffect(() => {
    if (activeKey === null) {
      setShown(false);
      shownSurfaceRef.current = null;
      return;
    }
    if (shownSurfaceRef.current !== null && shownSurfaceRef.current === item?.surface) {
      setShown(true);
      return;
    }
    setShown(false);
    let alive = true;
    const floor = item?.waitFor ? GUIDE_TAIL_FLOOR_MS : GUIDE_DELAY_MS;
    const base = new Promise<void>((r) => {
      window.setTimeout(r, floor);
    });
    const wait = item
      ? anchorSettled(
          item.waitFor ??
            (item.form === "focus" ? item.anchor : NOTICE_ANCHOR) ??
            ""
        )
      : Promise.resolve(true);
    void Promise.all([base, wait]).then(([, ready]) => {
      if (!alive) return;
      // 等满上限仍然量不到尺寸 → 整步跳过（有界，不会挂着不动）
      if (ready === false) {
        skip(activeKey);
        return;
      }
      shownSurfaceRef.current = item?.surface ?? null;
      setShown(true);
    });
    return () => {
      alive = false;
    };
  }, [activeKey, item]);

  // 等的那会儿用户已经动手了（点了设置 / 聚焦输入）→ 这次不弹。
  // 引导不该在用户已经开头做事时盖上来；这一项**保持未看过**，等它那个表面下次
  // 再出现时再出（skip 不写 seen）。只认 pointerdown / focusin：那才是"动手"，
  // 键盘敲击可能只是路过（且 Esc 另有退场通道）。
  useEffect(() => {
    if (activeKey === null || shown) return;
    const onEngage = () => skip(activeKey);
    document.addEventListener("pointerdown", onEngage, true);
    document.addEventListener("focusin", onEngage, true);
    return () => {
      document.removeEventListener("pointerdown", onEngage, true);
      document.removeEventListener("focusin", onEngage, true);
    };
  }, [activeKey, shown, skip]);

  /** 重量卡片尺寸（换步 / 缩放都会变）：值没变就不 setState，避免自激。 */
  const measureCard = useCallback(() => {
    const el = cardRef.current;
    if (!el) return;
    const w = el.offsetWidth || el.getBoundingClientRect().width;
    const h = el.offsetHeight || el.getBoundingClientRect().height;
    if (w <= 0 || h <= 0) return;
    setCard((c) => (c && c.w === w && c.h === h ? c : { w, h }));
  }, []);

  // 量锚点：每步进入量一次；随锚点 / 正面 / 视口尺寸变化重算；量不到整步跳过。
  useEffect(() => {
    if (!item || activeKey === null) {
      setBox(null);
      setFace(null);
      return;
    }
    const faceEl = document.querySelector<HTMLElement>(`[data-guide="${FACE_ANCHOR}"]`);
    if (!faceEl) {
      skip(activeKey);
      return;
    }
    // 锚点：优先用该项自己声明的（欢迎卡锚在整面），设置页通知则缺省挂内容区
    const key = item.anchor ?? (item.form === "focus" ? undefined : NOTICE_ANCHOR);
    if (!key) {
      skip(activeKey);
      return;
    }
    const el = document.querySelector<HTMLElement>(`[data-guide="${key}"]`);
    // **不存在**才在入口跳过。存在但还没尺寸（如标题刚开始逐字）不在这里判死刑——
    // 交给稳定等待，那边会等它长出来（2026-10-11 实测：混为一谈会把整条引导跳过）。
    if (!el) {
      skip(activeKey);
      return;
    }
    const apply = () => {
      if (!el.isConnected || !faceEl.isConnected) {
        skip(activeKey);
        return;
      }
      setBox(localBox(el, faceEl));
      const f = faceBox(faceEl);
      setFace(f.w > 0 && f.h > 0 ? f : null);
      measureCard();
    };
    apply();
    const ro = new ResizeObserver(apply);
    ro.observe(el);
    ro.observe(faceEl);
    ro.observe(document.documentElement);
    window.addEventListener("resize", apply);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", apply);
    };
  }, [activeKey, item, skip, measureCard]);

  // 每次渲染后校准卡片尺寸（在 paint 前完成，摆位不会闪）
  useLayoutEffect(() => {
    measureCard();
  });

  // 只有 Esc 退场：曾用"任意键即退"，但用户随手一按就把没读完的引导关掉、还被永久
  // 记成看过（2026-10-11 Air 实测）。Esc 是标准 dismiss 键，不会误触。
  useEffect(() => {
    if (activeKey === null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") exit(activeKey);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [activeKey, exit]);

  // 挡住滚轮：引导期间后面的内容不该动（动了锚点就跑了）
  useEffect(() => {
    const el = rootRef.current;
    if (!el || activeKey === null) return;
    const onWheel = (e: WheelEvent) => e.preventDefault();
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [activeKey, box]);

  // 点遮罩不消费：只有卡片上的按钮与 Esc 能关掉它。
  // 曾经"点遮罩 = 下一步"（对齐覆盖层的"点遮罩关闭面板"），但引导这次点击的意图
  // 很可能是"我想去点别处"，结果把没读完的引导消耗掉、还永久记成看过（Air 实测）。

  /** 主按钮：推进一步；若该步声明了 startKeys（欢迎卡 → 三步），顺带把它们入队。 */
  const handlePrimary = useCallback(() => {
    if (activeKey === null) return;
    const start = GUIDE_ITEMS[activeKey]?.startKeys;
    advance(activeKey);
    if (start && start.length > 0) enqueue(start);
  }, [activeKey, advance, enqueue]);

  if (!item || !box) return null;

  const focus = item.form === "focus";
  const pos = focus && card && face ? place(box, card, face) : null;
  // 欢迎卡（带 startKeys）：它是教程入口，只有一个按钮「开始探索」。
  // 不再给「跳过」——后续每一步自己就带跳过，这里再放一个只会让人问"跳过什么"（Air 2026-10-11）。
  const starts = item.startKeys && item.startKeys.length > 0 ? item.startKeys : null;
  const primaryLabel = starts ? "开始探索" : focus ? "下一步" : "知道了";
  const showSkip = focus;
  const dotIndex =
    tourKeys && activeKey !== null ? tourKeys.indexOf(activeKey) : -1;
  // 通知卡：在子页内容区里**双向居中**。压暗目标已经换成内容区本身（导航与页标题
  // 都在暗色之外），卡就不再需要绕面板边缘做算术。宽度上限也别再卡成接近内容区的
  // 固定值——面板随窗口放大时卡得跟着居中，否则会变成靠左的一小块（Air 实测）。
  const noticeW = Math.round(
    Math.max(140, Math.min(300, box.width - NOTICE_INSET * 2))
  );
  const noticeTop = card
    ? Math.min(
        Math.max(
          box.top + (box.height - card.h) / 2,
          box.top + NOTICE_INSET
        ),
        box.top + box.height - card.h - NOTICE_INSET
      )
    : box.top + NOTICE_INSET;

  return (
    <div
      ref={rootRef}
      className={styles.root}
      data-guide-host=""
      data-guide-hidden={shown ? undefined : "1"}
    >
      {focus ? (
        <div
          className={styles.hole}
          style={{
            left: box.left,
            top: box.top,
            width: box.width,
            height: box.height,
          }}
        />
      ) : (
        <div
          className={styles.dim}
          style={{
            left: box.left,
            top: box.top,
            width: box.width,
            height: box.height,
          }}
        />
      )}

      <div
        ref={cardRef}
        className={`${styles.card} ${shown && card ? styles.cardIn : ""}`}
        onClick={(e) => e.stopPropagation()}
        style={
          focus
            ? {
                left: pos?.left ?? 0,
                top: pos?.top ?? 0,
                width: FOCUS_CARD_W,
                visibility: pos ? "visible" : "hidden",
              }
            : {
                left: box.left + box.width / 2,
                top: noticeTop,
                width: noticeW,
                transform: "translateX(-50%)",
              }
        }
      >
        {(item.title || dotIndex >= 0) && (
          <div className={styles.head}>
            {item.title && <span className={styles.title}>{item.title}</span>}
            {dotIndex >= 0 && tourKeys && (
              <span
                className={styles.dots}
                data-guide-dots={dotIndex + 1}
                aria-label={`第 ${dotIndex + 1} 步，共 ${tourKeys.length} 步`}
              >
                {tourKeys.map((k, i) => (
                  <span key={k} className={i === dotIndex ? styles.dotOn : styles.dot} />
                ))}
              </span>
            )}
          </div>
        )}
        {item.lead && <div className={styles.lead}>{item.lead}</div>}
        <div className={styles.body}>{item.body}</div>
        {item.note && <div className={styles.note}>注：{item.note}</div>}
        <div className={`${styles.actions} ${showSkip ? "" : styles.actionsCenter}`}>
          {showSkip && (
            <button
              type="button"
              className={styles.ghost}
              onClick={() => activeKey !== null && exit(activeKey)}
            >
              跳过
            </button>
          )}
          <button type="button" className={styles.primary} onClick={handlePrimary}>
            {primaryLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
