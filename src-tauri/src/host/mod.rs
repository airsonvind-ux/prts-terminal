//! 能力层（M2a）：宿主原语对插件的统一出口。
//!
//! 三层契约（D30）：宿主实现 → 能力接口（本模块，面向插件开发者）→ 工具与
//! SKILL（面向模型）。插件不直接碰宿主内部：它拿到的 [`PluginCtx`] 里只有
//! 自己声明、且宿主实际登记的服务；调用时先做授权判定，再把**调用方 id** 交给
//! 服务实现——`fs.plugin` 靠它把路径钉在 `plugin-data/<id>/` 下。
//!
//! 诚实边界（D33）：这是「可见性 + 评审纪律」，不是沙箱。同 crate 内的代码
//! 仍能 `std::fs` 任意读写，门禁只覆盖 `host::internal` 里的实现（M2a 后续
//! 提交落地）与 [`PluginCtx::call`] 这一个出口。

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

/// 一条宿主能力的静态描述。
///
/// 由服务实现以 `static` 形式持有，因此 `HostApi` 可以直接返回 `&'static`；
/// `since` 是该服务出现的协议版本，低于它的插件看不到这条能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostServiceInfo {
    /// 服务键，同时就是插件声明的能力名（D40：一对一，不设第二套词表）。
    pub key: &'static str,
    /// 一句话说明，写进协议文档与将来的扩展页。
    pub summary: &'static str,
    /// 是否持有跨调用的状态（M2 登记的服务都是无状态的）。
    pub stateful: bool,
    /// 是否需要用户逐次授权；M2 没有审批器，这类服务在加载期一律拒。
    pub requires_user_grant: bool,
    /// 建议的调用上限；M2 的内置服务是同步调用，不执行超时（见规格 §5）。
    pub timeout: Option<Duration>,
    /// 是否可并发调用；为假时宿主串行化同一服务的调用。
    pub concurrency_safe: bool,
    /// 该服务出现的协议版本。
    pub since: u32,
}

/// 能力调用失败。
///
/// `Display` 文案会被插件原样看到（M2b 映射成 `-32000` 等错误码），因此每条都
/// 带服务键与调用方，便于在日志里定位是谁在调什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    /// 插件没声明这条能力，或声明了但宿主没有授权给它。
    CapabilityDenied { caller: String, key: String },
    /// 宿主没有登记这条服务（通常先被 `CapabilityDenied` 拦住，这里只在宿主
    /// 内部调用时出现）。
    UnknownService(String),
    /// 参数不合法（缺字段、类型不对、越界路径等）。
    InvalidArgs { key: String, detail: String },
    /// 服务内部失败（IO、子进程、协议等）。
    Failed { key: String, detail: String },
}

impl HostError {
    /// 构造「参数不合法」，省得每处都写全字段。
    pub fn invalid_args(key: &str, detail: impl Into<String>) -> Self {
        HostError::InvalidArgs {
            key: key.to_string(),
            detail: detail.into(),
        }
    }

    /// 构造「服务内部失败」，省得每处都写全字段。
    pub fn failed(key: &str, detail: impl Into<String>) -> Self {
        HostError::Failed {
            key: key.to_string(),
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::CapabilityDenied { caller, key } => {
                write!(f, "插件 {caller} 未获授权的能力：{key}")
            }
            HostError::UnknownService(key) => write!(f, "宿主没有登记能力：{key}"),
            HostError::InvalidArgs { key, detail } => write!(f, "能力 {key} 参数非法：{detail}"),
            HostError::Failed { key, detail } => write!(f, "能力 {key} 调用失败：{detail}"),
        }
    }
}

impl std::error::Error for HostError {}

/// 宿主能力的调用面。
///
/// `list` / `info` 供加载期做声明裁剪与将来的扩展页展示；`call` 是唯一的执行
/// 入口，`caller` 是调用方插件 id（宿主自己调用时传 `"host"`）。
pub trait HostApi: Send + Sync {
    /// 该协议版本可见的服务，按键名升序（顺序确定，便于测试与文档生成）。
    fn list(&self, protocol: u32) -> Vec<&'static HostServiceInfo>;
    /// 按服务键查找；不看协议版本（版本判定交给 [`resolve_granted`]）。
    fn info(&self, key: &str) -> Option<&'static HostServiceInfo>;
    /// 执行一次能力调用。
    fn call(&self, caller: &str, key: &str, args: Value) -> Result<Value, HostError>;
}

/// 服务实现侧要实现的接口。
///
/// 每条服务持有一个 `static` 描述，注册进 [`HostServices`] 后即可被
/// [`PluginCtx`] 调到。实现放在 `host::internal` 下，crate 内其它模块看不见。
pub trait HostService: Send + Sync {
    fn info(&self) -> &'static HostServiceInfo;
    fn call(&self, caller: &str, args: Value) -> Result<Value, HostError>;
}

/// 宿主侧的能力注册表：[`HostApi`] 的唯一生产实现。
///
/// 注册期查重（同键两条服务是接线错误，直接失败），调用期按键路由。M2 登记的
/// 服务在 `host::internal` 里构造，本类型只管「有哪几条、怎么调」。
#[derive(Default)]
pub struct HostServices {
    services: Vec<Arc<dyn HostService>>,
}

impl HostServices {
    pub fn new() -> Self {
        Self {
            services: Vec::new(),
        }
    }

    /// 登记一条服务；同键重复登记返回错误（不覆盖，避免静默换实现）。
    pub fn register(&mut self, service: Arc<dyn HostService>) -> Result<(), String> {
        let key = service.info().key;
        if self.services.iter().any(|existing| existing.info().key == key) {
            return Err(format!("宿主能力重复登记：{key}"));
        }
        self.services.push(service);
        Ok(())
    }

    /// 已登记的服务键（按登记顺序；仅供装配期自检与测试）。
    pub fn keys(&self) -> Vec<&'static str> {
        self.services.iter().map(|service| service.info().key).collect()
    }
}

impl HostApi for HostServices {
    fn list(&self, protocol: u32) -> Vec<&'static HostServiceInfo> {
        let mut infos: Vec<&'static HostServiceInfo> = self
            .services
            .iter()
            .map(|service| service.info())
            .filter(|info| info.since <= protocol)
            .collect();
        infos.sort_by_key(|info| info.key);
        infos
    }

    fn info(&self, key: &str) -> Option<&'static HostServiceInfo> {
        self.services
            .iter()
            .map(|service| service.info())
            .find(|info| info.key == key)
    }

    fn call(&self, caller: &str, key: &str, args: Value) -> Result<Value, HostError> {
        let service = self
            .services
            .iter()
            .find(|service| service.info().key == key)
            .ok_or_else(|| HostError::UnknownService(key.to_string()))?;
        service.call(caller, args)
    }
}

/// 加载期裁剪失败的原因（决定插件整体不加载）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantError {
    /// 声明了宿主没有登记的能力（D25：fail-closed，不认识的键一律拒）。
    Unknown(Vec<String>),
    /// 声明了需要用户授权的服务，而当前版本没有审批器（M2 一律拒）。
    NeedsApproval(Vec<String>),
    /// 声明了该协议版本尚未提供的服务（`since` 高于插件协议）。
    TooNew {
        keys: Vec<String>,
        since: u32,
        protocol: u32,
    },
}

impl std::fmt::Display for GrantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GrantError::Unknown(keys) => {
                write!(f, "声明了未登记的能力：{}", keys.join("、"))
            }
            GrantError::NeedsApproval(keys) => write!(
                f,
                "声明了需要用户授权的服务：{}（当前版本没有审批器，一律拒）",
                keys.join("、")
            ),
            GrantError::TooNew {
                keys,
                since,
                protocol,
            } => write!(
                f,
                "声明了协议 {protocol} 尚未提供的能力：{}（需要协议 {since}）",
                keys.join("、")
            ),
        }
    }
}

impl std::error::Error for GrantError {}

/// 计算一个插件实际获得的权限：**声明 ∩ 已登记**。
///
/// 三条拒绝路径按顺序判定：不认识 → 需要审批 → 协议版本不够。返回的集合只含
/// 通过判定的键，[`PluginCtx`] 之后的调用据此决定放不放行。
pub fn resolve_granted(
    api: &dyn HostApi,
    declared: &[String],
    protocol: u32,
) -> Result<HashSet<String>, GrantError> {
    let mut unknown: Vec<String> = Vec::new();
    let mut needs_approval: Vec<String> = Vec::new();
    let mut too_new: Vec<String> = Vec::new();
    let mut granted: HashSet<String> = HashSet::new();

    for key in declared {
        match api.info(key) {
            None => unknown.push(key.clone()),
            Some(info) if info.requires_user_grant => needs_approval.push(key.clone()),
            Some(info) if info.since > protocol => too_new.push(key.clone()),
            Some(_) => {
                granted.insert(key.clone());
            }
        }
    }

    if !unknown.is_empty() {
        return Err(GrantError::Unknown(unknown));
    }
    if !needs_approval.is_empty() {
        return Err(GrantError::NeedsApproval(needs_approval));
    }
    if !too_new.is_empty() {
        let since = too_new
            .iter()
            .filter_map(|key| api.info(key).map(|info| info.since))
            .max()
            .unwrap_or(protocol);
        return Err(GrantError::TooNew {
            keys: too_new,
            since,
            protocol,
        });
    }

    Ok(granted)
}

/// 交给插件的能力句柄：只含它被授权的那几条服务。
///
/// `id` 在加载期注入、之后不可改，调用时原样传给服务实现——这是 `fs.plugin`
/// 能把路径钉死在 `<id>/` 下的唯一依据。
pub struct PluginCtx {
    id: String,
    api: Arc<dyn HostApi>,
    granted: HashSet<String>,
}

impl PluginCtx {
    /// 加载期构造（宿主侧用）。
    pub fn new(id: impl Into<String>, api: Arc<dyn HostApi>, granted: HashSet<String>) -> Self {
        Self {
            id: id.into(),
            api,
            granted,
        }
    }

    /// 调用一条能力；未授权直接失败，不会走到服务实现。
    pub fn call(&self, key: &str, args: Value) -> Result<Value, HostError> {
        if !self.granted.contains(key) {
            return Err(HostError::CapabilityDenied {
                caller: self.id.clone(),
                key: key.to_string(),
            });
        }
        self.api.call(&self.id, key, args)
    }

    /// 本插件被授权的服务键。
    pub fn granted(&self) -> &HashSet<String> {
        &self.granted
    }

    /// 本插件的 manifest id。
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 测试专用：绕开加载期裁剪，直接构造一个带指定授权的句柄。
    ///
    /// 生产代码不得调用——`for_test` 只在 `cfg(test)` 下存在，M2a 的
    /// `CapabilityDenied` 用例靠它构造「声明了但没授权」的场景。
    #[cfg(test)]
    pub fn for_test(id: &str, api: Arc<dyn HostApi>, granted: HashSet<String>) -> Self {
        Self::new(id, api, granted)
    }
}

impl std::fmt::Debug for PluginCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginCtx")
            .field("id", &self.id)
            .field("granted", &self.granted)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    static ECHO_INFO: HostServiceInfo = HostServiceInfo {
        key: "test.echo",
        summary: "回声（测试桩）",
        stateful: false,
        requires_user_grant: false,
        timeout: None,
        concurrency_safe: true,
        since: 1,
    };

    static FUTURE_INFO: HostServiceInfo = HostServiceInfo {
        key: "test.future",
        summary: "协议 2 才有的服务（测试桩）",
        stateful: false,
        requires_user_grant: false,
        timeout: None,
        concurrency_safe: true,
        since: 2,
    };

    static APPROVAL_INFO: HostServiceInfo = HostServiceInfo {
        key: "test.approval",
        summary: "需要用户授权的服务（测试桩）",
        stateful: true,
        requires_user_grant: true,
        timeout: None,
        concurrency_safe: false,
        since: 1,
    };

    /// 只回显调用方与参数的服务。
    struct EchoService {
        info: &'static HostServiceInfo,
    }

    impl EchoService {
        fn new(info: &'static HostServiceInfo) -> Arc<dyn HostService> {
            Arc::new(Self { info })
        }
    }

    impl HostService for EchoService {
        fn info(&self) -> &'static HostServiceInfo {
            self.info
        }

        fn call(&self, caller: &str, args: Value) -> Result<Value, HostError> {
            Ok(json!({ "caller": caller, "args": args }))
        }
    }

    fn registry() -> HostServices {
        let mut services = HostServices::new();
        services.register(EchoService::new(&ECHO_INFO)).unwrap();
        services.register(EchoService::new(&FUTURE_INFO)).unwrap();
        services.register(EchoService::new(&APPROVAL_INFO)).unwrap();
        services
    }

    fn keys_of(infos: Vec<&'static HostServiceInfo>) -> Vec<&'static str> {
        infos.iter().map(|info| info.key).collect()
    }

    #[test]
    fn list_filters_by_protocol_and_sorts_by_key() {
        let services = registry();
        assert_eq!(
            keys_of(services.list(1)),
            vec!["test.approval", "test.echo"],
            "协议 1 看不到 since=2 的服务，且按键名升序"
        );
        assert_eq!(
            keys_of(services.list(2)),
            vec!["test.approval", "test.echo", "test.future"]
        );
    }

    #[test]
    fn info_finds_registered_only() {
        let services = registry();
        assert_eq!(services.info("test.echo").map(|info| info.key), Some("test.echo"));
        assert!(services.info("test.absent").is_none());
    }

    #[test]
    fn call_routes_to_service_and_passes_caller() {
        let services = registry();
        let reply = services
            .call("probe", "test.echo", json!({ "n": 1 }))
            .unwrap();
        assert_eq!(reply, json!({ "caller": "probe", "args": { "n": 1 } }));
    }

    #[test]
    fn call_on_unknown_service_is_error() {
        let services = registry();
        let err = services.call("probe", "test.absent", json!({})).unwrap_err();
        assert_eq!(err, HostError::UnknownService("test.absent".to_string()));
    }

    #[test]
    fn duplicate_registration_is_rejected() {
        let mut services = HostServices::new();
        services.register(EchoService::new(&ECHO_INFO)).unwrap();
        let err = services.register(EchoService::new(&ECHO_INFO)).unwrap_err();
        assert!(err.contains("test.echo"), "错误必须指出是哪个键：{err}");
        assert_eq!(services.keys(), vec!["test.echo"]);
    }

    #[test]
    fn ctx_denies_service_outside_granted_set() {
        let api: Arc<dyn HostApi> = Arc::new(registry());
        let ctx = PluginCtx::for_test("probe", api, HashSet::new());
        let err = ctx.call("test.echo", json!({})).unwrap_err();
        assert_eq!(
            err,
            HostError::CapabilityDenied {
                caller: "probe".to_string(),
                key: "test.echo".to_string(),
            }
        );
        assert!(ctx.granted().is_empty());
        assert_eq!(ctx.id(), "probe");
    }

    #[test]
    fn ctx_forwards_granted_call_with_its_own_id() {
        let api: Arc<dyn HostApi> = Arc::new(registry());
        let granted: HashSet<String> = ["test.echo".to_string()].into_iter().collect();
        let ctx = PluginCtx::for_test("probe", api, granted);
        let reply = ctx.call("test.echo", json!({ "n": 2 })).unwrap();
        assert_eq!(reply, json!({ "caller": "probe", "args": { "n": 2 } }));
        assert_eq!(ctx.granted().len(), 1);
    }

    #[test]
    fn resolve_granted_keeps_declared_registered_keys() {
        let services = registry();
        let granted = resolve_granted(&services, &["test.echo".to_string()], 1).unwrap();
        assert_eq!(granted.len(), 1);
        assert!(granted.contains("test.echo"));
    }

    #[test]
    fn resolve_granted_rejects_unknown_key_and_names_it() {
        let services = registry();
        let err = resolve_granted(
            &services,
            &["test.echo".to_string(), "game.capture".to_string()],
            1,
        )
        .unwrap_err();
        assert_eq!(err, GrantError::Unknown(vec!["game.capture".to_string()]));
        assert!(err.to_string().contains("game.capture"));
    }

    #[test]
    fn resolve_granted_rejects_approval_services_without_approver() {
        let services = registry();
        let err = resolve_granted(&services, &["test.approval".to_string()], 1).unwrap_err();
        assert_eq!(err, GrantError::NeedsApproval(vec!["test.approval".to_string()]));
    }

    #[test]
    fn resolve_granted_rejects_service_newer_than_protocol() {
        let services = registry();
        let err = resolve_granted(&services, &["test.future".to_string()], 1).unwrap_err();
        assert_eq!(
            err,
            GrantError::TooNew {
                keys: vec!["test.future".to_string()],
                since: 2,
                protocol: 1,
            }
        );
    }

    #[test]
    fn resolve_granted_accepts_empty_declaration() {
        let services = registry();
        let granted = resolve_granted(&services, &[], 1).unwrap();
        assert!(granted.is_empty());
    }
}
