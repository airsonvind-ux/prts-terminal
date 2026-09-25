// src/components/content/BootSequence.tsx
// 启动演出（2026-09 定稿）：restoring 期间的对话区内容——终端风格的自检序列。
//
// 演出与真实的关系（定稿规则）：
//   - 总时长 = max(真实, 剧本最小)：真实慢 → 停在当前句等它；真实快 → 剧本照走完
//   - 文案只报真实在做的事：全篇消费 useUIStore.restoreInfo
//     （index → read[n/N] → ready），不产生任何虚构进度
//   - [n/N] 计数仅在 N ≥ 2 时显示（单份时「1/1」无意义）
//
// 三幕（过渡＝一条连续的运动线，原型已验）：
//   幕一 自检日志：铭牌 + 接入清单（CSS 入场）→ 三句日志逐句打字
//   幕二 欢迎页（仅回归用户）：收尾句整句停留后——光标收笔 → 日志自上而下
//        错峰退场（加速离开）→ 字标收拢入场（与退场重叠、减速落定）
//   幕三 淡出露出对话（首次使用跳过幕二：欢迎页是目的地，与覆盖层之下无缝衔接）
//
// 动效口径：出场 ease-in（cubic-bezier(.4,0,1,1)）、入场 ease-out
// （cubic-bezier(0,0,.2,1)）；时长与错峰见下方常量（CSS 里的 transition 同步）。

import { useEffect, useRef, useState } from "react";
import { useUIStore } from "../../stores/useUIStore";
import { WelcomeMark } from "./WelcomeMark";
import styles from "./BootSequence.module.css";

// ── 剧本节奏（调演出长相改这里；与 CSS 的 transition 时长同步）──
/** 打字机单字间隔（ms） */
const CHAR_MS = 20;
/** 每条日志句最短展示（ms，自开始打字起算） */
const MIN_LINE_MS = 460;
/** 行间小停（ms） */
const LINE_GAP_MS = 90;
/** 静态块（铭牌 + 清单）入场后开始第一句（ms） */
const BLOCK_SETTLE_MS = 480;
/** 收尾句打完后的整句停留（ms；先把最后一句话读完再过渡） */
const CLOSE_HOLD_MS = 900;
/** 过渡：光标收笔淡出（ms） */
const CURSOR_OFF_MS = 160;
/** 过渡：日志退场波起点（相对过渡 t0，ms） */
const WAVE_BASE_MS = 140;
/** 过渡：退场波每元素错峰（ms） */
const WAVE_STAGGER_MS = 42;
/** 过渡：退场波单元素时长（ms；与 CSS .wave 同步） */
const WAVE_DUR_MS = 330;
/** 过渡：欢迎入场起点（相对过渡 t0，ms；与退场重叠） */
const WELCOME_AT_MS = 320;
/** 过渡：欢迎最后一件（细线）落定后的停留（ms） */
const HOLD_AFTER_SETTLE_MS = 400;
/** 过渡：覆盖层淡出（ms；与 CSS .root transition 同步） */
const EXIT_MS = 400;
/** 欢迎入场尾段：细线延迟 720ms + 时长 320ms（与 CSS 同步） */
const WELCOME_TAIL_MS = 720 + 320;
/** 日志幕的元素数：铭牌 + 清单 3 + 日志句（回归 3 / 首次 2）——退场波落定用 */
const waveCountFor = (returning: boolean) => 4 + (returning ? 3 : 2);

const INDEX_LINE = "正在校验作战记录索引…";
const READ_LINE = "正在调取作战记录…";
const CLOSE_LINE_RETURNING = "链路建立完毕。欢迎回来，博士。";
const CLOSE_LINE_FIRST_USE = "链路建立完毕。博士，终端就绪。";

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

interface Line {
  id: "index" | "read" | "close";
  text: string;
}

type Scene = "log" | "welcome";

interface Props {
  /** 演出完成（含淡出）后回调；由 ChatView 撤下本覆盖层 */
  onDone: () => void;
}

export function BootSequence({ onDone }: Props) {
  /** 已完成的行（变暗展示） */
  const [doneLines, setDoneLines] = useState<Line[]>([]);
  /** 当前行 + 已打出字数（打字机） */
  const [active, setActive] = useState<{ line: Line; typed: number } | null>(
    null
  );
  /** 当前幕：log（自检日志）→ welcome（欢迎页） */
  const [scene, setScene] = useState<Scene>("log");
  /** 日志退场波：置位后各元素依次 .gone（延迟 = 序号 × 错峰） */
  const [waving, setWaving] = useState(false);
  /** 覆盖层淡出中 */
  const [closing, setClosing] = useState(false);

  const cursorRef = useRef<HTMLSpanElement | null>(null);

  const readDone = useUIStore((s) => s.restoreInfo.readDone);
  const readTotal = useUIStore((s) => s.restoreInfo.readTotal);

  useEffect(() => {
    let cancelled = false;

    /** 轮询等待条件成立（60ms 步进；取消时立即退出） */
    const until = async (cond: () => boolean): Promise<void> => {
      while (!cond()) {
        if (cancelled) return;
        await sleep(60);
      }
    };

    /** 真实工作是否已结束（ready 与 restoring 复位在 finally 相邻发生，取或） */
    const realDone = () =>
      useUIStore.getState().restoreInfo.phase === "ready" ||
      !useUIStore.getState().restoring;

    /**
     * 逐字打出并满足最短展示；
     * holdUntil：真实未完时停在本句（光标续闪、计数继续刷新）；
     * keepActive：不并入已完成列表（收尾句用，光标留到过渡）。
     */
    const playLine = async (
      line: Line,
      opts?: { holdUntil?: () => boolean; keepActive?: boolean }
    ): Promise<void> => {
      const started = Date.now();
      setActive({ line, typed: 0 });
      for (let i = 1; i <= line.text.length; i++) {
        await sleep(CHAR_MS);
        if (cancelled) return;
        setActive({ line, typed: i });
      }
      const rest = Math.max(0, MIN_LINE_MS - (Date.now() - started));
      await sleep(rest);
      if (opts?.holdUntil) await until(opts.holdUntil);
      if (opts?.keepActive) return;
      await sleep(LINE_GAP_MS);
      if (cancelled) return;
      setDoneLines((prev) => [...prev, line]);
      setActive(null);
    };

    /** 光标收笔：冻结闪烁相位 → 淡出（防相位跳变） */
    const freezeCursor = () => {
      const el = cursorRef.current;
      if (!el) return;
      const cur = getComputedStyle(el).opacity;
      el.style.animation = "none";
      el.style.opacity = cur;
      void el.offsetWidth;
      el.style.transition = `opacity ${CURSOR_OFF_MS}ms ease`;
      el.style.opacity = "0";
    };

    void (async () => {
      // 静态块入场由 CSS 负责；这里等它落定
      await sleep(BLOCK_SETTLE_MS);
      if (cancelled) return;

      // 句 1：校验索引（真实工序：读会话索引）
      await playLine({ id: "index", text: INDEX_LINE });
      if (cancelled) return;

      // 等索引结果定音（有记录 → 进入「调取」；无记录/失败 → 直奔收尾）
      await until(
        () => realDone() || useUIStore.getState().restoreInfo.readTotal > 0
      );
      if (cancelled) return;

      // 句 2：调取作战记录（仅当有记录；展示期间 [n/N] 实时刷新）
      if (useUIStore.getState().restoreInfo.readTotal > 0) {
        await playLine({ id: "read", text: READ_LINE }, { holdUntil: realDone });
        if (cancelled) return;
      }

      // 等真实就绪（首次使用/失败路径在这里同样收敛）
      await until(realDone);
      if (cancelled) return;

      // 句 3：收尾（首次使用换就绪句）
      const readTotalNow = useUIStore.getState().restoreInfo.readTotal;
      const returning = readTotalNow > 0;
      const closeText = returning ? CLOSE_LINE_RETURNING : CLOSE_LINE_FIRST_USE;
      await playLine({ id: "close", text: closeText }, { keepActive: true });
      if (cancelled) return;

      // 整句停留：先把最后一句话读完，再开始过渡
      await sleep(CLOSE_HOLD_MS);
      if (cancelled) return;

      // 过渡 t0：光标收笔
      freezeCursor();

      // 日志退场波（自上而下错峰、加速离开）
      await sleep(WAVE_BASE_MS);
      if (cancelled) return;
      setWaving(true);

      // 幕二（仅回归用户）：欢迎入场（与退场重叠、减速落定）
      if (returning) {
        await sleep(WELCOME_AT_MS - WAVE_BASE_MS);
        if (cancelled) return;
        setScene("welcome");
        await sleep(WELCOME_TAIL_MS + HOLD_AFTER_SETTLE_MS);
      } else {
        // 首次使用跳过幕二：等退场波落定后停留
        const waveSettle =
          (waveCountFor(false) - 1) * WAVE_STAGGER_MS + WAVE_DUR_MS;
        await sleep(waveSettle + HOLD_AFTER_SETTLE_MS);
      }
      if (cancelled) return;

      // 幕三：淡出露出对话（首次使用时欢迎页就在覆盖层之下，无缝衔接）
      setClosing(true);
      await sleep(EXIT_MS);
      if (cancelled) return;
      onDone();
    })();

    return () => {
      cancelled = true;
    };
  }, [onDone]);

  /** 退场波延迟：序号 × 错峰（铭牌 0、清单 1-3、日志句 4+） */
  const waveDelay = (index: number) =>
    waving ? `${index * WAVE_STAGGER_MS}ms` : undefined;

  return (
    <div
      className={`${styles.root} ${closing ? styles.closing : ""}`}
      data-boot
      data-scene={scene}
    >
      <div className={styles.scene}>
        <div
          className={`${styles.banner} ${styles.wave} ${
            waving ? styles.gone : ""
          }`}
          data-wave="0"
          style={{ transitionDelay: waveDelay(0) }}
        >
          PRTS · Preliminary RodesIsland Terminal System
        </div>
        <div className={styles.modules}>
          {["作战数据库 … 已接入", "知识库 … 已接入", "记忆模块 … 已接入"].map(
            (text, i) => (
              <div
                key={text}
                className={`${styles.module} ${styles.wave} ${
                  waving ? styles.gone : ""
                }`}
                data-wave={1 + i}
                style={{ transitionDelay: waveDelay(1 + i) }}
              >
                {text}
              </div>
            )
          )}
        </div>
        <div className={styles.log}>
          {doneLines.map((line, i) => (
            <div
              key={line.id}
              className={`${styles.line} ${styles.lineDone} ${styles.wave} ${
                waving ? styles.gone : ""
              }`}
              data-wave={4 + i}
              style={{ transitionDelay: waveDelay(4 + i) }}
            >
              <span className={styles.prompt}>{">"}</span>
              <span>{line.text}</span>
              {line.id === "read" && readTotal >= 2 && (
                <span className={styles.count}>
                  {" "}
                  [{readDone}/{readTotal}]
                </span>
              )}
            </div>
          ))}
          {active && (
            <div
              className={`${styles.line} ${styles.lineActive} ${styles.wave} ${
                waving ? styles.gone : ""
              }`}
              data-wave={4 + doneLines.length}
              style={{ transitionDelay: waveDelay(4 + doneLines.length) }}
            >
              <span className={styles.prompt}>{">"}</span>
              <span>{active.line.text.slice(0, active.typed)}</span>
              {active.line.id === "read" &&
                readTotal >= 2 &&
                active.typed >= READ_LINE.length && (
                  <span className={styles.count}>
                    {" "}
                    [{readDone}/{readTotal}]
                  </span>
                )}
              <span ref={cursorRef} className={styles.cursor} />
            </div>
          )}
        </div>
      </div>
      <div className={`${styles.scene} ${styles.welcomeScene}`}>
        <WelcomeMark entering={scene !== "welcome"} />
      </div>
    </div>
  );
}
