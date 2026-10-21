//! Tauri IPC 薄层 —— 前端 ←→ 后端命令适配
//! 各命令按职责拆到子模块；本文件只保留共享导入与 re-export。

mod agent;
mod compaction;
mod conversation;
mod files;
mod interaction;
mod memory;
mod prelude;
mod settings;
mod skills;
mod system;
mod workspace;

pub use agent::*;
pub use compaction::*;
pub use conversation::*;
pub use files::*;
pub use interaction::*;
pub use memory::*;
pub use settings::*;
pub use skills::*;
pub use system::*;
pub use workspace::*;

#[cfg(test)]
use compaction::refresh_frozen_snapshots;

#[cfg(test)]
use files::list_skills_from;

#[cfg(test)]
use memory::{experience_list_rows, validate_longterm};

#[cfg(test)]
use settings::{guard_config_write, CONFIG_WRITE_GUARD_MSG};

#[cfg(test)]
use workspace::{
    list_dir_entries, resolve_workspace_target, search_workspace_entries, WORKSPACE_SEARCH_LIMIT,
};

#[cfg(test)]
use prelude::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ApiType;

    #[test]
    fn read_license_returns_full_text_in_dev() {
        // 开发构建下应能读到仓库根的 LICENSE 全文（不是简短声明）
        let text = read_license().expect("开发构建的 LICENSE 应可读");
        assert!(text.contains("Apache License"), "应是 Apache 许可证");
        assert!(
            text.len() > 5000,
            "应是全文而非简短声明，实际 {} 字节",
            text.len()
        );
    }

    fn exp_entry(id: &str, ts: u64, title: &str) -> crate::memory::schema::ExperienceEntry {
        crate::memory::schema::ExperienceEntry {
            id: id.into(),
            ts,
            conversation_id: "c1".into(),
            line_from: String::new(),
            turn_from: 0,
            turn_to: 0,
            level: "short".into(),
            title: title.into(),
            summary: String::new(),
            tags: Vec::new(),
            revision_count: 0,
            absorbed_in: None,
        }
    }

    #[test]
    fn experience_list_orders_newest_first_within_batch() {
        // 批间新在前（ts 降序）；批内按创建序倒序（同一 ts 的末条在上）。
        let entries = vec![
            exp_entry("a", 100, "旧批"),
            exp_entry("b", 200, "批2-1"),
            exp_entry("c", 200, "批2-2"),
            exp_entry("d", 200, "批2-3"),
        ];
        let rows = experience_list_rows(&entries);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["d", "c", "b", "a"]);
    }

    #[test]
    fn validate_longterm_bounds() {
        let max = crate::orchestrator::memory::MAX_LONGTERM_CHARS;
        assert!(validate_longterm("").is_ok(), "空串允许（＝清空）");
        assert!(validate_longterm(&"字".repeat(max)).is_ok(), "上限内应放行");
        let err = validate_longterm(&"字".repeat(max + 1)).unwrap_err();
        assert!(err.contains("上限"), "超限应报错而不静默截断: {err}");
    }

    // ── list_skills：临时夹具目录（运行根语义；不再依赖 dev 源目录）──

    #[test]
    fn list_skills_from_returns_entries_with_description() {
        let root = std::env::temp_dir().join(format!(
            "prts-cmd-skills-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("calc.md"),
            "---\nname: 数值计算\ndescription: 计算规则\n---\n\n## 正文",
        )
        .unwrap();
        std::fs::write(
            root.join("tui.md"),
            "---\nname: 行动推演\ndescription: 推演规则\n---\n\n## 正文",
        )
        .unwrap();

        let skills = list_skills_from(&root).expect("夹具目录应可读");
        assert_eq!(skills.len(), 2);
        let calc = skills
            .iter()
            .find(|s| s.name == "数值计算")
            .expect("应含数值计算技能");
        assert_eq!(
            calc.description, "计算规则",
            "frontmatter description 应被解析"
        );
        assert!(calc.source.is_none(), "fixture 无 source → None（用户态）");
        // file_path：绝对路径且在目标目录内（打开链接用；文件名≠name）
        let fp = calc.file_path.as_ref().expect("应记录源文件路径");
        let p = std::path::Path::new(fp);
        assert!(p.is_absolute() && p.starts_with(&root));
        assert!(p.extension().and_then(|e| e.to_str()) == Some("md"));
        // 按 name 排序（SkillIndex.names 语义）
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "清单应按 name 排序");

        let _ = std::fs::remove_dir_all(&root);
    }

    // ── open_path 放行校验（纯函数；不触发真实 opener）──

    #[test]
    fn skill_entry_serializes_camel_case_file_path() {
        // 锁定 wire 契约：SkillEntry `#[serde(rename_all = "camelCase")]` →
        // 输出 filePath；前端 types.ts 按 filePath 读取（防字段契约断裂回归——
        // 曾因前端读 file_path 恒 undefined 致点击静默失效）
        let entry = SkillEntry {
            name: "数值计算".into(),
            description: "d".into(),
            file_path: Some("F:/skills/数值计算.md".into()),
            source: Some("builtin".into()),
        };
        let v = serde_json::to_value(&entry).unwrap();
        assert!(
            v.get("filePath").is_some(),
            "应序列化为 camelCase filePath，got: {v}"
        );
        assert_eq!(
            v.get("filePath").and_then(|x| x.as_str()),
            Some("F:/skills/数值计算.md")
        );
        assert!(
            v.get("file_path").is_none(),
            "不应输出 snake_case file_path"
        );
        assert!(v.get("name").is_some());
        assert!(v.get("description").is_some());
        assert_eq!(
            v.get("source").and_then(|x| x.as_str()),
            Some("builtin"),
            "source 应原样透出（前端据此显示来源与删除门控）"
        );
    }

    #[test]
    fn open_target_accepts_existing_file_rejects_others() {
        // 绝对路径 + 存在（任意位置，白名单已放开）→ 放行
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let f = std::env::temp_dir().join(format!("prts-open-{nanos}.md"));
        std::fs::write(&f, "x").unwrap();
        assert!(is_openable_target(&f));
        // 技能源文件（真实存在，包形态）→ 同样放行（不设白名单）
        let skill_md = crate::skills_index::bundled_skills_dir()
            .join("数值计算")
            .join("SKILL.md");
        assert!(is_openable_target(&skill_md));
        // 相对路径 → 拒绝
        assert!(!is_openable_target(std::path::Path::new("数值计算.md")));
        // 不存在的绝对路径 → 拒绝
        assert!(!is_openable_target(std::path::Path::new(
            "C:/definitely/not/exist-prts-xyz.md"
        )));
        // 目录（非文件）→ 拒绝
        assert!(!is_openable_target(&std::env::temp_dir()));
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn agent_request_deserializes_camelcase() {
        let json = r#"{
            "input": "你好",
            "history": [
                { "role": "user", "content": "之前的问题" },
                { "role": "assistant", "content": "之前的回答" }
            ],
            "sessionId": "abc-123"
        }"#;

        let req: AgentRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.input, "你好");
        assert_eq!(req.session_id, "abc-123");
        assert_eq!(req.history.len(), 2);
        assert_eq!(req.history[0].role, "user");
        assert_eq!(req.history[1].role, "assistant");
    }

    #[test]
    fn agent_request_deserializes_with_error_messages() {
        // error 消息不做过滤——那是 orchestrator 的职责
        let json = r#"{
            "input": "重试",
            "history": [
                { "role": "user", "content": "你好" },
                { "role": "error", "content": "超时了" },
                { "role": "user", "content": "重试" }
            ],
            "sessionId": "xyz-456"
        }"#;

        let req: AgentRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.history.len(), 3);
        assert_eq!(req.history[1].role, "error");
    }

    #[test]
    fn agent_request_empty_history() {
        let json = r#"{
            "input": "第一句话",
            "history": [],
            "sessionId": "new-001"
        }"#;

        let req: AgentRequest = serde_json::from_str(json).unwrap();
        assert!(req.history.is_empty());
        assert_eq!(req.input, "第一句话");
    }

    #[test]
    fn agent_request_with_conversation_id() {
        // 新前端：携带 conversationId（缓存区按对话分区的维度）
        let json = r#"{
            "input": "你好",
            "history": [],
            "sessionId": "abc-123",
            "conversationId": "conv-7"
        }"#;

        let req: AgentRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.conversation_id, "conv-7");
        assert_eq!(req.session_id, "abc-123");
    }

    #[test]
    fn agent_request_without_conversation_id_defaults_empty() {
        // 旧前端：不传 conversationId → serde default 空串（命令层兜底为 sessionId）
        let json = r#"{
            "input": "你好",
            "history": [],
            "sessionId": "abc-123"
        }"#;

        let req: AgentRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.conversation_id, "");
    }

    // ── verify_connection 空 base_url ──

    #[tokio::test]
    async fn verify_connection_empty_base_url_rejected() {
        // provider 为空 + base_url 为 None → 无映射可用，应返回错误
        let config = LlmConfig {
            provider: String::new(),
            api_type: ApiType::OpenAiCompletions,
            api_key: String::new(),
            base_url: None,
            headers: None,
            model: String::new(),
            max_tokens: 4096,
            thinking_level: None,
        };
        let result = verify_connection(config).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Base URL"));
    }

    // ── M3-B 设置写闸 ──

    #[test]
    fn config_write_guard_allows_when_idle_blocks_when_active() {
        let reg = TurnRegistry::new();
        // 空闲 → Ok
        assert!(guard_config_write(&reg).is_ok());

        // 有活动 turn → Err（文案为命令层同款中文）
        let signal = reg.register("s1", "conv-1");
        assert_eq!(
            guard_config_write(&reg).unwrap_err(),
            CONFIG_WRITE_GUARD_MSG
        );

        // abort 不释放 busy：快照仍在，直到 process 返回后 unregister
        assert!(reg.abort("s1"));
        assert!(
            guard_config_write(&reg).is_err(),
            "中止后仍 busy（轮未收尾）"
        );

        reg.unregister("s1");
        assert!(guard_config_write(&reg).is_ok(), "注销（轮收尾）后放行");
        let _ = signal;
    }

    #[test]
    fn config_write_guard_is_session_scope_safe() {
        // busy 按全局口径（任一对话处理中即拦截）——多会话注册任一即拦
        let reg = TurnRegistry::new();
        reg.register("s1", "c1");
        assert!(guard_config_write(&reg).is_err());
        reg.unregister("s1");
        assert!(guard_config_write(&reg).is_ok());
    }

    // ── 空间页 · 工作区查看（树 / 搜索） ──

    fn space_root(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("prts-space-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn list_dir_entries_dirs_first_then_name() {
        let root = space_root("list");
        std::fs::create_dir_all(root.join("zeta")).unwrap();
        std::fs::create_dir_all(root.join("Alpha")).unwrap();
        std::fs::write(root.join("b.txt"), "x").unwrap();
        std::fs::write(root.join("a.txt"), "x").unwrap();

        let entries = list_dir_entries(&root).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Alpha", "zeta", "a.txt", "b.txt"],
            "目录在前，各自按名"
        );
        assert!(entries[0].is_dir, "目录标记");
        assert!(!entries[2].is_dir, "文件标记");
        // 路径为拼接形式（非 canonical，不带 \\?\ 前缀）——直接回传给前端用
        assert!(entries[0].path.ends_with("Alpha"));
        assert!(!entries[0].path.contains("\\\\?\\"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_workspace_target_guards_escape() {
        let root = space_root("guard");
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        assert!(
            resolve_workspace_target(&root, Some(&sub.to_string_lossy())).is_ok(),
            "根内子目录放行"
        );
        assert_eq!(
            resolve_workspace_target(&root, None).unwrap(),
            root,
            "省略 = 根自身"
        );
        assert_eq!(
            resolve_workspace_target(&root, Some("  ")).unwrap(),
            root,
            "空白项等同省略"
        );
        assert!(
            resolve_workspace_target(&root, Some("sub")).is_err(),
            "相对路径拒绝"
        );
        let outside = std::env::temp_dir();
        assert!(
            resolve_workspace_target(&root, Some(&outside.to_string_lossy())).is_err(),
            "根外目录拒绝"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn search_skips_hidden_and_dependency_dirs() {
        let root = space_root("search");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("notes/崖.md"), "x").unwrap();
        std::fs::create_dir_all(root.join("node_modules/hit")).unwrap();
        std::fs::write(root.join("node_modules/hit/崖.js"), "x").unwrap();
        std::fs::create_dir_all(root.join(".git/hit")).unwrap();
        std::fs::write(root.join(".git/hit/崖"), "x").unwrap();
        std::fs::write(root.join(".hidden崖.md"), "x").unwrap();

        let hits = search_workspace_entries(&root, "崖");
        let paths: Vec<&str> = hits.iter().map(|h| h.relative_path.as_str()).collect();
        assert_eq!(paths, vec!["notes/崖.md"], "命中只来自可见目录");

        // 嵌套命中的展开链：根下一级 → 命中所在目录（绝对路径，前端不拼路径）
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        std::fs::write(root.join("a/b/c/崖深处.md"), "x").unwrap();
        let deep = search_workspace_entries(&root, "崖深处");
        assert_eq!(deep.len(), 1);
        assert_eq!(deep[0].relative_path, "a/b/c/崖深处.md");
        assert_eq!(deep[0].parent_subdir, "a/b/c");
        assert_eq!(deep[0].ancestor_dirs.len(), 3, "a → b → c 三层展开链");
        assert!(deep[0].ancestor_dirs[0].ends_with("a"));
        assert!(deep[0].ancestor_dirs[2].ends_with("c"));

        assert!(
            search_workspace_entries(&root, "  ").is_empty(),
            "空查询不搜"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn search_caps_results_and_matches_case_insensitively() {
        let root = space_root("cap");
        std::fs::create_dir_all(root.join("d")).unwrap();
        for i in 0..100 {
            std::fs::write(root.join(format!("d/Note{i}.md")), "x").unwrap();
        }

        let hits = search_workspace_entries(&root, "note");
        assert_eq!(hits.len(), WORKSPACE_SEARCH_LIMIT, "结果上限 80");
        assert_eq!(hits[0].parent_subdir, "d", "父目录相对根");
        assert!(!hits[0].is_dir);
        assert!(
            hits[0].path.ends_with("d") || hits[0].path.contains("d"),
            "命中带绝对路径（供树定位）"
        );
        assert_eq!(hits[0].ancestor_dirs.len(), 1, "展开链：命中所在目录");

        let dir_hits = search_workspace_entries(&root, "d");
        let root_dir_hit = dir_hits
            .iter()
            .find(|h| h.is_dir && h.relative_path == "d")
            .expect("根级目录命中");
        assert!(
            root_dir_hit.parent_subdir.is_empty(),
            "根级 parentSubdir 为空串"
        );
        assert!(root_dir_hit.ancestor_dirs.is_empty(), "根级命中无祖先目录");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── 压缩后快照刷新（批 4 / 批 5：压缩并更新）──

    #[test]
    fn refresh_frozen_snapshots_refreshes_all_stores() {
        let data = space_root("snap-refresh");
        let sessions = data.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let memory = Arc::new(Mutex::new(MemoryStore::load(&data).unwrap()));
        let snapshots = Arc::new(Mutex::new(SnapshotStore::new(&sessions)));
        let skills = Arc::new(Mutex::new(SkillSnapshotStore::new(&sessions)));
        let prefix = Arc::new(Mutex::new(PrefixSnapshotStore::new(&sessions)));

        // 冻结初始态：记忆为空块、技能目录 A
        {
            let mem = memory.lock().unwrap();
            snapshots.lock().unwrap().get_or_assemble(Some("c1"), &mem);
        }
        skills
            .lock()
            .unwrap()
            .get_or_freeze(Some("c1"), Some("目录 A"));
        prefix
            .lock()
            .unwrap()
            .get_or_freeze(Some("c1"), "系统 A", &[]);

        // 产生差异：记忆写入 + 技能目录 B
        {
            let mut mem = memory.lock().unwrap();
            mem.growth_update(PROFILE_NAME, Some("画像"), None, None, SOURCE_USER)
                .unwrap();
        }
        refresh_frozen_snapshots(
            "c1",
            Some("目录 B"),
            &[],
            &memory,
            &snapshots,
            &skills,
            &prefix,
        );

        assert_eq!(
            std::fs::read_to_string(sessions.join("c1.skills.txt")).unwrap(),
            "目录 B",
            "技能快照应刷为新目录"
        );
        assert!(
            std::fs::read_to_string(sessions.join("c1.memory.txt"))
                .unwrap()
                .contains("画像"),
            "记忆快照应含新写入"
        );

        // 完整前缀也应刷为新目录（含新记忆）
        let prefix_json = std::fs::read_to_string(sessions.join("c1.prefix.json")).unwrap();
        assert!(
            prefix_json.contains("目录 B"),
            "前缀快照应含新目录：{prefix_json}"
        );
        assert!(prefix_json.contains("画像"), "前缀快照应含新记忆");

        // 活体不可读 → 跳过（保持旧值）
        refresh_frozen_snapshots("c1", None, &[], &memory, &snapshots, &skills, &prefix);
        assert_eq!(
            std::fs::read_to_string(sessions.join("c1.skills.txt")).unwrap(),
            "目录 B",
            "不可读时保持"
        );

        let _ = std::fs::remove_dir_all(&data);
    }
}
