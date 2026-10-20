//! 应用内更新服务（Phase 5 · 4.4）
//!
//! 责任边界：
//! - `tauri-plugin-updater`：单源清单解析、下载、签名校验、NSIS 安装；
//! - 本模块：源顺序与重试、状态机、事件、下载完成后等待用户确认安装、
//!   安装前的活跃对话与 MAA 保护。
//!
//! 前端只通过命令与 `update-status` 事件消费；不直接持有 updater 权限。
//! 开发模式（`tauri dev`）不自动检查，也拒绝安装。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::{Error as UpdaterError, Update, UpdaterExt};
use url::Url;

use crate::turn_registry::TurnRegistry;

/// 前端监听的更新状态事件名。
pub const UPDATE_EVENT: &str = "update-status";

const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
const RETRY_DELAY: Duration = Duration::from_millis(400);
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(120);
/// 下载/安装前的最低剩余空间。安装包约 12 MB，但解压与替换需要余量。
const MIN_FREE_SPACE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdatePhase {
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Downloaded,
    Installing,
    Error,
    Dev,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateErrorStage {
    Check,
    Download,
    Install,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSourceInfo {
    pub id: String,
    pub label: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableUpdate {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
    pub pub_date: Option<String>,
    pub source: String,
    /// 清单里的安装包直链；下载失败时供前端提供手动下载。
    pub download_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub phase: UpdatePhase,
    pub current_version: String,
    pub dev_mode: bool,
    pub sources: Vec<UpdateSourceInfo>,
    pub available: Option<AvailableUpdate>,
    pub source: Option<String>,
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub last_checked_at: Option<String>,
    /// 部分源失败但仍有可用结论时的非致命提示。
    pub warnings: Vec<String>,
    /// 发布页或手动下载入口；来自配置，和可用更新的安装包直链分开。
    pub manual_download_url: Option<String>,
    pub error: Option<String>,
    /// 失败发生在哪个阶段；前端据此区分“检查失败/下载失败/安装失败”。
    pub error_stage: Option<UpdateErrorStage>,
}

/// 安装前写入、重启后展示一次的更新日志。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostInstallNotice {
    pub version: String,
    pub previous_version: String,
    pub notes: Option<String>,
    pub pub_date: Option<String>,
}

/// 安装前阻塞原因（前端据此禁用按钮并说明原因；后端仍会复检）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallBlockers {
    pub active_turn: bool,
    pub maa_running: bool,
}

/// 已下载、等待用户确认安装的更新。
struct DownloadedUpdate {
    bytes: Vec<u8>,
    update: Update,
}

struct Runtime {
    status: UpdateStatus,
    pending: Option<DownloadedUpdate>,
}

/// 单源检查错误。`retryable` 决定是否在源内重试；404/缺清单不会重试。
struct SourceError {
    message: String,
    retryable: bool,
}

impl SourceError {
    fn retryable(message: String) -> Self {
        Self {
            message,
            retryable: true,
        }
    }

    fn permanent(message: String) -> Self {
        Self {
            message,
            retryable: false,
        }
    }
}

/// 多源检查结果：选中的最高版本 + 其他源的非致命错误。
struct CheckOutcome {
    found: Option<(UpdateSourceInfo, Update)>,
    warnings: Vec<String>,
}

/// 更新服务共享状态。
///
/// `busy` 是单飞闸：检查、下载、安装同一时刻只允许一个。
/// `runtime` 只做短持锁，不跨 await。
pub struct UpdateService {
    runtime: Mutex<Runtime>,
    busy: AtomicBool,
}

struct BusyGuard<'a> {
    flag: &'a AtomicBool,
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

impl UpdateService {
    pub fn new(
        current_version: String,
        sources: Vec<UpdateSourceInfo>,
        manual_download_url: Option<String>,
    ) -> Self {
        let dev_mode = tauri::is_dev();
        Self {
            runtime: Mutex::new(Runtime {
                status: UpdateStatus {
                    // dev 下不启动自动检查；初始状态也直接呈现 Dev，
                    // 避免关于页先显示“尚未检查更新”再被拒。
                    phase: if dev_mode {
                        UpdatePhase::Dev
                    } else {
                        UpdatePhase::Idle
                    },
                    current_version,
                    dev_mode,
                    sources,
                    available: None,
                    source: None,
                    downloaded_bytes: None,
                    total_bytes: None,
                    last_checked_at: None,
                    warnings: Vec::new(),
                    manual_download_url,
                    error: None,
                    error_stage: None,
                },
                pending: None,
            }),
            busy: AtomicBool::new(false),
        }
    }

    fn acquire(&self) -> Result<BusyGuard<'_>, String> {
        self.busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| BusyGuard { flag: &self.busy })
            .map_err(|_| "更新任务正在进行中".to_string())
    }

    fn status(&self) -> UpdateStatus {
        let rt = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        rt.status.clone()
    }

    fn emit(&self, app: &AppHandle) {
        let _ = app.emit(UPDATE_EVENT, self.status());
    }

    fn fail(&self, app: &AppHandle, message: String, stage: UpdateErrorStage) {
        {
            let mut rt = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            rt.status.phase = UpdatePhase::Error;
            rt.status.error = Some(message);
            rt.status.error_stage = Some(stage);
        }
        self.emit(app);
    }
}

/// 从 `tauri.conf.json` 的 `plugins.updater.endpoints` 读取源。
///
/// 顺序就是尝试顺序：Gitee 在前，GitHub 在后；自定义源按配置顺序排列。
pub fn sources_from_config(app: &AppHandle) -> Vec<UpdateSourceInfo> {
    let Some(updater) = app.config().plugins.0.get("updater") else {
        return Vec::new();
    };
    let Some(endpoints) = updater.get("endpoints").and_then(Value::as_array) else {
        return Vec::new();
    };

    endpoints
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            let endpoint = value.as_str()?;
            let lower = endpoint.to_ascii_lowercase();
            let (id, label) = if lower.contains("gitee.com") {
                ("gitee".to_string(), "Gitee".to_string())
            } else if lower.contains("github.com") {
                ("github".to_string(), "GitHub".to_string())
            } else {
                (format!("source-{}", index + 1), "自定义源".to_string())
            };
            Some(UpdateSourceInfo {
                id,
                label,
                endpoint: endpoint.to_string(),
            })
        })
        .collect()
}

/// 从 `plugins.updater.manualDownloadUrl` 读取手动下载/发布页入口。
/// 空字符串按未配置处理；仓库建好后在配置里填发布页 URL。
pub fn manual_download_url_from_config(app: &AppHandle) -> Option<String> {
    let updater = app.config().plugins.0.get("updater")?;
    let raw = updater.get("manualDownloadUrl")?.as_str()?.trim();
    if raw.is_empty() {
        None
    } else {
        Some(raw.to_string())
    }
}

fn now_iso() -> String {
    chrono::Local::now().to_rfc3339()
}

/// 更新日志：`~/.prts/logs/update.log`（开发构建为 `.prts-dev`）。
/// best-effort，写不进去不影响更新流程；不记录私钥、密码或签名原文。
fn update_log_path() -> PathBuf {
    crate::config::data_dir().join("logs").join("update.log")
}

fn append_update_log(message: &str) {
    use std::io::Write;
    let path = update_log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(file, "[{stamp}] {message}");
    }
}

fn post_install_notice_path() -> PathBuf {
    crate::config::data_dir()
        .join("update")
        .join("post-install.json")
}

fn write_post_install_notice(notice: &PostInstallNotice) -> Result<(), String> {
    let path = post_install_notice_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建更新状态目录失败：{e}"))?;
    }
    let body = serde_json::to_string_pretty(notice)
        .map_err(|e| format!("序列化更新日志失败：{e}"))?;
    std::fs::write(&path, body).map_err(|e| format!("写入更新日志失败：{e}"))?;
    Ok(())
}

fn remove_post_install_notice() {
    let _ = std::fs::remove_file(post_install_notice_path());
}

/// 读取并删除安装后的更新日志；版本不匹配说明安装未真正落地，按过期处理。
fn take_post_install_notice(current_version: &str) -> Option<PostInstallNotice> {
    let path = post_install_notice_path();
    let body = std::fs::read_to_string(&path).ok()?;
    let notice: PostInstallNotice = match serde_json::from_str(&body) {
        Ok(notice) => notice,
        Err(error) => {
            append_update_log(&format!("post-install notice unreadable error={error}"));
            let _ = std::fs::remove_file(path);
            return None;
        }
    };
    let _ = std::fs::remove_file(path);
    if notice.version != current_version {
        append_update_log(&format!(
            "post-install notice stale expected={} actual={current_version}",
            notice.version
        ));
        return None;
    }
    Some(notice)
}

/// 比较两个 SemVer；解析失败时保留先到者，避免因异常字符串误替换。
fn version_is_newer(candidate: &str, current: &str) -> bool {
    match (Version::parse(candidate), Version::parse(current)) {
        (Ok(candidate), Ok(current)) => candidate > current,
        _ => false,
    }
}

/// 这些状态表示“清单不存在/暂无发布物”，不是网络故障。
fn missing_manifest_status(status: u16) -> bool {
    matches!(status, 204 | 404 | 410)
}

#[cfg(windows)]
fn available_space(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free: u64 = 0;
    unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(wide.as_ptr()),
            Some(&mut free as *mut u64),
            None,
            None,
        )
        .ok()?;
    }
    Some(free)
}

#[cfg(not(windows))]
fn available_space(_path: &Path) -> Option<u64> {
    None
}

/// 安装盘剩余空间预检。取当前 exe 所在目录；无法读取时跳过并记日志。
fn ensure_disk_space() -> Result<(), String> {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    else {
        append_update_log("disk-check skipped: cannot resolve install directory");
        return Ok(());
    };
    let Some(free) = available_space(&dir) else {
        append_update_log(&format!(
            "disk-check skipped: free space unavailable path={}",
            dir.display()
        ));
        return Ok(());
    };
    let free_mb = free / (1024 * 1024);
    let required_mb = MIN_FREE_SPACE_BYTES / (1024 * 1024);
    append_update_log(&format!(
        "disk-check path={} free={}MB required={}MB",
        dir.display(),
        free_mb,
        required_mb
    ));
    if free < MIN_FREE_SPACE_BYTES {
        return Err(format!(
            "磁盘剩余空间不足：{} 至少需要 {} MB，当前 {} MB",
            dir.display(),
            required_mb,
            free_mb
        ));
    }
    Ok(())
}

fn proxy_url() -> Option<Url> {
    ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
        .iter()
        .find_map(|key| std::env::var(key).ok())
        .and_then(|raw| Url::parse(&raw).ok())
}

async fn check_source(
    app: &AppHandle,
    source: &UpdateSourceInfo,
) -> Result<Option<Update>, SourceError> {
    let endpoint = Url::parse(&source.endpoint)
        .map_err(|e| SourceError::permanent(format!("{} 源地址无效：{e}", source.label)))?;
    let mut builder = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|e| SourceError::permanent(format!("{} 源初始化失败：{e}", source.label)))?;
    if let Some(proxy) = proxy_url() {
        builder = builder.proxy(proxy);
    }
    let updater = builder
        .timeout(CHECK_TIMEOUT)
        .build()
        .map_err(|e| SourceError::permanent(format!("{} 源初始化失败：{e}", source.label)))?;

    match updater.check().await {
        Ok(result) => Ok(result),
        Err(UpdaterError::ReleaseNotFound) => classify_missing_manifest(source).await,
        Err(error) => Err(SourceError::retryable(format!("{}：{error}", source.label))),
    }
}

/// 插件把 404、500 和格式错误都归到 `ReleaseNotFound`。
/// 这里补一次轻量探针，把“尚未发布清单”和真正的源故障分开。
async fn probe_manifest_status(source: &UpdateSourceInfo) -> Result<u16, String> {
    let mut builder = reqwest::Client::builder().timeout(CHECK_TIMEOUT);
    if let Some(proxy) = proxy_url() {
        builder = builder.proxy(
            reqwest::Proxy::all(proxy.as_str())
                .map_err(|e| format!("{} 代理配置失败：{e}", source.label))?,
        );
    }
    let client = builder
        .build()
        .map_err(|e| format!("{} 探针初始化失败：{e}", source.label))?;
    let response = client
        .get(&source.endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "PRTS-Terminal-Updater")
        .send()
        .await
        .map_err(|e| format!("{}：{e}", source.label))?;
    Ok(response.status().as_u16())
}

async fn classify_missing_manifest(
    source: &UpdateSourceInfo,
) -> Result<Option<Update>, SourceError> {
    let status = match probe_manifest_status(source).await {
        Ok(status) => status,
        Err(error) => return Err(SourceError::retryable(error)),
    };

    if missing_manifest_status(status) {
        append_update_log(&format!(
            "check source={} manifest-missing http={status}",
            source.label
        ));
        return Ok(None);
    }
    if (200..300).contains(&status) {
        return Err(SourceError::permanent(format!(
            "{}：清单格式无效（HTTP {status}）",
            source.label
        )));
    }
    if (400..500).contains(&status) && status != 408 && status != 429 {
        return Err(SourceError::permanent(format!(
            "{}：更新源返回 HTTP {status}",
            source.label
        )));
    }
    Err(SourceError::retryable(format!(
        "{}：更新源返回 HTTP {status}",
        source.label
    )))
}

async fn check_source_with_retry(
    app: &AppHandle,
    source: &UpdateSourceInfo,
    attempts: usize,
) -> Result<Option<Update>, SourceError> {
    let mut last_error: Option<SourceError> = None;
    for attempt in 0..attempts.max(1) {
        match check_source(app, source).await {
            Ok(result) => return Ok(result),
            Err(error) => {
                append_update_log(&format!(
                    "check source={} attempt={} error={}",
                    source.label,
                    attempt + 1,
                    error.message
                ));
                if !error.retryable {
                    return Err(error);
                }
                last_error = Some(error);
                if attempt + 1 < attempts.max(1) {
                    tokio::time::sleep(RETRY_DELAY).await;
                }
            }
        }
    }
    Err(last_error.expect("至少有一次检查错误"))
}

/// 检查所有源，取最高版本；任一源返回“无更新”但另一源报错时，
/// 仍采用可达源的结论，并把错误放进 warnings。
async fn check_sources(
    app: &AppHandle,
    sources: &[UpdateSourceInfo],
) -> Result<CheckOutcome, String> {
    if sources.is_empty() {
        return Err("尚未配置更新源".to_string());
    }

    let mut best: Option<(UpdateSourceInfo, Update)> = None;
    let mut saw_valid = false;
    let mut warnings = Vec::new();

    for source in sources {
        match check_source_with_retry(app, source, 1).await {
            Ok(Some(update)) => {
                saw_valid = true;
                let replace = best.as_ref().map_or(true, |(_, current)| {
                    version_is_newer(&update.version, &current.version)
                });
                append_update_log(&format!(
                    "check source={} update={} selected={}",
                    source.label, update.version, replace
                ));
                if replace {
                    best = Some((source.clone(), update));
                }
            }
            Ok(None) => {
                saw_valid = true;
                append_update_log(&format!("check source={} up-to-date", source.label));
            }
            Err(error) => {
                append_update_log(&format!(
                    "check source={} failed error={}",
                    source.label, error.message
                ));
                warnings.push(error.message);
            }
        }
    }

    if let Some(found) = best {
        return Ok(CheckOutcome {
            found: Some(found),
            warnings,
        });
    }
    if saw_valid {
        return Ok(CheckOutcome {
            found: None,
            warnings,
        });
    }
    Err(if warnings.is_empty() {
        "没有可用的更新源".to_string()
    } else {
        warnings.join("；")
    })
}

fn available_from(source: &UpdateSourceInfo, update: &Update) -> AvailableUpdate {
    AvailableUpdate {
        version: update.version.clone(),
        current_version: update.current_version.clone(),
        notes: update.body.clone(),
        pub_date: update.date.map(|date| date.to_string()),
        source: source.id.clone(),
        download_url: update.download_url.to_string(),
    }
}

async fn check_internal(app: &AppHandle, service: &UpdateService) -> Result<UpdateStatus, String> {
    if tauri::is_dev() {
        {
            let mut rt = service.runtime.lock().unwrap_or_else(|e| e.into_inner());
            rt.status.phase = UpdatePhase::Dev;
            rt.status.error = None;
            rt.status.error_stage = None;
        }
        service.emit(app);
        return Ok(service.status());
    }

    let _guard = service.acquire()?;
    {
        let mut rt = service.runtime.lock().unwrap_or_else(|e| e.into_inner());
        rt.status.phase = UpdatePhase::Checking;
        rt.status.error = None;
        rt.status.error_stage = None;
        rt.status.available = None;
        rt.status.source = None;
        rt.status.downloaded_bytes = None;
        rt.status.total_bytes = None;
        rt.status.warnings.clear();
    }
    service.emit(app);

    let sources = service.status().sources;
    append_update_log(&format!("check start sources={}", sources.len()));
    match check_sources(app, &sources).await {
        Ok(outcome) => {
            if let Some((source, update)) = outcome.found {
                let available = available_from(&source, &update);
                {
                    let mut rt = service.runtime.lock().unwrap_or_else(|e| e.into_inner());
                    rt.status.phase = UpdatePhase::Available;
                    rt.status.available = Some(available);
                    rt.status.source = Some(source.id.clone());
                    rt.status.last_checked_at = Some(now_iso());
                    rt.status.warnings = outcome.warnings;
                    rt.status.error = None;
                    rt.status.error_stage = None;
                }
                append_update_log(&format!(
                    "check selected source={} version={}",
                    source.id, update.version
                ));
                service.emit(app);
            } else {
                {
                    let mut rt = service.runtime.lock().unwrap_or_else(|e| e.into_inner());
                    rt.status.phase = UpdatePhase::UpToDate;
                    rt.status.available = None;
                    rt.status.source = None;
                    rt.status.last_checked_at = Some(now_iso());
                    rt.status.warnings = outcome.warnings;
                    rt.status.error = None;
                    rt.status.error_stage = None;
                }
                append_update_log("check result up-to-date");
                service.emit(app);
            }
            Ok(service.status())
        }
        Err(error) => {
            append_update_log(&format!("check failed error={error}"));
            {
                let mut rt = service.runtime.lock().unwrap_or_else(|e| e.into_inner());
                rt.status.last_checked_at = Some(now_iso());
            }
            service.fail(app, error, UpdateErrorStage::Check);
            Ok(service.status())
        }
    }
}

fn ordered_sources(service: &UpdateService) -> Vec<UpdateSourceInfo> {
    let status = service.status();
    let mut sources = status.sources;
    if let Some(current) = status.source {
        if let Some(index) = sources.iter().position(|source| source.id == current) {
            let preferred = sources.remove(index);
            sources.insert(0, preferred);
        }
    }
    sources
}

async fn download_internal(
    app: &AppHandle,
    service: Arc<UpdateService>,
    version: &str,
) -> Result<UpdateStatus, String> {
    if tauri::is_dev() {
        service.fail(
            app,
            "开发模式不执行下载".to_string(),
            UpdateErrorStage::Download,
        );
        return Err("开发模式不执行下载".to_string());
    }

    let _guard = service.acquire()?;
    let current = service.status();
    let available = current
        .available
        .as_ref()
        .ok_or_else(|| "请先检查更新".to_string())?;
    if available.version != version {
        let message = format!("待下载版本与检查结果不一致：{version}");
        service.fail(app, message.clone(), UpdateErrorStage::Download);
        return Err(message);
    }

    if let Err(message) = ensure_disk_space() {
        service.fail(app, message.clone(), UpdateErrorStage::Download);
        return Err(message);
    }

    append_update_log(&format!("download start version={version}"));
    let sources = ordered_sources(&service);
    {
        let mut rt = service.runtime.lock().unwrap_or_else(|e| e.into_inner());
        rt.pending = None;
        rt.status.warnings.clear();
        rt.status.error_stage = None;
    }
    let mut errors = Vec::new();
    for source in sources {
        append_update_log(&format!(
            "download source={} version={version}",
            source.label
        ));
        let update = match check_source_with_retry(app, &source, 2).await {
            Ok(Some(update)) if update.version == version => update,
            Ok(_) => {
                let message = format!("{}：清单中没有 {version}", source.label);
                append_update_log(&format!(
                    "download source={} skipped: {message}",
                    source.label
                ));
                errors.push(message);
                continue;
            }
            Err(error) => {
                append_update_log(&format!(
                    "download source={} check-failed error={}",
                    source.label, error.message
                ));
                errors.push(error.message);
                continue;
            }
        };

        {
            let mut rt = service.runtime.lock().unwrap_or_else(|e| e.into_inner());
            rt.status.phase = UpdatePhase::Downloading;
            rt.status.source = Some(source.id.clone());
            rt.status.downloaded_bytes = Some(0);
            rt.status.total_bytes = None;
            rt.status.error = None;
            rt.status.error_stage = None;
        }
        service.emit(app);

        let mut update = update;
        update.timeout = Some(DOWNLOAD_TIMEOUT);

        let app_for_progress = app.clone();
        let service_for_progress = Arc::clone(&service);
        let mut last_emit = Instant::now() - PROGRESS_EMIT_INTERVAL;
        let mut downloaded: u64 = 0;

        let result = update
            .download(
                move |chunk_len, total| {
                    downloaded = downloaded.saturating_add(chunk_len as u64);
                    let mut should_emit = false;
                    {
                        let mut rt = service_for_progress
                            .runtime
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        rt.status.downloaded_bytes = Some(downloaded);
                        rt.status.total_bytes = total;
                        if last_emit.elapsed() >= PROGRESS_EMIT_INTERVAL {
                            last_emit = Instant::now();
                            should_emit = true;
                        }
                    }
                    if should_emit {
                        service_for_progress.emit(&app_for_progress);
                    }
                },
                || {},
            )
            .await;

        match result {
            Ok(bytes) => {
                let total = bytes.len() as u64;
                {
                    let mut rt = service.runtime.lock().unwrap_or_else(|e| e.into_inner());
                    rt.pending = Some(DownloadedUpdate { bytes, update });
                    rt.status.phase = UpdatePhase::Downloaded;
                    rt.status.source = Some(source.id.clone());
                    rt.status.downloaded_bytes = Some(total);
                    rt.status.total_bytes = Some(total);
                    rt.status.error = None;
                    rt.status.error_stage = None;
                }
                service.emit(app);
                append_update_log(&format!(
                    "download success source={} version={} bytes={total}",
                    source.id, version
                ));
                return Ok(service.status());
            }
            Err(error) => {
                append_update_log(&format!(
                    "download source={} failed error={error}",
                    source.label
                ));
                errors.push(format!("{}：{error}", source.label));
            }
        }
    }

    let message = if errors.is_empty() {
        "下载失败：没有可用更新源".to_string()
    } else {
        format!("下载失败：{}", errors.join("；"))
    };
    append_update_log(&format!("download failed error={message}"));
    service.fail(app, message.clone(), UpdateErrorStage::Download);
    Err(message)
}

async fn install_internal(
    app: &AppHandle,
    service: &UpdateService,
    turns: &TurnRegistry,
) -> Result<(), String> {
    if tauri::is_dev() {
        service.fail(
            app,
            "开发模式不执行安装".to_string(),
            UpdateErrorStage::Install,
        );
        return Err("开发模式不执行安装".to_string());
    }

    let _guard = service.acquire()?;

    if turns.active_count() > 0 {
        append_update_log("install blocked: active turn");
        return Err("对话正在处理中，完成或中止后再安装".to_string());
    }
    if crate::maa::manager::status_view().running {
        append_update_log("install blocked: MAA running");
        return Err("MAA 任务正在运行，完成或中止后再安装".to_string());
    }
    if let Err(message) = ensure_disk_space() {
        append_update_log(&format!("install blocked: {message}"));
        service.fail(app, message.clone(), UpdateErrorStage::Install);
        return Err(message);
    }

    let expected_version = service
        .status()
        .available
        .as_ref()
        .map(|available| available.version.clone());
    let pending = {
        let mut rt = service.runtime.lock().unwrap_or_else(|e| e.into_inner());
        rt.pending.take()
    };
    let Some(pending) = pending else {
        append_update_log("install blocked: no downloaded update");
        return Err("没有已下载的更新".to_string());
    };
    if expected_version.as_deref() != Some(pending.update.version.as_str()) {
        append_update_log("install blocked: pending version mismatch");
        {
            let mut rt = service.runtime.lock().unwrap_or_else(|e| e.into_inner());
            rt.pending = Some(pending);
        }
        return Err("待安装版本与当前更新不一致，请重新下载".to_string());
    }

    {
        let mut rt = service.runtime.lock().unwrap_or_else(|e| e.into_inner());
        rt.status.phase = UpdatePhase::Installing;
        rt.status.error = None;
        rt.status.error_stage = None;
    }
    service.emit(app);
    append_update_log(&format!(
        "install start version={} bytes={}",
        pending.update.version,
        pending.bytes.len()
    ));

    // 安装器会覆盖安装目录；先停掉 MAA 桥，避免文件占用。
    crate::maa::manager::shutdown();

    let status = service.status();
    let notice = PostInstallNotice {
        version: pending.update.version.clone(),
        previous_version: status.current_version,
        notes: status
            .available
            .as_ref()
            .and_then(|available| available.notes.clone()),
        pub_date: status
            .available
            .as_ref()
            .and_then(|available| available.pub_date.clone()),
    };
    if let Err(error) = write_post_install_notice(&notice) {
        append_update_log(&format!("post-install notice write skipped error={error}"));
    }

    match pending.update.install(&pending.bytes) {
        Ok(()) => {
            append_update_log("install launcher started");
            Ok(())
        }
        Err(error) => {
            remove_post_install_notice();
            let message = format!("启动安装器失败：{error}");
            append_update_log(&format!("install failed error={message}"));
            {
                let mut rt = service.runtime.lock().unwrap_or_else(|e| e.into_inner());
                rt.pending = Some(pending);
            }
            service.fail(app, message.clone(), UpdateErrorStage::Install);
            Err(message)
        }
    }
}

/// 启动后台检查：首启检查一次，之后每 24 小时一次。
///
/// `tauri dev` 直接跳过；没有配置源时也不空跑。
pub fn start_background_checks(app: AppHandle) {
    if tauri::is_dev() {
        return;
    }

    let service = Arc::clone(app.state::<Arc<UpdateService>>().inner());
    if service.status().sources.is_empty() {
        return;
    }

    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let _ = check_internal(&app, &service).await;

        loop {
            tokio::time::sleep(Duration::from_secs(24 * 60 * 60)).await;
            let _ = check_internal(&app, &service).await;
        }
    });
}

#[tauri::command]
pub fn updater_status(service: State<'_, Arc<UpdateService>>) -> UpdateStatus {
    service.status()
}

#[tauri::command]
pub async fn updater_check(
    app: AppHandle,
    service: State<'_, Arc<UpdateService>>,
) -> Result<UpdateStatus, String> {
    let service = Arc::clone(service.inner());
    check_internal(&app, &service).await
}

#[tauri::command]
pub async fn updater_download(
    app: AppHandle,
    service: State<'_, Arc<UpdateService>>,
    version: String,
) -> Result<UpdateStatus, String> {
    let service = Arc::clone(service.inner());
    download_internal(&app, service, &version).await
}

#[tauri::command]
pub async fn updater_install(
    app: AppHandle,
    service: State<'_, Arc<UpdateService>>,
    turns: State<'_, Arc<TurnRegistry>>,
) -> Result<(), String> {
    let service = Arc::clone(service.inner());
    let turns = Arc::clone(turns.inner());
    install_internal(&app, &service, &turns).await
}

/// 安装前实时阻塞检查：活跃对话 / MAA 运行中。
#[tauri::command]
pub fn updater_install_blockers(turns: State<'_, Arc<TurnRegistry>>) -> InstallBlockers {
    InstallBlockers {
        active_turn: turns.active_count() > 0,
        maa_running: crate::maa::manager::is_running(),
    }
}

/// 重启后取一次安装日志；同一日志只返回一次。
#[tauri::command]
pub fn updater_take_post_install_notice(
    service: State<'_, Arc<UpdateService>>,
) -> Option<PostInstallNotice> {
    take_post_install_notice(&service.status().current_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: &str) -> UpdateSourceInfo {
        UpdateSourceInfo {
            id: id.to_string(),
            label: id.to_string(),
            endpoint: format!("https://{id}.example/latest.json"),
        }
    }

    #[test]
    fn phase_serializes_camel_case() {
        assert_eq!(
            serde_json::to_string(&UpdatePhase::UpToDate).unwrap(),
            "\"upToDate\""
        );
        assert_eq!(
            serde_json::to_string(&UpdatePhase::Downloaded).unwrap(),
            "\"downloaded\""
        );
        assert_eq!(
            serde_json::to_string(&UpdateErrorStage::Check).unwrap(),
            "\"check\""
        );
    }

    #[test]
    fn ordered_sources_prefers_current_source() {
        let service = UpdateService::new(
            "0.1.0".to_string(),
            vec![source("gitee"), source("github")],
            None,
        );
        {
            let mut rt = service.runtime.lock().unwrap();
            rt.status.source = Some("github".to_string());
        }
        let ordered = ordered_sources(&service);
        assert_eq!(ordered[0].id, "github");
        assert_eq!(ordered[1].id, "gitee");
    }

    #[test]
    fn ordered_sources_keeps_config_order_without_current() {
        let service = UpdateService::new(
            "0.1.0".to_string(),
            vec![source("gitee"), source("github")],
            None,
        );
        let ordered = ordered_sources(&service);
        assert_eq!(ordered[0].id, "gitee");
        assert_eq!(ordered[1].id, "github");
    }

    #[test]
    fn version_compare_picks_newer_semver() {
        assert!(version_is_newer("0.1.1", "0.1.0"));
        assert!(version_is_newer("0.2.0", "0.1.9"));
        assert!(version_is_newer("0.1.1-rc.1", "0.1.0"));
        assert!(version_is_newer("0.1.1", "0.1.1-rc.1"));
        assert!(!version_is_newer("0.1.1-rc.1", "0.1.1"));
        assert!(!version_is_newer("0.1.0", "0.1.0"));
        assert!(!version_is_newer("0.1.0", "0.1.1"));
        assert!(!version_is_newer("not-semver", "0.1.0"));
    }

    #[test]
    fn only_missing_manifest_statuses_are_treated_as_no_update() {
        assert!(missing_manifest_status(204));
        assert!(missing_manifest_status(404));
        assert!(missing_manifest_status(410));
        assert!(!missing_manifest_status(200));
        assert!(!missing_manifest_status(403));
        assert!(!missing_manifest_status(500));
    }

    #[cfg(windows)]
    #[test]
    fn windows_disk_space_probe_reads_current_drive() {
        let dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        assert!(available_space(&dir).is_some());
    }
}
