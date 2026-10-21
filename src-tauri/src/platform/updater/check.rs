// 更新源检查：单源探测、缺失清单分类、重试与多源取最高版本。

use tauri::AppHandle;
use tauri_plugin_updater::{Error as UpdaterError, Update, UpdaterExt};
use url::Url;

use super::{
    append_update_log, missing_manifest_status, now_iso, proxy_url, version_is_newer,
    AvailableUpdate, CheckOutcome, SourceError, UpdateErrorStage, UpdatePhase, UpdateService,
    UpdateSourceInfo, UpdateStatus, CHECK_TIMEOUT, RETRY_DELAY,
};

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

pub(super) async fn check_source_with_retry(
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
                let replace = best.as_ref().is_none_or(|(_, current)| {
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

pub(super) async fn check_internal(
    app: &AppHandle,
    service: &UpdateService,
) -> Result<UpdateStatus, String> {
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
