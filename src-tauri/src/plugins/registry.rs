//! 插件台账：M1 只在内存里记录「加载了谁、状态如何、贡献了哪些工具」。
//!
//! 不落盘：启停状态与插件配置的持久化是 M3 的事（D18 把配置与启停状态分两处）。
//!
//! M1 的台账「只写不读」：`get` / `list` 供 M3 的扩展页与诊断消费，本里程碑
//! 内没有读调用点（`load_all` 只写）。因此相关项**逐项**标 `allow(dead_code)`
//! 并注明失效时机——不要改成文件级 allow，那会把将来真正的死代码一起藏掉。

use std::collections::HashMap;

use super::manifest::PluginManifest;

/// 插件的加载状态（M2b 起五态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginState {
    /// process 插件已拉起、握手还没完成（builtin 插件不会停在态）。
    Starting,
    /// manifest 校验通过，工具已注册（或本插件不贡献工具）。
    Loaded,
    /// process 插件运行中故障（崩溃、协议错、超时杀进程），由调用路径写入。
    #[allow(dead_code)] // 提交 4a 的故障路径写入后删除
    Faulted(String),
    /// 加载失败，原因即 [`super::PluginError`] 的 Display 文本。
    Failed(String),
    /// 被用户停用（M3 的启停写这里；M2 不会构造该值）。
    #[allow(dead_code)] // M3 启停接入后删除
    Disabled,
}

/// 一个插件的台账记录。
#[derive(Debug, Clone, PartialEq)]
pub struct PluginRecord {
    pub manifest: PluginManifest,
    pub state: PluginState,
    /// 本插件成功注册的工具名；失败记录恒为空（注册回滚保证）。
    pub tools: Vec<String>,
    /// process 插件在 `initialize` 里自报的能力。**只作诊断**，不参与授权
    /// （N8：插件不能自报能力），供 M3 的扩展页显示。
    pub reported_capabilities: Vec<String>,
}

/// 插件台账（按 plugin id 索引）。
#[derive(Debug, Default)]
pub struct PluginRegistry {
    records: HashMap<String, PluginRecord>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 写入/替换一条记录（同 id 以最后一次为准）。
    pub fn insert(&mut self, record: PluginRecord) {
        self.records.insert(record.manifest.id.clone(), record);
    }

    #[allow(dead_code)] // M3 扩展页读取后删除
    pub fn get(&self, id: &str) -> Option<&PluginRecord> {
        self.records.get(id)
    }

    /// 按 id 升序列出全部记录（供 M3 扩展页消费）。
    #[allow(dead_code)] // M3 扩展页读取后删除
    pub fn list(&self) -> Vec<&PluginRecord> {
        let mut records: Vec<&PluginRecord> = self.records.values().collect();
        records.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        records
    }
}
