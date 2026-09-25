// src/components/content/BootSequence.tsx
// 启动演出（2026-09 定稿）：restoring 期间的对话区内容——终端风格的自检序列。
//
// 演出与真实的关系（定稿规则）：
//   - 总时长 = max(真实, 剧本最小)：真实慢 → 停在当前句等它；真实快 → 剧本照走完
//   - 文案只报真实在做的事：全篇消费 useUIStore.restoreInfo
//     （index → read[n/N] → ready），不产生任何虚构进度
//   - [n/N] 计数仅在 N ≥ 2 时显示（单份时「1/1」无意义）
//
// 三幕：
//   幕一 自检日志：铭牌 + 接入清单（CSS 入场）→ 三句日志逐句打字
//   幕二 欢迎页（仅回归用户）：收尾句整句停留后交叉渐变到 PRTS 字标，停留一拍
//   幕三 淡出露出对话。首次使用无幕二——欢迎页就是目的地，与覆盖层之下
//        的欢迎页无缝衔接；收尾句也换成就绪句。

import { useEffect, useState } from "react";
import { useUIStore } from "../../stores/useUIStore";
import styles from "./BootSequence.module.css";

// ── 剧本节奏（调演出长相改这里）──
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
/** 日志 → 欢迎页交叉渐变（ms；与 CSS scene transition 同步） */
const SCENE_FADE_MS = 280;
/** 欢迎页停留（ms） */
const WELCOME_HOLD_MS = 650;
/** 覆盖层淡出（ms；与 CSS root transition 同步） */
const FADE_MS = 280;

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
  /** 收尾淡出中 */
  const [closing, setClosing] = useState(false);

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
      const closeText =
        readTotalNow === 0 ? CLOSE_LINE_FIRST_USE : CLOSE_LINE_RETURNING;
      await playLine({ id: "close", text: closeText }, { keepActive: true });
      if (cancelled) return;

      // 整句停留：先把最后一句话读完，再开始过渡
      await sleep(CLOSE_HOLD_MS);
      if (cancelled) return;

      // 幕二（仅回归用户）：日志 → 欢迎页交叉渐变 + 停留
      if (readTotalNow > 0) {
        setScene("welcome");
        await sleep(SCENE_FADE_MS + WELCOME_HOLD_MS);
        if (cancelled) return;
      }

      // 幕三：淡出露出对话（首次使用时欢迎页就在覆盖层之下，无缝衔接）
      setClosing(true);
      await sleep(FADE_MS);
      if (cancelled) return;
      onDone();
    })();

    return () => {
      cancelled = true;
    };
  }, [onDone]);

  return (
    <div
      className={`${styles.root} ${closing ? styles.closing : ""}`}
      data-boot
      data-scene={scene}
    >
      <div
        className={`${styles.scene} ${
          scene === "welcome" ? styles.sceneHidden : ""
        }`}
      >
        <div className={styles.banner}>
          PRTS · Preliminary RodesIsland Terminal System
        </div>
        <div className={styles.modules}>
          <div className={styles.module}>作战数据库 … 已接入</div>
          <div className={styles.module}>知识库 … 已接入</div>
          <div className={styles.module}>记忆模块 … 已接入</div>
        </div>
        <div className={styles.log}>
          {doneLines.map((line) => (
            <div key={line.id} className={`${styles.line} ${styles.lineDone}`}>
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
            <div className={`${styles.line} ${styles.lineActive}`}>
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
              <span className={styles.cursor} />
            </div>
          )}
        </div>
      </div>
      <div
        className={`${styles.scene} ${styles.welcomeScene} ${
          scene === "welcome" ? "" : styles.sceneHidden
        }`}
      >
        <div className={styles.welcomeLogo}>PRTS</div>
        <div className={styles.welcomeTagline}>明日方舟AI辅助终端</div>
      </div>
    </div>
  );
}
