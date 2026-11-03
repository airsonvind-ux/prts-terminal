//! 插件管理命令（M3 §七）：列表、启停、安装预览/确认、卸载、重试。
//!
//! 全部 `async`：里面要么碰盘（安装/卸载），要么要拿几把锁，跑在主线程会卡界面。
//! 真正的活儿交给 `spawn_blocking`，命令本身只做参数搬运。

#![allow(clippy::too_many_arguments)] // Tauri 命令参数面与前端 invoke 契约固定，拆 struct 会改变 IPC 形状

use super::prelude::*;

use crate::host::HostServices;
use crate::plugins::install::{InstallPreview, Installer};
use crate::plugins::{
    PluginKind, PluginRecord, PluginRegistry, PluginRoots, PluginState, PluginsConfig,
};

/// 有设置页的内置插件。前端 `PluginSettingsHost` 有一张同名的组件表——
/// 两边一起改才生效，所以别再往别处硬编码 id。
const PLUGINS_WITH_SETTINGS_PAGE: [&str; 1] = ["maa"];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginView {
    pub id: String,
    pub name: String,
    pub version: String,
    /// `builtin` | `process`
    pub kind: String,
    /// `loaded` | `starting` | `failed` | `faulted` | `disabled`
    pub state: String,
    /// 异常原因（只有 failed / faulted 有）。
    pub reason: Option<String>,
    /// 这个插件注册的工具名。只给名字不给数量：用户看「工具 1」没有意义，
    /// 看 `run_maa_task` 才知道它到底往对话里加了什么。
    pub tools: Vec<String>,
    pub has_settings_page: bool,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UnregisteredDir {
    pub dir_name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginList {
    pub plugins: Vec<PluginView>,
    pub unregistered: Vec<UnregisteredDir>,
}

/// 一条台账记录 → 一行的显示数据。
fn view_of(record: &PluginRecord) -> PluginView {
    let (state, reason, tools) = match &record.state {
        PluginState::Starting => ("starting".to_string(), None, Vec::new()),
        PluginState::Loaded => ("loaded".to_string(), None, record.tools.clone()),
        PluginState::Faulted(reason) => ("faulted".to_string(), Some(reason.clone()), Vec::new()),
        PluginState::Failed(reason) => ("failed".to_string(), Some(reason.clone()), Vec::new()),
        PluginState::Disabled => ("disabled".to_string(), None, Vec::new()),
    };
    PluginView {
        id: record.manifest.id.clone(),
        name: record.manifest.name.clone(),
        version: record.manifest.version.clone(),
        kind: match record.manifest.kind {
            PluginKind::Builtin => "builtin".to_string(),
            PluginKind::Process => "process".to_string(),
        },
        state,
        reason,
        tools,
        has_settings_page: record.manifest.kind == PluginKind::Builtin
            && PLUGINS_WITH_SETTINGS_PAGE.contains(&record.manifest.id.as_str()),
        capabilities: record.manifest.capabilities.clone(),
    }
}

/// 台账快照 → 列表（在持锁区内只做拷贝，出锁后再组装）。
fn list_from_ledger(ledger: &Arc<Mutex<PluginRegistry>>) -> PluginList {
    let guard = ledger.lock().unwrap_or_else(|poison| poison.into_inner());
    let mut plugins: Vec<PluginView> = guard.list().iter().map(|record| view_of(record)).collect();
    plugins.sort_by(|a, b| a.id.cmp(&b.id));
    let unregistered = guard
        .unregistered()
        .iter()
        .map(|dir| UnregisteredDir {
            dir_name: dir.dir_name.clone(),
            reason: dir.reason.clone(),
        })
        .collect();
    PluginList {
        plugins,
        unregistered,
    }
}

#[tauri::command]
pub fn list_plugins(ledger: State<'_, Arc<Mutex<PluginRegistry>>>) -> PluginList {
    list_from_ledger(&ledger)
}

/// 台账里的展示名（提醒文案要用）；查不到就退回 id，不编造。
fn plugin_name(ledger: &Arc<Mutex<PluginRegistry>>, id: &str) -> String {
    let guard = ledger.lock().unwrap_or_else(|poison| poison.into_inner());
    guard
        .get(id)
        .map(|record| record.manifest.name.clone())
        .unwrap_or_else(|| id.to_string())
}

/// 收权类操作（停用 / 卸载 / 重试）的并发闸：有前台轮在跑就拒绝。
///
/// 收权会撕掉别人正在用的东西——停用 MAA 会连桥一起收，正在跑的长任务永远
/// 等不到结果。给权（启用 / 安装）不设闸：它们不杀任何东西，也不影响当前轮
/// （前缀已冻结）。
fn ensure_no_active_turn(turns: &TurnRegistry, action: &str) -> Result<(), String> {
    if turns.active_count() > 0 {
        Err(format!(
            "对话处理中，{action}会打断正在执行的任务；等这一轮结束再试"
        ))
    } else {
        Ok(())
    }
}

/// 「安装文件夹」按钮：选一个目录，交给 `preview_plugin_install` 做预览。
///
/// 与设置页其它选择器同构：`run_on_main_thread` 弹窗，oneshot 桥回 async 命令；
/// 用户取消 → `Ok(None)`。
#[tauri::command]
pub async fn pick_plugin_folder(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
    app.dialog()
        .file()
        .set_title("选择插件文件夹")
        .pick_folder(move |picked| {
            let value = picked.and_then(|fp| fp.as_path().map(|p| p.to_string_lossy().into_owned()));
            let _ = tx.send(value);
        });
    rx.await.map_err(|e| format!("目录选择失败: {e}"))
}

#[tauri::command]
pub async fn set_plugin_enabled(
    id: String,
    enabled: bool,
    reg: State<'_, Arc<ToolRegistry>>,
    config: State<'_, Arc<PluginsConfig>>,
    services: State<'_, Arc<HostServices>>,
    ledger: State<'_, Arc<Mutex<PluginRegistry>>>,
    notices: State<'_, Arc<Mutex<Notices>>>,
    turn_registry: State<'_, Arc<TurnRegistry>>,
) -> Result<PluginList, String> {
    let reg = Arc::clone(&reg);
    let config = Arc::clone(&config);
    let services = Arc::clone(&services);
    let ledger = Arc::clone(&ledger);
    let notices = Arc::clone(&notices);
    let turns = Arc::clone(&turn_registry);
    tauri::async_runtime::spawn_blocking(move || {
        let name = plugin_name(&ledger, &id);
        if enabled {
            crate::plugins::enable_plugin(&id, &reg, &config, &services, &ledger)?;
        } else {
            ensure_no_active_turn(&turns, "停用")?;
            crate::plugins::disable_plugin(&id, &reg, &config, &services, &ledger)?;
        }
        let context = format!(
            "插件 {name} {}",
            if enabled { "已启用" } else { "已停用" }
        );
        crate::plugins::refresh_catalogs(&reg, &notices, &ledger, Some(&context));
        Ok(list_from_ledger(&ledger))
    })
    .await
    .map_err(|error| format!("插件操作没有跑完：{error}"))?
}

#[tauri::command]
pub async fn preview_plugin_install(
    path: String,
    roots: State<'_, Arc<PluginRoots>>,
    installer: State<'_, Arc<Installer>>,
    services: State<'_, Arc<HostServices>>,
    ledger: State<'_, Arc<Mutex<PluginRegistry>>>,
) -> Result<InstallPreview, String> {
    let roots = Arc::clone(&roots);
    let installer = Arc::clone(&installer);
    let services = Arc::clone(&services);
    let ledger = Arc::clone(&ledger);
    tauri::async_runtime::spawn_blocking(move || {
        let known = {
            let guard = ledger.lock().unwrap_or_else(|poison| poison.into_inner());
            guard
                .list()
                .iter()
                .map(|record| record.manifest.id.clone())
                .collect()
        };
        installer.preview(std::path::Path::new(&path), &roots, &services, &known)
    })
    .await
    .map_err(|error| format!("预览没有跑完：{error}"))?
}

#[tauri::command]
pub async fn confirm_plugin_install(
    token: String,
    roots: State<'_, Arc<PluginRoots>>,
    installer: State<'_, Arc<Installer>>,
    config: State<'_, Arc<PluginsConfig>>,
    services: State<'_, Arc<HostServices>>,
    reg: State<'_, Arc<ToolRegistry>>,
    ledger: State<'_, Arc<Mutex<PluginRegistry>>>,
    notices: State<'_, Arc<Mutex<Notices>>>,
) -> Result<PluginList, String> {
    let roots = Arc::clone(&roots);
    let installer = Arc::clone(&installer);
    let config = Arc::clone(&config);
    let services = Arc::clone(&services);
    let reg = Arc::clone(&reg);
    let ledger = Arc::clone(&ledger);
    let notices = Arc::clone(&notices);
    tauri::async_runtime::spawn_blocking(move || {
        let record = installer.confirm(&token, &roots, &config, &services, &reg, &ledger)?;
        let context = format!("插件 {} 已安装", record.manifest.name);
        crate::plugins::refresh_catalogs(&reg, &notices, &ledger, Some(&context));
        Ok(list_from_ledger(&ledger))
    })
    .await
    .map_err(|error| format!("安装没有跑完：{error}"))?
}

#[tauri::command]
pub async fn uninstall_plugin(
    id: String,
    delete_data: bool,
    roots: State<'_, Arc<PluginRoots>>,
    config: State<'_, Arc<PluginsConfig>>,
    reg: State<'_, Arc<ToolRegistry>>,
    ledger: State<'_, Arc<Mutex<PluginRegistry>>>,
    notices: State<'_, Arc<Mutex<Notices>>>,
    turn_registry: State<'_, Arc<TurnRegistry>>,
) -> Result<PluginList, String> {
    let roots = Arc::clone(&roots);
    let config = Arc::clone(&config);
    let reg = Arc::clone(&reg);
    let ledger = Arc::clone(&ledger);
    let notices = Arc::clone(&notices);
    let turns = Arc::clone(&turn_registry);
    tauri::async_runtime::spawn_blocking(move || {
        let name = plugin_name(&ledger, &id);
        ensure_no_active_turn(&turns, "卸载")?;
        crate::plugins::uninstall_plugin(&id, delete_data, &roots, &config, &reg, &ledger)?;
        let context = format!("插件 {name} 已卸载");
        crate::plugins::refresh_catalogs(&reg, &notices, &ledger, Some(&context));
        Ok(list_from_ledger(&ledger))
    })
    .await
    .map_err(|error| format!("卸载没有跑完：{error}"))?
}

#[tauri::command]
pub async fn retry_plugin(
    id: String,
    ledger: State<'_, Arc<Mutex<PluginRegistry>>>,
    turn_registry: State<'_, Arc<TurnRegistry>>,
) -> Result<PluginList, String> {
    let ledger = Arc::clone(&ledger);
    let turns = Arc::clone(&turn_registry);
    tauri::async_runtime::spawn_blocking(move || {
        ensure_no_active_turn(&turns, "重试")?;
        crate::plugins::retry_plugin(&id, &ledger)?;
        Ok(list_from_ledger(&ledger))
    })
    .await
    .map_err(|error| format!("重试没有跑完：{error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::parse_manifest;
    use std::path::PathBuf;

    fn record(id: &str, state: PluginState, tools: &[&str]) -> PluginRecord {
        let text = format!(
            r#"{{"manifestVersion":1,"id":"{id}","name":"插件 {id}","version":"0.1.1",
                "kind":"builtin","activation":[],"capabilities":["maa.bridge"],
                "contributes":{{}}}}"#
        );
        PluginRecord {
            manifest: parse_manifest(&text).expect("样本 manifest"),
            state,
            tools: tools.iter().map(|name| name.to_string()).collect(),
            reported_capabilities: Vec::new(),
            dir: PathBuf::from(format!("C:/plugins/{id}")),
        }
    }

    #[test]
    fn loaded_rows_carry_tool_names_and_abnormal_rows_carry_none() {
        let loaded = view_of(&record("maa", PluginState::Loaded, &["run_maa_task"]));
        assert_eq!(loaded.state, "loaded");
        assert_eq!(loaded.tools, vec!["run_maa_task"], "给名字，不给数量");
        assert!(loaded.has_settings_page, "maa 有设置页");
        assert_eq!(loaded.kind, "builtin");

        let failed = view_of(&record("x", PluginState::Failed("坏了".into()), &[]));
        assert_eq!(failed.state, "failed");
        assert_eq!(failed.reason.as_deref(), Some("坏了"));
        assert!(failed.tools.is_empty(), "异常行没有工具面");
        assert!(!failed.has_settings_page);

        let faulted = view_of(&record("y", PluginState::Faulted("崩了".into()), &["t"]));
        assert_eq!(faulted.state, "faulted");
        assert!(faulted.tools.is_empty());

        let disabled = view_of(&record("z", PluginState::Disabled, &[]));
        assert_eq!(disabled.state, "disabled");
        assert!(disabled.tools.is_empty());

        let starting = view_of(&record("s", PluginState::Starting, &[]));
        assert_eq!(starting.state, "starting");
        assert!(starting.tools.is_empty());
    }

    #[test]
    fn list_is_sorted_and_carries_unregistered_dirs() {
        let mut registry = PluginRegistry::new();
        registry.insert(record("zeta", PluginState::Loaded, &[]));
        registry.insert(record("alpha", PluginState::Loaded, &[]));
        registry.insert_unregistered("broken", "manifest 读不出来");
        let ledger = Arc::new(Mutex::new(registry));

        let list = list_from_ledger(&ledger);
        let ids: Vec<&str> = list.plugins.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "zeta"], "列表按 id 排序");
        assert_eq!(list.unregistered.len(), 1);
        assert_eq!(list.unregistered[0].dir_name, "broken");
    }

    #[test]
    fn the_wire_shape_is_camel_case() {
        let list = PluginList {
            plugins: vec![view_of(&record("maa", PluginState::Loaded, &["t"]))],
            unregistered: vec![UnregisteredDir {
                dir_name: "broken".into(),
                reason: "坏".into(),
            }],
        };
        let value = serde_json::to_value(&list).expect("序列化");
        let row = &value["plugins"][0];
        assert!(row.get("tools").is_some(), "前端读 tools");
        assert!(row.get("hasSettingsPage").is_some());
        assert!(value["unregistered"][0].get("dirName").is_some());
    }

    #[test]
    fn taking_away_is_refused_while_a_turn_runs() {
        let turns = TurnRegistry::new();
        assert!(ensure_no_active_turn(&turns, "停用").is_ok(), "空闲时可收权");

        let _signal = turns.register("session", "conversation");
        assert_eq!(turns.active_count(), 1);

        let refused = ensure_no_active_turn(&turns, "停用").expect_err("处理中应拒绝");
        assert!(
            refused.contains("停用会打断正在执行的任务") && refused.contains("等这一轮结束再试"),
            "文案要说明原因与动作：{refused}"
        );
        assert!(
            ensure_no_active_turn(&turns, "卸载").is_err(),
            "卸载同属收权，也要拒"
        );
    }
}
