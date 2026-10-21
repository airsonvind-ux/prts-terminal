//! 缓存区（工作记忆）—— 静态桶 + 动态桶（架构升级 · 模块 7，P0）
//!
//! ## 静态缓存角色重构（模块 7 P0）
//!
//! 静态缓存 = LLM 读知识条目后的**自写总结**（以条目标注的"要点"为纲，
//! 见 `knowledge.rs` 的 frontmatter schema），角色 = **再学习检验 + 关键知识
//! 对话内常驻的提醒**：
//! - 权威仍是知识库全文（按需重载）——总结失准降级为 **cache miss**
//!   （`load_static` 返回 None → LLM 重载全文），而非静默推演错误
//! - 创建时机：LLM 自主（读后主动 store；重读发现理解有误则更新，无误不更新）
//! - 卸载策略：静态缓存在对话内始终载入，仅上下文压缩时卸载；
//!   动态缓存载入状态每轮卸载，桶内容跨轮保留（按需重新载入）
//!
//! ## 边界（缓存 ≠ 记忆）
//!
//! 缓存区是**单对话工作记忆**（CoALA Working Memory）：生命周期绑定单个
//! 对话，对话结束即整体失效。跨对话共享的 LLM 长期记忆属规划中的
//! "记忆区"（见主控架构要素清单），与缓存区是两个系统。
//!
//! ## 缓存区与 log 分层（模块 7 P0）
//!
//! `CacheStore` 独立于 session log——log 只记录 `cache_*` **操作**
//! （`session.rs` 的 cache/store、cache/load、cache/remove 事件，可回放），
//! 不存条目内容；条目内容只活在 CacheStore 中。

#![allow(dead_code)] // 2026-08：缓存系统停用（工具取消注册），存储层保留待升级为记忆系统；恢复后移除

use std::collections::HashMap;

/// 缓存条目。
#[derive(Debug, Clone, PartialEq)]
pub struct CacheEntry {
    /// 条目键（LLM 自主命名，工具描述给出命名规范指引）
    pub key: String,
    /// 条目内容
    pub content: String,
    /// 来源知识条目名（静态桶：LLM 自写总结的来源；动态桶：None）
    pub source_entry: Option<String>,
}

impl CacheEntry {
    /// 动态桶条目（LLM 工作中产生的重要数据，如地图二维结构）。
    /// 测试构造便捷用；生产路径（tools/cache.rs）直接构造字面量。
    #[allow(dead_code)]
    pub fn dynamic(key: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            content: content.into(),
            source_entry: None,
        }
    }

    /// 静态桶条目（LLM 读知识条目后的自写总结，source 关联知识条目）。
    /// 测试构造便捷用；生产路径（tools/cache.rs）直接构造字面量。
    #[allow(dead_code)]
    pub fn static_summary(key: impl Into<String>, content: impl Into<String>, source_entry: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            content: content.into(),
            source_entry: Some(source_entry.into()),
        }
    }
}

/// 缓存区：静态桶（对话内常驻的提醒）+ 动态桶（桶内容跨轮保留）。
///
/// 存储与注入分离：条目存于此（缓存工具经 CacheStores 分区读写）；
/// 注入 Reference 层由上下文组装与 ContextOp 标记协作完成（模块 C 已接线）。
#[derive(Debug, Default)]
pub struct CacheStore {
    static_entries: HashMap<String, CacheEntry>,
    dynamic_entries: HashMap<String, CacheEntry>,
}

impl CacheStore {
    /// 测试构造便捷用；生产经 `CacheStores::partition` 创建分区。
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    /// 静态桶全部条目按 key 排序（key, content 的 clone）。
    ///
    /// 供每 turn 静态注入使用：静态缓存角色 = 对话内常驻提醒（见文件顶部
    /// 角色重构说明），每轮把当前全部静态条目逐条 reference_load 进
    /// ActiveContext，由 prompt 组装消费。排序保证注入顺序确定
    /// （组装可复现性，与 `ActiveContext::reference_entries` 同一原则）。
    pub fn static_entries(&self) -> Vec<(String, String)> {
        let mut keys: Vec<&String> = self.static_entries.keys().collect();
        keys.sort();
        keys.into_iter()
            .map(|k| {
                let e = &self.static_entries[k];
                (k.clone(), e.content.clone())
            })
            .collect()
    }

    /// 动态桶全部 key 排序（clone），供 cache_list 列清单（动态桶只列名）。
    pub fn dynamic_keys(&self) -> Vec<String> {
        let mut keys: Vec<&String> = self.dynamic_entries.keys().collect();
        keys.sort();
        keys.into_iter().map(|k| (*k).clone()).collect()
    }

    /// 静态桶清单条目：按 key 排序返回 (key, 内容字符数, 内容前 60 字符预览)。
    ///
    /// 供 cache_list 列清单（静态桶条目名附内容预览，LLM 据此判断是否值得
    /// cache_retrieve 全文）。预览按 chars().take(60)，超长在末尾加「…」。
    pub fn static_previews(&self) -> Vec<(String, usize, String)> {
        let mut keys: Vec<&String> = self.static_entries.keys().collect();
        keys.sort();
        keys.into_iter()
            .map(|k| {
                let e = &self.static_entries[k];
                let len = e.content.chars().count();
                let preview = if len > 60 {
                    format!("{}…", e.content.chars().take(60).collect::<String>())
                } else {
                    e.content.clone()
                };
                (k.clone(), len, preview)
            })
            .collect()
    }

    /// 静态桶写入（LLM 读知识条目后自写总结；同名覆盖为更新语义）。
    pub fn store_static(&mut self, entry: CacheEntry) {
        self.static_entries.insert(entry.key.clone(), entry);
    }

    /// 动态桶写入（同名覆盖；每轮可重载）。
    pub fn store_dynamic(&mut self, entry: CacheEntry) {
        self.dynamic_entries.insert(entry.key.clone(), entry);
    }

    /// 静态桶读取。None = cache miss（LLM 应重载知识库全文，而非静默使用旧记忆）。
    pub fn load_static(&self, key: &str) -> Option<&CacheEntry> {
        self.static_entries.get(key)
    }

    /// 动态桶读取。None = cache miss（LLM 决定是否重新产生数据）。
    pub fn load_dynamic(&self, key: &str) -> Option<&CacheEntry> {
        self.dynamic_entries.get(key)
    }

    /// 静态桶卸载（仅上下文压缩时使用）。
    pub fn remove_static(&mut self, key: &str) -> Option<CacheEntry> {
        self.static_entries.remove(key)
    }

    /// 动态桶卸载。
    pub fn remove_dynamic(&mut self, key: &str) -> Option<CacheEntry> {
        self.dynamic_entries.remove(key)
    }

    /// 清空动态桶。
    ///
    /// 注意：与「每轮卸载载入状态」是两个动作——后者只移除上下文注入，
    /// 不动桶内容。当前无调用点：对话删除经 `CacheStores::remove_partition`
    /// 整体销毁分区；保留供未来会话内显式重置场景使用。
    #[allow(dead_code)]
    pub fn clear_dynamic(&mut self) {
        self.dynamic_entries.clear();
    }

    /// 动态桶条目数（观测/测试用）。
    #[allow(dead_code)]
    pub fn dynamic_len(&self) -> usize {
        self.dynamic_entries.len()
    }

    /// 静态桶条目数（观测/测试用）。
    #[allow(dead_code)]
    pub fn static_len(&self) -> usize {
        self.static_entries.len()
    }
}

/// 缓存区按对话分区的集合：conversationId → 该对话的 [`CacheStore`]。
///
/// 呼应文件顶部「边界（缓存 ≠ 记忆）」声明：缓存区是**单对话工作记忆**，
/// 生命周期绑定单个对话——分区随对话首次写入而创建（get_or_create），
/// 对话删除即整体移除（`remove_partition`，前端 clear_conversation_cache 调用），
/// 切换对话时各分区互不干扰（保留）。跨对话共享的 LLM 长期记忆仍属
/// 规划中的「记忆区」，与本类型是两个系统。
///
/// 进程内生命周期：随 Tauri State 存活于整个进程；分区从创建到被移除
/// 之间跨轮持久（静态桶常驻、动态桶内容跨轮保留）。
#[derive(Debug, Default)]
pub struct CacheStores {
    partitions: HashMap<String, CacheStore>,
}

impl CacheStores {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取指定对话的分区；不存在时插入空 CacheStore（get_or_create 语义）。
    ///
    /// 写入路径使用（cache_store / cache_remove 工具）；静态注入用 [`Self::get`]
    /// （只读，不创建空分区——「找不到则跳过」）。
    pub fn partition(&mut self, conversation_id: &str) -> &mut CacheStore {
        self.partitions
            .entry(conversation_id.to_string())
            .or_default()
    }

    /// 只读取指定对话的分区；不存在返回 None（不创建）。
    ///
    /// 供每 turn 静态注入：从未写入过缓存的对话没有分区，跳过即可，
    /// 不应因只读路径凭空产生空分区。
    pub fn get(&self, conversation_id: &str) -> Option<&CacheStore> {
        self.partitions.get(conversation_id)
    }

    /// 移除指定对话的分区（对话删除即整体失效）。
    /// 返回该分区是否存在（幂等：不存在返回 false，命令层仍视为成功）。
    pub fn remove_partition(&mut self, conversation_id: &str) -> bool {
        self.partitions.remove(conversation_id).is_some()
    }

    /// 当前分区数（观测/测试用）。
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.partitions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_bucket_store_load_roundtrip() {
        let mut store = CacheStore::new();
        store.store_static(CacheEntry::static_summary(
            "buff.总结",
            "同类取最高/刷新/叠加层数/唯一/加时",
            "Buff机制基础",
        ));
        let entry = store.load_static("buff.总结").unwrap();
        assert_eq!(entry.key, "buff.总结");
        assert_eq!(entry.source_entry.as_deref(), Some("Buff机制基础"));
        // 未命中 → cache miss（降级为重载全文，而非静默旧记忆）
        assert!(store.load_static("不存在的键").is_none());
    }

    #[test]
    fn static_store_overwrites_same_key() {
        let mut store = CacheStore::new();
        store.store_static(CacheEntry::static_summary("k", "v1", "条目A"));
        store.store_static(CacheEntry::static_summary("k", "v2", "条目A"));
        assert_eq!(store.load_static("k").unwrap().content, "v2");
        assert_eq!(store.static_len(), 1);
    }

    #[test]
    fn dynamic_bucket_is_separate_from_static() {
        let mut store = CacheStore::new();
        store.store_static(CacheEntry::static_summary("地图", "静态", "条目"));
        store.store_dynamic(CacheEntry::dynamic("地图", "动态"));
        // 同名键分桶独立
        assert_eq!(store.load_static("地图").unwrap().content, "静态");
        assert_eq!(store.load_dynamic("地图").unwrap().content, "动态");
        assert!(store.load_dynamic("地图").unwrap().source_entry.is_none());
    }

    #[test]
    fn clear_dynamic_keeps_static() {
        let mut store = CacheStore::new();
        store.store_static(CacheEntry::static_summary("s", "静态", "条目"));
        store.store_dynamic(CacheEntry::dynamic("d", "动态"));
        store.clear_dynamic();
        assert_eq!(store.dynamic_len(), 0);
        assert_eq!(store.static_len(), 1);
    }

    #[test]
    fn remove_returns_entry() {
        let mut store = CacheStore::new();
        store.store_dynamic(CacheEntry::dynamic("d", "内容"));
        let removed = store.remove_dynamic("d").unwrap();
        assert_eq!(removed.content, "内容");
        assert!(store.load_dynamic("d").is_none());
        // 重复移除 → None（幂等）
        assert!(store.remove_dynamic("d").is_none());
    }

    #[test]
    fn static_entries_sorted_by_key() {
        let mut store = CacheStore::new();
        store.store_static(CacheEntry::static_summary("buff.总结", "B 内容", "Buff机制基础"));
        store.store_static(CacheEntry::static_summary("伤害.公式", "A 内容", "数值计算"));
        store.store_static(CacheEntry::static_summary("索敌.规则", "C 内容", "索敌机制"));
        // 按 key 字典序输出（与 ActiveContext::reference_entries 同一原则：
        // 注入顺序确定 → prompt 组装可复现，request/header 去重不误判）
        // 注意：字符串按 Unicode 码点排序，ASCII（b=0x62）排在汉字（伤=0x4F24）前
        let entries = store.static_entries();
        assert_eq!(
            entries,
            vec![
                ("buff.总结".to_string(), "B 内容".to_string()),
                ("伤害.公式".to_string(), "A 内容".to_string()),
                ("索敌.规则".to_string(), "C 内容".to_string()),
            ]
        );
        // 返回的是 clone：修改原 store 不影响已取出的条目
        let mut entries = store.static_entries();
        entries[0].1 = "已修改".to_string();
        assert_eq!(store.load_static("伤害.公式").unwrap().content, "A 内容");
    }

    // ── CacheStores：对话分区集合 ──

    #[test]
    fn partition_created_on_first_access_and_reused() {
        let mut stores = CacheStores::new();
        assert_eq!(stores.len(), 0);

        // 同 id 两次访问 → 同一分区（get_or_create）
        stores.partition("conv-1").store_static(CacheEntry::static_summary("k", "v", "条目"));
        let same = stores.partition("conv-1");
        assert_eq!(same.load_static("k").unwrap().content, "v");
        assert_eq!(stores.len(), 1);

        // 不同 id → 不同分区，互不干扰（切换对话保留）
        stores.partition("conv-2").store_static(CacheEntry::static_summary("k", "v2", "条目"));
        assert_eq!(stores.len(), 2);
        assert_eq!(stores.partition("conv-1").load_static("k").unwrap().content, "v");
        assert_eq!(stores.partition("conv-2").load_static("k").unwrap().content, "v2");
    }

    #[test]
    fn partition_get_does_not_create() {
        let mut stores = CacheStores::new();
        // 只读访问不创建空分区（静态注入「找不到则跳过」语义）
        assert!(stores.get("conv-1").is_none());
        assert_eq!(stores.len(), 0);

        stores.partition("conv-1");
        assert!(stores.get("conv-1").is_some());
        // 静态注入看到的分区内容与写入路径一致
        stores.partition("conv-1").store_static(CacheEntry::static_summary("k", "v", "条目"));
        let entries = stores.get("conv-1").unwrap().static_entries();
        assert_eq!(entries, vec![("k".to_string(), "v".to_string())]);
    }

    #[test]
    fn remove_partition_returns_existence() {
        let mut stores = CacheStores::new();
        // 不存在 → false（幂等）
        assert!(!stores.remove_partition("conv-1"));

        stores.partition("conv-1");
        stores.partition("conv-2");
        assert!(stores.remove_partition("conv-1"));
        assert_eq!(stores.len(), 1);
        // 移除后再访问 → 重新创建空分区（get_or_create 语义）
        assert!(stores.partition("conv-1").static_entries().is_empty());
        // 重新创建后再次移除 → 存在（返回 true）
        assert!(stores.remove_partition("conv-1"));
        // 重复移除 → false（幂等）
        assert!(!stores.remove_partition("conv-1"));
    }
}
