// 供应商单色图标。图标数据来自 Hanako 的生成文件（@lobehub/icons，MIT）。

import { PROVIDER_ICONS } from "./providerIcons";

const PROVIDER_TO_ICON: Record<string, string> = {
  anthropic: "anthropic",
  deepseek: "deepseek",
  "deepseek-responses": "deepseek",
  dashscope: "qwen",
  qwen: "qwen",
  openai: "openai",
  gemini: "gemini",
  volcengine: "volcengine",
  moonshot: "moonshot",
  "kimi-coding": "kimi",
  kimi: "kimi",
  zhipu: "zhipu",
  "zhipu-coding": "zhipu",
  siliconflow: "siliconcloud",
  groq: "groq",
  mistral: "mistral",
  minimax: "minimax",
  openrouter: "openrouter",
  fireworks: "fireworks",
  mimo: "xiaomimimo",
  ollama: "ollama",
};

function iconKeyFor(provider?: string): string | undefined {
  if (!provider) return undefined;
  const p = provider.toLowerCase();
  if (PROVIDER_TO_ICON[p]) return PROVIDER_TO_ICON[p];
  const base = p.replace(
    /-(coding-plan|coding|token-plan|oauth|codex-oauth|codex)$/,
    ""
  );
  if (PROVIDER_TO_ICON[base]) return PROVIDER_TO_ICON[base];
  if (base.startsWith("openai")) return "openai";
  return undefined;
}

export function ProviderIcon({
  provider,
  className,
  opacity = 1,
}: {
  provider?: string;
  className?: string;
  opacity?: number;
}) {
  const iconKey = iconKeyFor(provider);
  const icon = iconKey ? PROVIDER_ICONS[iconKey] : undefined;

  // Anthropic 不在 lobehub 生成集里；用简洁的单色 A 形标记补位。
  if (iconKey === "anthropic") {
    return (
      <svg
        className={className}
        width="1em"
        height="1em"
        viewBox="0 0 24 24"
        fill="currentColor"
        aria-hidden="true"
        style={{ opacity }}
      >
        <path d="M12 2.8 21.2 21h-3.9l-1.8-3.8H8.5L6.7 21H2.8L12 2.8Zm0 6.1-2.4 5.2h4.8L12 8.9Z" />
      </svg>
    );
  }

  if (!icon) {
    return (
      <svg
        className={className}
        width="1em"
        height="1em"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.6"
        aria-hidden="true"
        style={{ opacity }}
      >
        <rect x="4.5" y="4.5" width="15" height="15" rx="4" />
        <circle cx="12" cy="12" r="2.5" fill="currentColor" stroke="none" />
      </svg>
    );
  }

  return (
    <svg
      className={className}
      width="1em"
      height="1em"
      viewBox={icon.viewBox}
      fill="currentColor"
      fillRule="evenodd"
      aria-hidden="true"
      style={{ opacity }}
      dangerouslySetInnerHTML={{ __html: icon.content }}
    />
  );
}
