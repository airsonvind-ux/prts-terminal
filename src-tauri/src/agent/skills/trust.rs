//! Skill 信任与载荷扫描（R-29）
//!
//! 安装预览、索引建表、运行时闸共用同一套判断：
//! - 技能包目录内是否存在可执行/脚本文件；
//! - 包指纹（FNV-1a 64，非密码学哈希，只用于展示与变更识别）；
//! - 信任级别（builtin 恒 trusted；其余含脚本或 community 为 untrusted）。
//!
//! 单 `.md` 技能没有包目录，也没有脚本载荷；其 resource_dir 为 None。

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::is_reparse_point;

/// 视为可执行/脚本载荷的扩展名（小写比较）。
pub const SCRIPT_EXTENSIONS: &[&str] = &[
    "exe", "dll", "bat", "cmd", "ps1", "py", "js", "mjs", "cjs", "vbs", "lnk", "url", "scr", "msi",
    "jar", "com", "sh",
];

/// 技能信任级别（内部判断；前端以字符串 "trusted"/"untrusted" 透出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkillTrust {
    Trusted,
    Untrusted,
}

impl SkillTrust {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Untrusted => "untrusted",
        }
    }
}

/// 包内一个可执行/脚本文件（相对包根路径 + 字节数）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillScriptFile {
    /// 相对包根的路径，统一使用 `/` 分隔（跨平台展示与测试稳定）。
    pub path: String,
    pub size_bytes: u64,
}

/// 一次技能载荷检查的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInspection {
    /// 包目录（单 `.md` 为 None）。
    pub resource_dir: Option<PathBuf>,
    /// 可执行/脚本文件（按相对路径排序）。
    pub scripts: Vec<SkillScriptFile>,
    /// FNV-1a 64 包指纹（`fnv1a64:...`）；单文件为文件内容指纹。
    pub package_hash: String,
}

/// 判断扩展名是否属于脚本/可执行载荷。
pub fn is_script_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SCRIPT_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// 根据来源与脚本命中计算信任级别。
///
/// - `builtin`：应用随包提供，trusted（即使内置包将来带脚本，也由应用自身负责）；
/// - 其他包含脚本：untrusted；
/// - `community` 即使纯文本：untrusted；
/// - 其余（用户纯文本）：trusted。
pub fn trust_for(source: Option<&str>, scripts: &[SkillScriptFile]) -> SkillTrust {
    if source == Some("builtin") {
        return SkillTrust::Trusted;
    }
    if !scripts.is_empty() || source == Some("community") {
        return SkillTrust::Untrusted;
    }
    SkillTrust::Trusted
}

/// 检查一个技能条目路径：`SKILL.md` 视为包根标记，其余 `.md` 视为单文件。
pub fn inspect_skill_path(path: &Path) -> Result<PackageInspection, String> {
    let is_package = path.file_name().and_then(|n| n.to_str()) == Some("SKILL.md");
    if is_package {
        let dir = path
            .parent()
            .ok_or_else(|| format!("技能包 SKILL.md 无父目录：{}", path.display()))?;
        inspect_package_dir(dir)
    } else {
        inspect_single_file(path)
    }
}

/// 检查技能包目录：递归收集文件、计算指纹、列出脚本。
pub fn inspect_package_dir(dir: &Path) -> Result<PackageInspection, String> {
    if !dir.is_dir() {
        return Err(format!("技能包目录不存在或不可读：{}", dir.display()));
    }
    let mut files = Vec::new();
    collect_files(dir, &mut files)?;
    files.sort_by_key(|path| relative_path(dir, path));

    let mut hash = FNV_OFFSET;
    let mut scripts = Vec::new();
    for path in files {
        let rel = relative_path(dir, &path);
        fnv_update(&mut hash, rel.as_bytes());
        fnv_update(&mut hash, &[0]);
        let bytes =
            std::fs::read(&path).map_err(|e| format!("读取文件失败（{}）：{e}", path.display()))?;
        fnv_update(&mut hash, &bytes);
        fnv_update(&mut hash, &[0]);
        if is_script_file(&path) {
            scripts.push(SkillScriptFile {
                path: rel,
                size_bytes: bytes.len() as u64,
            });
        }
    }

    Ok(PackageInspection {
        resource_dir: Some(dir.to_path_buf()),
        scripts,
        package_hash: format!("fnv1a64:{hash:016x}"),
    })
}

/// 检查单 `.md` 技能：没有包目录与附件脚本。
fn inspect_single_file(path: &Path) -> Result<PackageInspection, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("读取技能文件失败（{}）：{e}", path.display()))?;
    let mut hash = FNV_OFFSET;
    fnv_update(&mut hash, &bytes);
    Ok(PackageInspection {
        resource_dir: None,
        scripts: Vec::new(),
        package_hash: format!("fnv1a64:{hash:016x}"),
    })
}

/// 递归收集普通文件（跳过符号链接；安装入口已显式拒绝符号链接）。
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let rd =
        std::fs::read_dir(dir).map_err(|e| format!("读取目录失败（{}）：{e}", dir.display()))?;
    let mut entries: Vec<_> = rd
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("读取目录失败（{}）：{e}", dir.display()))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("读取元数据失败（{}）：{e}", path.display()))?;
        if is_reparse_point(&meta) {
            continue;
        }
        if meta.is_dir() {
            collect_files(&path, out)?;
        } else if meta.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

/// 相对路径统一为 `/` 分隔。
fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

fn fnv_update(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-skill-trust-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn package_inspection_lists_scripts_and_hashes() {
        let dir = temp_dir("scripts");
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: 测试\ndescription: d\n---\n\n正文",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts").join("run.PS1"), "Write-Host hi").unwrap();
        std::fs::write(dir.join("notes.md"), "note").unwrap();

        let out = inspect_package_dir(&dir).unwrap();
        assert_eq!(
            out.scripts,
            vec![SkillScriptFile {
                path: "scripts/run.PS1".into(),
                size_bytes: 13,
            }]
        );
        assert!(out.resource_dir.as_deref() == Some(dir.as_path()));
        assert!(out.package_hash.starts_with("fnv1a64:"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn single_md_has_no_resource_dir_or_scripts() {
        let dir = temp_dir("single");
        let file = dir.join("note.md");
        std::fs::write(&file, "---\nname: 单文件\ndescription: d\n---").unwrap();
        let out = inspect_skill_path(&file).unwrap();
        assert!(out.resource_dir.is_none());
        assert!(out.scripts.is_empty());
        assert!(out.package_hash.starts_with("fnv1a64:"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trust_rules_keep_builtin_and_user_text_trusted() {
        assert_eq!(trust_for(Some("builtin"), &[]), SkillTrust::Trusted);
        assert_eq!(trust_for(Some("user"), &[]), SkillTrust::Trusted);
        assert_eq!(
            trust_for(
                Some("user"),
                &[SkillScriptFile {
                    path: "run.ps1".into(),
                    size_bytes: 1
                }]
            ),
            SkillTrust::Untrusted
        );
        assert_eq!(trust_for(Some("community"), &[]), SkillTrust::Untrusted);
    }
}
