// 设置页 · "模型"子页（v2 供应商分区接线）
//
// 受控视角：llm = providers[activeProvider] 拼出的 LlmConfig（provider =
// active 名 + 段顶层 thinking_level），由 Panel 组装传入。**多供应商配置各自
// 持久在分区内**——切供应商 = 只 patch active_provider，UI 自动回填该家
// （无清 key/恢复缓存逻辑；key 常显该家掩码或空）。
//
// patch 形态（对齐后端 v2）：
// - 家字段（api_type/model/api_key/base_url/max_tokens）→ providers.{active}.{field}
// - 切供应商 → active_provider
// - 思考档 → thinking_level（段顶层全局）
//
// mask 契约：get_settings 逐家掩码；👁 reveal_api_key 带 provider 参数。
// **明文只在 👁 期间存在**：本页与面板态一律持掩码（写入后由 Panel 归一为掩码，
// 见 mergeSettings），故「已配 key」= api_key === MASKED_SECRET。
// headers / max_tokens 内部参数不暴露 UI（settings.json 手动写入保留）。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  ApiType,
  LlmConfig,
  LlmPatch,
  ModelInfo,
  ProviderInfo,
  ProviderPatch,
} from "../../types";
import { setModelInfo, setModelThinking } from "../../stores/useProcessingStore";
import { showToast } from "../../stores/useToastStore";
import { resolveThinking, thinkingLabel, THINKING_LABELS } from "../../thinking";
import { SectionWarning } from "./ui/SettingsSection";
import { ModelListSection, type ModelListStatus } from "./SettingsModelList";
import { MASKED_SECRET } from "./SettingsPanel";
import styles from "./SettingsProvider.module.css";
import { Select } from "./ui/Select";

const API_TYPES: { value: ApiType; label: string }[] = [
  { value: "openai-completions", label: "OpenAI Completions" },
  { value: "anthropic-messages", label: "Anthropic Messages" },
  { value: "openai-responses", label: "OpenAI Responses" },
];

// 推理档位标签与派生规则已收到 `src/thinking.ts`（单一来源：模型页 + 悬停卡共用）

// 密码显隐图标（通行 eye / eye-off 描边 SVG）
const ICON_EYE = (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z" />
    <circle cx="12" cy="12" r="3" />
  </svg>
);
const ICON_EYE_OFF = (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94" />
    <path d="M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19" />
    <path d="M14.12 14.12a3 3 0 1 1-4.24-4.24" />
    <line x1="1" y1="1" x2="23" y2="23" />
  </svg>
);

// 推理档位标签与派生规则已收到 `src/thinking.ts`（单一来源：模型页 + 悬停卡共用）

type VerifyStatus = "idle" | "loading" | "success" | "error";

// 模型列表缓存（跨挂载；仅远端列表，非秘密——切回该家时免重拉）
export const modelListCache: Record<string, ModelInfo[]> = {};

export interface SettingsModelProps {
  /** active 供应商视角配置（provider = active 名；thinking_level = 段顶层） */
  llm: LlmConfig;
  /** busy（对话处理中）：控件禁用 + 漏斗拒 patch */
  busy: boolean;
  /** v2 llm 段 patch（Panel 漏斗：即时保存 + 失败回滚） */
  onPatch: (patch: LlmPatch, label?: string) => void;
}

export function SettingsModel({ llm, busy, onPatch }: SettingsModelProps) {
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [providersLoading, setProvidersLoading] = useState(true);
  const [providersError, setProvidersError] = useState("");

  // ── 自定义供应商草稿（点"自定义…"进入；名称保存即切 active）──
  const [customDraft, setCustomDraft] = useState(false);
  const [customName, setCustomName] = useState("");
  const [customApiType, setCustomApiType] = useState<ApiType>("openai-completions");
  const [customBaseUrl, setCustomBaseUrl] = useState("");

  // ── 模型列表 ──
  const [modelList, setModelList] = useState<ModelInfo[]>([]);
  const [modelListStatus, setModelListStatus] = useState<ModelListStatus>("idle");
  const [modelListError, setModelListError] = useState("");

  // ── 文本 draft（失焦 patch）──
  const [baseUrlDraft, setBaseUrlDraft] = useState(llm.base_url ?? "");

  // ── API Key 编辑态（mask 契约）──
  const [keyEdited, setKeyEdited] = useState(false);
  const [keyDraft, setKeyDraft] = useState("");
  const [revealing, setRevealing] = useState(false);
  const [revealedKey, setRevealedKey] = useState<string | null>(null);

  // ── UI 状态 ──
  const [verifyStatus, setVerifyStatus] = useState<VerifyStatus>("idle");
  const [verifyError, setVerifyError] = useState("");

  const llmRef = useRef(llm);
  llmRef.current = llm;

  /** 家字段 patch 包装（写入 active 供应商条目；懒建由后端处理） */
  const home = useCallback(
    (pp: ProviderPatch): LlmPatch =>
      ({ providers: { [llmRef.current.provider]: pp } }),
    []
  );

  // prop 同步（回滚/重载后 draft 复位）
  useEffect(() => {
    setBaseUrlDraft(llm.base_url ?? "");
    if (!keyEdited) setKeyDraft("");
  }, [llm, keyEdited]);

  // active 是否自定义（非预设且非空），或处于"自定义草稿"态
  const isCustom = useMemo(() => {
    if (customDraft) return true;
    if (!llm.provider) return false;
    return !providers.some((p) => p.id === llm.provider);
  }, [customDraft, llm.provider, providers]);

  // 挂载：拉取供应商列表
  useEffect(() => {
    invoke<ProviderInfo[]>("list_providers")
      .then((list) => {
        setProviders(list ?? []);
        setProvidersLoading(false);
      })
      .catch((e) => {
        setProvidersError(String(e));
        setProvidersLoading(false);
      });
  }, []);

  // ── 模型列表拉取（掩码态先 reveal 明文）──
  // quiet = 自动拉取（方案 b）：失败静默回 idle（保留手动入口），不弹错
  const fetchModels = useCallback(async (quiet = false) => {
    const { provider, api_key, base_url, api_type } = llmRef.current;
    if (!provider) {
      if (!quiet) {
        setModelListError("请先选择供应商");
        setModelListStatus("error");
      }
      return;
    }
    let effectiveKey = api_key;
    if (api_key === MASKED_SECRET) {
      try {
        effectiveKey =
          (await invoke<string>("reveal_api_key", { provider })) ?? "";
      } catch {
        effectiveKey = "";
      }
    }
    if (!effectiveKey) {
      if (!quiet) {
        setModelListError("请先填写 API Key");
        setModelListStatus("error");
      }
      return;
    }
    setModelListStatus("loading");
    setModelListError("");
    try {
      const list = await invoke<ModelInfo[]>("list_models", {
        provider,
        apiKey: effectiveKey,
        baseUrl: base_url,
        apiType: api_type,
      });
      modelListCache[provider] = list;
      setModelList(list);
      setModelListStatus(list.length > 0 ? "loaded" : "empty");
    } catch (e) {
      if (quiet) {
        // 自动拉失败：静默回 idle（手动入口保留，不弹错、不阻塞 UI）
        setModelListError("");
        setModelListStatus("idle");
      } else {
        setModelListError(String(e));
        setModelListStatus("error");
      }
    }
  }, []);

  // active 变化：展示该家缓存（如有）；**无缓存且已配 key → 静默自动拉一次**
  useEffect(() => {
    const provider = llm.provider;
    const cached = modelListCache[provider];
    if (cached && cached.length > 0) {
      setModelList(cached);
      setModelListStatus("loaded");
      return;
    }
    setModelList([]);
    setModelListStatus("idle");
    const key = llm.api_key;
    if (provider && key && key.trim() !== "") {
      void fetchModels(true);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [llm.provider]);

  const selectedModelInfo = useMemo(
    () => modelList.find((m) => m.id === llm.model) ?? null,
    [modelList, llm.model]
  );

  // ── 思考强度派生（口径见 src/thinking.ts，与后端 thinking.rs 同源） ──
  const thinking = useMemo(
    () => resolveThinking(llm.thinking_level, selectedModelInfo),
    [llm.thinking_level, selectedModelInfo]
  );

  // 悬停卡要的「生效思考档」：模型 / 档位一变就同步（缓存里存的是算好的快照）
  useEffect(() => {
    setModelThinking(thinkingLabel(llm.thinking_level, selectedModelInfo));
  }, [llm.thinking_level, selectedModelInfo]);

  // ── 供应商切换（只 patch active_provider；各家配置分区保留） ──
  const handleProviderChange = (newProvider: string) => {
    if (newProvider === "__custom__") {
      // 进入自定义草稿（名称保存才切 active）
      setCustomDraft(true);
      setCustomName("");
      setCustomApiType("openai-completions");
      setCustomBaseUrl("");
      return;
    }
    setCustomDraft(false);
    onPatch({ active_provider: newProvider }, "供应商");
  };

  /** 保存自定义供应商：切 active + 建条目（类型/URL 来自草稿） */
  const saveCustom = () => {
    const name = customName.trim();
    if (!name) return;
    setCustomDraft(false);
    onPatch(
      {
        active_provider: name,
        providers: {
          [name]: {
            api_type: customApiType,
            base_url: customBaseUrl || "",
          },
        },
      },
      "供应商"
    );
    setCustomName("");
    setCustomBaseUrl("");
  };

  // ── API 类型切换（联动 base_url）──
  const handleApiTypeChange = (t: ApiType) => {
    if (customDraft) {
      setCustomApiType(t);
      return;
    }
    const prev = llmRef.current;
    const pp: ProviderPatch = { api_type: t, model: "" };
    if (
      prev.provider === "deepseek" &&
      t === "anthropic-messages"
    ) {
      pp.base_url = "https://api.deepseek.com/anthropic";
    }
    onPatch(home(pp), "API 类型");
  };

  // ── 失焦提交 helper ──
  const commitOnBlur = (patch: LlmPatch, label: string, changed: boolean) => {
    if (!changed) return;
    onPatch(patch, label);
  };

  // ── API Key 瞬态查看（provider = active）──
  const toggleReveal = async () => {
    if (revealedKey !== null) {
      setRevealedKey(null);
      return;
    }
    if (llmRef.current.api_key !== MASKED_SECRET) return;
    setRevealing(true);
    try {
      const plain = await invoke<string>("reveal_api_key", {
        provider: llmRef.current.provider,
      });
      setRevealedKey(plain || "");
    } catch (e) {
      showToast(`获取密钥失败：${String(e)}`, { kind: "warning" });
    } finally {
      setRevealing(false);
    }
  };

  // ── 验证连接 ──
  const handleVerify = async () => {
    setVerifyStatus("loading");
    setVerifyError("");
    let effectiveKey = llmRef.current.api_key;
    if (effectiveKey === MASKED_SECRET) {
      try {
        effectiveKey =
          (await invoke<string>("reveal_api_key", {
            provider: llmRef.current.provider,
          })) ?? "";
      } catch (e) {
        setVerifyStatus("error");
        setVerifyError(String(e));
        setTimeout(() => setVerifyStatus("idle"), 3000);
        return;
      }
    }
    if (!effectiveKey) {
      setVerifyStatus("error");
      setVerifyError("请先填写 API Key");
      setTimeout(() => setVerifyStatus("idle"), 3000);
      return;
    }
    try {
      await invoke("verify_connection", {
        config: { ...llmRef.current, api_key: effectiveKey } as unknown as Record<
          string,
          unknown
        >,
      });
      setVerifyStatus("success");
      setTimeout(() => setVerifyStatus("idle"), 2000);
    } catch (e) {
      setVerifyStatus("error");
      setVerifyError(String(e));
      setTimeout(() => setVerifyStatus("idle"), 3000);
    }
  };

  // ── 派生展示 ──
  const providerDisplayName = useMemo(() => {
    if (isCustom) return llm.provider || "自定义";
    return (
      providers.find((p) => p.id === llm.provider)?.displayName ??
      (llm.provider || "请选择供应商")
    );
  }, [isCustom, llm.provider, providers]);

  const keyDisplayValue =
    revealedKey !== null
      ? revealedKey
      : keyEdited
        ? keyDraft
        : llm.api_key;

  const fieldDisabled = busy || !llm.provider || customDraft;
  // 👁 可用性：有密钥即可查看（面板态一律持掩码）；**编辑中不可查看**——
  // 否则"显示旧值"会盖住正在输入的新值，失焦还会把它吞掉。
  const canReveal = !busy && !keyEdited && llm.api_key !== "";
  const keyToggleTitle =
    revealedKey !== null
      ? "隐藏"
      : llm.api_key === ""
        ? "暂无密钥"
        : keyEdited
          ? "编辑中不可查看"
          : "显示";

  return (
    <div className={styles.container}>
      {busy && <SectionWarning>对话处理中，模型与供应商暂不可改</SectionWarning>}
      {/* ── 供应商 ── */}
      <div className={styles.field}>
        <span className={styles.label}>供应商</span>
        <Select
          ariaLabel="供应商"
          value={isCustom ? "__custom__" : llm.provider}
          disabled={providersLoading || busy}
          placeholder={providersLoading ? "加载中…" : undefined}
          onChange={handleProviderChange}
          options={[
            ...providers.map((p) => ({ value: p.id, label: p.displayName })),
            // 自定义态：把当前自定义供应商当一项列出来（触发器要显示它的名字）
            ...(isCustom
              ? [{ value: "__custom__", label: providerDisplayName }]
              : []),
          ]}
          footer={isCustom ? undefined : { value: "__custom__", label: "自定义…" }}
        />
        {providersError && (
          <span className={styles.fieldHint}>获取供应商列表失败：{providersError}</span>
        )}
      </div>

      {/* ── 自定义草稿 / 自定义编辑区 ── */}
      {(customDraft || (isCustom && llm.provider)) && (
        <>
          {customDraft && (
            <>
              <label className={styles.field}>
                <span className={styles.label}>供应商名称</span>
                <input
                  className={styles.input}
                  type="text"
                  disabled={busy}
                  value={customName}
                  onChange={(e) => setCustomName(e.target.value)}
                  onBlur={saveCustom}
                  placeholder="例如：my-proxy"
                />
              </label>
              <div className={styles.field}>
                <span className={styles.label}>API 类型</span>
                <Select
                  ariaLabel="API 类型"
                  value={customApiType}
                  disabled={busy}
                  onChange={(v) => handleApiTypeChange(v as ApiType)}
                  options={API_TYPES}
                />
              </div>
              <label className={styles.field}>
                <span className={styles.label}>Base URL</span>
                <input
                  className={styles.input}
                  type="text"
                  disabled={busy}
                  value={customBaseUrl}
                  onChange={(e) => setCustomBaseUrl(e.target.value)}
                  placeholder="https://api.example.com/v1"
                />
              </label>
              <button className={styles.fetchButton} onClick={saveCustom} disabled={busy}>
                使用此自定义供应商
              </button>
            </>
          )}
        </>
      )}

      {/* ── API 类型（非草稿态；active 家） ── */}
      {!customDraft && (
        <div className={styles.field}>
          <span className={styles.label}>API 类型</span>
          <Select
            ariaLabel="API 类型"
            value={llm.api_type}
            disabled={busy || !llm.provider}
            onChange={(v) => handleApiTypeChange(v as ApiType)}
            options={API_TYPES}
          />
        </div>
      )}

      {/* ── API Key（mask 契约；家字段） ── */}
      <label className={styles.field}>
        <span className={styles.label}>API Key</span>
        <div className={styles.keyRow}>
          <input
            className={styles.input}
            type={revealedKey !== null ? "text" : "password"}
            disabled={fieldDisabled}
            readOnly={revealedKey !== null}
            value={keyDisplayValue}
            onCopy={(e) => {
              if (revealedKey !== null) e.preventDefault();
            }}
            onCut={(e) => {
              if (revealedKey !== null) e.preventDefault();
            }}
            onPaste={(e) => {
              if (revealedKey !== null) e.preventDefault();
            }}
            placeholder="sk-..."
            onFocus={(e) => {
              // 聚焦**不清空**，只全选（首键即替换）：否则"点一下内容就没了"
              // 看起来像把密钥删了。keyEdited 只由真实输入置位。
              if (revealedKey === null) e.currentTarget.select();
            }}
            onChange={(e) => {
              if (revealedKey !== null) return;
              setKeyEdited(true);
              setKeyDraft(e.target.value);
            }}
            onBlur={() => {
              if (revealedKey !== null) {
                setRevealedKey(null);
                return;
              }
              // 只有真正编辑过才上送：判据是「动过」而非「与现值不同」——
              // 掩码态下把内容清空是「清除密钥」这一明确意图，必须能上送
              // （与现值比较时空串会被误判为"未改"，永远清不掉）。
              if (!keyEdited) return;
              const next = keyDraft;
              setKeyEdited(false);
              onPatch(home({ api_key: next }), "API Key");
            }}
          />
          <button
            className={`${styles.keyToggle} ${revealedKey !== null ? styles.keyToggleActive : ""}`}
            type="button"
            disabled={!canReveal}
            onClick={() => void toggleReveal()}
            title={keyToggleTitle}
          >
            {revealing
              ? "…"
              : revealedKey !== null
                ? ICON_EYE_OFF
                : ICON_EYE}
          </button>
        </div>
      </label>

      {/* ── 模型（家字段） ── */}
      <div className={styles.field}>
        <div className={styles.labelRow}>
          <span className={styles.label}>模型</span>
          {modelListStatus === "loaded" && (
            <button
              className={styles.refreshBtn}
              disabled={busy}
              onClick={() => void fetchModels()}
              title="刷新模型列表"
            >
              ↻
            </button>
          )}
        </div>
        <ModelListSection
          status={modelListStatus}
          models={modelList}
          selectedModel={llm.model}
          error={modelListError}
          onSelect={(id) => {
            onPatch(home({ model: id }), "模型");
            const m = modelList.find((mi) => mi.id === id);
            if (m) {
              setModelInfo({
                name: m.displayName,
                contextLength: m.contextLength,
                thinking: thinkingLabel(llm.thinking_level, m),
              });
            }
          }}
          onFetch={fetchModels}
        />
      </div>

      {/* ── 思考强度（段顶层 thinking_level） ── */}
      {thinking && (
        <div className={styles.field}>
          <span className={styles.label}>思考强度</span>
          <Select
            ariaLabel="思考强度"
            value={thinking.effective}
            disabled={busy}
            onChange={(v) => onPatch({ thinking_level: v }, "思考强度")}
            options={thinking.levels.map((lvl) => ({
              value: lvl,
              label: `${THINKING_LABELS[lvl] ?? lvl}${
                lvl === thinking.defaultLevel ? "（默认）" : ""
              }`,
            }))}
          />
          <span className={styles.fieldNote}>推理深度档位；随所选模型能力提供</span>
        </div>
      )}

      {/* ── Base URL（家字段） ── */}
      <label className={styles.field}>
        <span className={styles.label}>Base URL</span>
        <input
          className={styles.input}
          type="text"
          disabled={fieldDisabled}
          value={baseUrlDraft}
          onChange={(e) => setBaseUrlDraft(e.target.value)}
          onBlur={() => {
            const next = baseUrlDraft.trim();
            commitOnBlur(
              home({ base_url: next }),
              "Base URL",
              next !== (llm.base_url ?? "")
            );
          }}
          placeholder={
            providers.find((p) => p.id === llm.provider)?.baseUrl ?? ""
          }
          spellCheck={false}
        />
      </label>

      {/* 内部高级参数（headers / max_tokens）不暴露 UI——settings.json 手动写入保留 */}

      {/* ── 验证连接 ── */}
      <div className={styles.verifyRow}>
        <button
          className={`${styles.verifyButton} ${
            verifyStatus === "loading"
              ? styles.verifyLoading
              : verifyStatus === "success"
                ? styles.verifySuccess
                : verifyStatus === "error"
                  ? styles.verifyError
                  : ""
          }`}
          onClick={handleVerify}
          disabled={verifyStatus === "loading"}
        >
          {verifyStatus === "idle" && "验证连接"}
          {verifyStatus === "loading" && "验证中…"}
          {verifyStatus === "success" && "连接成功"}
          {verifyStatus === "error" && "连接失败"}
        </button>
        {verifyStatus === "error" && (
          <span className={styles.verifyErrorText}>{verifyError}</span>
        )}
      </div>
    </div>
  );
}

