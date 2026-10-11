// src/components/guide/GuideHost.tsx
// 说明层宿主（应用内引导）· 一套机制、两种形态
//
// 聚焦式：给锚点套一圈超大 spread 阴影来"挖洞"（洞恒等于锚点，缩放自动跟随），
//   气泡贴锚点摆；通知式：压暗面板 + 居中说明卡。
//
// 兜底是硬约束：每步进入量一次锚点，ResizeObserver 挂锚点与根随尺寸重算；
//   **量不到就整步跳过**（不猜位置、不留半张气泡）。用户一动键盘即退。
//
// portal 到 body：外壳是 3D 变换的，position:fixed 在变换祖先里会失基准并被裁
// （与 ToastHost / 右键菜单同一原因）。
//
// 宿主本身 pointer-events:none —— 被高亮的锚点仍然可点，界面不被挡住。

import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { GUIDE_ITEMS, NOTICE_ANCHOR } from "./guideContent";
import { useActiveGuideKey, useGuideStore } from "../../stores/useGuideStore";
import { useUIStore } from "../../stores/useUIStore";
import styles from "./GuideHost.module.css";

interface Rect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** 量一个锚点（data-guide 值）。元素不存在或尺寸为 0 → null（调用方整步跳过）。 */
function measure(key: string): Rect | null {
  const el = document.querySelector<HTMLElement>(`[data-guide="${key}"]`);
  if (!el) return null;
  const r = el.getBoundingClientRect();
  if (r.width <= 0 || r.height <= 0) return null;
  return { left: r.left, top: r.top, width: r.width, height: r.height };
}

/** 气泡摆位：优先锚点下方，放不下翻到上方；水平居中并夹在视口内。 */
function place(rect: Rect, w: number, h: number): { left: number; top: number } {
  const GAP = 10;
  const EDGE = 8;
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  const below = rect.top + rect.height + GAP;
  const above = rect.top - GAP - h;
  const top =
    below + h <= vh - EDGE
      ? below
      : above >= EDGE
        ? above
        : Math.max(EDGE, vh - h - EDGE);
  let left = rect.left + rect.width / 2 - w / 2;
  left = Math.max(EDGE, Math.min(left, vw - w - EDGE));
  return { left, top };
}

export function GuideHost() {
  const activeKey = useActiveGuideKey();
  const advance = useGuideStore((s) => s.advance);
  const exit = useGuideStore((s) => s.exit);
  const overlay = useUIStore((s) => s.overlay);
  const item = activeKey !== null ? GUIDE_ITEMS[activeKey] : undefined;

  const [rect, setRect] = useState<Rect | null>(null);
  const cardRef = useRef<HTMLDivElement>(null);
  const [card, setCard] = useState<{ w: number; h: number } | null>(null);

  // 量锚点：每步进入量一次；随锚点尺寸与视口变化重算；量不到整步跳过。
  useEffect(() => {
    if (!item) {
      setRect(null);
      return;
    }
    const key = item.form === "focus" ? item.anchor : NOTICE_ANCHOR;
    if (!key) {
      advance();
      return;
    }
    const el = document.querySelector<HTMLElement>(`[data-guide="${key}"]`);
    // 量不到（不存在 / 尺寸为 0）→ 整步跳过：不猜位置、不留半张气泡
    if (!el || !measure(key)) {
      advance();
      return;
    }
    const apply = () => {
      if (!el.isConnected) {
        advance();
        return;
      }
      const r = el.getBoundingClientRect();
      setRect(
        r.width > 0 && r.height > 0
          ? { left: r.left, top: r.top, width: r.width, height: r.height }
          : null
      );
    };
    apply();
    const ro = new ResizeObserver(apply);
    ro.observe(el);
    ro.observe(document.documentElement);
    window.addEventListener("resize", apply);
    window.addEventListener("scroll", apply, true);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", apply);
      window.removeEventListener("scroll", apply, true);
    };
  }, [activeKey, item, advance]);

  // 换步重置卡片尺寸；尺寸一到就记下来（摆位要它）
  useLayoutEffect(() => {
    setCard(null);
  }, [activeKey]);
  useLayoutEffect(() => {
    if (!rect || card) return;
    const el = cardRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    setCard({ w: r.width, h: r.height });
  });

  // 用户一动键盘即退（卡片内按键除外——那是在点自己的按钮）
  useEffect(() => {
    if (!item) return;
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (t && typeof t.closest === "function" && t.closest("[data-guide-host]")) {
        return;
      }
      exit();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [item, exit]);

  // 别的面板打开时，欢迎页导览退场（锚点被盖住，气泡会浮在面板上）
  useEffect(() => {
    if (activeKey !== null && activeKey.startsWith("welcome.") && overlay !== null) {
      exit();
    }
  }, [activeKey, overlay, exit]);

  if (!item || !rect) return null;

  const focus = item.form === "focus";
  const pos = focus && card ? place(rect, card.w, card.h) : null;

  return createPortal(
    <div className={styles.root} data-guide-host="">
      {focus ? (
        <div
          className={styles.hole}
          style={{
            left: rect.left,
            top: rect.top,
            width: rect.width,
            height: rect.height,
          }}
        />
      ) : (
        <div
          className={styles.dim}
          style={{
            left: rect.left,
            top: rect.top,
            width: rect.width,
            height: rect.height,
          }}
        />
      )}

      <div
        ref={cardRef}
        className={`${styles.card} ${card ? styles.cardIn : ""}`}
        style={
          focus
            ? {
                left: pos?.left ?? 0,
                top: pos?.top ?? 0,
                visibility: pos ? "visible" : "hidden",
              }
            : {
                left: rect.left + rect.width / 2,
                top: rect.top + rect.height / 2,
                transform: "translate(-50%, -50%)",
              }
        }
      >
        {item.title && <div className={styles.title}>{item.title}</div>}
        <div className={styles.body}>{item.body}</div>
        {item.note && <div className={styles.note}>注：{item.note}</div>}
        <div className={styles.actions}>
          {focus && (
            <button type="button" className={styles.ghost} onClick={exit}>
              跳过
            </button>
          )}
          <button type="button" className={styles.primary} onClick={advance}>
            {focus ? "下一步" : "知道了"}
          </button>
        </div>
      </div>
    </div>,
    document.body
  );
}
