//! skills_install —— 技能安装 / 移除（批 3 · 设置页动作）
//!
//! 参照既有实现 `skill-package-installer` / `safe-fs` 的成熟方案移植：
//! - 校验（路径 / 结构 / frontmatter / 命名 / 查重）→ 拷贝（隐藏临时名）→
//!   rename 落位（原子）→ 失败清理临时（不留半截产物）；
//! - **符号链接硬拒**（包内出现即拒绝，防越界读取与循环）；
//! - 支持两种形态：单 `.md` 文件，或技能包文件夹（根级含 `SKILL.md`；
//!   容忍一层包装目录——沿用既有实现 `findSkillPackageRoot` 语义）。
//!
//! 落位命名：以 frontmatter `name` 为准（`{name}.md` / `{name}/`）——查重
//! 收敛为「技能名 + 目标路径」两道；与现有条目同名一律拒绝安装（内置是
//! 启动同步的"源为准"区，覆盖式同步会在下次启动恢复内置版本）。
//!
//! 安装不改写文件内容（内容主权归用户）；移除只删运行根**直属**条目，
//! `source: builtin` 一律拒绝（前端按钮只是表象门，此处是实门）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::memory::store::atomic_write;
use crate::skills::parse_skill_frontmatter;
use crate::skills_index::SkillIndex;

/// 单文件大小上限（正文每轮全量入内存；防手滑选错大文件）。
const MAX_SINGLE_FILE_BYTES: u64 = 256 * 1024;
/// 文件夹包总大小上限（文本 + 小资源；防误选大目录）。
const MAX_PACKAGE_BYTES: u64 = 2 * 1024 * 1024;
/// 技能名长度上限（宽松；目录行是给模型的路径线索，超长无收益）。
const MAX_NAME_CHARS: usize = 40;

/// 安装结果（命令层转 DTO 返回前端）。
#[derive(Debug)]
pub struct InstalledSkill {
    pub name: String,
    pub description: String,
    pub file_path: PathBuf,
    pub source: Option<String>,
}

/// 安装技能：单 `.md` 文件或技能包文件夹（按源形态自动识别）。
pub fn install_skill(source: &Path, skills_root: &Path) -> Result<InstalledSkill, String> {
    if !source.is_absolute() {
        return Err("安装源必须是绝对路径".into());
    }
    std::fs::create_dir_all(skills_root)
        .map_err(|e| format!("技能目录不可用（{}）：{e}", skills_root.display()))?;
    let src = source
        .canonicalize()
        .map_err(|e| format!("安装源不存在或不可读（{}）：{e}", source.display()))?;
    let root_c = skills_root
        .canonicalize()
        .map_err(|e| format!("技能目录不可读（{}）：{e}", skills_root.display()))?;
    if src.starts_with(&root_c) {
        return Err("所选文件 / 文件夹已在技能目录中".into());
    }

    let meta =
        std::fs::metadata(&src).map_err(|e| format!("读取安装源失败（{}）：{e}", src.display()))?;
    if meta.is_dir() {
        install_folder(&src, skills_root)
    } else {
        install_single_file(&src, skills_root)
    }
}

/// 单文件安装。
fn install_single_file(source: &Path, skills_root: &Path) -> Result<InstalledSkill, String> {
    let ext_ok = source
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("md"))
        .unwrap_or(false);
    if !ext_ok {
        return Err("只支持 .md 单文件或含 SKILL.md 的技能文件夹".into());
    }
    let size = std::fs::metadata(source)
        .map_err(|e| format!("读取安装源失败（{}）：{e}", source.display()))?
        .len();
    if size > MAX_SINGLE_FILE_BYTES {
        return Err(format!(
            "文件过大（{size} 字节，上限 {MAX_SINGLE_FILE_BYTES}）：请确认选中的是技能文本"
        ));
    }
    let text =
        std::fs::read_to_string(source).map_err(|e| format!("读取失败（需 UTF-8 文本）：{e}"))?;
    let (name, description, source_tag) = validate_meta(&text)?;
    let target = prepare_target(skills_root, &name, false)?;
    atomic_write(&target, &text).map_err(|e| format!("写入技能文件失败：{e}"))?;
    Ok(InstalledSkill {
        name,
        description,
        file_path: target,
        source: source_tag,
    })
}

/// 文件夹包安装。
fn install_folder(source: &Path, skills_root: &Path) -> Result<InstalledSkill, String> {
    let pkg = find_package_root(source).ok_or_else(|| {
        "所选文件夹（含一级子目录）中未找到 SKILL.md——请选择技能包根目录".to_string()
    })?;
    assert_no_symlinks(&pkg)?;
    let total = dir_total_bytes(&pkg)?;
    if total > MAX_PACKAGE_BYTES {
        return Err(format!(
            "技能文件夹过大（合计 {total} 字节，上限 {MAX_PACKAGE_BYTES}）"
        ));
    }
    let text = std::fs::read_to_string(pkg.join("SKILL.md"))
        .map_err(|e| format!("读取 SKILL.md 失败（需 UTF-8 文本）：{e}"))?;
    let (name, description, source_tag) = validate_meta(&text)?;
    let target = prepare_target(skills_root, &name, true)?;
    copy_dir_into(&pkg, &target)?;
    Ok(InstalledSkill {
        name,
        description,
        file_path: target.join("SKILL.md"),
        source: source_tag,
    })
}

/// 移除用户技能：`file_path` 取自列表（单文件 = 根级 `.md`；文件夹包 = 包内 `SKILL.md`）。
///
/// 只允许移除运行根**直属**条目；`source: builtin` 拒绝（内置由启动同步
/// 维护，删除无意义且会被恢复）。返回被移除的技能名。
pub fn remove_skill(file_path: &Path, skills_root: &Path) -> Result<String, String> {
    let root_c = skills_root
        .canonicalize()
        .map_err(|e| format!("技能目录不可读（{}）：{e}", skills_root.display()))?;
    let path_c = file_path
        .canonicalize()
        .map_err(|e| format!("目标不存在或不可读（{}）：{e}", file_path.display()))?;
    let is_package = path_c.file_name().and_then(|n| n.to_str()) == Some("SKILL.md");
    let entry = if is_package {
        path_c
            .parent()
            .ok_or_else(|| "路径异常：SKILL.md 无父目录".to_string())?
            .to_path_buf()
    } else {
        path_c.clone()
    };
    if entry.parent() != Some(root_c.as_path()) {
        return Err("只能移除技能目录直属的条目（深层文件请经由其条目操作）".into());
    }
    let text =
        std::fs::read_to_string(&path_c).map_err(|e| format!("读取失败（需 UTF-8 文本）：{e}"))?;
    let meta = parse_skill_frontmatter(&text)
        .ok_or_else(|| "无法解析 frontmatter，拒绝移除".to_string())?;
    if meta.source.as_deref() == Some("builtin") {
        return Err("内置技能不提供移除（如需删除，请在技能目录中自行操作）".into());
    }
    if is_package {
        std::fs::remove_dir_all(&entry)
            .map_err(|e| format!("删除技能文件夹失败（{}）：{e}", entry.display()))?;
    } else {
        std::fs::remove_file(&entry)
            .map_err(|e| format!("删除技能文件失败（{}）：{e}", entry.display()))?;
    }
    Ok(meta.name)
}

// ═══════════════════════════════════════════════════════
// 内部辅助
// ═══════════════════════════════════════════════════════

/// frontmatter 校验：name 合法 + description 非空；返回 (name, desc, source)。
fn validate_meta(text: &str) -> Result<(String, String, Option<String>), String> {
    let meta = parse_skill_frontmatter(text)
        .ok_or_else(|| "缺少或无法解析 frontmatter（至少需要 name）".to_string())?;
    let name = validate_skill_name(&meta.name)?;
    let description = meta.description.trim().to_string();
    if description.is_empty() {
        return Err("frontmatter 缺少 description（描述即激活判据，不可为空）".into());
    }
    Ok((name, description, meta.source))
}

/// 技能名校验（宽口径：允许中文与 ASCII；禁路径分隔符 / Windows 保留字符 /
/// 控制字符——落盘名由它派生，必须跨平台安全）。
fn validate_skill_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err("技能名（frontmatter name）为空".into());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("技能名过长（上限 {MAX_NAME_CHARS} 字）：{name}"));
    }
    if name.starts_with('.') {
        return Err(format!("技能名不能以「.」开头：{name}"));
    }
    if name.chars().any(|c| {
        c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    }) {
        return Err(format!(
            "技能名含非法字符（路径分隔符 / Windows 保留字符 / 控制字符）：{name}"
        ));
    }
    Ok(name.to_string())
}

/// 目标路径查重与准备：技能名不撞活体索引；目标（文件与目录两种形态）
/// 不撞盘上现有条目。返回落位路径。
fn prepare_target(skills_root: &Path, name: &str, folder: bool) -> Result<PathBuf, String> {
    let existing: HashSet<String> = SkillIndex::load(skills_root)
        .map(|idx| idx.names().into_iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();
    if existing.contains(name) {
        return Err(format!(
            "与现有技能重名：「{name}」。请先移除旧条目，或修改技能名后再装"
        ));
    }
    let file_target = skills_root.join(format!("{name}.md"));
    let dir_target = skills_root.join(name);
    if file_target.exists() || dir_target.exists() {
        return Err(format!(
            "技能目录已有同名条目（{} / {}）",
            file_target.display(),
            dir_target.display()
        ));
    }
    Ok(if folder { dir_target } else { file_target })
}

/// 定位技能包根：所选目录根级优先；缺 SKILL.md 时看一级子目录（容忍一层包装）。
fn find_package_root(source_dir: &Path) -> Option<PathBuf> {
    if source_dir.join("SKILL.md").is_file() {
        return Some(source_dir.to_path_buf());
    }
    let mut subs: Vec<PathBuf> = std::fs::read_dir(source_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            !p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with('.'))
                .unwrap_or(false)
        })
        .filter(|p| p.is_dir())
        .collect();
    subs.sort();
    subs.into_iter().find(|p| p.join("SKILL.md").is_file())
}

/// 递归检查符号链接：包内出现即拒绝（沿用既有实现 `assertNoSymlinkEntries`）。
fn assert_no_symlinks(dir: &Path) -> Result<(), String> {
    let rd =
        std::fs::read_dir(dir).map_err(|e| format!("读取目录失败（{}）：{e}", dir.display()))?;
    for entry in rd {
        let entry = entry.map_err(|e| format!("读取目录失败（{}）：{e}", dir.display()))?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("读取元数据失败（{}）：{e}", path.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "技能文件夹含符号链接，出于安全考虑拒绝安装：{}",
                path.display()
            ));
        }
        if meta.is_dir() {
            assert_no_symlinks(&path)?;
        }
    }
    Ok(())
}

/// 递归统计目录总大小（字节）。
fn dir_total_bytes(dir: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    let rd =
        std::fs::read_dir(dir).map_err(|e| format!("读取目录失败（{}）：{e}", dir.display()))?;
    for entry in rd {
        let entry = entry.map_err(|e| format!("读取目录失败（{}）：{e}", dir.display()))?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("读取元数据失败（{}）：{e}", path.display()))?;
        if meta.is_dir() {
            total += dir_total_bytes(&path)?;
        } else {
            total += meta.len();
        }
    }
    Ok(total)
}

/// 目录拷贝落位：先复制到同根隐藏临时目录（`.` 前缀，不入索引），
/// 再 rename 落位（原子）；任一步失败清理临时（沿用既有实现 `safeCopyDir` 语义；
/// 目标必须不存在，故无备份/回滚分支）。
fn copy_dir_into(src: &Path, dst: &Path) -> Result<(), String> {
    let parent = dst
        .parent()
        .ok_or_else(|| format!("目标路径无父目录：{}", dst.display()))?;
    let name = dst
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("目标路径非法：{}", dst.display()))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(".tmp-{name}-{nanos}"));
    let result = copy_tree(src, &tmp).and_then(|_| {
        std::fs::rename(&tmp, dst)
            .map_err(|e| format!("落位失败（{} → {}）：{e}", tmp.display(), dst.display()))
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    result
}

/// 递归复制目录树（普通文件 / 目录；符号链接在入口已拒绝）。
fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("创建目录失败（{}）：{e}", dst.display()))?;
    let rd =
        std::fs::read_dir(src).map_err(|e| format!("读取目录失败（{}）：{e}", src.display()))?;
    for entry in rd {
        let entry = entry.map_err(|e| format!("读取目录失败（{}）：{e}", src.display()))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&from)
            .map_err(|e| format!("读取元数据失败（{}）：{e}", from.display()))?;
        if meta.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .map_err(|e| format!("复制文件失败（{}）：{e}", from.display()))?;
        }
    }
    Ok(())
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-skill-install-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fixture_skill_md(dir: &Path, file: &str, name: &str, desc: &str) -> PathBuf {
        let p = dir.join(file);
        std::fs::write(
            &p,
            format!("---\nname: {name}\ndescription: {desc}\n---\n\n## 正文"),
        )
        .unwrap();
        p
    }

    #[test]
    fn install_single_file_lands_named_by_frontmatter() {
        let root = temp_dir("root1");
        let src = temp_dir("src1");
        let file = fixture_skill_md(&src, "whatever.md", "黑话小抄", "术语对照");
        let out = install_skill(&file, &root).unwrap();
        assert_eq!(out.name, "黑话小抄");
        assert_eq!(out.description, "术语对照");
        assert!(
            root.join("黑话小抄.md").is_file(),
            "落盘名 = frontmatter name"
        );
        assert!(out.file_path.ends_with("黑话小抄.md"));
        // 字节一致（不改写内容）
        let src_bytes = std::fs::read(&file).unwrap();
        let dst_bytes = std::fs::read(root.join("黑话小抄.md")).unwrap();
        assert_eq!(src_bytes, dst_bytes);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&src);
    }

    #[test]
    fn install_rejects_invalid_sources() {
        let root = temp_dir("root2");
        let src = temp_dir("src2");

        // 非绝对路径
        let rel = Path::new("rel.md");
        assert!(install_skill(rel, &root).unwrap_err().contains("绝对路径"));

        // 不存在
        let missing = src.join("no-such.md");
        assert!(install_skill(&missing, &root)
            .unwrap_err()
            .contains("不存在"));

        // 非 .md
        let txt = src.join("note.txt");
        std::fs::write(&txt, "x").unwrap();
        assert!(install_skill(&txt, &root).unwrap_err().contains("只支持"));

        // 无 name / 无 desc
        let no_name = src.join("noname.md");
        std::fs::write(&no_name, "---\ndescription: d\n---\n\n## x").unwrap();
        assert!(install_skill(&no_name, &root)
            .unwrap_err()
            .contains("frontmatter"));

        let no_desc = src.join("nodesc.md");
        std::fs::write(&no_desc, "---\nname: 测试\n---\n\n## x").unwrap();
        assert!(install_skill(&no_desc, &root)
            .unwrap_err()
            .contains("description"));

        // 非法名（路径分隔符）
        let bad_name = src.join("badname.md");
        std::fs::write(&bad_name, "---\nname: a/b\ndescription: d\n---\n\n## x").unwrap();
        assert!(install_skill(&bad_name, &root)
            .unwrap_err()
            .contains("非法字符"));

        // 过大
        let big = src.join("big.md");
        let big_text = format!(
            "---\nname: 大文件\ndescription: d\n---\n\n{}",
            "字".repeat(300 * 1024)
        );
        std::fs::write(&big, big_text).unwrap();
        assert!(install_skill(&big, &root).unwrap_err().contains("过大"));

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&src);
    }

    #[test]
    fn install_folder_package_and_wrapper_tolerance() {
        let root = temp_dir("root3");
        let src = temp_dir("src3");
        // src3/pack/SKILL.md + references/（模拟"选到了上一层文件夹"）
        std::fs::create_dir_all(src.join("pack").join("references")).unwrap();
        std::fs::write(
            src.join("pack").join("SKILL.md"),
            "---\nname: 黑话小抄\ndescription: 术语对照\n---\n\n## 正文\n见 references/terms.md",
        )
        .unwrap();
        std::fs::write(src.join("pack").join("references").join("terms.md"), "词条").unwrap();

        // 传上一层（src3）→ 容忍一层包装定位到 pack/
        let out = install_skill(&src, &root).unwrap();
        assert_eq!(out.name, "黑话小抄");
        assert!(root.join("黑话小抄").join("SKILL.md").is_file());
        assert!(
            root.join("黑话小抄")
                .join("references")
                .join("terms.md")
                .is_file(),
            "资源应一并复制"
        );
        assert!(out.file_path.ends_with("SKILL.md"));

        // 二次安装同名 → 拒绝
        let err = install_skill(&src, &root).unwrap_err();
        assert!(err.contains("重名"), "got: {err}");

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&src);
    }

    #[test]
    fn install_folder_requires_skill_md() {
        let root = temp_dir("root4");
        let src = temp_dir("src4");
        std::fs::write(src.join("random.md"), "x").unwrap();
        let err = install_skill(&src, &root).unwrap_err();
        assert!(err.contains("SKILL.md"), "got: {err}");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&src);
    }

    #[test]
    fn install_rejects_source_inside_root() {
        let root = temp_dir("root5");
        fixture_skill_md(&root, "已有.md", "已有", "d");
        let inside = root.join("已有.md");
        let err = install_skill(&inside, &root).unwrap_err();
        assert!(err.contains("已在技能目录中"), "got: {err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_skill_rules() {
        let root = temp_dir("root6");
        // 用户单文件 / 用户包 / 内置 / 深层文件（非直属）
        fixture_skill_md(&root, "user1.md", "用户一", "d");
        std::fs::create_dir_all(root.join("用户包")).unwrap();
        std::fs::write(
            root.join("用户包").join("SKILL.md"),
            "---\nname: 用户包\ndescription: d\n---\n\n## x",
        )
        .unwrap();
        std::fs::write(
            root.join("内置一.md"),
            "---\nname: 内置一\ndescription: d\nsource: builtin\n---\n\n## x",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("deep.md"), "x").unwrap();

        // 单文件移除
        let name = remove_skill(&root.join("user1.md"), &root).unwrap();
        assert_eq!(name, "用户一");
        assert!(!root.join("user1.md").exists());

        // 包移除（file_path = 包内 SKILL.md）→ 整目录删除
        let name = remove_skill(&root.join("用户包").join("SKILL.md"), &root).unwrap();
        assert_eq!(name, "用户包");
        assert!(!root.join("用户包").exists());

        // 内置拒绝
        let err = remove_skill(&root.join("内置一.md"), &root).unwrap_err();
        assert!(err.contains("内置技能不提供移除"), "got: {err}");

        // 非直属拒绝
        let err = remove_skill(&root.join("sub").join("deep.md"), &root).unwrap_err();
        assert!(err.contains("直属"), "got: {err}");

        let _ = std::fs::remove_dir_all(&root);
    }
}
