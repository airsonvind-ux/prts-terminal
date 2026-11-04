// 工具视图卡：插件交来的是「内容」，宿主把它落成会话目录里的文件；
// 这里只负责摆出来——html 进沙箱 iframe，image 进 img，doc 只是一行可点的文件。
//
// 三条容易做错、错了不报错只是不工作的点（spike 结论）：
//   1. iframe 的 src 在 Windows 上是 http://prts-view.localhost/…，
//      macOS/Linux 才是 prts-view://localhost/…；
//   2. 沙箱不给 allow-same-origin，所以不能靠 origin 白名单做校验；
//   3. 父级 CSP 不下传到跨源 iframe，文档策略全靠协议响应头。
//
// 路径在事件里是相对的（`views/<file>`），绝对路径经 session_file_abs_path
// 换算一次并缓存——一张视图一辈子只换一次。

import { useEffect, useRef, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { showToast } from "../../stores/useToastStore";
import { draftKeyFor, useComposerDraftStore } from "../../stores/useComposerDraftStore";
import type { ToolResultView } from "../../types";
import {
  createViewRateLimiter,
  getViewOrigin,
  parseViewMessage,
  VIEW_TEXT_LIMIT,
  viewUrl,
  withViewSource,
  type ViewMessage,
} from "./viewBridge";
import styles from "./ViewCard.module.css";

/** 视图文档的源由 Rust 给（Windows 是 http://prts-view.localhost），见 viewBridge。 */

const absPathCache = new Map<string, Promise<string>>();

/** 相对路径 → 绝对路径。按 (会话 id, 相对路径) 缓存，失败也缓存（同样的文件不会自己回来）。 */
function resolveSessionFile(conversationId: string, relative: string): Promise<string> {
  const key = `${conversationId}\u0000${relative}`;
  let cached = absPathCache.get(key);
  if (!cached) {
    cached = invoke<string>("session_file_abs_path", { conversationId, path: relative });
    absPathCache.set(key, cached);
  }
  return cached;
}

/** 视图高度：作者给建议值，宿主 clamp 到 40–160。 */
export function viewHeightPx(view: ToolResultView): number {
  const raw = view.height ?? 120;
  return Math.min(160, Math.max(40, Math.round(raw)));
}

/** 宿主外壳的标题行：标题 + 展开。展开后是应用内浮层，不是新窗口。 */
export function ViewCard({
  view,
  conversationId,
}: {
  view: ToolResultView;
  conversationId: string;
}) {
  const [expanded, setExpanded] = useState(false);
  const [absPath, setAbsPath] = useState<string | null>(null);
  const [missing, setMissing] = useState(false);
  const [origin, setOrigin] = useState<string | null>(null);
  const frameRef = useRef<HTMLIFrameElement | null>(null);
  const largeFrameRef = useRef<HTMLIFrameElement | null>(null);
  const limiterRef = useRef<(() => boolean) | null>(null);
  if (!limiterRef.current) limiterRef.current = createViewRateLimiter();

  useEffect(() => {
    if (view.kind !== "html") return;
    let alive = true;
    void getViewOrigin().then((value) => {
      if (alive) setOrigin(value.origin);
    });
    return () => {
      alive = false;
    };
  }, [view.kind]);

  useEffect(() => {
    if (view.kind === "html") return;
    let alive = true;
    resolveSessionFile(conversationId, view.path)
      .then((path) => {
        if (alive) setAbsPath(path);
      })
      .catch(() => {
        if (alive) setMissing(true);
      });
    return () => {
      alive = false;
    };
  }, [conversationId, view.kind, view.path]);

  // 沙箱文档发回来的动作。校验只认「来自这张视图的两个 iframe」：
  // 沙箱是不透明源，event.origin 恒为 "null"，白名单会把消息全拒掉。
  useEffect(() => {
    if (view.kind !== "html") return;
    const handle = (message: ViewMessage) => {
      if (!limiterRef.current?.()) {
        showToast("这块视图操作太频繁，稍等一下", { kind: "warning" });
        return;
      }
      if (message.type === "local") return;
      if (message.type === "toast") {
        const text = message.text?.trim();
        if (text) showToast(text, { kind: "info" });
        return;
      }
      if (message.type === "prompt") {
        void submitPrompt(message.text ?? "");
        return;
      }
      // call：需要有一条脱离模型、但仍进同一轮的执行通道，单独一步做。
      showToast("这块视图还不支持直接调用工具", { kind: "warning" });
    };
    const onMessage = (event: MessageEvent) => {
      const windows = [
        frameRef.current?.contentWindow ?? null,
        largeFrameRef.current?.contentWindow ?? null,
      ];
      if (!windows.some((win) => win !== null && win === event.source)) return;
      const message = parseViewMessage(event.data);
      if (message) handle(message);
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);

    async function submitPrompt(text: string): Promise<void> {
      const trimmed = text.trim();
      if (!trimmed) return;
      if (trimmed.length > VIEW_TEXT_LIMIT) {
        showToast(`这条交互太长了（上限 ${VIEW_TEXT_LIMIT} 字）`, { kind: "warning" });
        return;
      }
      const prefixed = withViewSource(view.title, trimmed);
      try {
        const outcome = await invoke<{ delivered: boolean }>("view_prompt", {
          conversationId,
          text: prefixed,
        });
        if (outcome?.delivered) return;
        const key = draftKeyFor(conversationId);
        const existing = useComposerDraftStore.getState().textByKey[key] ?? "";
        useComposerDraftStore
          .getState()
          .setText(key, existing ? `${existing}\n${prefixed}` : prefixed);
        showToast("已填进输入框，发送与否由你定", { kind: "info" });
      } catch (err) {
        showToast(String(err), { kind: "warning", dedupeKey: "view-prompt-failed" });
      }
    }
  }, [view.kind, view.title, conversationId]);

  const src = view.kind === "html" && origin ? viewUrl(origin, conversationId, view.path) : null;
  const imageSrc =
    view.kind === "image" && absPath ? safeAssetUrl(absPath) : null;
  const unavailable = missing || (view.kind === "image" && !imageSrc && absPath !== null);

  return (
    <section className={styles.card} data-view-card data-view-kind={view.kind}>
      <header className={styles.header}>
        <span className={styles.title}>{view.title}</span>
        <button
          type="button"
          className={styles.expand}
          onClick={() => setExpanded(true)}
          aria-label="展开视图"
        >
          展开
        </button>
      </header>

      {unavailable ? (
        <p className={styles.missing}>这块视图已不可用</p>
      ) : view.kind === "html" && src ? (
        <iframe
          ref={frameRef}
          className={styles.frame}
          style={{ height: viewHeightPx(view) }}
          src={src}
          sandbox="allow-scripts"
          title={view.title}
          data-view-frame
        />
      ) : view.kind === "html" ? (
        <p className={styles.missing}>视图加载中…</p>
      ) : view.kind === "image" && imageSrc ? (
        <img
          className={styles.image}
          style={{ maxHeight: viewHeightPx(view) }}
          src={imageSrc}
          alt={view.title}
          data-view-image
        />
      ) : view.kind === "doc" ? (
        <button
          type="button"
          className={styles.doc}
          onClick={() => openDoc(absPath)}
          disabled={!absPath}
        >
          <span className={styles.docName}>{fileName(view.path)}</span>
          <span className={styles.docAction}>打开</span>
        </button>
      ) : (
        <p className={styles.missing}>视图加载中…</p>
      )}

      {expanded && (
        <div className={styles.overlay} role="dialog" aria-label={view.title}>
          <div className={styles.panel}>
            <header className={styles.panelHeader}>
              <span className={styles.title}>{view.title}</span>
              <button
                type="button"
                className={styles.expand}
                onClick={() => setExpanded(false)}
              >
                关闭
              </button>
            </header>
            {view.kind === "html" && src ? (
              <iframe
                ref={largeFrameRef}
                className={styles.frameLarge}
                src={src}
                sandbox="allow-scripts"
                title={view.title}
                data-view-frame-large
              />
            ) : imageSrc ? (
              <img className={styles.imageLarge} src={imageSrc} alt={view.title} />
            ) : (
              <p className={styles.missing}>这块视图已不可用</p>
            )}
          </div>
        </div>
      )}
    </section>
  );
}

function fileName(relative: string): string {
  const index = relative.lastIndexOf("/");
  return index >= 0 ? relative.slice(index + 1) : relative;
}

function safeAssetUrl(path: string): string | null {
  try {
    return convertFileSrc(path);
  } catch {
    return null;
  }
}

function openDoc(path: string | null): void {
  if (!path) return;
  void invoke("open_path", { path }).catch((err) =>
    showToast(String(err), { kind: "warning", dedupeKey: "open-view-failed" })
  );
}
