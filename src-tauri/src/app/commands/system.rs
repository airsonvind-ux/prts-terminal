use super::prelude::*;

/// 从本地 ModelRegistry 查询模型信息（不上网）。
///
/// 仅在 models.json 中的预置模型可查。自定义供应商的模型不在注册表中，返回 None。
/// 供前端启动时初始化模型名称和上下文窗口大小，避免依赖设置页手动点选。
#[tauri::command]
pub fn get_local_model_info(
    model_registry: State<'_, Arc<ModelRegistry>>,
    provider: String,
    model_id: String,
) -> Result<Option<ModelInfo>, String> {
    let entry = model_registry.get_model(&provider, &model_id);
    Ok(entry.map(|e| ModelInfo {
        id: model_id,
        display_name: e.name.clone(),
        context_length: e.context,
        supports_reasoning: e.reasoning,
        supports_vision: e.supports_vision,
        compat: e.compat.clone().unwrap_or_default(),
        thinking_levels: e.thinking_levels.clone(),
        default_thinking_level: e.default_thinking_level.clone(),
        enriched: true,
    }))
}

// ═══════════════════════════════════════════════════════
// quit_app —— 关闭应用
// ═══════════════════════════════════════════════════════

/// 关闭终端应用。
///
/// 设置页"通用"标签中的"关闭终端"按钮调用此命令。
/// 无人机悬浮窗无原生窗口控件（关闭/最小化/最大化），
/// 关闭入口统一收在设置页内。
#[tauri::command]
pub fn quit_app(app_handle: AppHandle) {
    app_handle.exit(0);
}

// ═══════════════════════════════════════════════════════
// verify_connection —— 验证 LLM 连接
// ═══════════════════════════════════════════════════════

/// 验证 LLM 供应商连接是否可用。
///
/// 用传入的 LlmConfig 发起一次模型列表请求（5 秒超时），验证连通性与凭据。
///
/// 分流：Anthropic 走 `{root}/v1/models`（`x-api-key`，`strip_api_path` 剥掉已带
/// 的路径段）；其余按 OpenAI 兼容走 `{base}/models`（`Authorization: Bearer`，空 key
/// 不发）。状态映射：200 → Ok；401/403 → “API Key 无效”；404 → “API 端点不存在”。
/// 成功：返回 Ok。失败：返回可读的错误描述。
#[tauri::command]
pub async fn verify_connection(config: LlmConfig) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let base_url = config
        .base_url
        .as_deref()
        .or_else(|| default_base_url(&config.provider));

    let Some(base_url) = base_url else {
        return Err("未配置 Base URL，请填写或选择供应商".into());
    };

    // 按 API 类型构造验证请求：不同协议的 /models 端点路径和鉴权方式不同
    let (url, auth_header): (String, Option<(_, _)>) = match config.api_type {
        ApiType::AnthropicMessages => {
            // Anthropic 协议：用 x-api-key 鉴权，剥离尾部路径后请求 /v1/models
            let root = strip_api_path(base_url);
            (
                format!("{}/v1/models", root),
                Some(("x-api-key", config.api_key.clone())),
            )
        }
        _ => {
            // OpenAI 兼容协议：用 Bearer 鉴权
            let url = format!("{}/models", base_url.trim_end_matches('/'));
            let auth = if config.api_key.is_empty() {
                None
            } else {
                Some(("authorization", format!("Bearer {}", config.api_key)))
            };
            (url, auth.map(|(k, v)| (k, v)))
        }
    };

    let mut request = client.get(&url);
    if let Some((key, value)) = auth_header {
        request = request.header(key, &value);
    }

    // 解析并附加用户自定义 Headers
    if let Some(ref headers_str) = config.headers {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(headers_str) {
            if let Some(obj) = json.as_object() {
                for (k, v) in obj {
                    if let Some(val) = v.as_str() {
                        request = request.header(k.as_str(), val);
                    }
                }
            }
        }
    }

    let response = request.send().await.map_err(|e| {
        if e.is_timeout() {
            "连接超时：请检查 Base URL 和网络连接".into()
        } else if e.is_connect() {
            "无法连接服务器：请检查 Base URL 是否正确".into()
        } else {
            format!("请求失败: {e}")
        }
    })?;

    match response.status().as_u16() {
        200 => Ok(()),
        401 | 403 => Err("API Key 无效".into()),
        404 => Err("API 端点不存在：请检查 Base URL".into()),
        s => Err(format!("服务器返回异常状态 ({s})")),
    }
}

/// 剥离 base_url 中的 API 路径后缀，返回供应商根 URL。
/// 例如：https://api.deepseek.com/anthropic → https://api.deepseek.com
fn strip_api_path(url: &str) -> String {
    let url = url.trim_end_matches('/');
    // 尝试解析为 URL，失败则简单处理
    if let Ok(parsed) = reqwest::Url::parse(url) {
        let host = parsed.host_str().unwrap_or("");
        let scheme = parsed.scheme();
        // 对已知供应商，返回根路径
        if host.contains("api.deepseek.com") {
            return format!("{scheme}://api.deepseek.com");
        }
        if host.contains("api.anthropic.com") {
            return format!("{scheme}://api.anthropic.com");
        }
        // 其他情况：保留 host，去掉路径
        format!("{scheme}://{host}")
    } else {
        url.to_string()
    }
}

/// 列出所有可用供应商。
///
/// 从内存中的 ModelRegistry 枚举供应商信息，返回 id、展示名、
/// api_type、base_url、默认 compat。前端供应商下拉渲染此列表。
#[tauri::command]
pub fn list_providers(model_registry: State<'_, Arc<ModelRegistry>>) -> Vec<ProviderInfo> {
    model_registry.list_providers()
}

/// 获取指定供应商的可用模型列表。
///
/// 按 api_type 分流：
/// - OpenAI 兼容协议：GET {base_url}/models + Bearer
/// - Anthropic 协议：GET {root}/v1/models + x-api-key
///
/// 解析远端模型 ID 列表，逐个在 ModelRegistry 中匹配能力信息。
#[tauri::command]
pub async fn list_models(
    provider: String,
    api_key: String,
    base_url: Option<String>,
    api_type: Option<String>,
    model_registry: State<'_, Arc<ModelRegistry>>,
) -> Result<Vec<ModelInfo>, String> {
    let base = base_url
        .as_deref()
        .or_else(|| model_registry.get_base_url(&provider));

    let Some(base) = base else {
        return Err("未找到该供应商的 Base URL，请先选择供应商".into());
    };

    let is_anthropic = api_type.as_deref() == Some("anthropic-messages");

    let (url, auth_header): (String, Option<(&str, String)>) = if is_anthropic {
        // Anthropic 协议：剥离路径回到根 URL，请求 /v1/models，用 x-api-key
        let root = strip_api_path(base);
        let url = format!("{}/v1/models", root);
        let auth = if api_key.is_empty() {
            None
        } else {
            Some(("x-api-key", api_key.clone()))
        };
        (url, auth)
    } else {
        // OpenAI 兼容协议
        let url = format!("{}/models", base.trim_end_matches('/'));
        let auth = if api_key.is_empty() {
            None
        } else {
            Some(("authorization", format!("Bearer {}", api_key)))
        };
        (url, auth)
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;

    let mut request = client.get(&url);
    if let Some((key, value)) = auth_header {
        request = request.header(key, &value);
    }

    let response = request.send().await.map_err(|e| {
        if e.is_timeout() {
            "获取模型列表超时：请检查网络连接".into()
        } else if e.is_connect() {
            "无法连接服务器：请检查 Base URL".into()
        } else {
            format!("请求失败: {e}")
        }
    })?;

    if !response.status().is_success() {
        return Err(format!(
            "服务器返回 HTTP {}，请检查 API Key 和 Base URL",
            response.status().as_u16()
        ));
    }

    // 解析 OpenAI /models 响应格式：{ "data": [{ "id": "...", ... }, ...] }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("解析模型列表失败: {e}"))?;

    let data = body["data"]
        .as_array()
        .ok_or("模型列表格式异常：缺少 data 数组")?;

    let models: Vec<ModelInfo> = data
        .iter()
        .filter_map(|entry| {
            let id = entry["id"].as_str()?;
            let matched = model_registry.get_model(&provider, id);

            Some(ModelInfo {
                id: id.to_string(),
                display_name: matched
                    .map(|m| m.name.clone())
                    .unwrap_or_else(|| id.to_string()),
                context_length: matched.map(|m| m.context).unwrap_or(0),
                supports_reasoning: matched.map(|m| m.reasoning).unwrap_or(false),
                supports_vision: matched.map(|m| m.supports_vision).unwrap_or(false),
                compat: matched.and_then(|m| m.compat.clone()).unwrap_or_default(),
                thinking_levels: matched
                    .map(|m| m.thinking_levels.clone())
                    .unwrap_or_default(),
                default_thinking_level: matched.and_then(|m| m.default_thinking_level.clone()),
                enriched: matched.is_some(),
            })
        })
        .collect();

    Ok(models)
}
