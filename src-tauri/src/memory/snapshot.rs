//! memory::snapshot —— 记忆区冻结快照（随对话档案落盘，跨重启逐字不变）
//!
//! ## 语义（对齐方案 v2 §5）
//!
//! sidecar 路径 = `sessions/{conversationId}.memory.txt`（与对话内容文件同目录，
//! 同生共死）。**首次组装即写**；此后同一对话一律读回原文——**唯一刷新点＝新对话**
//! （新 conversationId → 首见组装）。重启后读 sidecar → 与重启前逐字一致，
//! 前缀缓存命中为硬约束。
//!
//! ## 时序与孤儿（裁决 1）
//!
//! 首轮 `agent_process` 先于前端轮末归档（`append_turn`），快照可能先于对话档案
//! 诞生，这是**有意**的（反向"档案在、快照不在"会导致下一轮换版记忆区，伤缓存）。
//! 孤儿 sidecar 在 `delete_conversation` 侧显式清扫；列表侧可另行兜底扫描。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::memory::prompt::assemble_memory_block;
use crate::memory::store::{atomic_write, MemoryStore};

/// 冻结快照存储：内存缓存 + 盘上 sidecar。
pub struct SnapshotStore {
    /// `sessions/` 目录
    dir: PathBuf,
    cache: HashMap<String, String>,
}

impl SnapshotStore {
    /// 在 `sessions/` 目录上建立快照存储（目录不存在则创建）。
    pub fn new(sessions_dir: &Path) -> Self {
        std::fs::create_dir_all(sessions_dir).ok();
        Self {
            dir: sessions_dir.to_path_buf(),
            cache: HashMap::new(),
        }
    }

    fn sidecar_path(&self, conversation_id: &str) -> PathBuf {
        self.dir.join(format!("{conversation_id}.memory.txt"))
    }

    /// 取冻结快照：内存缓存 → sidecar → 新组装（并落盘）。
    ///
    /// `conversation_id = None`（或无真实对话标识 / 非法 id）：**只组装不落盘**
    /// ——不缓存也不写 sidecar（防每请求唯一 id 产生垃圾快照，批 1 审查修复 🟡3）。
    pub fn get_or_assemble(
        &mut self,
        conversation_id: Option<&str>,
        store: &MemoryStore,
    ) -> String {
        let cid = match conversation_id {
            Some(c) if is_safe_id(c) => c,
            _ => return assemble_memory_block(store),
        };
        if let Some(v) = self.cache.get(cid) {
            return v.clone();
        }
        let path = self.sidecar_path(cid);
        if let Ok(s) = std::fs::read_to_string(&path) {
            self.cache.insert(cid.to_string(), s.clone());
            return s;
        }
        // 首见：组装 + 落盘
        let block = assemble_memory_block(store);
        if let Err(e) = atomic_write(&path, &block) {
            eprintln!("[memory] 快照落盘失败（{}）：{e}", path.display());
        }
        self.cache.insert(cid.to_string(), block.clone());
        block
    }

    /// 删除对话时清快照（内存 + 盘上；幂等）。
    pub fn remove(&mut self, conversation_id: &str) {
        self.cache.remove(conversation_id);
        if is_safe_id(conversation_id) {
            let _ = std::fs::remove_file(self.sidecar_path(conversation_id));
        }
    }

    /// 该对话是否已封存（内存或盘上有快照）。
    #[allow(dead_code)] // 诊断/测试消费；批 2 清扫与任务四可见性预留
    pub fn is_frozen(&self, conversation_id: &str) -> bool {
        self.cache.contains_key(conversation_id)
            || (is_safe_id(conversation_id) && self.sidecar_path(conversation_id).exists())
    }

    /// 启动清扫：删除 `sessions/` 下的**孤儿**冻结文件（无对应 `{id}.jsonl`）。
    ///
    /// 只删孤儿、不碰有主文件（含内存缓存）。返回删除数量。
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
            let Some(id) = name.strip_suffix(".memory.txt") else {
                continue;
            };
            if !is_safe_id(id) {
                continue;
            }
            // 有主：会话内容文件存在 → 保留；否则删除
            if !self.dir.join(format!("{id}.jsonl")).exists()
                && std::fs::remove_file(&path).is_ok()
            {
                removed += 1;
            }
        }
        removed
    }
}

/// 会话 id 文件名安全校验。
///
/// **口径与 `session_store::valid_id` 严格一致**（仅 `[A-Za-z0-9_-]`，非空）；
/// 两处为不同模块的独立实现，修改任一侧时必须同步（同一 conversationId 既要
/// 能当 jsonl 文件名，也要能当 sidecar 文件名）。
fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::schema::PROFILE_NAME;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-mem-snap-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn freeze_within_conversation_and_persist() {
        let data = temp_dir("freeze");
        let sessions = data.join("sessions");
        let mut store = MemoryStore::load(&data).unwrap();
        store
            .growth_update(PROFILE_NAME, Some("第一版画像"), None, None)
            .unwrap();

        let mut snaps = SnapshotStore::new(&sessions);
        let first = snaps.get_or_assemble(Some("c1"), &store);
        assert!(first.contains("第一版画像"));
        assert!(snaps.is_frozen("c1"));
        let sidecar = sessions.join("c1.memory.txt");
        assert!(sidecar.exists(), "首见组装应落盘 sidecar");

        // 记忆变化后，同一对话仍返回旧快照（冻结）
        store
            .growth_update(PROFILE_NAME, Some("第二版画像"), None, None)
            .unwrap();
        let again = snaps.get_or_assemble(Some("c1"), &store);
        assert_eq!(again, first, "对话内冻结：不随记忆变化");

        // 新对话 → 新快照（刷新点）
        let other = snaps.get_or_assemble(Some("c2"), &store);
        assert!(other.contains("第二版画像"));
        assert_ne!(other, first);

        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn restart_reads_sidecar_byte_identical() {
        let data = temp_dir("restart");
        let sessions = data.join("sessions");
        let mut store = MemoryStore::load(&data).unwrap();
        store.growth_create("伤害计算", "暴击与减伤", "正文", vec![], "手动").unwrap();

        let before = {
            let mut snaps = SnapshotStore::new(&sessions);
            snaps.get_or_assemble(Some("c9"), &store)
        };

        // 模拟重启：进程内缓存清空、记忆已变化，但同对话读回 sidecar
        store.growth_update("伤害计算", Some("改动后的正文"), None, None).unwrap();
        let mut fresh = SnapshotStore::new(&sessions);
        let after = fresh.get_or_assemble(Some("c9"), &store);
        assert_eq!(after, before, "重启后同对话记忆区逐字一致");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn empty_memory_freezes_empty_sidecar() {
        let data = temp_dir("empty");
        let sessions = data.join("sessions");
        let store = MemoryStore::load(&data).unwrap();
        let mut snaps = SnapshotStore::new(&sessions);
        let block = snaps.get_or_assemble(Some("c3"), &store);
        assert_eq!(block, "");
        assert!(sessions.join("c3.memory.txt").exists(), "空块也落盘（冻结）");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn none_conversation_id_assembles_without_persisting() {
        let data = temp_dir("none");
        let sessions = data.join("sessions");
        let mut store = MemoryStore::load(&data).unwrap();
        store.growth_update(PROFILE_NAME, Some("画像"), None, None).unwrap();
        let mut snaps = SnapshotStore::new(&sessions);
        let block = snaps.get_or_assemble(None, &store);
        assert!(block.contains("画像"), "仍返回组装结果");
        // 不落盘、不缓存
        assert!(!snaps.is_frozen("x"));
        let entries: Vec<_> = std::fs::read_dir(&sessions).unwrap().filter_map(|e| e.ok()).collect();
        assert!(entries.is_empty(), "sessions/ 不应产生任何文件");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn remove_clears_sidecar_and_cache() {
        let data = temp_dir("remove");
        let sessions = data.join("sessions");
        let store = MemoryStore::load(&data).unwrap();
        let mut snaps = SnapshotStore::new(&sessions);
        snaps.get_or_assemble(Some("c4"), &store);
        assert!(snaps.is_frozen("c4"));
        snaps.remove("c4");
        assert!(!snaps.is_frozen("c4"));
        assert!(!sessions.join("c4.memory.txt").exists());
        // 幂等
        snaps.remove("c4");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn unsafe_id_is_not_persisted() {
        let data = temp_dir("unsafe");
        let sessions = data.join("sessions");
        let store = MemoryStore::load(&data).unwrap();
        let mut snaps = SnapshotStore::new(&sessions);
        let block = snaps.get_or_assemble(Some("../evil"), &store);
        assert_eq!(block, "", "仍返回组装结果（不阻断）");
        assert!(!sessions.join("../evil.memory.txt").exists());
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn safe_id_rules() {
        assert!(is_safe_id("a-b_c1"));
        assert!(is_safe_id("abc123"));
        assert!(!is_safe_id(""));
        assert!(!is_safe_id("a/b"));
        assert!(!is_safe_id(".hidden"));
        assert!(!is_safe_id("a b"));
    }

    #[test]
    fn sweep_orphans_removes_only_ownerless() {
        let data = temp_dir("sweep");
        let sessions = data.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        // 有主：c1 有 jsonl；孤儿：c2 无 jsonl；非快照文件保留
        std::fs::write(sessions.join("c1.jsonl"), "{}").unwrap();
        std::fs::write(sessions.join("c1.memory.txt"), "block1").unwrap();
        std::fs::write(sessions.join("c2.memory.txt"), "orphan").unwrap();
        std::fs::write(sessions.join("index.json"), "{}").unwrap();
        let snaps = SnapshotStore::new(&sessions);
        assert_eq!(snaps.sweep_orphans(), 1);
        assert!(sessions.join("c1.memory.txt").exists(), "有主保留");
        assert!(!sessions.join("c2.memory.txt").exists(), "孤儿删除");
        assert!(sessions.join("index.json").exists(), "非快照文件不动");
        // 幂等
        assert_eq!(snaps.sweep_orphans(), 0);
        let _ = std::fs::remove_dir_all(&data);
    }
}
