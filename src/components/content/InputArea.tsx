// src/components/content/InputArea.tsx
// 输入区 · 36px 最小高度
//
// 职责：文本输入框 + 发送按钮。
// 内部维护输入文本状态，Enter 发送 / Shift+Enter 换行。
//
// 发送按钮双态：
//   未聚焦 → 宽药丸（48×28，border-radius 14px）
//   聚焦   → 缩为正圆（28×28，border-radius 50%）
//   输入中 → ::before 对数充能（width = --send-charge%，无封顶）
//   发送后 → ::after 涟漪动画（transform scale，不触发 layout）
//
// IME 处理：拼写期间冻结充能进度。

import { useState, useRef, useEffect, useCallback, type KeyboardEvent, type CompositionEvent, type ChangeEvent } from "react";
import { measureWithoutScrollJump } from "./messageScroll";
import styles from "./InputArea.module.css";

interface Props {
  /** 空闲发送（ready 态；乐观清空输入）。附件由 ChatView 侧持有并随本次发送附加。 */
  onSubmit: (text: string) => void;
  /** 已有附件（允许“无文字仅附件”发送） */
  hasAttachments?: boolean;
  /** 粘贴图片（无路径 blob）→ 交给上层上传 */
  onPasteImage?: (file: File) => void;
  /** 生成中插话：后端入队成功返回 true → 清输入；失败 false（已 toast）→ 保留输入 */
  onSteer: (text: string) => Promise<boolean>;
  /** 生成中中止（无字方块） */
  onStop: () => void;
  /** 当前对话生成中（processing/streaming）——按钮三态判定 */
  generating: boolean;
  /** 硬禁（错误 / 会话恢复中 / 压缩中）——不发送也不插话不中止 */
  disabled: boolean;
}

export function InputArea({ onSubmit, onSteer, onStop, generating, disabled, hasAttachments = false, onPasteImage }: Props) {
  const [input, setInput] = useState("");
  const [isFocused, setIsFocused] = useState(false);
  const [isComposing, setIsComposing] = useState(false);
  // 多行态：内容超过单行时切换（右端凹弧退场 → 对称圆角），见 .multiline
  const [multiline, setMultiline] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  /** 粘贴图片（无路径 blob）→ 交给上层上传；纯文本粘贴放行默认行为。 */
  const handlePaste = useCallback(
    (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
      const items = e.clipboardData?.items;
      if (!items) return;
      for (const it of Array.from(items)) {
        if (it.kind === "file" && it.type.startsWith("image/")) {
          const file = it.getAsFile();
          if (file) {
            e.preventDefault();
            onPasteImage?.(file);
            return;
          }
        }
      }
    },
    [onPasteImage]
  );
  const frozenInputRef = useRef("");
  // 上次的目标高度（单行 28 起步）。动画触发条件 = target 变化
  // （而非 current ≠ target——后者在动画播放中恒成立，动画自我延续）
  const lastTargetRef = useRef(28);
  const [sendRipple, setSendRipple] = useState(false);

  /**
   * 对数充能：0→渐近 100%，永不封顶。
   * 早期加速（前几个字符填满大部分）→ 后期渐近（每字符增量递减）。
   * IME 拼写期间 effectiveLen 冻结，确认后统一更新。
   */
  const effectiveLen = isComposing ? frozenInputRef.current.length : input.length;
  const charge = effectiveLen === 0
    ? 0
    : Math.min((Math.log(effectiveLen + 1) / Math.log(31)) * 100, 100);

  const hasText = input.trim().length > 0;
  const canSubmit = hasText || hasAttachments;

  const isCircle = isFocused;
  const isActive = isFocused || charge > 0;

  /* 充能条实际宽度（%）：聚焦按 charge 渐增；失焦有内容则满、无内容则空。
   * 与 --send-charge 同源——splitPct 必须由同一值驱动，
   * 否则失焦时充能条满而箭头分界线停在 charge 原值（「箭头保留部分黑色」的根因） */
  const sendCharge = isFocused ? charge : charge > 0 ? 100 : 0;

  /* 充能条 → SVG 坐标映射
   * 宽药丸 48px / 正圆 28px，SVG 20px 居中 */
  const btnW = isCircle ? 28 : 48;
  const svgLeft = (btnW - 20) / 2;
  const chargeInSvg = Math.max(0, (sendCharge / 100) * btnW - svgLeft);
  const splitPct = Math.min((chargeInSvg / 20) * 100, 100);

  /**
   * 发送后清空输入并复位高度。统一失焦（插话与普通发送一致；按回车即可继续输入）。
   */
  const clearAfterSend = () => {
    setInput("");
    frozenInputRef.current = "";
    // 清空后复位输入框高度（回落到 CSS 默认 28px）与单行态
    if (textareaRef.current) {
      const ta = textareaRef.current;
      ta.getAnimations().forEach((a) => a.cancel());
      measureWithoutScrollJump(() => {
        ta.style.height = "";
      });
    }
    lastTargetRef.current = 28;
    setMultiline(false);
    if (textareaRef.current) textareaRef.current?.blur();
  };

  /**
   * 统一提交入口（按钮点击与 Enter）。三态语义：
   * - 生成中且有字 → 插话（onSteer，成功才清空）；
   * - 空闲且有字 → 发送（乐观清空）；
   * - 无字 / 硬禁 → 忽略且**保留输入**（修复原 busy 回车静默丢字 bug）。
   */
  const submit = () => {
    if (disabled) return;
    const trimmed = input.trim();
    if (trimmed.length === 0 && !hasAttachments) return;
    setSendRipple(true);
    if (generating) {
      void onSteer(trimmed).then((ok) => {
        if (ok) clearAfterSend();
      });
    } else {
      onSubmit(trimmed);
      clearAfterSend();
    }
  };

  /** 中止方块：生成中且无字时按钮形变为中止（点击不经过 submit） */
  const activeGenerating = generating && !disabled;
  const stopShape = activeGenerating && !hasText;

  /** 按钮禁用：硬禁，或既非中止态又无内容（有文字/附件发送恒可点） */
  const btnDisabled = disabled || (!stopShape && !canSubmit);

  /** 按钮形态（测试钩子；send/steer 同款样式仅行为不同） */
  const btnMode = stopShape
    ? "stop"
    : canSubmit
      ? activeGenerating
        ? "steer"
        : "send"
      : "idle";

  /** 悬停说明：只写功能名（细节由用户自己理解功能后判断） */
  const btnTitle = btnMode === "stop" ? "中止" : btnMode === "steer" ? "插话" : "发送";

  /** 内容高度同步：复位 → 判定 → 撑高/回落。
   * 复位必须在判定前——Chrome 的 scrollHeight = max(内容, 客户区)，
   * 不复位则元素残留高度抬高读数（「删除不回落」的根因）。
   * 动画触发条件 = 目标高度变化（target ≠ lastTarget）——
   * 用 current ≠ target 会在动画播放中恒成立（current 是中间值），
   * 动画自我延续（「每次输入都触发动画」的根因）。
   * 同行数输入/动画中再输入（target 不变）→ cancel 后直接落位零动画；
   * 跨行数 → WAAPI 显式 [current → target] 动画 */
  const resizeToContent = () => {
    const ta = textareaRef.current;
    if (!ta) return;
    const current = ta.getBoundingClientRect().height;
    ta.getAnimations().forEach((a) => a.cancel());
    /* 整个“复位→测量→落位”包在稳定测量窗口内：复位高度会让对话区变高、
     * 浏览器趁机把滚动位置钳下去（实测距底恰好差增高量），写回在高度恢复之后。 */
    measureWithoutScrollJump(() => {
      ta.style.transition = "none";
      ta.style.height = "";
      const ml = ta.scrollHeight > ta.clientHeight;
      const target = ml ? ta.scrollHeight : 28;
      ta.style.transition = "";
      setMultiline(ml);
      if (target === lastTargetRef.current) {
        ta.style.height = ml ? `${target}px` : "";
      } else {
        ta.style.height = ml ? `${target}px` : "";
        ta.animate(
          [{ height: `${current}px` }, { height: `${target}px` }],
          { duration: 300, easing: "cubic-bezier(0.4, 0, 0.2, 1)" }
        );
        lastTargetRef.current = target;
      }
    });
  };

  /** 输入变化：文本状态 + 高度/多行同步 */
  const handleChange = (e: ChangeEvent<HTMLTextAreaElement>) => {
    setInput(e.target.value);
    resizeToContent();
  };

  /** 聚焦：按钮变圆。
   * 不立即测量——聚焦瞬间 textarea 宽度 = 失焦宽度（按钮尚未变圆），
   * 立即测量会按旧宽度撑高（边界场景：文字已缩为 X 行但高度仍 X+1 行——「先撑后缩」）。
   * 等按钮宽度动画（0.3s）结束、宽度稳定后一次性测量——高度与聚焦宽度匹配：
   * 边界场景 target 不变 → 零动画；多行场景撑高一次 */
  const handleFocus = () => {
    setIsFocused(true);
    const ta = textareaRef.current;
    setTimeout(() => {
      // 仅当仍处于聚焦态时测量——聚焦后 320ms 内失焦会触发 handleBlur 收缩，
      // 定时器此时再测量会按失焦宽度撑高（与「失焦收缩」设计冲突）
      if (document.activeElement === ta) resizeToContent();
    }, 320);
  };

  /** 失焦：按钮回药丸 + 收缩回初始（WAAPI 回落动画，内容保留） */
  const handleBlur = () => {
    setIsFocused(false);
    const ta = textareaRef.current;
    if (!ta) return;
    const current = ta.getBoundingClientRect().height;
    ta.getAnimations().forEach((a) => a.cancel());
    // 动画目标 = 复位后的实际渲染高度（不能硬编码 28——CSS height 28px 在
    // content-box 下渲染 36px，硬编码会误判单行需要回落 → 播放 36→28→36 的
    // 虚假动画，「高度没变却发生动画/先升高再回落」的根因）
    // 同样包在稳定测量窗口内：复位高度会改变对话区高度
    const at = measureWithoutScrollJump(() => {
      ta.style.transition = "none";
      ta.style.height = "";
      const natural = ta.getBoundingClientRect().height;
      ta.style.transition = "";
      return natural;
    });
    if (Math.abs(current - at) > 0.5) {
      ta.animate(
        [{ height: `${current}px` }, { height: `${at}px` }],
        { duration: 300, easing: "cubic-bezier(0.4, 0, 0.2, 1)" }
      );
    }
    lastTargetRef.current = 28;
    setMultiline(false);
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !isComposing) {
      e.preventDefault();
      submit();
    }
  };

  const handleCompositionStart = (_e: CompositionEvent<HTMLTextAreaElement>) => {
    frozenInputRef.current = input;
    setIsComposing(true);
  };

  const handleCompositionEnd = (_e: CompositionEvent<HTMLTextAreaElement>) => {
    setIsComposing(false);
  };

  // 涟漪动画结束后清除类名
  useEffect(() => {
    if (!sendRipple) return;
    const timer = setTimeout(() => setSendRipple(false), 500);
    return () => clearTimeout(timer);
  }, [sendRipple]);

  return (
    <div className={styles.wrapper}>
      <textarea
        ref={textareaRef}
        className={`${styles.textarea}${multiline ? ` ${styles.multiline}` : ""}`}
        value={input}
        onChange={handleChange}
        onFocus={handleFocus}
        onBlur={handleBlur}
        onKeyDown={handleKeyDown}
        onCompositionStart={handleCompositionStart}
        onCompositionEnd={handleCompositionEnd}
        onPaste={handlePaste}
        placeholder="说点什么…"
        rows={1}
      />
      <div className={styles.btnWrapper}>
        <button
          className={`${styles.sendBtn} ${isCircle ? styles.sendBtnFocused : ""} ${sendRipple ? styles.sendBtnRipple : ""}${stopShape ? ` ${styles.sendBtnStop}` : ""}`}
          style={{ "--send-charge": `${sendCharge}%` } as React.CSSProperties}
          onClick={stopShape ? onStop : submit}
          disabled={btnDisabled}
          data-mode={btnMode}
          title={btnTitle}
          aria-label={btnTitle}
        >
          {stopShape ? (
            /* 中止方块（--warn 警示色；与发送/插话箭头同定位与形态语言） */
            <svg className={styles.arrowSvg} viewBox="0 0 20 20">
              <rect x="4.5" y="4.5" width="11" height="11" rx="2" fill="currentColor" />
            </svg>
          ) : (
            <>
          {/* 充能条覆盖区箭头：与充能条同色 */}
          <svg
          className={styles.arrowSvg}
          viewBox="0 0 20 20"
          style={{ color: "var(--depth-4)", clipPath: `inset(0 ${100 - splitPct}% 0 0)` } as React.CSSProperties}
        >
          <path
            d="M 3,10 L 17,10 L 13,6 M 17,10 L 13,14"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
        {/* 非充能条区箭头 */}
        <svg
          className={styles.arrowSvg}
          viewBox="0 0 20 20"
          style={{ color: isActive ? "#fff" : "var(--text-2)", clipPath: `inset(0 0 0 ${splitPct}%)` } as React.CSSProperties}
        >
          <path
            d="M 3,10 L 17,10 L 13,6 M 17,10 L 13,14"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
            </>
          )}
      </button>
      </div>
    </div>
  );
}
