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

/** 局部坐标参照系之外的会话焦点卡宽度（正面宽 400，268 留出余量） */
const FOCUS_CARD_W = 268;

/**
 * 设置页左侧导航宽（`SettingsPanel.module.css` 的 `.sidebar`）。
 * 通知卡靠内容区一侧（右对齐）摆，不盖导航——用户才看得出"这条在解释本页"，
 * 也才认得出左边九个入口还在。
 */
const SETTINGS_NAV_W = 52;

/** 通知卡与面板边缘的间距 */
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
  const exit = useGuideStore((s) => s.exit);
  const tourKeys = useGuideStore((s) => s.tourKeys);
  const item = activeKey !== null ? GUIDE_ITEMS[activeKey] : undefined;

  const rootRef = useRef<HTMLDivElement>(null);
  const cardRef = useRef<HTMLDivElement>(null);
  const [box, setBox] = useState<Box | null>(null);
  const [face, setFace] = useState<{ w: number; h: number } | null>(null);
  const [card, setCard] = useState<{ w: number; h: number } | null>(null);

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
      advance(activeKey);
      return;
    }
    const key = item.form === "focus" ? item.anchor : NOTICE_ANCHOR;
    if (!key) {
      advance(activeKey);
      return;
    }
    const el = document.querySelector<HTMLElement>(`[data-guide="${key}"]`);
    if (!el || !localBox(el, faceEl)) {
      advance(activeKey);
      return;
    }
    const apply = () => {
      if (!el.isConnected || !faceEl.isConnected) {
        advance(activeKey);
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
  }, [activeKey, item, advance, measureCard]);

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

  // 遮罩不消费引导：只有卡片上的按钮与 Esc 能关掉它。
  // 曾经"点遮罩 = 下一步"（对齐覆盖层的"点遮罩关闭面板"），但语义相反：
  // 面板那次点击的意图是"关"，引导这次很可能是"我想去点别处"（比如左导航），
  // 结果把没读完的引导消耗掉、还永久记成看过（2026-10-11 Air 实测）。

  if (!item || !box) return null;

  const focus = item.form === "focus";
  const pos = focus && card && face ? place(box, card, face) : null;
  const dotIndex =
    tourKeys && activeKey !== null ? tourKeys.indexOf(activeKey) : -1;
  // 通知卡：贴内容区（右对齐），不盖左侧导航；宽度按"面板宽 − 导航 − 两侧间距"算。
  // 面板本身只有 0.72×正面宽（≈288px），写死一个接近它的绝对宽度会把整页盖住
  // （2026-10-11 Air 实测）。
  const noticeLeft = SETTINGS_NAV_W + NOTICE_INSET;
  const noticeW = Math.max(
    120,
    Math.min(240, box.width - noticeLeft - NOTICE_INSET)
  );

  return (
    <div
      ref={rootRef}
      className={styles.root}
      data-guide-host=""
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
        className={`${styles.card} ${card ? styles.cardIn : ""}`}
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
                left: box.left + noticeLeft,
                top: box.top + NOTICE_INSET,
                width: noticeW,
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
        <div className={styles.body}>{item.body}</div>
        {item.note && <div className={styles.note}>注：{item.note}</div>}
        <div className={styles.actions}>
          {focus && (
            <button
              type="button"
              className={styles.ghost}
              onClick={() => activeKey !== null && exit(activeKey)}
            >
              跳过
            </button>
          )}
          <button
            type="button"
            className={styles.primary}
            onClick={() => activeKey !== null && advance(activeKey)}
          >
            {focus ? "下一步" : "知道了"}
          </button>
        </div>
      </div>
    </div>
  );
}
