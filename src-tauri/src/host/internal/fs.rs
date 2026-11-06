//! `fs.plugin` 与 `fs.temp`：插件私有目录的读写。
//!
//! 两条服务共用一个实现，只在**根目录**与**op 集合**上不同：
//!
//! | 服务 | 根 | op |
//! |---|---|---|
//! | `fs.plugin` | `<data>/plugin-data/<id>/` | read / write / list / stat / delete / purge |
//! | `fs.temp` | `<data>/plugin-temp/<id>/` | read / write / list / clear |
//!
//! 边界是**不可协商**的（D25 的 fail-closed 落在路径上）：
//!
//! - 路径必须相对，且不含 `..`、盘符、根；符号链接一律拒（连父目录里的也不行）；
//! - `caller` 是 [`HostService::call`] 传进来的插件 id，拼进根之后**再也不被
//!   覆盖**——插件改不了自己的沙箱范围；
//! - `delete` 只删文件与空目录；递归删除只认 `purge`，且必须显式
//!   `confirm: true`——一个手滑的 `delete` 不该带走整个插件数据目录。
//!
//! 诚实边界（D33）：这是能力层的门，不是内核沙箱。同 crate 的宿主代码仍能
//! 直接 `std::fs`，这里拦的是**插件经由能力层**的路径。

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::host::{HostError, HostService, HostServiceInfo};

const FS_PLUGIN: &str = "fs.plugin";
const FS_TEMP: &str = "fs.temp";

static PLUGIN_INFO: HostServiceInfo = HostServiceInfo {
    key: FS_PLUGIN,
    summary: "插件私有数据目录：读写、列目录、删除；purge 需要 confirm",
    params_doc: "{ op: read|write|list|stat|delete|purge, path?, data?, confirm? }",
    returns_doc: "{ data?, bytes?, entries?, exists?, kind? }",
    stateful: false,
    requires_user_grant: false,
    timeout: Some(Duration::from_secs(30)),
    concurrency_safe: true,
    since: 1,
};

static TEMP_INFO: HostServiceInfo = HostServiceInfo {
    key: FS_TEMP,
    summary: "插件临时目录：读写、列目录、清空（应用退出时整体清理）",
    params_doc: "{ op: read|write|list|clear, path?, data? }",
    returns_doc: "{ data?, bytes?, entries?, existed? }",
    stateful: false,
    requires_user_grant: false,
    timeout: Some(Duration::from_secs(30)),
    concurrency_safe: true,
    since: 1,
};

/// 一条文件服务的实例：固定根 + 固定 op 集合。
pub struct FsService {
    info: &'static HostServiceInfo,
    /// 所有插件的父目录（`plugin-data` 或 `plugin-temp`）。
    root: PathBuf,
    /// 是否允许 `purge`/`clear` 这类批量删除。
    bulk_delete: bool,
}

impl FsService {
    /// 插件私有数据（`<data>/plugin-data`）。
    pub fn new(data_root: impl Into<PathBuf>) -> Self {
        Self {
            info: &PLUGIN_INFO,
            root: data_root.into().join("plugin-data"),
            bulk_delete: true,
        }
    }

    /// 插件临时目录（`<data>/plugin-temp`）。
    pub fn temp(data_root: impl Into<PathBuf>) -> Self {
        Self {
            info: &TEMP_INFO,
            root: data_root.into().join("plugin-temp"),
            bulk_delete: true,
        }
    }

    /// 校验插件 id 能安全地当目录名用。
    ///
    /// manifest 已经校验过 id 字符集，这里再挡一次是纵深防御：`caller` 终究是
    /// 一个字符串，将来若有别的调用路径传进来，不该靠上游的校验。
    fn plugin_dir(&self, caller: &str) -> Result<PathBuf, HostError> {
        let safe = !caller.is_empty()
            && caller
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
        if !safe {
            return Err(HostError::invalid_args(
                self.info.key,
                format!("插件 id 不能用作目录名：{caller:?}"),
            ));
        }
        Ok(self.root.join(caller))
    }

    /// 把相对路径钉到插件目录下，并挡掉逃逸与符号链接。
    fn resolve(&self, caller: &str, rel: &str, for_write: bool) -> Result<PathBuf, HostError> {
        let base = self.plugin_dir(caller)?;
        let rel_path = Path::new(rel);

        if rel_path.is_absolute() {
            return Err(self.escape(rel, "不允许绝对路径"));
        }
        for component in rel_path.components() {
            match component {
                Component::Normal(_) | Component::CurDir => {}
                Component::ParentDir => return Err(self.escape(rel, "不允许 `..`")),
                Component::RootDir | Component::Prefix(_) => {
                    return Err(self.escape(rel, "不允许盘符或根"))
                }
            }
        }

        let joined = base.join(rel_path);
        // 目标已存在：不许是符号链接，且规范化后必须还在插件目录下。
        if joined.exists() {
            if std::fs::symlink_metadata(&joined)
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false)
            {
                return Err(self.escape(rel, "不允许符号链接"));
            }
            self.ensure_within(&base, &joined, rel)?;
        } else if for_write {
            // 写入要给父目录建出来；建完再查一次，防止父目录是符号链接。
            if let Some(parent) = joined.parent() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    HostError::failed(self.info.key, format!("建目录失败：{err}"))
                })?;
                self.ensure_within(&base, parent, rel)?;
            }
        }

        Ok(joined)
    }

    /// 规范化后必须落在 `base` 之内。
    fn ensure_within(&self, base: &Path, target: &Path, rel: &str) -> Result<(), HostError> {
        let base = std::fs::canonicalize(base).unwrap_or_else(|_| base.to_path_buf());
        let target = std::fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
        if target.starts_with(&base) {
            Ok(())
        } else {
            Err(self.escape(rel, "解析后落在了插件目录之外（符号链接？）"))
        }
    }

    fn escape(&self, rel: &str, why: &str) -> HostError {
        HostError::invalid_args(self.info.key, format!("路径 {rel:?} 越界：{why}"))
    }

    fn path_arg(&self, args: &Value) -> Result<String, HostError> {
        match args.get("path") {
            None | Some(Value::Null) => Ok(String::new()),
            Some(Value::String(path)) => Ok(path.clone()),
            Some(other) => Err(HostError::invalid_args(
                self.info.key,
                format!("path 必须是字符串，收到 {other}"),
            )),
        }
    }

    fn read(&self, caller: &str, path: &str) -> Result<Value, HostError> {
        let file = self.resolve(caller, path, false)?;
        let bytes = std::fs::read(&file)
            .map_err(|err| HostError::failed(self.info.key, format!("读 {path:?} 失败：{err}")))?;
        let data = String::from_utf8(bytes).map_err(|_| {
            HostError::invalid_args(
                self.info.key,
                format!("{path:?} 不是 UTF-8 文本（二进制内容暂不支持）"),
            )
        })?;
        Ok(json!({ "data": data, "bytes": data.len() }))
    }

    fn write(&self, caller: &str, path: &str, args: &Value) -> Result<Value, HostError> {
        let data = match args.get("data") {
            Some(Value::String(data)) => data.clone(),
            Some(other) => {
                return Err(HostError::invalid_args(
                    self.info.key,
                    format!("data 必须是字符串，收到 {other}"),
                ))
            }
            None => {
                return Err(HostError::invalid_args(
                    self.info.key,
                    "write 需要 data 字段",
                ))
            }
        };
        let file = self.resolve(caller, path, true)?;
        std::fs::write(&file, data.as_bytes())
            .map_err(|err| HostError::failed(self.info.key, format!("写 {path:?} 失败：{err}")))?;
        Ok(json!({ "bytes": data.len() }))
    }

    fn list(&self, caller: &str, path: &str) -> Result<Value, HostError> {
        let dir = self.resolve(caller, path, false)?;
        let mut entries = Vec::new();
        let read_dir = match std::fs::read_dir(&dir) {
            Ok(read_dir) => read_dir,
            // 目录还不存在时列空表，比报错省事（插件首次启动常见）。
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(json!({ "entries": [] }))
            }
            Err(err) => {
                return Err(HostError::failed(
                    self.info.key,
                    format!("列 {path:?} 失败：{err}"),
                ))
            }
        };
        for entry in read_dir.flatten() {
            let meta = entry.metadata().ok();
            entries.push(json!({
                "name": entry.file_name().to_string_lossy(),
                "kind": if meta.as_ref().is_some_and(|m| m.is_dir()) { "dir" } else { "file" },
                "bytes": meta.as_ref().filter(|m| m.is_file()).map(|m| m.len()).unwrap_or(0),
            }));
        }
        entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Ok(json!({ "entries": entries }))
    }

    fn stat(&self, caller: &str, path: &str) -> Result<Value, HostError> {
        let target = self.resolve(caller, path, false)?;
        match std::fs::symlink_metadata(&target) {
            Ok(meta) => Ok(json!({
                "exists": true,
                "kind": if meta.is_dir() { "dir" } else { "file" },
                "bytes": if meta.is_file() { meta.len() } else { 0 },
            })),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(json!({ "exists": false }))
            }
            Err(err) => Err(HostError::failed(
                self.info.key,
                format!("查 {path:?} 失败：{err}"),
            )),
        }
    }

    fn delete(&self, caller: &str, path: &str) -> Result<Value, HostError> {
        if path.is_empty() {
            return Err(HostError::invalid_args(
                self.info.key,
                "delete 需要显式 path（清空整个目录请用 purge/clear）",
            ));
        }
        let target = self.resolve(caller, path, false)?;
        let meta = std::fs::symlink_metadata(&target)
            .map_err(|err| HostError::failed(self.info.key, format!("{path:?} 不存在：{err}")))?;
        if meta.is_dir() {
            // 只删空目录：非空目录报错，让插件自己决定要不要 purge。
            std::fs::remove_dir(&target).map_err(|err| {
                HostError::failed(
                    self.info.key,
                    format!("删目录 {path:?} 失败（只删空目录）：{err}"),
                )
            })?;
        } else {
            std::fs::remove_file(&target).map_err(|err| {
                HostError::failed(self.info.key, format!("删文件 {path:?} 失败：{err}"))
            })?;
        }
        Ok(json!({ "removed": true }))
    }

    fn purge(&self, caller: &str, path: &str, args: &Value) -> Result<Value, HostError> {
        if !self.bulk_delete {
            return Err(HostError::invalid_args(
                self.info.key,
                "这条服务不提供 purge",
            ));
        }
        if args.get("confirm") != Some(&Value::Bool(true)) {
            return Err(HostError::invalid_args(
                self.info.key,
                "purge 会递归删除，必须显式传 confirm: true",
            ));
        }
        let target = self.resolve(caller, path, false)?;
        if !target.exists() {
            return Ok(json!({ "removed": true }));
        }
        std::fs::remove_dir_all(&target)
            .or_else(|_| std::fs::remove_file(&target))
            .map_err(|err| {
                HostError::failed(self.info.key, format!("purge {path:?} 失败：{err}"))
            })?;
        Ok(json!({ "removed": true }))
    }

    fn clear(&self, caller: &str) -> Result<Value, HostError> {
        let dir = self.plugin_dir(caller)?;
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|err| {
                HostError::failed(self.info.key, format!("清空临时目录失败：{err}"))
            })?;
        }
        Ok(json!({ "removed": true }))
    }
}

impl HostService for FsService {
    fn info(&self) -> &'static HostServiceInfo {
        self.info
    }

    fn call(&self, caller: &str, args: Value) -> Result<Value, HostError> {
        let op = args
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| HostError::invalid_args(self.info.key, "缺少 op 字段"))?;
        let path = self.path_arg(&args)?;

        match (op, self.info.key) {
            ("read", _) => self.read(caller, &path),
            ("write", _) => self.write(caller, &path, &args),
            ("list", _) => self.list(caller, &path),
            ("stat", FS_PLUGIN) => self.stat(caller, &path),
            ("delete", FS_PLUGIN) => self.delete(caller, &path),
            ("purge", FS_PLUGIN) => self.purge(caller, &path, &args),
            ("clear", FS_TEMP) => self.clear(caller),
            (other, key) => Err(HostError::invalid_args(
                key,
                format!("未知 op：{other}（可用：{}）", self.ops_hint()),
            )),
        }
    }
}

impl FsService {
    fn ops_hint(&self) -> &'static str {
        if self.bulk_delete_ops() {
            "read/write/list/stat/delete/purge"
        } else {
            "read/write/list/clear"
        }
    }

    fn bulk_delete_ops(&self) -> bool {
        self.info.key == FS_PLUGIN
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "prts-fs-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn service(tag: &str) -> (PathBuf, FsService) {
        let root = temp_root(tag);
        let fs = FsService::new(root.clone());
        (root, fs)
    }

    #[test]
    fn write_then_read_round_trips() {
        let (root, fs) = service("roundtrip");
        let written = fs
            .call(
                "maa",
                json!({"op": "write", "path": "a/b.json", "data": "{\"x\":1}"}),
            )
            .unwrap();
        assert_eq!(written["bytes"], 7);
        let read = fs
            .call("maa", json!({"op": "read", "path": "a/b.json"}))
            .unwrap();
        assert_eq!(read["data"], "{\"x\":1}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plugins_are_isolated_by_caller() {
        let (root, fs) = service("isolation");
        fs.call(
            "maa",
            json!({"op": "write", "path": "secret.txt", "data": "maa"}),
        )
        .unwrap();
        let other = fs.call("other", json!({"op": "read", "path": "secret.txt"}));
        assert!(other.is_err(), "别的插件不该看见 maa 的文件");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_absolute_parent_and_outside_paths() {
        let (root, fs) = service("escape");
        for path in [
            "C:\\Windows\\win.ini",
            "/etc/passwd",
            "../secret.txt",
            "a/../../x",
        ] {
            let err = fs
                .call("maa", json!({"op": "read", "path": path}))
                .unwrap_err();
            assert!(
                matches!(err, HostError::InvalidArgs { .. }),
                "{path} 应该被拒：{err:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn rejects_symlink_inside_the_plugin_dir() {
        let (root, fs) = service("symlink");
        let outside = temp_root("symlink-outside");
        std::fs::write(outside.join("secret.txt"), b"top secret").unwrap();
        let base = root.join("plugin-data").join("maa");
        std::fs::create_dir_all(&base).unwrap();
        let link = base.join("link.txt");
        let made = std::os::windows::fs::symlink_file(outside.join("secret.txt"), &link);
        if made.is_err() {
            // 没有创建符号链接的权限（非开发者模式），跳过——不加 flaky 用例。
            let _ = std::fs::remove_dir_all(&root);
            let _ = std::fs::remove_dir_all(&outside);
            return;
        }
        let err = fs
            .call("maa", json!({"op": "read", "path": "link.txt"}))
            .unwrap_err();
        assert!(matches!(err, HostError::InvalidArgs { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn delete_only_removes_empty_dirs_and_purge_needs_confirm() {
        let (root, fs) = service("delete");
        fs.call(
            "maa",
            json!({"op": "write", "path": "d/x.txt", "data": "x"}),
        )
        .unwrap();
        let refused = fs.call("maa", json!({"op": "delete", "path": "d"}));
        assert!(refused.is_err(), "非空目录不能被 delete 带走");

        let no_confirm = fs.call("maa", json!({"op": "purge", "path": "d"}));
        assert!(no_confirm.is_err(), "purge 必须显式 confirm");

        fs.call("maa", json!({"op": "purge", "path": "d", "confirm": true}))
            .unwrap();
        assert!(!root.join("plugin-data").join("maa").join("d").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stat_and_list_report_missing_entries_calmly() {
        let (root, fs) = service("stat");
        assert_eq!(
            fs.call("maa", json!({"op": "stat", "path": "nope"}))
                .unwrap()["exists"],
            false
        );
        assert_eq!(
            fs.call("maa", json!({"op": "list"})).unwrap()["entries"],
            json!([])
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn temp_clears_and_rejects_plugin_only_ops() {
        let (root, _) = service("temp-root");
        let fs = FsService::temp(root.clone());
        fs.call(
            "maa",
            json!({"op": "write", "path": "scratch.txt", "data": "x"}),
        )
        .unwrap();
        assert!(fs.call("maa", json!({"op": "purge", "path": ""})).is_err());
        fs.call("maa", json!({"op": "clear"})).unwrap();
        assert!(!root.join("plugin-temp").join("maa").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unsafe_caller_is_rejected() {
        let (root, fs) = service("caller");
        let err = fs
            .call("../evil", json!({"op": "write", "path": "x", "data": "x"}))
            .unwrap_err();
        assert!(matches!(err, HostError::InvalidArgs { .. }));
        assert!(!root.join("evil").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
