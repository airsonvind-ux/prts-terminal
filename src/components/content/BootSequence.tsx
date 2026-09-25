// src/components/content/BootSequence.tsx
// 启动演出（2026-09 定稿）：restoring 期间的对话区内容——终端风格的自检序列。
//
// 演出与真实的关系（定稿规则）：
//   - 总时长 = max(真实, 剧本最小)：真实慢 → 停在当前句等它；真实快 → 剧本照走完
//   - 文案只报真实在做的事：全篇消费 useUIStore.restoreInfo
//     （index → read[n/N] → ready），不产生任何虚构进度
//   - 首次使用（readTotal === 0）：跳过「调取」一拍，收尾换就绪句
//   - [n/N] 计数仅在 N ≥ 2 时显示（单份时「1/1」无意义）
//
// 结构：铭牌 + 接入清单（CSS 入场）→「校验索引」「调取作战记录」「收尾」逐句打字
// → 淡出。淡出完成后回调 onDone（ChatView 撤下覆盖层，露出底下已就绪的对话）。

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
/** 收尾句播完后的停顿（ms） */
const POST_HOLD_MS = 200;
/** 淡出时长（ms；与 CSS transition 同步） */
const FADE_MS = 260;

const INDEX_LINE = "正在校验作战记录索引…";
const READ_LINE = "正在调取作战记录…";
const CLOSE_LINE_RETURNING = "链路建立完毕。欢迎回来，博士。";
const CLOSE_LINE_FIRST_USE = "链路建立完毕。博士，终端就绪。";

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

interface Line {
  id: "index" | "read" | "close";
  text: string;
}

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
     * keepActive：不并入已完成列表（收尾句用，光标留到淡出）。
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
        () =>
          realDone() ||
          useUIStore.getState().restoreInfo.readTotal > 0
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
      const closeText =
        useUIStore.getState().restoreInfo.readTotal === 0
          ? CLOSE_LINE_FIRST_USE
          : CLOSE_LINE_RETURNING;
      await playLine({ id: "close", text: closeText }, { keepActive: true });
      if (cancelled) return;

      await sleep(POST_HOLD_MS);
      if (cancelled) return;
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
            {active.line.id === "read" && readTotal >= 2 && active.typed >= READ_LINE.length && (
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
  );
}
