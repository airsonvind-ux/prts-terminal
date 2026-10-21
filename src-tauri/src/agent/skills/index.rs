//! skills_index —— Skill 条目索引（turn 级快照）
//!
//! 每 turn 入口扫描 skills 目录，正文全量读入内存；激活时纯内存操作，
//! 无 IO 失败面（读文件失败只可能发生在扫描期，一次性暴露）。
//! 结构仿 knowledge::index::KnowledgeIndex：递归扫描、逐层按 file_name()
//! 排序保证遍历顺序确定；frontmatter 解析复用 skills::parse_skill_frontmatter，
//! 正文剥离复用 knowledge::body_after_frontmatter（判定规则与解析器一致）。
//!
//! 目录模型（2026-10-03 定案：单目录混装）：
//! - 运行根 [`skills_dir`] = `data_dir()/skills`——唯一读写根；用户可写，
//!   内置技能经 [`sync_bundled_skills`] 于启动时同步进入；
//! - 内置源 [`bundled_skills_dir`] = dev `content/skills` / 打包 `resources/skills`；
//! - 同步语义：源为准、覆盖式；用户条目只增不删（内置被删 / 改 → 下次启动
//!   恢复），内置退役条目清理（版本间改名 / 改形态时旧副本退场）；
//!   同步失败由调用方记日志、不阻断启动。
//! - 条目形态（批 3）：单个 `.md` 文件，或**技能包目录**（根级含 `SKILL.md`
//!   ——作为一个技能单元收录、不再下钻其子目录）；`.` 前缀条目不入索引
//!   （隐藏文件与安装残留临时目录的约定）。
//!
//! 模块 B 已接线：本索引由 orchestrator 每 turn 在 process 入口重建
//! （turn 级快照），经 ToolContext 通道注入激活工具，并经
//! assemble_system_prompt 的 Catalog 层注入 system prompt。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::skills::SkillMeta;

/// Skill 索引：name → 元数据 + 正文（剥 frontmatter）+ 源文件路径。
#[derive(Debug)]
pub struct SkillIndex {
    skills: HashMap<String, SkillMeta>,
    bodies: HashMap<String, String>,
    /// name → 源 md 文件绝对路径（空间页/工具链接打开用；文件名≠name）
    paths: HashMap<String, PathBuf>,
}

impl SkillIndex {
    /// 递归扫描 dir 下所有 `.md` 文件（跳过 `_index.md`）建立索引。
    ///
    /// - 无 frontmatter / 无 name → `eprintln!` 告警并跳过
    /// - name 重名 → `eprintln!` 告警，保留先注册的
    /// - dir 不存在或不可读 → Err（描述性信息）
    pub fn load(dir: &Path) -> Result<Self, String> {
        if !dir.is_dir() {
            return Err(format!(
                "Skill 目录不存在或不可读：{}",
                dir.display()
            ));
        }

        let mut skills: HashMap<String, SkillMeta> = HashMap::new();
        let mut bodies: HashMap<String, String> = HashMap::new();
        let mut paths: HashMap<String, PathBuf> = HashMap::new();

        // 队列式遍历（避免递归深度问题）；read_dir 迭代顺序 OS 依赖：
        // 每层先收集为 Vec、按 file_name() 排序再处理（目录与文件统一按名
        // 排序），保证遍历顺序确定（仿 KnowledgeIndex）。
        let mut queue: std::collections::VecDeque<PathBuf> =
            std::collections::VecDeque::from([dir.to_path_buf()]);
        while let Some(d) = queue.pop_front() {
            // 技能包目录（根级含 SKILL.md，且非运行根自身）：作为一个技能
            // 单元收录，不再下钻——其 references 等资源不产生独立条目。
            if d.as_path() != dir && d.join("SKILL.md").is_file() {
                register_skill(&mut skills, &mut bodies, &mut paths, &d.join("SKILL.md"))?;
                continue;
            }
            let rd = std::fs::read_dir(&d)
                .map_err(|e| format!("读取 Skill 目录失败（{}）：{e}", d.display()))?;
            let mut dir_entries: Vec<_> = rd
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("读取 Skill 目录失败（{}）：{e}", d.display()))?;
            dir_entries.sort_by_key(|e| e.file_name());
            for entry in dir_entries {
                let path = entry.path();
                // '.' 前缀条目（隐藏文件 / 安装残留临时目录）不入索引
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with('.'))
                    .unwrap_or(false)
                {
                    continue;
                }

                if path.is_dir() {
                    queue.push_back(path);
                    continue;
                }

                let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                // 跳过索引文档（_index.md，任意层级）与非 .md 文件
                if file_name == "_index.md" || !file_name.ends_with(".md") {
                    continue;
                }

                register_skill(&mut skills, &mut bodies, &mut paths, &path)?;
            }
        }

        Ok(Self { skills, bodies, paths })
    }

    /// 降级空索引（空表）：索引加载失败时使用，激活恒未命中。
    pub fn empty() -> Self {
        Self {
            skills: HashMap::new(),
            bodies: HashMap::new(),
            paths: HashMap::new(),
        }
    }

    /// 按 name 查元数据。
    pub fn resolve(&self, name: &str) -> Option<&SkillMeta> {
        self.skills.get(name)
    }

    /// 按 name 查正文（剥 frontmatter）。
    pub fn body(&self, name: &str) -> Option<&str> {
        self.bodies.get(name).map(String::as_str)
    }

    /// 按 name 查源文件路径（空间页/工具链接打开用）。
    pub fn path(&self, name: &str) -> Option<&Path> {
        self.paths.get(name).map(PathBuf::as_path)
    }

    /// 全部条目名，按 name 排序（激活工具未命中时报错列目录用）。
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.skills.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// 目录清单文本：每条一行 `- {name}：{description}`，按 name 排序，
    /// 行间 `\n` 连接；空索引返回空字符串。
    pub fn catalog_text(&self) -> String {
        self.names()
            .into_iter()
            .map(|name| {
                let desc = self
                    .skills
                    .get(name)
                    .map(|m| m.description.as_str())
                    .unwrap_or_default();
                format!("- {name}：{desc}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 全部条目 `(name, description)` 对，按 name 排序（提醒 diff / 基线用，批 3）。
    pub fn catalog_pairs(&self) -> Vec<(String, String)> {
        self.names()
            .into_iter()
            .map(|name| {
                let desc = self
                    .skills
                    .get(name)
                    .map(|m| m.description.clone())
                    .unwrap_or_default();
                (name.to_string(), desc)
            })
            .collect()
    }
}

/// 读取并注册一个技能文件（解析失败 → 告警跳过；重名 → 告警保留先注册的）。
fn register_skill(
    skills: &mut HashMap<String, SkillMeta>,
    bodies: &mut HashMap<String, String>,
    paths: &mut HashMap<String, PathBuf>,
    path: &Path,
) -> Result<(), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("读取 Skill 文件失败（{}）：{e}", path.display()))?;
    let Some(meta) = crate::skills::parse_skill_frontmatter(&text) else {
        // 无 frontmatter 或 name 为空均落此分支（解析器已保证
        // name 非空才返回 Some，此处无需二次检查）
        eprintln!("[skills] 跳过 frontmatter 无效的 Skill：{}", path.display());
        return Ok(());
    };
    if skills.contains_key(&meta.name) {
        eprintln!(
            "[skills] 重名 Skill 已忽略（保留先注册的）：{}",
            path.display()
        );
        return Ok(());
    }
    // 正文全量读入内存（激活时纯内存操作，无 IO 失败面）。
    // 剥离复用 knowledge::body_after_frontmatter：判定规则与
    // parse_frontmatter 一致（LF/CRLF 兼容），勿重复实现。
    let name = meta.name.clone();
    let body = crate::knowledge::body_after_frontmatter(&text).to_string();
    skills.insert(name.clone(), meta);
    bodies.insert(name.clone(), body);
    paths.insert(name, path.to_path_buf());
    Ok(())
}

/// 技能运行根（唯一读写根）：`data_dir()/skills`。
///
/// 单目录混装（2026-10-03 定案）：内置技能经 [`sync_bundled_skills`] 于
/// 启动时同步进入本目录，与用户技能同住；全部消费方（索引 / catalog /
/// activate / list_skills / notices / policy）一律读本目录。
/// 纯路径解析，不建目录（创建由同步负责；纪律照 config.rs）。
pub fn skills_dir() -> PathBuf {
    crate::config::data_dir().join("skills")
}

/// 内置技能源目录解析（随包只读资源）。
///
/// 收口于 [`crate::config::bundled_skills_dir`]：开发构建 = 源码树 `content/skills`；
/// 发布构建 = 程序目录下 `resources/skills`（由 tauri.conf bundle.resources
/// 从 `content/skills/` 铺入）。**不做存在性探测**。
pub fn bundled_skills_dir() -> PathBuf {
    crate::config::bundled_skills_dir()
}

/// 同步结果：本次被写入 / 退役的文件（名，遍历序确定）+ 未触碰计数。
#[derive(Debug, Default)]
pub struct SkillSyncReport {
    /// 新增或字节变化被覆盖的文件相对路径（分隔符随平台）
    pub changed: Vec<String>,
    /// 字节一致、未触碰的文件数
    pub unchanged: usize,
    /// 内置退役清理删除的运行根直属条目名（版本间改名 / 改形态的退场）
    pub removed: Vec<String>,
}

/// 内置技能同步：源为准、覆盖式。
///
/// - 递归遍历 `src` 下全部文件（现行形态：技能包目录 `{name}/SKILL.md`，
///   目录树原样镜像；任意扩展名随包复制）；
/// - `dst` 缺失同名文件 → 复制；存在但字节不同 → 覆盖；字节一致 → 不触碰
///   （不重写，mtime 不变；幂等）；
/// - **用户条目只增不删**：`dst` 中与源无对应的条目一律保留；
/// - **内置退役清理**（[`retire_stale_builtins`]）：运行根直属、`source: builtin`
///   且源中已无对应路径的旧形态副本 → 删除（版本间改名 / 改形态的退场通道）；
/// - `dst` 不存在则创建；`src` 不存在 → Err（调用方记日志、决定是否阻断）。
pub fn sync_bundled_skills(src: &Path, dst: &Path) -> Result<SkillSyncReport, String> {
    if !src.is_dir() {
        return Err(format!("内置技能源目录不存在或不可读：{}", src.display()));
    }
    std::fs::create_dir_all(dst)
        .map_err(|e| format!("创建技能运行根失败（{}）：{e}", dst.display()))?;

    let mut report = SkillSyncReport::default();
    // 队列式遍历（同 SkillIndex::load）；每层按 file_name 排序保证顺序确定
    let mut queue: std::collections::VecDeque<PathBuf> =
        std::collections::VecDeque::from([src.to_path_buf()]);
    while let Some(d) = queue.pop_front() {
        let rd = std::fs::read_dir(&d)
            .map_err(|e| format!("读取内置技能源失败（{}）：{e}", d.display()))?;
        let mut dir_entries: Vec<_> = rd
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("读取内置技能源失败（{}）：{e}", d.display()))?;
        dir_entries.sort_by_key(|e| e.file_name());
        for entry in dir_entries {
            let path = entry.path();
            if path.is_dir() {
                queue.push_back(path);
                continue;
            }
            let rel = path
                .strip_prefix(src)
                .map_err(|_| format!("源文件不在源根内（异常）：{}", path.display()))?;
            let target = dst.join(rel);
            let bytes = std::fs::read(&path)
                .map_err(|e| format!("读取源文件失败（{}）：{e}", path.display()))?;
            // 字节一致 → 不触碰（幂等；不重写、mtime 不变）
            let same = std::fs::read(&target)
                .map(|existing| existing == bytes)
                .unwrap_or(false);
            if same {
                report.unchanged += 1;
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("创建目标子目录失败（{}）：{e}", parent.display()))?;
            }
            std::fs::write(&target, &bytes)
                .map_err(|e| format!("写入技能文件失败（{}）：{e}", target.display()))?;
            report.changed.push(rel.to_string_lossy().into_owned());
        }
    }
    // 内置退役清理（源为准的延伸）：内置条目在版本间改名 / 改形态
    // （如单文件 → 文件夹包）时，运行根中旧形态副本在此退场。
    retire_stale_builtins(src, dst, &mut report);
    Ok(report)
}

/// 内置退役清理：运行根**直属**条目中「判定为内置（`source: builtin`）且
/// 在内置源中无对应路径」者 → 删除。用户条目（无 source 或非 builtin）不动；
/// `'.'` 前缀条目（隐藏 / 临时）不动。
///
/// 对应关系按相对路径（文件名 / 目录名）匹配——源里还有的条目由复制通道
/// 维护，不走清理。删除失败静默跳过（由后续同步重试），不阻断启动。
fn retire_stale_builtins(src: &Path, dst: &Path, report: &mut SkillSyncReport) {
    let Ok(rd) = std::fs::read_dir(dst) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if src.join(name).exists() {
            continue;
        }
        // 判定内置：文件（.md）或目录（根级 SKILL.md）的 frontmatter source
        let builtin = if path.is_dir() {
            std::fs::read_to_string(path.join("SKILL.md"))
                .ok()
                .and_then(|t| crate::skills::parse_skill_frontmatter(&t))
                .map(|m| m.source.as_deref() == Some("builtin"))
                .unwrap_or(false)
        } else {
            if !name.ends_with(".md") {
                continue;
            }
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| crate::skills::parse_skill_frontmatter(&t))
                .map(|m| m.source.as_deref() == Some("builtin"))
                .unwrap_or(false)
        };
        if !builtin {
            continue;
        }
        let removed = if path.is_dir() {
            std::fs::remove_dir_all(&path).is_ok()
        } else {
            std::fs::remove_file(&path).is_ok()
        };
        if removed {
            report.removed.push(name.to_string());
        }
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建唯一临时目录（不引入 tempfile crate）。
    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-skills-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 假 skills 目录：两个有效条目（不同 name，文件名字母序与 name 序相反，
    /// 验证 catalog 按 name 排序而非按文件名）、一个无 frontmatter 文件、
    /// 一个无 name 文件、一个重名 name 文件（与 alpha.md 重名）。
    fn fixture(root: &Path) {
        std::fs::write(
            root.join("alpha.md"),
            "---\nname: 贝塔\ndescription: 条目 B\n---\n\n## 正文 B",
        )
        .unwrap();
        std::fs::write(
            root.join("gamma.md"),
            "---\nname: 阿尔法\ndescription: 条目 A\n---\n\n## 正文 A",
        )
        .unwrap();
        // 无 frontmatter → 跳过
        std::fs::write(root.join("nofront.md"), "## 没有元数据").unwrap();
        // 有 frontmatter 但无 name → 跳过
        std::fs::write(
            root.join("noname.md"),
            "---\ndescription: 没有名字\n---\n\n## 无名字",
        )
        .unwrap();
        // 重名 name（与 alpha.md 的「贝塔」重名）→ 告警，保留先注册的
        std::fs::write(
            root.join("zzz-dup.md"),
            "---\nname: 贝塔\ndescription: 重复条目\n---\n\n## 重复",
        )
        .unwrap();
        // 索引文档 → 跳过
        std::fs::write(root.join("_index.md"), "# Skill 索引").unwrap();
    }

    #[test]
    fn load_resolves_body_and_catalog() {
        let root = unique_temp_dir("load");
        fixture(&root);
        let idx = SkillIndex::load(&root).unwrap();

        // 只注册两个有效条目：阿尔法、贝塔
        let alpha = idx.resolve("阿尔法").unwrap();
        assert_eq!(alpha.description, "条目 A");
        let beta = idx.resolve("贝塔").unwrap();
        assert_eq!(beta.description, "条目 B");
        assert!(idx.resolve("不存在").is_none());

        // body：剥掉 frontmatter 区，不含分隔线；frontmatter 首行 `---` 也已剥离
        let body = idx.body("阿尔法").unwrap();
        assert!(!body.contains("---"), "body 不应含 frontmatter 分隔线：{body}");
        assert_eq!(idx.body("不存在"), None);

        // catalog_text 按 name 排序（贝塔 < 阿尔法，与文件名顺序相反）且行格式正确
        assert_eq!(
            idx.catalog_text(),
            "- 贝塔：条目 B\n- 阿尔法：条目 A"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_files_are_skipped() {
        let root = unique_temp_dir("skip");
        fixture(&root);
        let idx = SkillIndex::load(&root).unwrap();

        // 无 frontmatter、无 name、_index.md、重名均不入索引
        assert_eq!(idx.names(), vec!["贝塔", "阿尔法"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn duplicate_name_keeps_first_registered() {
        let root = unique_temp_dir("dup");
        fixture(&root);
        let idx = SkillIndex::load(&root).unwrap();

        // 每层按文件名字母序处理：alpha.md 先于 zzz-dup.md 注册，
        // 重名的「贝塔」保留 alpha.md 的 meta 与正文
        let beta = idx.resolve("贝塔").unwrap();
        assert_eq!(beta.description, "条目 B");
        assert_eq!(idx.body("贝塔"), Some("## 正文 B"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_nonexistent_dir_returns_err() {
        let err = SkillIndex::load(Path::new("Z:/definitely/not/a/dir/xyz")).unwrap_err();
        assert!(!err.is_empty(), "错误信息应为描述性信息");
    }

    #[test]
    fn empty_index_has_no_catalog_or_names() {
        let idx = SkillIndex::empty();
        assert_eq!(idx.catalog_text(), "");
        assert!(idx.names().is_empty());
        assert!(idx.resolve("任意 Skill").is_none());
        assert!(idx.body("任意 Skill").is_none());
    }

    #[test]
    fn skills_dir_is_data_dir_runtime_root() {
        // 运行根 = data_dir()/skills（dev 与打包同款；纯路径解析，不建目录）
        assert_eq!(skills_dir(), crate::config::data_dir().join("skills"));
    }

    #[test]
    fn bundled_skills_dir_returns_dev_path() {
        // dev 源路径随源码分发，数值计算 为已落盘条目（包形态）：强断言路径真实可用
        let dir = bundled_skills_dir();
        assert!(
            dir.join("数值计算").join("SKILL.md").is_file(),
            "dev 源应存在（包形态）：{}",
            dir.display()
        );
    }

    // ── sync_bundled_skills ──

    /// 递归收集目录下全部文件（相对路径串；排序后返回，集合语义）。
    fn collect_files(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut queue: std::collections::VecDeque<PathBuf> =
            std::collections::VecDeque::from([root.to_path_buf()]);
        while let Some(d) = queue.pop_front() {
            let mut entries: Vec<_> = std::fs::read_dir(&d)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let p = entry.path();
                if p.is_dir() {
                    queue.push_back(p);
                } else {
                    out.push(
                        p.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn sync_copies_all_files_including_nested() {
        let src = unique_temp_dir("sync-src");
        let dst = unique_temp_dir("sync-dst");
        std::fs::write(src.join("a.md"), "A").unwrap();
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub").join("b.md"), "B").unwrap();

        let r = sync_bundled_skills(&src, &dst).unwrap();
        assert_eq!(r.changed.len(), 2, "两个文件都应复制：{:?}", r.changed);
        assert_eq!(r.unchanged, 0);
        assert_eq!(r.changed[0], "a.md", "遍历按 file_name 排序，a.md 在前");
        assert!(r.changed[1].ends_with("b.md"));
        assert_eq!(std::fs::read_to_string(dst.join("a.md")).unwrap(), "A");
        assert_eq!(
            std::fs::read_to_string(dst.join("sub").join("b.md")).unwrap(),
            "B"
        );

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn sync_restores_modified_and_deleted_keeps_extras() {
        let src = unique_temp_dir("sync-src2");
        let dst = unique_temp_dir("sync-dst2");
        std::fs::write(src.join("a.md"), "A1").unwrap();
        std::fs::write(src.join("b.md"), "B1").unwrap();
        sync_bundled_skills(&src, &dst).unwrap();

        // 内嵌副本被改 / 被删 → 再同步（源为准）恢复
        std::fs::write(dst.join("a.md"), "MODIFIED").unwrap();
        std::fs::remove_file(dst.join("b.md")).unwrap();
        // 多余文件（用户技能 / 旧版遗留）→ 保留不动（只增不删）
        std::fs::write(dst.join("user-extra.md"), "EXTRA").unwrap();

        let r = sync_bundled_skills(&src, &dst).unwrap();
        assert_eq!(
            std::fs::read_to_string(dst.join("a.md")).unwrap(),
            "A1",
            "被改文件应恢复"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("b.md")).unwrap(),
            "B1",
            "被删文件应恢复"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("user-extra.md")).unwrap(),
            "EXTRA",
            "多余文件应保留"
        );
        assert_eq!(r.changed.len(), 2, "仅两个恢复写入：{:?}", r.changed);
        assert!(
            r.changed.iter().all(|s| !s.contains("extra")),
            "多余文件不应出现在 changed：{:?}",
            r.changed
        );

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn sync_idempotent_second_run_no_writes() {
        let src = unique_temp_dir("sync-src3");
        let dst = unique_temp_dir("sync-dst3");
        std::fs::write(src.join("a.md"), "A").unwrap();
        std::fs::write(src.join("b.md"), "B").unwrap();

        sync_bundled_skills(&src, &dst).unwrap();
        let r2 = sync_bundled_skills(&src, &dst).unwrap();
        assert!(r2.changed.is_empty(), "二次同步不应有写入：{:?}", r2.changed);
        assert_eq!(r2.unchanged, 2);

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn sync_bundled_source_byte_identical_and_catalog_equal() {
        // 验收：真实内置源 → 运行根副本 字节逐一一致；catalog 逐字一致
        let src = bundled_skills_dir();
        let dst = unique_temp_dir("sync-real");
        let r = sync_bundled_skills(&src, &dst).unwrap();

        let src_files = collect_files(&src);
        let mut changed = r.changed.clone();
        changed.sort();
        assert_eq!(changed, src_files, "首轮同步应复刻全部源文件");
        for rel in &src_files {
            let s = std::fs::read(src.join(rel)).unwrap();
            let d = std::fs::read(dst.join(rel)).unwrap();
            assert_eq!(s, d, "{rel} 字节应一致");
        }
        for folder in ["数值计算", "行动推演", "轴", "集成战略"] {
            assert!(
                src_files
                    .iter()
                    .any(|f| f.starts_with(folder) && f.ends_with("SKILL.md")),
                "源应含 {folder}/SKILL.md"
            );
        }

        // catalog（system prompt 内容）与源侧逐字一致
        let src_idx = SkillIndex::load(&src).unwrap();
        let dst_idx = SkillIndex::load(&dst).unwrap();
        assert_eq!(src_idx.catalog_text(), dst_idx.catalog_text());

        // 注意：src 为真实内置源，绝不清理；仅清理临时副本
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn sync_missing_source_returns_err() {
        let dst = unique_temp_dir("sync-nosrc");
        let err = sync_bundled_skills(Path::new("Z:/definitely/not/a/dir/xyz"), &dst)
            .unwrap_err();
        assert!(!err.is_empty(), "错误信息应为描述性信息");
        // 源缺失时不产生部分效果（目标目录保持空）
        assert_eq!(std::fs::read_dir(&dst).unwrap().count(), 0);

        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn package_dir_is_single_unit_and_not_descended() {
        // 技能包目录（根级 SKILL.md）：作为一个单元；其 references 下 .md
        // 即使带合法 frontmatter 也不产生独立条目
        let root = unique_temp_dir("pkg");
        std::fs::create_dir_all(root.join("黑话小抄").join("references")).unwrap();
        std::fs::write(
            root.join("黑话小抄").join("SKILL.md"),
            "---\nname: 黑话小抄\ndescription: 术语对照\n---\n\n## 正文",
        )
        .unwrap();
        std::fs::write(
            root.join("黑话小抄").join("references").join("terms.md"),
            "---\nname: 不该出现\ndescription: x\n---\n\n## x",
        )
        .unwrap();
        // 平铺单文件照旧
        std::fs::write(
            root.join("loose.md"),
            "---\nname: 散装\ndescription: y\n---\n\n## y",
        )
        .unwrap();

        let idx = SkillIndex::load(&root).unwrap();
        assert_eq!(idx.names(), vec!["散装", "黑话小抄"]);
        assert!(idx.path("黑话小抄").unwrap().ends_with("SKILL.md"));
        assert!(idx.resolve("不该出现").is_none(), "包内资源不应成为独立技能");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hidden_entries_skipped() {
        let root = unique_temp_dir("hidden");
        std::fs::write(
            root.join("ok.md"),
            "---\nname: 正常\ndescription: d\n---\n\n## 正文",
        )
        .unwrap();
        std::fs::write(
            root.join(".ghost.md"),
            "---\nname: 隐藏\ndescription: d\n---\n\n## 正文",
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".tmp-残留")).unwrap();
        std::fs::write(
            root.join(".tmp-残留").join("SKILL.md"),
            "---\nname: 残留\ndescription: d\n---\n\n## 正文",
        )
        .unwrap();

        let idx = SkillIndex::load(&root).unwrap();
        assert_eq!(idx.names(), vec!["正常"]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn catalog_pairs_sorted_with_desc() {
        let root = unique_temp_dir("pairs");
        fixture(&root);
        let idx = SkillIndex::load(&root).unwrap();
        assert_eq!(
            idx.catalog_pairs(),
            vec![
                ("贝塔".to_string(), "条目 B".to_string()),
                ("阿尔法".to_string(), "条目 A".to_string())
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sync_retires_stale_builtin_entries_only() {
        let src = unique_temp_dir("sync-ret-src");
        let dst = unique_temp_dir("sync-ret-dst");
        // 源：新形态包
        std::fs::create_dir_all(src.join("数值计算")).unwrap();
        std::fs::write(
            src.join("数值计算").join("SKILL.md"),
            "---\nname: 数值计算\ndescription: d\nsource: builtin\n---\n\n## x",
        )
        .unwrap();
        // 运行根：旧形态（内置，退役目标）/ 用户文件 / 用户包 / 隐藏残留（均保留）
        std::fs::write(
            dst.join("数值计算.md"),
            "---\nname: 数值计算\ndescription: d\nsource: builtin\n---\n\n## x",
        )
        .unwrap();
        std::fs::write(
            dst.join("用户.md"),
            "---\nname: 用户\ndescription: d\n---\n\n## x",
        )
        .unwrap();
        std::fs::create_dir_all(dst.join("用户包")).unwrap();
        std::fs::write(
            dst.join("用户包").join("SKILL.md"),
            "---\nname: 用户包\ndescription: d\n---\n\n## x",
        )
        .unwrap();
        std::fs::create_dir_all(dst.join(".tmp-残留")).unwrap();

        let r = sync_bundled_skills(&src, &dst).unwrap();
        assert!(!dst.join("数值计算.md").exists(), "旧形态内置应退役");
        assert!(dst.join("数值计算").join("SKILL.md").is_file(), "新形态已同步");
        assert!(dst.join("用户.md").exists(), "用户条目不动");
        assert!(dst.join("用户包").exists(), "用户包不动");
        assert!(dst.join(".tmp-残留").exists(), "隐藏残留不动");
        assert_eq!(r.removed, vec!["数值计算.md".to_string()]);

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&dst);
    }
}
