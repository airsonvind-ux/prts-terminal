// 设置页 · "模型"子页（v2 供应商分区接线）
//
// 受控视角：llm = providers[activeProvider] 拼出的 LlmConfig（provider =
// active 名 + 段顶层 thinking_level），由 Panel 组装传入。**多供应商配置各自
// 持久在分区内**——切供应商 = 只 patch active_provider，UI 自动回填该家。
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

import type { ApiType, LlmConfig, LlmPatch } from "../../types";
import { THINKING_LABELS } from "../../thinking";
import { SectionWarning } from "./ui/SettingsSection";
import { ModelListSection } from "./SettingsModelList";
import styles from "./SettingsProvider.module.css";
import { Select } from "./ui/Select";
import { useModelSettings } from "./useModelSettings";

export { modelListCache } from "./useModelSettings";

const API_TYPES: { value: ApiType; label: string }[] = [
  { value: "openai-completions", label: "OpenAI Completions" },
  { value: "anthropic-messages", label: "Anthropic Messages" },
  { value: "openai-responses", label: "OpenAI Responses" },
];

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

export interface SettingsModelProps {
  /** active 供应商视角配置（provider = active 名；thinking_level = 段顶层） */
  llm: LlmConfig;
  /** busy（对话处理中）：控件禁用 + 漏斗拒 patch */
  busy: boolean;
  /** v2 llm 段 patch（Panel 漏斗：即时保存 + 失败回滚） */
  onPatch: (patch: LlmPatch, label?: string) => void;
}

export function SettingsModel({ llm, busy, onPatch }: SettingsModelProps) {
  const {
    providers,
    providersLoading,
    providersError,
    customDraft,
    customName,
    setCustomName,
    customApiType,
    customBaseUrl,
    setCustomBaseUrl,
    modelList,
    modelListStatus,
    modelListError,
    baseUrlDraft,
    setBaseUrlDraft,
    keyEdited,
    setKeyEdited,
    keyDraft,
    setKeyDraft,
    revealing,
    revealedKey,
    setRevealedKey,
    verifyStatus,
    verifyError,
    handleProviderChange,
    saveCustom,
    handleApiTypeChange,
    fetchModels,
    commitOnBlur,
    toggleReveal,
    handleVerify,
    selectModel,
    isCustom,
    thinking,
    providerDisplayName,
    keyDisplayValue,
    fieldDisabled,
    canReveal,
    keyToggleTitle,
    home,
  } = useModelSettings(llm, busy, onPatch);

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
          onSelect={selectModel}
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
