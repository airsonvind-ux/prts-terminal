//! 外部插件的安装管道（M3 只支持「选择文件夹」，zip 留到 M4）。
//!
//! 流程：复制到 `plugin-tmp/<token>/` → 读 manifest 走十二条校验 → 重名检查 →
//! 返回预览 → 确认后 `rename` 落到 `plugin-tmp` 的兄弟目录 `plugins/<id>/`（同卷，
//! 原子）→ 尝试加载 → 失败回滚删目录。
//!
//! 目录守卫沿用 skills 的约定：拒绝符号链接、拒绝 `..` 与绝对路径、总大小上限
//! 100 MB。`token` 只是内存里的随机键，前端拿不到真实路径。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::manifest::{self, PluginKind, PluginManifest};
use super::{install_record, PluginRecord, PluginRegistry, PluginRoots, PluginsConfig};
use crate::host::HostServices;
use crate::tools::ToolRegistry;

/// 一个插件目录的大小上限（与 skills 的 2 MB 不同：插件自带可执行文件）。
pub const MAX_PLUGIN_BYTES: u64 = 100 * 1024 * 1024;
/// 文件数上限：防止把整个磁盘塞进来。
pub const MAX_PLUGIN_FILES: usize = 10_000;
/// 暂存目录名（在 data_dir 下）。
pub const STAGING_DIR: &str = "plugin-tmp";

/// 预览结果：给前端的只有这些，暂存路径不出后端。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPreview {
    pub token: String,
    pub name: String,
    pub id: String,
    pub version: String,
    pub author: Option<String>,
    /// `builtin` 恒为拒（预览阶段就挡掉），这里只是给前端显示。
    pub kind: String,
    pub capabilities: Vec<String>,
    pub file_count: usize,
    pub total_bytes: u64,
}

/// 已经复制进暂存区、等着确认的一次安装。
struct Pending {
    staging: PathBuf,
    manifest: PluginManifest,
}

/// 安装管道。`pending` 只在内存里：重启后暂存区会被清空（见 `new`）。
pub struct Installer {
    staging_root: PathBuf,
    pending: Mutex<HashMap<String, Pending>>,
}

impl Installer {
    /// 构造时清空 `plugin-tmp/`：上次没确认的安装不该留到这次。
    pub fn new(data_dir: &Path) -> Self {
        let staging_root = data_dir.join(STAGING_DIR);
        let _ = fs::remove_dir_all(&staging_root);
        let _ = fs::create_dir_all(&staging_root);
        Self {
            staging_root,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// 复制、校验、算预览。失败的临时目录立刻清掉。
    #[cfg_attr(not(test), allow(dead_code))] // 提交 5 的 command 层接线后删除
    pub fn preview(
        &self,
        source: &Path,
        roots: &PluginRoots,
        services: &Arc<HostServices>,
        known: &HashSet<String>,
    ) -> Result<InstallPreview, String> {
        let (file_count, total_bytes) = measure(source)?;
        let token = new_token();
        let staging = self.staging_root.join(&token);
        copy_tree(source, &staging)?;

        let manifest = match read_and_validate(&staging, source, services, roots, known) {
            Ok(manifest) => manifest,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging);
                return Err(error);
            }
        };

        let preview = InstallPreview {
            token: token.clone(),
            name: manifest.name.clone(),
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            author: manifest.author.clone(),
            kind: match manifest.kind {
                PluginKind::Builtin => "builtin".to_string(),
                PluginKind::Process => "process".to_string(),
            },
            capabilities: manifest.capabilities.clone(),
            file_count,
            total_bytes,
        };
        self.pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(
                token,
                Pending {
                    staging,
                    manifest,
                },
            );
        Ok(preview)
    }

    /// 确认安装：原子落位到 `plugins/<id>/`，再走一次安装；失败删目录回滚。
    #[cfg_attr(not(test), allow(dead_code))] // 提交 5 的 command 层接线后删除
    #[allow(clippy::too_many_arguments)]
    pub fn confirm(
        &self,
        token: &str,
        roots: &PluginRoots,
        config: &PluginsConfig,
        services: &Arc<HostServices>,
        reg: &ToolRegistry,
        ledger: &Arc<Mutex<PluginRegistry>>,
    ) -> Result<PluginRecord, String> {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(token)
            .ok_or_else(|| "这次安装已经过期，请重新选择文件夹".to_string())?;

        let target = roots.user.join(&pending.manifest.id);
        if target.exists() {
            let _ = fs::remove_dir_all(&pending.staging);
            return Err(format!(
                "插件目录已经存在：{}（先卸载旧的再装）",
                target.display()
            ));
        }
        if let Err(error) = fs::create_dir_all(&roots.user) {
            let _ = fs::remove_dir_all(&pending.staging);
            return Err(format!("建插件目录失败（{}）：{error}", roots.user.display()));
        }
        // 同一个数据盘下 rename 是原子的：要么完整落位，要么什么都看不到。
        if let Err(error) = fs::rename(&pending.staging, &target) {
            let _ = fs::remove_dir_all(&pending.staging);
            return Err(format!("安装落位失败（{}）：{error}", target.display()));
        }

        let (tools, reported) = match install_record(&pending.manifest, &target, reg, services, ledger) {
            Ok(installed) => installed,
            Err(error) => {
                let _ = fs::remove_dir_all(&target);
                return Err(format!("插件装上了但没起来：{error}"));
            }
        };
        if let Err(error) = config.set_enabled(&pending.manifest.id, true) {
            let _ = fs::remove_dir_all(&target);
            return Err(format!("写启用状态失败：{error}"));
        }

        let record = PluginRecord {
            manifest: pending.manifest,
            state: super::PluginState::Loaded,
            tools,
            reported_capabilities: reported,
            dir: target,
        };
        ledger
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(record.clone());
        Ok(record)
    }

    /// 暂存区里还有几份没确认的安装（只给测试与诊断用）。
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .len()
    }
}

/// 读完 manifest 后做安装面特有的三道关：builtin 拒、能力必须在服务注册表里、
/// 不能和已装的插件重名。
fn read_and_validate(
    staging: &Path,
    source: &Path,
    services: &Arc<HostServices>,
    roots: &PluginRoots,
    known: &HashSet<String>,
) -> Result<PluginManifest, String> {
    let manifest_path = staging.join(manifest::MANIFEST_FILE_NAME);
    if !manifest_path.is_file() {
        return Err(format!(
            "这个文件夹里没有 {}（选的是插件文件夹吗？zip 请先解压）",
            manifest::MANIFEST_FILE_NAME
        ));
    }
    let text = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("读不出 {}：{error}", manifest_path.display()))?;
    let manifest = manifest::parse_manifest(&text).map_err(|error| format!("manifest 不是合法 JSON：{error}"))?;

    let dir_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let mut seen = HashSet::new();
    manifest::validate_with(&manifest, dir_name, &mut seen, &services.keys())
        .map_err(|error| format!("插件声明不合法：{error}"))?;

    if manifest.kind == PluginKind::Builtin {
        return Err("内置插件不能从外部安装（kind 必须是 process）".to_string());
    }
    if known.contains(&manifest.id) || roots.user.join(&manifest.id).exists() {
        return Err(format!(
            "插件 id 已经存在：{}（先卸载旧的，或换一个 id）",
            manifest.id
        ));
    }
    Ok(manifest)
}

/// 算完目录大小与文件数，顺便把守卫全跑一遍。
fn measure(source: &Path) -> Result<(usize, u64), String> {
    if !source.is_dir() {
        return Err("选中的不是文件夹（zip 请先解压，再选解压出来的文件夹）".to_string());
    }
    let mut files = Vec::new();
    walk(source, &mut files, 0)?;
    let mut total = 0u64;
    for file in &files {
        let bytes = fs::metadata(file)
            .map_err(|error| format!("读不了 {}：{error}", file.display()))?
            .len();
        total = total.saturating_add(bytes);
        if total > MAX_PLUGIN_BYTES {
            return Err(format!(
                "插件包太大了：超过 {} MB",
                MAX_PLUGIN_BYTES / 1024 / 1024
            ));
        }
    }
    Ok((files.len(), total))
}

/// 收集普通文件；符号链接与特殊文件一律拒（沿路径打穿的第一道门）。
fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) -> Result<(), String> {
    if depth > 32 {
        return Err("目录嵌套太深（超过 32 层）".to_string());
    }
    let entries = fs::read_dir(dir).map_err(|error| format!("读不了目录 {}：{error}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("读目录项失败（{}）：{error}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("读不了 {} 的类型：{error}", path.display()))?;
        if file_type.is_symlink() {
            return Err(format!(
                "插件包里不能有符号链接：{}（会被拒绝，避免指到包外）",
                path.display()
            ));
        }
        if file_type.is_dir() {
            walk(&path, out, depth + 1)?;
        } else if file_type.is_file() {
            out.push(path);
            if out.len() > MAX_PLUGIN_FILES {
                return Err(format!("文件太多了：超过 {MAX_PLUGIN_FILES} 个"));
            }
        } else {
            return Err(format!("不支持的文件类型：{}", path.display()));
        }
    }
    Ok(())
}

/// 复制目录树到暂存区；只复制刚校验过的普通文件。
fn copy_tree(source: &Path, staging: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    walk(source, &mut files, 0)?;
    fs::create_dir_all(staging).map_err(|error| format!("建暂存目录失败：{error}"))?;
    for file in files {
        let relative = file
            .strip_prefix(source)
            .map_err(|_| format!("路径不在插件目录里：{}", file.display()))?;
        if relative.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(format!("路径不干净：{}", relative.display()));
        }
        let target = staging.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("建目录失败（{}）：{error}", parent.display()))?;
        }
        fs::copy(&file, &target)
            .map_err(|error| format!("复制 {} 失败：{error}", file.display()))?;
    }
    Ok(())
}

/// 内存 token：时间戳 + 进程内计数，够用且不引依赖。
fn new_token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or_default();
    format!(
        "{nanos:016x}{:04x}",
        NEXT.fetch_add(1, Ordering::Relaxed) as u16
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::PluginState;

    fn scratch(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "prts-install-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn roots(data: &Path) -> PluginRoots {
        PluginRoots {
            bundled: crate::config::bundled_plugins_dir(),
            user: data.join("plugins"),
            data: data.to_path_buf(),
        }
    }

    fn services(roots: &PluginRoots) -> Arc<HostServices> {
        super::super::default_services(roots, None).0
    }

    fn process_manifest(id: &str, exe: &str) -> String {
        format!(
            r#"{{"manifestVersion":1,"id":"{id}","name":"演示 {id}","version":"0.1.1",
                "kind":"process","entry":{{"exe":"{exe}"}},
                "requires":{{"protocol":1}},"activation":[],
                "capabilities":[],"contributes":{{}}}}"#
        )
    }

    #[test]
    fn a_folder_without_manifest_is_rejected() {
        let data = scratch("no-manifest");
        let source = scratch("no-manifest-src");
        let roots = roots(&data);
        let installer = Installer::new(&data);
        let err = installer
            .preview(&source, &roots, &services(&roots), &HashSet::new())
            .unwrap_err();
        assert!(err.contains("plugin.json"), "got: {err}");
        assert_eq!(installer.pending_count(), 0);
        let _ = fs::remove_dir_all(&data);
        let _ = fs::remove_dir_all(&source);
    }

    #[test]
    fn builtin_packages_are_rejected_at_preview() {
        let data = scratch("builtin");
        let source = scratch("builtin-src");
        fs::write(
            source.join("plugin.json"),
            r#"{"manifestVersion":1,"id":"sneaky","name":"冒充内置","version":"0.1.1",
                "kind":"builtin","activation":[],"capabilities":[],"contributes":{}}"#,
        )
        .expect("写 manifest");
        let roots = roots(&data);
        let installer = Installer::new(&data);
        let err = installer
            .preview(&source, &roots, &services(&roots), &HashSet::new())
            .unwrap_err();
        assert!(err.contains("内置插件不能从外部安装"), "got: {err}");
        let _ = fs::remove_dir_all(&data);
        let _ = fs::remove_dir_all(&source);
    }

    #[test]
    fn capabilities_must_exist_in_the_service_registry() {
        let data = scratch("capability");
        let source = scratch("capability-src");
        fs::write(
            source.join("plugin.json"),
            r#"{"manifestVersion":1,"id":"hollow","name":"空心","version":"0.1.1",
                "kind":"process","entry":{"exe":"a.exe"},"requires":{"protocol":1},
                "activation":[],"capabilities":["game.capture"],"contributes":{}}"#,
        )
        .expect("写 manifest");
        let roots = roots(&data);
        let installer = Installer::new(&data);
        let err = installer
            .preview(&source, &roots, &services(&roots), &HashSet::new())
            .unwrap_err();
        assert!(err.contains("game.capture"), "got: {err}");
        assert!(err.contains("capability"), "错误要指出是能力问题，got: {err}");
        let _ = fs::remove_dir_all(&data);
        let _ = fs::remove_dir_all(&source);
    }

    #[test]
    fn duplicate_ids_are_rejected_before_touching_disk() {
        let data = scratch("duplicate");
        let source = scratch("duplicate-src");
        fs::write(source.join("plugin.json"), process_manifest("maa", "a.exe")).expect("写 manifest");
        let roots = roots(&data);
        let installer = Installer::new(&data);
        let known: HashSet<String> = ["maa".to_string()].into_iter().collect();
        let err = installer
            .preview(&source, &roots, &services(&roots), &known)
            .unwrap_err();
        assert!(err.contains("已经存在"), "got: {err}");
        let _ = fs::remove_dir_all(&data);
        let _ = fs::remove_dir_all(&source);
    }

    #[test]
    fn symlinks_inside_the_package_are_refused() {
        let data = scratch("symlink");
        let source = scratch("symlink-src");
        fs::write(source.join("plugin.json"), process_manifest("linked", "a.exe")).expect("写 manifest");
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(
            source.join("plugin.json"),
            source.join("alias.json"),
        )
        .is_ok();
        #[cfg(not(windows))]
        let made = std::os::unix::fs::symlink(source.join("plugin.json"), source.join("alias.json")).is_ok();
        if !made {
            eprintln!("跳过：这个环境建不了符号链接");
            let _ = fs::remove_dir_all(&data);
            let _ = fs::remove_dir_all(&source);
            return;
        }
        let roots = roots(&data);
        let installer = Installer::new(&data);
        let err = installer
            .preview(&source, &roots, &services(&roots), &HashSet::new())
            .unwrap_err();
        assert!(err.contains("符号链接"), "got: {err}");
        let _ = fs::remove_dir_all(&data);
        let _ = fs::remove_dir_all(&source);
    }

    #[test]
    fn confirming_moves_the_folder_and_rolls_back_when_it_cannot_start() {
        let data = scratch("confirm");
        let source = scratch("confirm-src");
        fs::write(source.join("plugin.json"), process_manifest("demo", "missing.exe")).expect("写 manifest");
        let roots = roots(&data);
        let services = services(&roots);
        let installer = Installer::new(&data);
        let preview = installer
            .preview(&source, &roots, &services, &HashSet::new())
            .expect("预览应当成功");
        assert_eq!(preview.id, "demo");
        assert_eq!(preview.kind, "process");
        assert_eq!(installer.pending_count(), 1);

        let reg = ToolRegistry::new();
        let config = PluginsConfig::load(&data);
        let ledger = Arc::new(Mutex::new(PluginRegistry::new()));
        let err = installer
            .confirm(&preview.token, &roots, &config, &services, &reg, &ledger)
            .unwrap_err();
        assert!(err.contains("没起来"), "got: {err}");
        assert!(!roots.user.join("demo").exists(), "失败必须回滚删目录");
        assert!(ledger.lock().expect("台账锁不该中毒").get("demo").is_none());
        assert_eq!(installer.pending_count(), 0, "token 用掉就没了");

        let _ = fs::remove_dir_all(&data);
        let _ = fs::remove_dir_all(&source);
    }

    #[test]
    fn a_second_confirm_with_the_same_token_is_refused() {
        let data = scratch("token");
        let source = scratch("token-src");
        fs::write(source.join("plugin.json"), process_manifest("demo", "missing.exe")).expect("写 manifest");
        let roots = roots(&data);
        let services = services(&roots);
        let installer = Installer::new(&data);
        let preview = installer
            .preview(&source, &roots, &services, &HashSet::new())
            .expect("预览应当成功");
        let reg = ToolRegistry::new();
        let config = PluginsConfig::load(&data);
        let ledger = Arc::new(Mutex::new(PluginRegistry::new()));
        let _ = installer.confirm(&preview.token, &roots, &config, &services, &reg, &ledger);
        let err = installer
            .confirm(&preview.token, &roots, &config, &services, &reg, &ledger)
            .unwrap_err();
        assert!(err.contains("过期"), "got: {err}");
        let _ = fs::remove_dir_all(&data);
        let _ = fs::remove_dir_all(&source);
    }

    #[test]
    fn the_installer_clears_stale_staging_directories() {
        let data = scratch("stale");
        let stale = data.join(STAGING_DIR).join("old");
        fs::create_dir_all(&stale).expect("建旧暂存目录");
        let _installer = Installer::new(&data);
        assert!(!stale.exists(), "启动时应当清空 plugin-tmp");
        let _ = fs::remove_dir_all(&data);
    }

    #[test]
    fn installed_plugins_are_loaded() {
        // 端到端：确认预览后工具真的注册上来（用 builtin 之外的合法 process 插件需要
        // 可执行文件，这里只验证「落位 + 台账」这一步，加载失败的回滚在上面覆盖）。
        let data = scratch("loaded");
        let source = scratch("loaded-src");
        fs::write(source.join("plugin.json"), process_manifest("demo", "missing.exe")).expect("写 manifest");
        let roots = roots(&data);
        let services = services(&roots);
        let installer = Installer::new(&data);
        let preview = installer
            .preview(&source, &roots, &services, &HashSet::new())
            .expect("预览应当成功");
        assert_eq!(preview.name, "演示 demo");
        assert_eq!(preview.version, "0.1.1");
        assert!(preview.total_bytes > 0);
        assert_eq!(preview.file_count, 1);
        let _ = fs::remove_dir_all(&data);
        let _ = fs::remove_dir_all(&source);
    }

    #[test]
    fn records_keep_the_plugin_directory() {
        let record = PluginRecord {
            manifest: {
                let text = process_manifest("x", "a.exe");
                manifest::parse_manifest(&text).expect("样本 manifest")
            },
            state: PluginState::Loaded,
            tools: Vec::new(),
            reported_capabilities: Vec::new(),
            dir: PathBuf::from("C:/plugins/x"),
        };
        assert_eq!(record.dir, PathBuf::from("C:/plugins/x"));
    }
}
