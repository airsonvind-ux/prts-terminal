// 更新包下载与安装。
//
// 下载按源顺序重试，进度节流写入共享状态；安装前复检活跃对话、MAA 与磁盘。

use std::sync::Arc;
use std::time::Instant;

use tauri::AppHandle;

use crate::turn_registry::TurnRegistry;

use super::{
    append_update_log, ensure_disk_space, ordered_sources, remove_post_install_notice,
    write_post_install_notice, DownloadedUpdate, PostInstallNotice, UpdateErrorStage, UpdatePhase,
    UpdateService, UpdateStatus, DOWNLOAD_TIMEOUT, PROGRESS_EMIT_INTERVAL,
};
use super::check::check_source_with_retry;

pub(super) async fn download_internal(
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
        service.fail(app, message.clone(), UpdateErrorStage::Disk);
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

pub(super) async fn install_internal(
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
        service.fail(app, message.clone(), UpdateErrorStage::Disk);
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
