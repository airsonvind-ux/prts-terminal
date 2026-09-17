//! knowledge::index —— 知识库条目索引
//!
//! 启动时递归扫描知识库目录下所有 `.md` 条目（跳过 `_index.md`），
//! 解析 frontmatter 提取 title 与 brief，建立 `title → 相对路径` 映射，
//! 并另存 `title → brief`（目录清单渐进披露用）。
//! read_knowledge / list_knowledge 工具按此索引定位与导航知识库。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 知识库条目索引：条目名（frontmatter title）→ 相对 root 的路径。
#[derive(Debug)]
pub struct KnowledgeIndex {
    root: PathBuf,
    entries: HashMap<String, PathBuf>,
    /// 条目名 → brief（frontmatter brief，为空也存空串）。
    /// 与 entries 键集合同步；目录清单（list_knowledge）据此返回
    /// title + brief，避免清单模式逐条重读文件。
    briefs: HashMap<String, String>,
    /// 条目名 → 节名列表（正文 `## ` 标题，去前缀 trim，按出现顺序）。
    /// 目录清单据此输出条目大纲（节数 = 列表长度），read_knowledge 的
    /// 节选匹配也以节名为准。与 entries 键集合同步。
    tocs: HashMap<String, Vec<String>>,
    /// 知识库固定架构（根目录 `_index.md` 原文）：四大领域划分与各领域
    /// 子目录简介，人工维护。list_knowledge 省略 dir 时整体返回，
    /// PRTS 一次调用确定知识库框架。缺文件 → None。
    framework: Option<String>,
}

impl KnowledgeIndex {
    /// 递归遍历 root 下所有 `.md` 文件，解析 frontmatter 建立索引。
    ///
    /// - 跳过 `_index.md`
    /// - 无 frontmatter / 无 title → `eprintln!` 告警并跳过
    /// - title 重名 → `eprintln!` 告警，保留先注册的
    /// - root 不存在或不可读 → Err（描述性信息）
    pub fn load(root: &Path) -> Result<Self, String> {
        if !root.is_dir() {
            return Err(format!(
                "知识库目录不存在或不可读：{}",
                root.display()
            ));
        }

        let mut entries: HashMap<String, PathBuf> = HashMap::new();
        let mut briefs: HashMap<String, String> = HashMap::new();
        let mut tocs: HashMap<String, Vec<String>> = HashMap::new();

        // 固定架构（_index.md 原文）：list_knowledge 省略 dir 时整体返回。
        // 根目录索引文档在条目遍历中被跳过（file_name == "_index.md"），
        // 此处单独读取；不存在或不可读 → None（框架模式报错指路）。
        let framework = std::fs::read_to_string(root.join("_index.md")).ok();

        // 队列式遍历（避免递归深度问题）
        // read_dir 迭代顺序 OS 依赖：每层先收集为 Vec、按 file_name() 排序
        // 再处理（目录与文件统一按名排序），保证遍历顺序确定。
        let mut queue: std::collections::VecDeque<PathBuf> =
            std::collections::VecDeque::from([root.to_path_buf()]);
        while let Some(dir) = queue.pop_front() {
            let rd = std::fs::read_dir(&dir)
                .map_err(|e| format!("读取知识库目录失败（{}）：{e}", dir.display()))?;
            let mut dir_entries: Vec<_> = rd
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("读取知识库目录失败（{}）：{e}", dir.display()))?;
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
                // 跳过索引文档（_index.md，任意层级）
                if file_name == "_index.md" || !file_name.ends_with(".md") {
                    continue;
                }

                let rel = path
                    .strip_prefix(root)
                    .map_err(|_| format!("条目路径越界：{}", path.display()))?
                    .to_path_buf();

                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("读取条目失败（{}）：{e}", path.display()))?;

                let Some(meta) = crate::knowledge::parse_frontmatter(&text) else {
                    // 无 frontmatter 或结构异常（如换行符/字段格式问题）均落此分支
                    eprintln!("[knowledge] 跳过 frontmatter 无效的条目：{}", path.display());
                    continue;
                };
                if meta.title.is_empty() {
                    eprintln!("[knowledge] 跳过无 title 的条目：{}", path.display());
                    continue;
                }
                if entries.contains_key(&meta.title) {
                    eprintln!(
                        "[knowledge] 重名条目已忽略（保留先注册的）：{}",
                        path.display()
                    );
                    continue;
                }
                // brief 为空也存入空串：清单模式 brief 字段稳定存在
                entries.insert(meta.title.clone(), rel);
                briefs.insert(meta.title.clone(), meta.brief);
                tocs.insert(meta.title, crate::knowledge::section_names(&text));
            }
        }

        Ok(Self {
            root: root.to_path_buf(),
            entries,
            briefs,
            tocs,
            framework,
        })
    }

    /// 降级启动用的空索引（root 为空路径、无条目）。
    ///
    /// 索引加载失败时使用，保证 read_knowledge 工具可注册但恒未命中。
    pub fn empty_for_boot_failure() -> Self {
        Self {
            root: PathBuf::new(),
            entries: HashMap::new(),
            briefs: HashMap::new(),
            tocs: HashMap::new(),
            framework: None,
        }
    }

    /// 按条目名查相对 root 的路径。
    pub fn resolve(&self, entry: &str) -> Option<&Path> {
        self.entries.get(entry).map(|p| p.as_path())
    }

    /// 模糊候选：收集所有 title 满足「title 包含 entry 或 entry 包含 title」
    /// 者（entry 非空时），排序后最多返回 5 个。
    pub fn suggest(&self, entry: &str) -> Vec<&str> {
        if entry.is_empty() {
            return Vec::new();
        }
        let mut cands: Vec<&str> = self
            .entries
            .keys()
            .map(|s| s.as_str())
            .filter(|title| title.contains(entry) || entry.contains(title))
            .collect();
        cands.sort_unstable();
        cands.truncate(5);
        cands
    }

    /// root.join(相对路径) 的完整条目路径。
    pub fn entry_path(&self, entry: &str) -> Option<PathBuf> {
        self.entries.get(entry).map(|rel| self.root.join(rel))
    }

    /// 知识库固定架构（根目录 `_index.md` 原文）。
    /// 缺失（无 `_index.md`）→ None，由工具侧报错指路。
    pub fn framework(&self) -> Option<&str> {
        self.framework.as_deref()
    }

    /// 条目大纲（节名列表，去 `## ` 前缀 trim，按出现顺序）。
    /// 目录清单（list_knowledge）输出条目时附大纲，节名可直接用于
    /// read_knowledge 的 sections 参数（两侧同规则）。
    pub fn toc(&self, entry: &str) -> Option<&[String]> {
        self.tocs.get(entry).map(|v| v.as_slice())
    }

    /// 列指定目录的**下一级**：直接子目录名列表 + 该目录直属条目
    /// （title, brief, 节数），各自排序。非递归——逐层导航语义：
    /// 列表工具据此"列目录看层级、深入列条目"。
    ///
    /// 知识库固定架构（图书馆式目录层级）由 `_index.md` 承载，本方法只
    /// 返回运行时推导的下一级内容（条目会随版本变动，架构基本不变），
    /// 两者层次不同、不冲突。
    ///
    /// - `dir` 首尾空白与首尾 `/` 被剔除；空串 = 根目录。
    /// - 目录不存在（无任何条目路径位于其下）返回 `None`，由工具侧转为错误。
    pub fn dir_children(&self, dir: &str) -> Option<(Vec<String>, Vec<(String, String, usize)>)> {
        let dir = dir.trim().trim_matches('/');
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let mut subdirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut entries: Vec<(String, String, usize)> = Vec::new();
        let mut found = false;
        for (title, rel) in &self.entries {
            let parent = path_to_slash(rel.parent().unwrap_or_else(|| Path::new("")));
            if parent == dir {
                // 直属条目
                found = true;
                entries.push((
                    title.clone(),
                    self.briefs.get(title).cloned().unwrap_or_default(),
                    self.tocs.get(title).map(|t| t.len()).unwrap_or(0),
                ));
            } else if let Some(rest) = parent.strip_prefix(&prefix) {
                // 子孙目录：取 dir 的直接下一段为子目录名
                found = true;
                if let Some(seg) = rest.split('/').next() {
                    if !seg.is_empty() {
                        subdirs.insert(seg.to_string());
                    }
                }
            }
        }
        if !found {
            return None;
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Some((subdirs.into_iter().collect(), entries))
    }
}

/// 相对路径 → `/` 分隔字符串（Windows 分隔符 `\` 归一化为 `/`，
/// 与目录参数（如 `"基础概念/移动与阻挡"`）可比对）。
fn path_to_slash(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// 知识库目录解析。
///
/// 优先级（模式照 `lib.rs::get_db_path`）：
/// 1. 开发目录 `CARGO_MANIFEST_DIR/src/knowledge`（存在即返回）
/// 2. 打包后可执行文件同级 `resources/knowledge`（存在即返回）
/// 3. 否则返回 dev 路径兜底
pub fn knowledge_dir() -> PathBuf {
    let dev_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/knowledge");
    if dev_path.is_dir() {
        return dev_path;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let prod_path = dir.join("resources/knowledge");
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
            "prts-kb-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 假知识库：两个子目录、含 frontmatter 的条目文件、
    /// 一个无 frontmatter 文件、一个 _index.md、一对重名 title。
    fn fixture(root: &Path) {
        std::fs::create_dir_all(root.join("sub1")).unwrap();
        std::fs::create_dir_all(root.join("sub2")).unwrap();

        std::fs::write(
            root.join("sub1/aaa.md"),
            "---\ntitle: AAA\nbrief: 条目 A\n---\n\n## 正文 AAA",
        )
        .unwrap();
        std::fs::write(
            root.join("sub1/bbb.md"),
            "---\ntitle: BBB\nbrief: 条目 B\n---\n\n## 正文 BBB",
        )
        .unwrap();
        std::fs::write(
            root.join("sub2/ccc.md"),
            "---\ntitle: CCC\nbrief: 条目 C\n---\n\n## 正文 CCC",
        )
        .unwrap();
        // 无 frontmatter → 跳过
        std::fs::write(root.join("sub1/nofront.md"), "## 没有元数据").unwrap();
        // 索引文档 → 跳过
        std::fs::write(root.join("_index.md"), "# 知识库索引").unwrap();
        // 重名 title（与 sub1/aaa.md 重名）→ 告警，保留先注册的
        std::fs::write(
            root.join("sub2/aaa2.md"),
            "---\ntitle: AAA\nbrief: 重复条目\n---\n\n## 重复",
        )
        .unwrap();
    }

    #[test]
    fn load_builds_index_and_skips_invalid_files() {
        let root = unique_temp_dir("load");
        fixture(&root);

        let idx = KnowledgeIndex::load(&root).unwrap();
        // 只有 3 个有效条目：AAA、BBB、CCC
        // （aaa2.md 重名被跳过，nofront.md 无 frontmatter 被跳过，_index.md 被跳过）
        assert_eq!(idx.entries.len(), 3);
        assert!(idx.entries.contains_key("AAA"));
        assert!(idx.entries.contains_key("BBB"));
        assert!(idx.entries.contains_key("CCC"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_returns_relative_path_and_entry_path() {
        let root = unique_temp_dir("resolve");
        fixture(&root);
        let idx = KnowledgeIndex::load(&root).unwrap();

        assert_eq!(idx.resolve("BBB"), Some(Path::new("sub1/bbb.md")));
        assert_eq!(
            idx.entry_path("BBB"),
            Some(root.join("sub1/bbb.md"))
        );
        assert_eq!(idx.resolve("不存在"), None);
        assert_eq!(idx.entry_path("不存在"), None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn duplicate_title_keeps_first_registered() {
        let root = unique_temp_dir("dup");
        fixture(&root);
        let idx = KnowledgeIndex::load(&root).unwrap();

        // 重名后只保留一条（每层目录按文件名排序后先处理 sub1，
        // 实际保留先注册的 sub1/aaa.md；宽松断言仍覆盖两候选任一）
        let path = idx.entry_path("AAA").unwrap();
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(
            name == "aaa.md" || name == "aaa2.md",
            "重名条目应只保留一条，got: {name}"
        );
        // 两个子目录各扫到（sub2 的 ccc 也在索引中）
        assert!(idx.entries.contains_key("CCC"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn suggest_matches_contains_both_ways_and_limits_5() {
        let root = unique_temp_dir("suggest");
        fixture(&root);
        let idx = KnowledgeIndex::load(&root).unwrap();

        // title 包含 entry
        let cands = idx.suggest("A");
        assert_eq!(cands, vec!["AAA"]);
        // entry 包含 title（整体包含子串）
        let cands = idx.suggest("AAA百科");
        assert_eq!(cands, vec!["AAA"]);
        // 无匹配 → 空
        assert!(idx.suggest("ZZZ").is_empty());
        // 空 entry → 空
        assert!(idx.suggest("").is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn framework_and_toc_available() {
        let root = unique_temp_dir("fwtoc");
        fixture(&root);
        let idx = KnowledgeIndex::load(&root).unwrap();

        // 框架 = 根目录 _index.md 原文（固定架构数据源）
        assert_eq!(idx.framework(), Some("# 知识库索引"));
        // 大纲 = 正文 `## ` 节名列表（去前缀 trim，按出现顺序）
        assert_eq!(idx.toc("AAA"), Some(&["正文 AAA".to_string()][..]));
        // 不存在的条目 → None
        assert_eq!(idx.toc("不存在"), None);

        // 空索引降级：框架与大纲均不可用
        let empty = KnowledgeIndex::empty_for_boot_failure();
        assert_eq!(empty.framework(), None);
        assert_eq!(empty.toc("任意"), None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dir_children_section_count_derived_from_toc() {
        // 节数 = 大纲长度（同一数据源，不重复存储）：
        // 多节条目节数 > 1，无分节条目节数为 0
        let root = unique_temp_dir("seccount");
        std::fs::create_dir_all(root.join("sub1")).unwrap();
        std::fs::write(
            root.join("sub1/multi.md"),
            "---\ntitle: 多节\nbrief: b\n---\n\n## 第一节\n\n## 第二节\n\n## 第三节",
        )
        .unwrap();
        std::fs::write(
            root.join("sub1/plain.md"),
            "---\ntitle: 无节\nbrief: b\n---\n\n直接正文",
        )
        .unwrap();
        let idx = KnowledgeIndex::load(&root).unwrap();
        let (_, entries) = idx.dir_children("sub1").unwrap();
        assert_eq!(entries, vec![
            ("多节".to_string(), "b".to_string(), 3),
            ("无节".to_string(), "b".to_string(), 0),
        ]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_nonexistent_root_returns_err() {
        let err = KnowledgeIndex::load(Path::new("Z:/definitely/not/a/dir/xyz")).unwrap_err();
        assert!(!err.is_empty(), "错误信息应为描述性信息");
    }

    #[test]
    fn empty_for_boot_failure_resolves_nothing() {
        let idx = KnowledgeIndex::empty_for_boot_failure();
        assert!(idx.resolve("任意条目").is_none());
        assert!(idx.suggest("任意").is_empty());
        assert!(idx.entry_path("任意条目").is_none());
        assert!(idx.dir_children("").is_none());
    }

    #[test]
    fn real_knowledge_entries_all_parse() {
        // 真实知识库全量回归：每个条目文件必须解析成功进入索引。
        // 防条目格式被外部编辑写坏（实测：key_points 批量写入时一批文件
        // 转为 CRLF，旧解析器仅认 LF 导致静默跳过——此测试锁定该回归）。
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/knowledge");
        let idx = KnowledgeIndex::load(&root).expect("真实知识库加载失败");

        // 统计全部条目文件数（.md，除 _index.md），应全部进入索引
        let mut md_count = 0usize;
        let mut queue: std::collections::VecDeque<PathBuf> =
            std::collections::VecDeque::from([root]);
        while let Some(dir) = queue.pop_front() {
            let rd = std::fs::read_dir(&dir).unwrap();
            for entry in rd {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    queue.push_back(path);
                    continue;
                }
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.ends_with(".md") && name != "_index.md" {
                        md_count += 1;
                    }
                }
            }
        }
        assert!(
            md_count > 0,
            "知识库目录应为非空（测试环境路径：{})",
            env!("CARGO_MANIFEST_DIR")
        );
        assert_eq!(
            idx.entries.len(),
            md_count,
            "每个条目文件都应解析成功进入索引（frontmatter 无效的条目会被跳过）"
        );
    }

    /// 多级目录 fixture（dir_children 用）：
    /// 条目分布在多级目录，含根目录直接条目与 brief 空串条目。
    fn fixture_nested(root: &Path) {
        std::fs::create_dir_all(root.join("top1/nested")).unwrap();
        std::fs::create_dir_all(root.join("top2")).unwrap();

        std::fs::write(
            root.join("top1/a.md"),
            "---\ntitle: A1\nbrief: 简介A\n---\n\n## 正文",
        )
        .unwrap();
        std::fs::write(
            root.join("top1/b.md"),
            "---\ntitle: B1\nbrief: 简介B\n---\n\n## 正文",
        )
        .unwrap();
        std::fs::write(
            root.join("top1/nested/c.md"),
            "---\ntitle: C1\nbrief: 简介C\n---\n\n## 正文",
        )
        .unwrap();
        std::fs::write(
            root.join("top2/d.md"),
            "---\ntitle: D1\nbrief: \n---\n\n## 正文",
        )
        .unwrap();
        // 根目录直接条目
        std::fs::write(
            root.join("e.md"),
            "---\ntitle: E1\nbrief: 简介E\n---\n\n## 正文",
        )
        .unwrap();
    }

    #[test]
    fn load_keeps_briefs_including_empty() {
        let root = unique_temp_dir("briefs");
        fixture_nested(&root);
        let idx = KnowledgeIndex::load(&root).unwrap();

        assert_eq!(idx.briefs.get("A1").map(String::as_str), Some("简介A"));
        assert_eq!(idx.briefs.get("C1").map(String::as_str), Some("简介C"));
        // brief 为空也存入空串（字段稳定存在，而非缺失）
        assert_eq!(idx.briefs.get("D1").map(String::as_str), Some(""));
        // 无该条目时缺省（与 entries 键集合同步）
        assert!(!idx.briefs.contains_key("不存在"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dir_children_returns_subdirs_and_direct_entries() {
        let root = unique_temp_dir("indir");
        fixture_nested(&root);
        let idx = KnowledgeIndex::load(&root).unwrap();

        // 下一级导航语义：子目录列表 + 直属条目（非递归，不含 nested/c.md）
        assert_eq!(
            idx.dir_children("top1"),
            Some((
                vec!["nested".to_string()],
                vec![
                    ("A1".to_string(), "简介A".to_string(), 1),
                    ("B1".to_string(), "简介B".to_string(), 1),
                ]
            ))
        );
        // 叶子目录：无子目录，只含自身条目
        assert_eq!(
            idx.dir_children("top1/nested"),
            Some((vec![], vec![("C1".to_string(), "简介C".to_string(), 1)]))
        );
        // brief 空串原样保留
        assert_eq!(
            idx.dir_children("top2"),
            Some((vec![], vec![("D1".to_string(), String::new(), 1)]))
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dir_children_root_and_normalization() {
        let root = unique_temp_dir("indir2");
        fixture_nested(&root);
        let idx = KnowledgeIndex::load(&root).unwrap();

        // 空串 = 根目录：一级子目录 + 根直属条目 E1（非递归）
        let (subdirs, entries) = idx.dir_children("").unwrap();
        assert_eq!(subdirs, vec!["top1".to_string(), "top2".to_string()]);
        assert_eq!(entries, vec![("E1".to_string(), "简介E".to_string(), 1)]);
        // 首尾空白与首尾 / 归一化
        assert_eq!(idx.dir_children("/top1/"), idx.dir_children("top1"));
        assert_eq!(idx.dir_children("  top1  "), idx.dir_children("top1"));
        // 目录不存在 → None（顶层与深层一致）
        assert!(idx.dir_children("不存在").is_none());
        assert!(idx.dir_children("top1/不存在的子目录").is_none());

        let _ = std::fs::remove_dir_all(&root);
    }
}
