//! skills_index —— Skill 条目索引（turn 级快照）
//!
//! 每 turn 入口扫描 skills 目录，正文全量读入内存；激活时纯内存操作，
//! 无 IO 失败面（读文件失败只可能发生在扫描期，一次性暴露）。
//! 结构仿 knowledge::index::KnowledgeIndex：递归扫描、逐层按 file_name()
//! 排序保证遍历顺序确定；frontmatter 解析复用 skills::parse_skill_frontmatter，
//! 正文剥离复用 knowledge::body_after_frontmatter（判定规则与解析器一致）。
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
            let rd = std::fs::read_dir(&d)
                .map_err(|e| format!("读取 Skill 目录失败（{}）：{e}", d.display()))?;
            let mut dir_entries: Vec<_> = rd
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("读取 Skill 目录失败（{}）：{e}", d.display()))?;
            dir_entries.sort_by_key(|e| e.file_name());
            for entry in dir_entries {
                let path = entry.path();

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

                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("读取 Skill 文件失败（{}）：{e}", path.display()))?;

                let Some(meta) = crate::skills::parse_skill_frontmatter(&text) else {
                    // 无 frontmatter 或 name 为空均落此分支（解析器已保证
                    // name 非空才返回 Some，此处无需二次检查）
                    eprintln!("[skills] 跳过 frontmatter 无效的 Skill：{}", path.display());
                    continue;
                };
                if skills.contains_key(&meta.name) {
                    eprintln!(
                        "[skills] 重名 Skill 已忽略（保留先注册的）：{}",
                        path.display()
                    );
                    continue;
                }
                // 正文全量读入内存（激活时纯内存操作，无 IO 失败面）。
                // 剥离复用 knowledge::body_after_frontmatter：判定规则与
                // parse_frontmatter 一致（LF/CRLF 兼容），勿重复实现。
                let name = meta.name.clone();
                let body = crate::knowledge::body_after_frontmatter(&text).to_string();
                skills.insert(name.clone(), meta);
                bodies.insert(name.clone(), body);
                paths.insert(name, path);
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
}

/// Skill 目录解析。
///
/// 优先级（模式照 knowledge::index::knowledge_dir）：
/// 1. dev 路径 `CARGO_MANIFEST_DIR/src/skills`（存在即返回）
/// 2. 打包后可执行文件同级 `resources/skills`（存在即返回）
/// 3. 否则返回 dev 路径兜底
pub fn skills_dir() -> PathBuf {
    let dev_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/skills");
    if dev_path.is_dir() {
        return dev_path;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let prod_path = dir.join("resources/skills");
            if prod_path.is_dir() {
                return prod_path;
            }
        }
    }
    dev_path
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
    fn skills_dir_returns_dev_path() {
        // dev 路径随源码分发，数值计算.md 是已落盘条目：强断言路径真实可用
        let dir = skills_dir();
        assert!(dir.join("数值计算.md").is_file(), "dev 路径应存在：{}", dir.display());
    }
}
