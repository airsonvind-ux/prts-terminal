// 模型设置页的状态、副作用与提交逻辑。
//
// 受控视角：llm = providers[activeProvider] 拼出的 LlmConfig（provider =
// active 名 + 段顶层 thinking_level），由 Panel 组装传入。多供应商配置各自
// 持久在分区内——切供应商 = 只 patch active_provider，UI 自动回填该家。

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
import { useConversationStore } from "../../stores/useConversationStore";
import { showToast } from "../../stores/useToastStore";
import { resolveThinking, thinkingLabel } from "../../thinking";
import { MASKED_SECRET } from "./SettingsPanel";
import type { ModelListStatus } from "./SettingsModelList";

type VerifyStatus = "idle" | "loading" | "success" | "error";

/** get_local_model_info 的返回（与 App 启动初始化同口径）。 */
interface LocalModelInfo {
  displayName: string;
  contextLength: number;
  supportsVision: boolean;
  supportsReasoning?: boolean;
  thinkingLevels?: string[] | null;
  defaultThinkingLevel?: string | null;
}

// 模型列表缓存（跨挂载；仅远端列表，非秘密——切回该家时免重拉）
export const modelListCache: Record<string, ModelInfo[]> = {};

export function useModelSettings(
  llm: LlmConfig,
  busy: boolean,
  onPatch: (patch: LlmPatch, label?: string) => void
) {
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
  const [copying, setCopying] = useState(false);
  const [keyLast4, setKeyLast4] = useState<string | null>(null);

  // ── UI 状态 ──
  const [verifyStatus, setVerifyStatus] = useState<VerifyStatus>("idle");
  const [verifyError, setVerifyError] = useState("");

  const llmRef = useRef(llm);
  llmRef.current = llm;

  // 拉模型/验证连接时读取最新编辑态（避免闭包捕获旧 keyDraft）。
  const keyStateRef = useRef({ edited: keyEdited, draft: keyDraft });
  keyStateRef.current = { edited: keyEdited, draft: keyDraft };

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

  // 换供应商或密钥保存结果变化后，旧的末 4 位提示不再对应当前钥值。
  useEffect(() => {
    setKeyLast4(null);
  }, [llm.provider, llm.api_key]);

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

  // ── 模型列表拉取（掩码态只传 null，后端从凭据层取钥）──
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
    // 编辑中的草稿是用户显式输入；掩码态传 null 让后端补钥。
    const typedKey = keyStateRef.current.edited
      ? keyStateRef.current.draft
      : null;
    const apiKeyArg =
      typedKey !== null
        ? typedKey
        : api_key === MASKED_SECRET
          ? null
          : api_key;
    if (apiKeyArg === "") {
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
        apiKey: apiKeyArg,
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

  // active 变化：展示该家缓存（如有）；无缓存且已配 key → 静默自动拉一次
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

  // 供应商 / 模型变化时刷新全局模型快照。不能只依赖 selectModel()：
  // 切供应商只 patch active_provider，若该家模型已保存就不会再经过模型列表点击。
  // 同时同步活跃对话的展示元数据，避免新会话 / append_turn 继续写旧模型名。
  useEffect(() => {
    const provider = llm.provider;
    const modelId = llm.model;
    if (!provider || !modelId) return;
    let cancelled = false;
    const apply = (info: LocalModelInfo | null) => {
      if (cancelled) return;
      const next = info
        ? {
            name: info.displayName,
            contextLength: info.contextLength,
            supportsVision: info.supportsVision,
            thinking: thinkingLabel(llm.thinking_level, info),
          }
        : {
            name: modelId,
            contextLength: 0,
            supportsVision: false,
            thinking: null,
          };
      setModelInfo(next);
      const activeId = useConversationStore.getState().activeId;
      if (activeId) {
        useConversationStore
          .getState()
          .updateConversationModel(activeId, next.name, next.contextLength);
      }
    };
    invoke<LocalModelInfo | null>("get_local_model_info", { provider, modelId })
      .then(apply)
      .catch(() => apply(null));
    return () => {
      cancelled = true;
    };
  }, [llm.provider, llm.model, llm.thinking_level]);

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
    if (prev.provider === "deepseek" && t === "anthropic-messages") {
      pp.base_url = "https://api.deepseek.com/anthropic";
    }
    onPatch(home(pp), "API 类型");
  };

  // ── 失焦提交 helper ──
  const commitOnBlur = (patch: LlmPatch, label: string, changed: boolean) => {
    if (!changed) return;
    onPatch(patch, label);
  };

  // ── API Key 复制（provider = active；完整钥值不进入前端）──
  const copyKey = async () => {
    if (llmRef.current.api_key !== MASKED_SECRET) return;
    if (keyStateRef.current.edited) return;
    setCopying(true);
    try {
      const suffix = await invoke<string>("copy_api_key", {
        provider: llmRef.current.provider,
      });
      setKeyLast4(suffix || null);
      showToast(
        suffix
          ? `API Key 已复制到剪贴板（末 4 位：${suffix}）`
          : "API Key 已复制到剪贴板",
        { kind: "info" }
      );
    } catch (e) {
      showToast(`复制 API Key 失败：${String(e)}`, { kind: "warning" });
    } finally {
      setCopying(false);
    }
  };

  // ── 验证连接（保存态由后端补钥；编辑中的新钥作为 override）──
  const handleVerify = async () => {
    setVerifyStatus("loading");
    setVerifyError("");
    let keyOverride: string | null;
    if (keyStateRef.current.edited) {
      keyOverride = keyStateRef.current.draft;
    } else if (llmRef.current.api_key === MASKED_SECRET) {
      keyOverride = null;
    } else {
      keyOverride = llmRef.current.api_key;
    }
    if (keyOverride === "") {
      setVerifyStatus("error");
      setVerifyError("请先填写 API Key");
      setTimeout(() => setVerifyStatus("idle"), 3000);
      return;
    }
    try {
      await invoke("verify_saved_connection", {
        config: llmRef.current as unknown as Record<string, unknown>,
        keyOverride,
      });
      setVerifyStatus("success");
      setTimeout(() => setVerifyStatus("idle"), 2000);
    } catch (e) {
      setVerifyStatus("error");
      setVerifyError(String(e));
      setTimeout(() => setVerifyStatus("idle"), 3000);
    }
  };

  // ── 模型选择（选完即刷新悬停卡快照）──
  const selectModel = (id: string) => {
    onPatch(home({ model: id }), "模型");
    const m = modelList.find((mi) => mi.id === id);
    if (m) {
      setModelInfo({
        name: m.displayName,
        contextLength: m.contextLength,
        supportsVision: m.supportsVision,
        thinking: thinkingLabel(llm.thinking_level, m),
      });
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

  const keyDisplayValue = keyEdited ? keyDraft : llm.api_key;

  const fieldDisabled = busy || !llm.provider || customDraft;
  // 复制可用性：有密钥即可复制（面板态一律持掩码）；编辑中不可复制——
  // 用户正在输入的新钥尚未保存，复制旧值会误导。
  const canCopy =
    !busy && !keyEdited && !copying && llm.api_key !== "";
  const keyToggleTitle = copying
    ? "复制中…"
    : llm.api_key === ""
      ? "暂无密钥"
      : keyEdited
        ? "编辑中不可复制"
        : "复制";

  return {
    providers,
    providersLoading,
    providersError,
    customDraft,
    customName,
    setCustomName,
    customApiType,
    setCustomApiType,
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
    copying,
    keyLast4,
    setKeyLast4,
    verifyStatus,
    verifyError,
    handleProviderChange,
    saveCustom,
    handleApiTypeChange,
    fetchModels,
    commitOnBlur,
    copyKey,
    handleVerify,
    selectModel,
    isCustom,
    thinking,
    providerDisplayName,
    keyDisplayValue,
    fieldDisabled,
    canCopy,
    keyToggleTitle,
    home,
    llmRef,
  };
}
