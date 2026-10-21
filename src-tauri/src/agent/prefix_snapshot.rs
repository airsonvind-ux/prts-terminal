//! prefix_snapshot —— 对话完整前缀冻结快照（system 全文 + 工具定义）
//!
//! ## 语义（批 5 · 前缀完全冻结）
//!
//! sidecar 路径 = `sessions/{conversationId}.prefix.json`（与对话内容文件同目录，
//! 同生共死）。**首次组装即冻结**；此后同一对话一律读回原文——对话内 system
//! 全文与工具定义**逐字节不变**（前缀缓存为硬约束）。刷新点＝新对话 ＋ 压缩
//! （[`PrefixSnapshotStore::refresh`]：摘要成功后以活体重刷，有差才写）。
//!
//! 为什么单独冻结：system 的人格/界面段来自编译进程序的文本，工具定义来自
//! 注册表，二者随**应用升级**改变。冻结创建时的那份 → 升级后旧对话前缀不变
//! （“之前发生的一切被真正意义冻结”）；新能力从新对话生效。
//!
//! - **设计对照**：技能目录快照（`skills_snapshot.rs`）冻的是“模型看到的目录
//!   视图”，本模块冻的是**整个前缀**（人格 + 界面 + 目录 + 记忆区 + 工具面）。
//! - 无 / 非法 conversationId：只返回活体、不落盘（防垃圾快照，同其余快照裁决）。
//! - 读回失败（文件损坏 / 格式不符）：以活体重写落盘（自愈，不阻断本轮）。
//!
//! ## 时序与孤儿
//!
//! 首轮 `agent_process` 先于前端轮末归档（`append_turn`），快照可能先于对话
//! 档案诞生，这是**有意**的；孤儿 sidecar 在 `delete_conversation` 侧显式清扫，
//! 启动时 `sweep_orphans` 兜底（与记忆 / 技能快照同区）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::llm::ToolDef;
use crate::memory::store::atomic_write;

/// 冻结的完整前缀：渲染后的 system 全文 + 发送给 LLM 的工具定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrefixSnapshot {
    /// 渲染后的 system prompt 全文（人格 + 界面 + 技能目录 + 记忆区）。
    pub system: String,
    /// 工具定义（tools 数组原文）。
    pub tool_defs: Vec<ToolDef>,
}

/// 完整前缀冻结快照存储：内存缓存 + 盘上 sidecar。
pub struct PrefixSnapshotStore {
    /// `sessions/` 目录
    dir: PathBuf,
    cache: HashMap<String, PrefixSnapshot>,
}

impl PrefixSnapshotStore {
    /// 在 `sessions/` 目录上建立快照存储（目录不存在则创建）。
    pub fn new(sessions_dir: &Path) -> Self {
        std::fs::create_dir_all(sessions_dir).ok();
        Self {
            dir: sessions_dir.to_path_buf(),
            cache: HashMap::new(),
        }
    }

    fn sidecar_path(&self, conversation_id: &str) -> PathBuf {
        self.dir.join(format!("{conversation_id}.prefix.json"))
    }

    /// 取冻结前缀：内存缓存 → sidecar → 以活体冻结（并落盘）。
    ///
    /// - `conversation_id` 为 None / 非法：只返回活体（不落盘、不缓存）。
    /// - sidecar 存在但解析失败：视为未冻结，以活体重写（自愈）。
    pub fn get_or_freeze(
        &mut self,
        conversation_id: Option<&str>,
        live_system: &str,
        live_tools: &[ToolDef],
    ) -> PrefixSnapshot {
        let live = || PrefixSnapshot {
            system: live_system.to_string(),
            tool_defs: live_tools.to_vec(),
        };
        let cid = match conversation_id {
            Some(c) if is_safe_id(c) => c,
            _ => return live(),
        };
        if let Some(v) = self.cache.get(cid) {
            return v.clone();
        }
        let path = self.sidecar_path(cid);
        if let Ok(s) = std::fs::read_to_string(&path) {
            match serde_json::from_str::<PrefixSnapshot>(&s) {
                Ok(p) => {
                    self.cache.insert(cid.to_string(), p.clone());
                    return p;
                }
                Err(e) => {
                    eprintln!("[prefix] 快照解析失败，以活体重写（{}）：{e}", path.display());
                }
            }
        }
        // 首见（或自愈）：冻结 + 落盘
        let snap = live();
        match serde_json::to_string(&snap) {
            Ok(json) => {
                if let Err(e) = atomic_write(&path, &json) {
                    eprintln!("[prefix] 前缀快照落盘失败（{}）：{e}", path.display());
                }
            }
            Err(e) => eprintln!("[prefix] 前缀快照序列化失败：{e}"),
        }
        self.cache.insert(cid.to_string(), snap.clone());
        snap
    }

    /// 压缩后刷新（批 5）：以最新活体重写快照，**有差才写**。
    ///
    /// 调用时机由压缩命令保证：只在摘要**成功**（含降级规则产出）后调用——
    /// 该对话后续轮次的 system 前缀切到最新（压缩已重建上下文，属预期）。
    /// `conversation_id` 非法：不落盘（返回 false）。
    pub fn refresh(
        &mut self,
        conversation_id: &str,
        live_system: &str,
        live_tools: &[ToolDef],
    ) -> bool {
        if !is_safe_id(conversation_id) {
            return false;
        }
        let live = PrefixSnapshot {
            system: live_system.to_string(),
            tool_defs: live_tools.to_vec(),
        };
        let current = self.cache.get(conversation_id).cloned().or_else(|| {
            std::fs::read_to_string(self.sidecar_path(conversation_id))
                .ok()
                .and_then(|s| serde_json::from_str::<PrefixSnapshot>(&s).ok())
        });
        if let Some(c) = &current {
            if c.system == live.system && tool_defs_eq(&c.tool_defs, &live.tool_defs) {
                return false;
            }
        }
        let json = match serde_json::to_string(&live) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("[prefix] 快照刷新序列化失败：{e}");
                return false;
            }
        };
        let path = self.sidecar_path(conversation_id);
        if let Err(e) = atomic_write(&path, &json) {
            eprintln!("[prefix] 快照刷新落盘失败（{}）：{e}", path.display());
            return false;
        }
        self.cache.insert(conversation_id.to_string(), live);
        true
    }

    /// 删除对话时清快照（内存 + 盘上；幂等）。
    pub fn remove(&mut self, conversation_id: &str) {
        self.cache.remove(conversation_id);
        if is_safe_id(conversation_id) {
            let _ = std::fs::remove_file(self.sidecar_path(conversation_id));
        }
    }

    /// 该对话是否已冻结（内存或盘上有快照）。
    #[allow(dead_code)] // 诊断/测试消费
    pub fn is_frozen(&self, conversation_id: &str) -> bool {
        self.cache.contains_key(conversation_id)
            || (is_safe_id(conversation_id) && self.sidecar_path(conversation_id).exists())
    }

    /// 启动清扫：删除 `sessions/` 下的**孤儿**前缀快照（无对应 `{id}.jsonl`）。
    ///
    /// 只删孤儿、不碰有主文件（含内存缓存），也不碰其他 sidecar。
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
            let Some(id) = name.strip_suffix(".prefix.json") else {
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

/// 工具定义逐项相等（**顺序敏感**：tools 数组顺序即前缀的一部分）。
fn tool_defs_eq(a: &[ToolDef], b: &[ToolDef]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|(x, y)| {
            x.tool_type == y.tool_type
                && x.function.name == y.function.name
                && x.function.description == y.function.description
                && x.function.parameters == y.function.parameters
        })
}

/// 会话 id 文件名安全校验。
///
/// **口径与 `session_store::valid_id` / `memory::snapshot::is_safe_id` /
/// `skills_snapshot::is_safe_id` 严格一致**（仅 `[A-Za-z0-9_-]`，非空，不以 '.'
/// 开头）；多处为不同模块的独立实现，修改任一侧时必须同步。
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

    /// 创建唯一临时目录（不引入 tempfile crate，沿用其他快照夹具模式）。
    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-prefix-snap-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn defs(desc: &str) -> Vec<ToolDef> {
        vec![ToolDef::new(
            "echo",
            desc,
            serde_json::json!({"type": "object"}),
        )]
    }

    #[test]
    fn freeze_within_conversation_and_persist() {
        let sessions = temp_dir("freeze");
        let mut snaps = PrefixSnapshotStore::new(&sessions);

        let first = snaps.get_or_freeze(Some("c1"), "系统 A", &defs("描述 A"));
        assert_eq!(first.system, "系统 A");
        assert!(snaps.is_frozen("c1"));
        assert!(
            sessions.join("c1.prefix.json").exists(),
            "首见冻结应落盘 sidecar"
        );

        // 活体已变 → 仍读回冻结原文
        let again = snaps.get_or_freeze(Some("c1"), "系统 B", &defs("描述 B"));
        assert_eq!(again.system, "系统 A", "对话内冻结：不随活体变化");
        assert_eq!(again.tool_defs[0].function.description, "描述 A");

        // 新实例（模拟重启）：从盘上读回
        let mut fresh = PrefixSnapshotStore::new(&sessions);
        let restored = fresh.get_or_freeze(Some("c1"), "系统 B", &defs("描述 B"));
        assert_eq!(restored.system, "系统 A", "跨重启逐字不变");
    }

    #[test]
    fn refresh_writes_only_on_change() {
        let sessions = temp_dir("refresh");
        let mut snaps = PrefixSnapshotStore::new(&sessions);
        snaps.get_or_freeze(Some("c1"), "系统 A", &defs("描述 A"));

        assert!(!snaps.refresh("c1", "系统 A", &defs("描述 A")), "无差 → 不写");
        assert!(snaps.refresh("c1", "系统 B", &defs("描述 A")), "system 变 → 写");
        assert!(snaps.refresh("c1", "系统 B", &defs("描述 B")), "工具描述变 → 写");
        assert_eq!(
            snaps.get_or_freeze(Some("c1"), "系统 C", &defs("描述 C")).system,
            "系统 B",
            "刷新后读回新值"
        );
    }

    #[test]
    fn unsafe_id_does_not_persist() {
        let sessions = temp_dir("unsafe");
        let mut snaps = PrefixSnapshotStore::new(&sessions);
        let s = snaps.get_or_freeze(None, "系统 A", &defs("描述 A"));
        assert_eq!(s.system, "系统 A", "无 id → 返回活体");
        assert!(!snaps.is_frozen("c1"));
        let s2 = snaps.get_or_freeze(Some("../evil"), "系统 A", &defs("描述 A"));
        assert_eq!(s2.system, "系统 A");
        assert_eq!(
            std::fs::read_dir(&sessions).unwrap().count(),
            0,
            "非法 id 不落盘"
        );
    }

    #[test]
    fn sweep_removes_only_orphans() {
        let sessions = temp_dir("sweep");
        let mut snaps = PrefixSnapshotStore::new(&sessions);
        snaps.get_or_freeze(Some("orphan"), "系统", &defs("描述"));
        snaps.get_or_freeze(Some("kept"), "系统", &defs("描述"));
        std::fs::write(sessions.join("kept.jsonl"), "{}").unwrap();

        let n = snaps.sweep_orphans();
        assert_eq!(n, 1, "只删无主的孤儿");
        assert!(!sessions.join("orphan.prefix.json").exists());
        assert!(sessions.join("kept.prefix.json").exists());
        assert!(sessions.join("kept.jsonl").exists(), "不碰对话内容文件");
    }

    #[test]
    fn tool_defs_eq_is_order_sensitive() {
        let a = defs("描述");
        let mut b = defs("描述");
        assert!(tool_defs_eq(&a, &b));
        b[0].function.name = "other".into();
        assert!(!tool_defs_eq(&a, &b), "名字不同即不同");
    }
}
