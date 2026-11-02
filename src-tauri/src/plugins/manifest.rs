//! 插件 manifest：`plugin.json` 的结构、解析与十条校验。
//!
//! 校验顺序固定（规格 §5），第一条失败即返回，错误信息带字段名与实际值。
//! `id` 与目录名不一致只告警不拒绝——目录名是打包细节，`id` 才是契约。

use std::collections::HashSet;
use std::path::{Component, Path};

use semver::{Version, VersionReq};
use serde::Deserialize;
use serde_json::Value;

use super::PluginError;

/// 插件目录下的固定入口文件名。
pub const MANIFEST_FILE_NAME: &str = "plugin.json";

/// 本宿主支持的插件协议版本（M2 的 process 通道握手用；M1 只校验声明）。
pub const SUPPORTED_PROTOCOL: u32 = 1;

/// capability 白名单（D25：未知即拒绝，不做静默降级）。
pub const KNOWN_CAPABILITIES: [&str; 4] = ["maa.bridge", "fs.pluginData", "agent.query", "network"];

/// 插件 manifest（`plugin.json`）。
///
/// 未知字段一律忽略（serde 默认行为，向前兼容）。`capabilities` 用
/// `Vec<String>` 而不是带 `#[serde(other)]` 的 enum——后者会把未知值静默
/// 吞成某个默认分支，违反 D25 的「指出是哪一条」。
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    #[serde(default)]
    pub manifest_version: Option<u32>,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    pub kind: PluginKind,
    #[serde(default)]
    pub entry: Option<PluginEntry>,
    #[serde(default)]
    pub requires: Option<Requirements>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub network: Option<NetworkPolicy>,
    #[serde(default)]
    pub activation: Vec<String>,
    #[serde(default)]
    pub contributes: Contributions,
}

/// 插件形态。M1 只有 `builtin`（代码与宿主同进程）；`process` 留给 M2。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Builtin,
    Process,
}

/// `process` 插件的入口声明。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginEntry {
    pub exe: String,
    #[serde(default)]
    pub transport: Option<String>,
}

/// 插件对宿主的要求。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Requirements {
    #[serde(default)]
    pub app: Option<String>,
    #[serde(default)]
    pub protocol: Option<u32>,
}

/// 网络策略声明（只有 capability 含 `network` 时才校验）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPolicy {
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

/// manifest 的贡献声明。M1 只消费 `settingsForm` 的存在性，不渲染表单。
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contributions {
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub settings_form: Option<String>,
    #[serde(default)]
    pub settings_schema: Option<Value>,
    #[serde(default)]
    pub assets: Vec<String>,
}

/// 从文本解析 manifest（不做校验，校验见 [`validate`]）。
///
/// 返回 `String` 而不是 `PluginError`：调用方（[`read_manifest`]）负责补上
/// 路径上下文。
pub fn parse_manifest(text: &str) -> Result<PluginManifest, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

/// 读取并解析插件目录下的 `plugin.json`。
pub fn read_manifest(dir: &Path) -> Result<PluginManifest, PluginError> {
    let path = dir.join(MANIFEST_FILE_NAME);
    let text = std::fs::read_to_string(&path).map_err(|e| PluginError::ManifestRead {
        path: path.display().to_string(),
        error: e.to_string(),
    })?;
    parse_manifest(&text).map_err(|error| PluginError::ManifestParse {
        path: path.display().to_string(),
        error,
    })
}

/// 十条校验（规格 §5）：按顺序执行，第一条失败即返回。
///
/// `dir_name` 只用于「id 与目录名不一致」的告警；`seen_ids` 由调用方跨目录
/// 共享，用于规则 2 的同父目录 id 去重（`load_all` 按目录名排序后逐个传入）。
pub fn validate(
    manifest: &PluginManifest,
    dir_name: &str,
    seen_ids: &mut HashSet<String>,
) -> Result<(), PluginError> {
    // 1. id 格式
    if !is_valid_id(&manifest.id) {
        return Err(PluginError::Invalid {
            field: "id",
            value: manifest.id.clone(),
        });
    }

    // 2. 同一父目录下 id 唯一（第二个同 id 目录不再注册工具）
    if !seen_ids.insert(manifest.id.clone()) {
        return Err(PluginError::DuplicateId(manifest.id.clone()));
    }

    // 3. version 必须是合法 semver
    if Version::parse(&manifest.version).is_err() {
        return Err(PluginError::Invalid {
            field: "version",
            value: manifest.version.clone(),
        });
    }

    // 4. builtin 不得声明 entry（代码就在宿主进程里，没有可执行入口）
    if manifest.kind == PluginKind::Builtin && manifest.entry.is_some() {
        return Err(PluginError::Invalid {
            field: "entry",
            value: "builtin 插件不得声明 entry".to_string(),
        });
    }

    // 5. process 必须有合法的相对入口
    if manifest.kind == PluginKind::Process {
        let exe = match manifest.entry.as_ref() {
            Some(entry) => entry.exe.as_str(),
            None => {
                return Err(PluginError::Invalid {
                    field: "entry",
                    value: "process 插件缺少 entry".to_string(),
                })
            }
        };
        check_exe(exe)?;
    }

    // 6. 协议版本：process 必填且为 1；builtin 可省略，填了也须为 1
    let protocol = manifest.requires.as_ref().and_then(|r| r.protocol);
    match manifest.kind {
        PluginKind::Process if protocol != Some(SUPPORTED_PROTOCOL) => {
            return Err(PluginError::UnsupportedProtocol(protocol));
        }
        PluginKind::Builtin => {
            if let Some(found) = protocol {
                if found != SUPPORTED_PROTOCOL {
                    return Err(PluginError::UnsupportedProtocol(protocol));
                }
            }
        }
        _ => {}
    }

    // 7. requires.app：可解析且匹配当前宿主版本
    if let Some(app) = manifest.requires.as_ref().and_then(|r| r.app.as_deref()) {
        let required = VersionReq::parse(app).map_err(|_| PluginError::Invalid {
            field: "requires.app",
            value: app.to_string(),
        })?;
        let current = Version::parse(env!("CARGO_PKG_VERSION"))
            .expect("CARGO_PKG_VERSION 必须是合法 semver");
        if !required.matches(&current) {
            return Err(PluginError::RequiresApp {
                required: app.to_string(),
                current: current.to_string(),
            });
        }
    }

    // 8. capability 逐条白名单
    for cap in &manifest.capabilities {
        if !KNOWN_CAPABILITIES.contains(&cap.as_str()) {
            return Err(PluginError::UnknownCapability(cap.clone()));
        }
    }

    // 9. 声明 network 就必须说清楚能连哪
    if manifest.capabilities.iter().any(|c| c == "network") {
        let hosts = manifest
            .network
            .as_ref()
            .map(|n| n.allowed_hosts.as_slice())
            .unwrap_or(&[]);
        if hosts.is_empty() {
            return Err(PluginError::Invalid {
                field: "network.allowedHosts",
                value: "声明 network 能力但 allowedHosts 为空".to_string(),
            });
        }
        if hosts.iter().any(|h| h.trim().is_empty()) {
            return Err(PluginError::Invalid {
                field: "network.allowedHosts",
                value: "allowedHosts 含空条目".to_string(),
            });
        }
    }

    // 10. 设置表单声明
    if let Some(form) = manifest.contributes.settings_form.as_deref() {
        match form {
            "builtin-page" => {}
            "schema" => {
                let is_object = manifest
                    .contributes
                    .settings_schema
                    .as_ref()
                    .and_then(|s| s.get("type"))
                    .and_then(|t| t.as_str())
                    .map(|t| t == "object")
                    .unwrap_or(false);
                if !is_object {
                    return Err(PluginError::Invalid {
                        field: "settingsForm",
                        value: "settingsForm=schema 时 settingsSchema 必须是 type: object"
                            .to_string(),
                    });
                }
            }
            other => {
                return Err(PluginError::Invalid {
                    field: "settingsForm",
                    value: other.to_string(),
                })
            }
        }
    }

    // id 与目录名不一致：只告警（按 id 记账，目录名不参与契约）
    if manifest.id != dir_name {
        eprintln!(
            "[plugins] 目录名与 manifest id 不一致：{dir_name} ≠ {}（按 id 记账）",
            manifest.id
        );
    }

    Ok(())
}

/// `^[a-z][a-z0-9-]{1,63}$`（不引入 regex 依赖，手写字符判断）。
fn is_valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    let rest: Vec<char> = chars.collect();
    if rest.is_empty() || rest.len() > 63 {
        return false;
    }
    rest.iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
}

/// 规则 5 的 exe 检查：非空、相对路径、不含 `..`、无盘符。
fn check_exe(exe: &str) -> Result<(), PluginError> {
    if exe.trim().is_empty() {
        return Err(PluginError::Invalid {
            field: "entry.exe",
            value: "为空".to_string(),
        });
    }
    let path = Path::new(exe);
    if path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
    {
        return Err(PluginError::Invalid {
            field: "entry.exe",
            value: format!("{exe}（必须是相对路径）"),
        });
    }
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(PluginError::Invalid {
            field: "entry.exe",
            value: format!("{exe}（不得包含 ..）"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
    {
      "manifestVersion": 1,
      "id": "maa",
      "name": "MAA 自动化",
      "version": "0.1.1",
      "description": "把游戏操作交给本机 MAA",
      "kind": "builtin",
      "requires": { "app": ">=0.1.1" },
      "capabilities": ["maa.bridge"],
      "activation": ["onStartup"],
      "contributes": { "skills": [], "settingsForm": "builtin-page", "assets": [] }
    }
    "#;

    fn sample_value() -> Value {
        serde_json::from_str(SAMPLE).expect("样本 JSON 应合法")
    }

    fn from_value(v: &Value) -> PluginManifest {
        serde_json::from_value(v.clone()).expect("字段结构应可反序列化")
    }

    fn check(v: &Value) -> Result<(), PluginError> {
        let mut seen = HashSet::new();
        validate(&from_value(v), "maa", &mut seen)
    }

    fn process_value() -> Value {
        serde_json::json!({
            "id": "probe",
            "name": "探针",
            "version": "0.1.0",
            "kind": "process",
            "entry": { "exe": "bin/probe.exe" },
            "requires": { "protocol": 1 }
        })
    }

    #[test]
    fn sample_manifest_parses_with_all_fields() {
        let m = from_value(&sample_value());
        assert_eq!(m.manifest_version, Some(1));
        assert_eq!(m.id, "maa");
        assert_eq!(m.name, "MAA 自动化");
        assert_eq!(m.version, "0.1.1");
        assert_eq!(m.description.as_deref(), Some("把游戏操作交给本机 MAA"));
        assert_eq!(m.kind, PluginKind::Builtin);
        assert!(m.entry.is_none());
        assert_eq!(m.requires.as_ref().unwrap().app.as_deref(), Some(">=0.1.1"));
        assert_eq!(m.capabilities, vec!["maa.bridge"]);
        assert_eq!(m.activation, vec!["onStartup"]);
        assert_eq!(
            m.contributes.settings_form.as_deref(),
            Some("builtin-page")
        );
        assert!(check(&sample_value()).is_ok());
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let mut v = sample_value();
        v["futureField"] = serde_json::json!({ "anything": [1, 2, 3] });
        let m = from_value(&v);
        assert_eq!(m.id, "maa");
        assert!(check(&v).is_ok());
    }

    #[test]
    fn rejects_invalid_ids() {
        for bad in ["Maa", "_maa", "a", "maa_plugin", "maa.plugin", &"a".repeat(65)] {
            let mut v = sample_value();
            v["id"] = serde_json::json!(bad);
            match check(&v) {
                Err(PluginError::Invalid { field: "id", .. }) => {}
                other => panic!("id={bad:?} 应被拒绝，got: {other:?}"),
            }
        }
        // 边界：64 字符仍然合法，65 字符越界
        assert!(is_valid_id(&"a".repeat(64)));
        assert!(!is_valid_id(&"a".repeat(65)));
    }

    #[test]
    fn rejects_bad_version() {
        let mut v = sample_value();
        v["version"] = serde_json::json!("0.1");
        match check(&v) {
            Err(PluginError::Invalid { field: "version", .. }) => {}
            other => panic!("非法版本应被拒绝，got: {other:?}"),
        }
    }

    #[test]
    fn builtin_rejects_entry() {
        let mut v = sample_value();
        v["entry"] = serde_json::json!({ "exe": "bin/maa.exe" });
        match check(&v) {
            Err(PluginError::Invalid { field: "entry", .. }) => {}
            other => panic!("builtin 带 entry 应被拒绝，got: {other:?}"),
        }
    }

    #[test]
    fn process_requires_entry() {
        let mut v = process_value();
        v.as_object_mut().unwrap().remove("entry");
        match check(&v) {
            Err(PluginError::Invalid { field: "entry", .. }) => {}
            other => panic!("process 缺 entry 应被拒绝，got: {other:?}"),
        }
    }

    #[test]
    fn process_rejects_absolute_or_escaping_exe() {
        for bad in [
            "C:\\tools\\maa.exe",
            "C:maa.exe",
            "\\\\server\\share\\maa.exe",
            "/usr/bin/maa",
            "../maa.exe",
            "bin/../../maa.exe",
            "   ",
        ] {
            let mut v = process_value();
            v["entry"]["exe"] = serde_json::json!(bad);
            match check(&v) {
                Err(PluginError::Invalid { field: "entry.exe", .. }) => {}
                other => panic!("exe={bad:?} 应被拒绝，got: {other:?}"),
            }
        }
        // 合法相对路径放行
        let mut ok = process_value();
        ok["entry"]["exe"] = serde_json::json!("bin/maa.exe");
        assert!(check(&ok).is_ok());
    }

    #[test]
    fn process_requires_protocol_one() {
        let mut missing = process_value();
        missing["requires"] = serde_json::json!({});
        match check(&missing) {
            Err(PluginError::UnsupportedProtocol(None)) => {}
            other => panic!("process 缺 protocol 应被拒绝，got: {other:?}"),
        }

        let mut two = process_value();
        two["requires"]["protocol"] = serde_json::json!(2);
        match check(&two) {
            Err(PluginError::UnsupportedProtocol(Some(2))) => {}
            other => panic!("protocol=2 应被拒绝，got: {other:?}"),
        }

        // builtin 省略 protocol 合法
        assert!(check(&sample_value()).is_ok());

        // builtin 填了协议但版本不对也要拒绝
        let mut bad_builtin = sample_value();
        bad_builtin["requires"]["protocol"] = serde_json::json!(2);
        match check(&bad_builtin) {
            Err(PluginError::UnsupportedProtocol(Some(2))) => {}
            other => panic!("builtin protocol=2 应被拒绝，got: {other:?}"),
        }
    }

    #[test]
    fn rejects_requires_app_above_current_version() {
        let mut v = sample_value();
        v["requires"]["app"] = serde_json::json!(">=9.0.0");
        match check(&v) {
            Err(PluginError::RequiresApp { required, current }) => {
                assert_eq!(required, ">=9.0.0");
                assert_eq!(current, env!("CARGO_PKG_VERSION"));
            }
            other => panic!("过高版本要求应被拒绝，got: {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_capability() {
        let mut v = sample_value();
        v["capabilities"] = serde_json::json!(["maa.bridge", "fs.everything"]);
        match check(&v) {
            Err(PluginError::UnknownCapability(name)) => assert_eq!(name, "fs.everything"),
            other => panic!("未知 capability 应被拒绝，got: {other:?}"),
        }
    }

    #[test]
    fn network_capability_requires_allowed_hosts() {
        let mut v = sample_value();
        v["capabilities"] = serde_json::json!(["network"]);
        match check(&v) {
            Err(PluginError::Invalid {
                field: "network.allowedHosts",
                ..
            }) => {}
            other => panic!("缺 allowedHosts 应被拒绝，got: {other:?}"),
        }

        v["network"] = serde_json::json!({ "allowedHosts": ["api.example.com"] });
        assert!(check(&v).is_ok());

        v["network"] = serde_json::json!({ "allowedHosts": [""] });
        match check(&v) {
            Err(PluginError::Invalid {
                field: "network.allowedHosts",
                ..
            }) => {}
            other => panic!("空 allowedHosts 条目应被拒绝，got: {other:?}"),
        }
    }

    #[test]
    fn settings_form_must_be_known() {
        let mut v = sample_value();
        v["contributes"]["settingsForm"] = serde_json::json!("webview");
        match check(&v) {
            Err(PluginError::Invalid {
                field: "settingsForm",
                ..
            }) => {}
            other => panic!("未知 settingsForm 应被拒绝，got: {other:?}"),
        }

        // schema 表单必须带 type: object 的 settingsSchema
        v["contributes"]["settingsForm"] = serde_json::json!("schema");
        v["contributes"]["settingsSchema"] = serde_json::json!({ "type": "string" });
        match check(&v) {
            Err(PluginError::Invalid {
                field: "settingsForm",
                ..
            }) => {}
            other => panic!("schema 表单缺 object schema 应被拒绝，got: {other:?}"),
        }

        v["contributes"]["settingsSchema"] = serde_json::json!({ "type": "object", "properties": {} });
        assert!(check(&v).is_ok());
    }

    #[test]
    fn duplicate_id_is_rejected() {
        let mut seen = HashSet::new();
        let m = from_value(&sample_value());
        assert!(validate(&m, "maa", &mut seen).is_ok());
        match validate(&m, "maa-copy", &mut seen) {
            Err(PluginError::DuplicateId(id)) => assert_eq!(id, "maa"),
            other => panic!("重复 id 应被拒绝，got: {other:?}"),
        }
    }

    #[test]
    fn corrupt_json_is_an_error_not_a_panic() {
        let err = parse_manifest("{ \"id\": ").unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn read_manifest_reports_missing_file_and_bad_json() {
        let dir = std::env::temp_dir().join(format!(
            "prts-plugin-manifest-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        match read_manifest(&dir) {
            Err(PluginError::ManifestRead { .. }) => {}
            other => panic!("缺失 manifest 应报 ManifestRead，got: {other:?}"),
        }

        std::fs::write(dir.join(MANIFEST_FILE_NAME), "{ not json").unwrap();
        match read_manifest(&dir) {
            Err(PluginError::ManifestParse { .. }) => {}
            other => panic!("损坏 JSON 应报 ManifestParse，got: {other:?}"),
        }

        std::fs::write(dir.join(MANIFEST_FILE_NAME), SAMPLE).unwrap();
        let m = read_manifest(&dir).expect("样本应可读");
        assert_eq!(m.id, "maa");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
