//! skills_snapshot —— 技能目录冻结快照（随对话档案落盘，跨重启逐字不变）
//!
//! ## 语义（批 2 · 目录冻结）
//!
//! sidecar 路径 = `sessions/{conversationId}.skills.txt`（与对话内容文件同目录，
//! 同生共死）。**首次组装即冻结**；此后同一对话一律读回原文——对话内目录
//! 保持稳定（system prompt 前缀缓存为硬约束）。刷新点＝新对话 ＋ 压缩
//! （[`SkillSnapshotStore::refresh`]：摘要成功后以活体重刷，有差才写）。
//!
//! - 存的是**目录文本**（`SkillIndex::catalog_text()` 原文）：冻结的是"模型
//!   看到的目录视图"；激活工具仍走活体索引（视图冻结、工具保持活）。
//! - **活体目录本次不可读（None）时不冻结**：返回空文本、不落盘，下轮重试
//!   （防瞬时 IO 失败被冻成永久空目录）。
//! - 无 / 非法 conversationId：只返回活体、不落盘（防垃圾快照，同记忆快照裁决）。
//!
//! ## 时序与孤儿
//!
//! 首轮 `agent_process` 先于前端轮末归档（`append_turn`），快照可能先于对话
//! 档案诞生，这是**有意**的；孤儿 sidecar 在 `delete_conversation` 侧显式
//! 清扫，启动时 `sweep_orphans` 兜底（与记忆快照同区）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::memory::store::atomic_write;
use crate::session_id::is_safe_session_id;

/// 技能目录冻结快照存储：内存缓存 + 盘上 sidecar。
pub struct SkillSnapshotStore {
    /// `sessions/` 目录
    dir: PathBuf,
    cache: HashMap<String, String>,
}

impl SkillSnapshotStore {
    /// 在 `sessions/` 目录上建立快照存储（目录不存在则创建）。
    pub fn new(sessions_dir: &Path) -> Self {
        std::fs::create_dir_all(sessions_dir).ok();
        Self {
            dir: sessions_dir.to_path_buf(),
            cache: HashMap::new(),
        }
    }

    fn sidecar_path(&self, conversation_id: &str) -> PathBuf {
        self.dir.join(format!("{conversation_id}.skills.txt"))
    }

    /// 取冻结目录文本：内存缓存 → sidecar → 以 `live_catalog` 冻结（并落盘）。
    ///
    /// - `live_catalog = None`（活体目录本次不可读）：**不冻结、不落盘**，
    ///   返回空文本（下轮重试；防瞬时失败被冻成永久空目录）。
    /// - `conversation_id` 为 None / 非法：只返回活体（不落盘、不缓存）。
    pub fn get_or_freeze(
        &mut self,
        conversation_id: Option<&str>,
        live_catalog: Option<&str>,
    ) -> String {
        let cid = match conversation_id {
            Some(c) if is_safe_session_id(c) => c,
            _ => return live_catalog.unwrap_or_default().to_string(),
        };
        if let Some(v) = self.cache.get(cid) {
            return v.clone();
        }
        let path = self.sidecar_path(cid);
        if let Ok(s) = std::fs::read_to_string(&path) {
            self.cache.insert(cid.to_string(), s.clone());
            return s;
        }
        let Some(live) = live_catalog else {
            // 活体不可读：本次不冻结（下轮重试）
            return String::new();
        };
        // 首见：冻结 + 落盘
        if let Err(e) = atomic_write(&path, live) {
            eprintln!("[skills] 目录快照落盘失败（{}）：{e}", path.display());
        }
        self.cache.insert(cid.to_string(), live.to_string());
        live.to_string()
    }

    /// 压缩后刷新（批 4 · 压缩并更新）：以最新活体重写快照，**有差才写**。
    ///
    /// - `live_catalog = None`（活体不可读）：跳过、保持旧快照（返回 false）；
    /// - 与当前值（缓存 → sidecar）一致：不写、不触碰（返回 false）；
    /// - 有差异 / 尚未冻结：写盘 + 更新缓存（返回 true）；
    /// - 非法 id：不落盘（返回 false）。
    ///
    /// 调用时机由压缩命令保证：只在摘要**成功**（含降级规则产出）后调用——
    /// 该对话后续轮次的 system 前缀切到最新目录（压缩已重建上下文，属预期）。
    pub fn refresh(&mut self, conversation_id: &str, live_catalog: Option<&str>) -> bool {
        if !is_safe_session_id(conversation_id) {
            return false;
        }
        let Some(live) = live_catalog else {
            return false;
        };
        let current = self
            .cache
            .get(conversation_id)
            .cloned()
            .or_else(|| std::fs::read_to_string(self.sidecar_path(conversation_id)).ok());
        if current.as_deref() == Some(live) {
            return false;
        }
        let path = self.sidecar_path(conversation_id);
        if let Err(e) = atomic_write(&path, live) {
            eprintln!("[skills] 快照刷新落盘失败（{}）：{e}", path.display());
            return false;
        }
        self.cache
            .insert(conversation_id.to_string(), live.to_string());
        true
    }

    /// 删除对话时清快照（内存 + 盘上；幂等）。
    pub fn remove(&mut self, conversation_id: &str) {
        self.cache.remove(conversation_id);
        if is_safe_session_id(conversation_id) {
            let _ = std::fs::remove_file(self.sidecar_path(conversation_id));
        }
    }

    /// 该对话是否已冻结（内存或盘上有快照）。
    #[allow(dead_code)] // 诊断/测试消费
    pub fn is_frozen(&self, conversation_id: &str) -> bool {
        self.cache.contains_key(conversation_id)
            || (is_safe_session_id(conversation_id) && self.sidecar_path(conversation_id).exists())
    }

    /// 启动清扫：删除 `sessions/` 下的**孤儿**技能快照（无对应 `{id}.jsonl`）。
    ///
    /// 只删孤儿、不碰有主文件（含内存缓存），也不碰其他 sidecar（.memory.txt）。
    /// 返回删除数量。
    pub fn sweep_orphans(&self) -> usize {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return 0;
        };
        let mut removed = 0usize;
        for e in rd.flatten() {
            let path = e.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(id) = name.strip_suffix(".skills.txt") else {
                continue;
            };
            if !is_safe_session_id(id) {
                continue;
            }
            // 有主：会话内容文件存在 → 保留；否则删除
            if !self.dir.join(format!("{id}.jsonl")).exists() && std::fs::remove_file(&path).is_ok()
            {
                removed += 1;
            }
        }
        removed
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建唯一临时目录（不引入 tempfile crate，沿用记忆快照夹具模式）。
    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-skill-snap-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn freeze_within_conversation_and_persist() {
        let sessions = temp_dir("freeze");
        let mut snaps = SkillSnapshotStore::new(&sessions);

        let first = snaps.get_or_freeze(Some("c1"), Some("- 数值计算：计算规则"));
        assert_eq!(first, "- 数值计算：计算规则");
        assert!(snaps.is_frozen("c1"));
        assert!(
            sessions.join("c1.skills.txt").exists(),
            "首见冻结应落盘 sidecar"
        );

        // 活体变化后，同一对话仍返回旧快照（冻结）
        let again = snaps.get_or_freeze(Some("c1"), Some("- 新技能：新目录"));
        assert_eq!(again, first, "对话内冻结：不随活体目录变化");

        // 新对话 → 新快照（刷新点）
        let other = snaps.get_or_freeze(Some("c2"), Some("- 新技能：新目录"));
        assert_eq!(other, "- 新技能：新目录");
        assert_ne!(other, first);

        let _ = std::fs::remove_dir_all(&sessions);
    }

    #[test]
    fn restart_reads_sidecar_byte_identical() {
        let sessions = temp_dir("restart");
        let before = {
            let mut snaps = SkillSnapshotStore::new(&sessions);
            snaps.get_or_freeze(Some("c9"), Some("目录 A"))
        };

        // 模拟重启：进程内缓存清空、活体已变化，但同对话读回 sidecar
        let mut fresh = SkillSnapshotStore::new(&sessions);
        let after = fresh.get_or_freeze(Some("c9"), Some("目录 B"));
        assert_eq!(after, before, "重启后同对话目录逐字一致");
        let _ = std::fs::remove_dir_all(&sessions);
    }

    #[test]
    fn empty_catalog_freezes_empty_sidecar() {
        let sessions = temp_dir("empty");
        let mut snaps = SkillSnapshotStore::new(&sessions);
        let text = snaps.get_or_freeze(Some("c3"), Some(""));
        assert_eq!(text, "");
        assert!(
            sessions.join("c3.skills.txt").exists(),
            "空目录也落盘（冻结）"
        );
        // 封存后不随活体变化
        let again = snaps.get_or_freeze(Some("c3"), Some("新目录"));
        assert_eq!(again, "");
        let _ = std::fs::remove_dir_all(&sessions);
    }

    #[test]
    fn unreadable_live_defers_freeze() {
        let sessions = temp_dir("defer");
        let mut snaps = SkillSnapshotStore::new(&sessions);

        // 活体不可读：返回空、不冻结、不落盘（下轮重试）
        let text = snaps.get_or_freeze(Some("c5"), None);
        assert_eq!(text, "");
        assert!(!snaps.is_frozen("c5"), "活体不可读不应冻结");
        assert!(!sessions.join("c5.skills.txt").exists(), "不应落盘");

        // 活体恢复 → 正常冻结
        let text2 = snaps.get_or_freeze(Some("c5"), Some("目录 C"));
        assert_eq!(text2, "目录 C");
        assert!(snaps.is_frozen("c5"));
        let _ = std::fs::remove_dir_all(&sessions);
    }

    #[test]
    fn none_conversation_id_returns_live_without_persisting() {
        let sessions = temp_dir("none");
        let mut snaps = SkillSnapshotStore::new(&sessions);
        let text = snaps.get_or_freeze(None, Some("目录 X"));
        assert_eq!(text, "目录 X", "仍返回活体");
        let empty = snaps.get_or_freeze(None, None);
        assert_eq!(empty, "", "活体缺失时返回空");
        // 不落盘、不缓存
        assert!(!snaps.is_frozen("x"));
        let entries: Vec<_> = std::fs::read_dir(&sessions)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(entries.is_empty(), "sessions/ 不应产生任何文件");
        let _ = std::fs::remove_dir_all(&sessions);
    }

    #[test]
    fn remove_clears_sidecar_and_cache() {
        let sessions = temp_dir("remove");
        let mut snaps = SkillSnapshotStore::new(&sessions);
        snaps.get_or_freeze(Some("c4"), Some("目录"));
        assert!(snaps.is_frozen("c4"));
        snaps.remove("c4");
        assert!(!snaps.is_frozen("c4"));
        assert!(!sessions.join("c4.skills.txt").exists());
        // 幂等
        snaps.remove("c4");
        let _ = std::fs::remove_dir_all(&sessions);
    }

    #[test]
    fn unsafe_id_is_not_persisted() {
        let sessions = temp_dir("unsafe");
        let mut snaps = SkillSnapshotStore::new(&sessions);
        let text = snaps.get_or_freeze(Some("../evil"), Some("目录"));
        assert_eq!(text, "目录", "仍返回活体（不阻断）");
        assert!(!sessions.join("../evil.skills.txt").exists());
        let _ = std::fs::remove_dir_all(&sessions);
    }

    #[test]
    fn safe_id_rules() {
        assert!(is_safe_session_id("a-b_c1"));
        assert!(is_safe_session_id("abc123"));
        assert!(!is_safe_session_id(""));
        assert!(!is_safe_session_id("a/b"));
        assert!(!is_safe_session_id(".hidden"));
        assert!(!is_safe_session_id("a b"));
    }

    #[test]
    fn sweep_orphans_removes_only_ownerless_skills_snapshots() {
        let sessions = temp_dir("sweep");
        // 有主：c1 有 jsonl；孤儿：c2 无 jsonl；记忆 sidecar 与索引文件不动
        std::fs::write(sessions.join("c1.jsonl"), "{}").unwrap();
        std::fs::write(sessions.join("c1.skills.txt"), "catalog1").unwrap();
        std::fs::write(sessions.join("c2.skills.txt"), "orphan").unwrap();
        std::fs::write(sessions.join("c1.memory.txt"), "mem").unwrap();
        std::fs::write(sessions.join("index.json"), "{}").unwrap();

        let snaps = SkillSnapshotStore::new(&sessions);
        assert_eq!(snaps.sweep_orphans(), 1);
        assert!(sessions.join("c1.skills.txt").exists(), "有主保留");
        assert!(!sessions.join("c2.skills.txt").exists(), "孤儿删除");
        assert!(sessions.join("c1.memory.txt").exists(), "记忆 sidecar 不动");
        assert!(sessions.join("index.json").exists(), "非快照文件不动");
        // 幂等
        assert_eq!(snaps.sweep_orphans(), 0);
        let _ = std::fs::remove_dir_all(&sessions);
    }

    #[test]
    fn refresh_writes_only_on_difference() {
        let sessions = temp_dir("refresh");
        let mut snaps = SkillSnapshotStore::new(&sessions);
        snaps.get_or_freeze(Some("c1"), Some("目录 A"));

        // 无差：不写、保持
        assert!(!snaps.refresh("c1", Some("目录 A")));
        assert_eq!(
            std::fs::read_to_string(sessions.join("c1.skills.txt")).unwrap(),
            "目录 A"
        );

        // 有差：写盘 + 缓存更新（刷新后冻结读回新值）
        assert!(snaps.refresh("c1", Some("目录 B")));
        assert_eq!(
            std::fs::read_to_string(sessions.join("c1.skills.txt")).unwrap(),
            "目录 B"
        );
        assert_eq!(snaps.get_or_freeze(Some("c1"), Some("目录 C")), "目录 B");

        let _ = std::fs::remove_dir_all(&sessions);
    }

    #[test]
    fn refresh_skips_unreadable_live() {
        let sessions = temp_dir("refresh-skip");
        let mut snaps = SkillSnapshotStore::new(&sessions);
        snaps.get_or_freeze(Some("c1"), Some("目录 A"));

        assert!(!snaps.refresh("c1", None), "活体不可读 → 跳过");
        assert_eq!(
            std::fs::read_to_string(sessions.join("c1.skills.txt")).unwrap(),
            "目录 A",
            "旧快照保持"
        );

        let _ = std::fs::remove_dir_all(&sessions);
    }

    #[test]
    fn refresh_creates_when_absent_and_rejects_unsafe_id() {
        let sessions = temp_dir("refresh-new");
        let mut snaps = SkillSnapshotStore::new(&sessions);

        // 尚未冻结：有活体 → 建立
        assert!(snaps.refresh("c2", Some("目录 X")));
        assert!(sessions.join("c2.skills.txt").exists());

        // 非法 id：不落盘
        assert!(!snaps.refresh("../evil", Some("目录")));
        assert!(!sessions.join("../evil.skills.txt").exists());

        let _ = std::fs::remove_dir_all(&sessions);
    }
}
