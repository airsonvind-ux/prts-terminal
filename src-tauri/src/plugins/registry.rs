//! 插件台账：记录「加载了谁、状态如何、贡献了哪些工具」。
//!
//! 不落盘：启停状态与插件配置的持久化是 M3 的事（D18 把配置与启停状态分两处）。
//!
//! 台账从 M2b 起由 `Arc<Mutex<…>>` 持有：加载路径写 `Loaded`/`Failed`，调用路径
//! 写 `Faulted`。`get` / `list` 仍只供 M3 的扩展页与诊断消费，本里程碑内没有
//! 生产读调用点，因此**逐项**标 `allow(dead_code)` 并注明失效时机——不要改成
//! 文件级 allow，那会把将来真正的死代码一起藏掉。

use std::collections::HashMap;
use std::path::PathBuf;

use super::manifest::PluginManifest;

/// 插件的加载状态（M2b 起五态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginState {
    /// process 插件已拉起、握手还没完成（builtin 插件不会停在态）。
    Starting,
    /// manifest 校验通过，工具已注册（或本插件不贡献工具）。
    Loaded,
    /// process 插件运行中故障（崩溃、协议错、超时杀进程），由调用路径写入。
    Faulted(String),
    /// 加载失败，原因即 [`super::PluginError`] 的 Display 文本。
    Failed(String),
    /// 被用户停用：`plugins.json` 里写了 `false`，加载期就不注册工具（M3 的
    /// 扩展页负责改写这个文件）。
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
    /// 这个插件实际被读出来的目录。重新启用/重试要按原路径再装一次，
    /// 而目录名允许与 id 不同（校验规则 2 只告警），所以不能拿 id 拼。
    pub dir: PathBuf,
}

/// 一个「有目录但没有 manifest」的插件目录。
///
/// 它进不了台账（没有 id），但用户应该看得见——否则装错位置的插件会静默消失。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnregisteredDir {
    pub dir_name: String,
    pub reason: String,
}

/// 插件台账（按 plugin id 索引）。
#[derive(Debug, Default)]
pub struct PluginRegistry {
    records: HashMap<String, PluginRecord>,
    /// 读不出 manifest 的目录：只用于展示，不参与加载与启停。
    unregistered: Vec<UnregisteredDir>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 写入/替换一条记录（同 id 以最后一次为准）。
    pub fn insert(&mut self, record: PluginRecord) {
        self.records.insert(record.manifest.id.clone(), record);
    }

    /// 把一条已加载的记录标成故障（调用路径发现进程没了时写这里）。
    ///
    /// 记录不在台账里就什么都不做：加载阶段的失败已经在 `insert` 时落过
    /// `Failed`，这里只处理「装载之后才坏」的情况。
    pub fn mark_faulted(&mut self, id: &str, reason: String) {
        if let Some(record) = self.records.get_mut(id) {
            record.state = PluginState::Faulted(reason);
        }
    }

    /// 重启成功后把记录从 `Faulted` 拉回 `Loaded`。
    pub fn mark_loaded(&mut self, id: &str) {
        if let Some(record) = self.records.get_mut(id) {
            record.state = PluginState::Loaded;
        }
    }

    /// 重新启用成功后写回工具面（停用时这些名字被清空了）。
    pub fn mark_enabled(&mut self, id: &str, tools: Vec<String>, reported: Vec<String>) {
        if let Some(record) = self.records.get_mut(id) {
            record.state = PluginState::Loaded;
            record.tools = tools;
            record.reported_capabilities = reported;
        }
    }

    /// 重新启用失败：状态回落 `Failed`，工具面已被安装路径回滚清空。
    pub fn mark_failed(&mut self, id: &str, reason: String) {
        if let Some(record) = self.records.get_mut(id) {
            record.state = PluginState::Failed(reason);
            record.tools.clear();
        }
    }

    /// 停用：状态记 `Disabled`，工具面清空（工具已经从注册表里摘掉了）。
    pub fn mark_disabled(&mut self, id: &str) {
        if let Some(record) = self.records.get_mut(id) {
            record.state = PluginState::Disabled;
            record.tools.clear();
        }
    }

    /// 卸载：把这条记录整个摘掉。
    pub fn remove(&mut self, id: &str) -> Option<PluginRecord> {
        self.records.remove(id)
    }

    /// 记下一个读不出 manifest 的目录（重复目录名只留第一次）。
    pub fn insert_unregistered(&mut self, dir_name: &str, reason: &str) {
        if self
            .unregistered
            .iter()
            .any(|dir| dir.dir_name == dir_name)
        {
            return;
        }
        self.unregistered.push(UnregisteredDir {
            dir_name: dir_name.to_string(),
            reason: reason.to_string(),
        });
    }

    /// 读不出 manifest 的目录（按目录名排序，展示用）。
    pub fn unregistered(&self) -> Vec<&UnregisteredDir> {
        let mut dirs: Vec<&UnregisteredDir> = self.unregistered.iter().collect();
        dirs.sort_by(|a, b| a.dir_name.cmp(&b.dir_name));
        dirs
    }

    #[allow(dead_code)] // 生产路径只用 list；这里的 get 留给单元测试与诊断
    pub fn get(&self, id: &str) -> Option<&PluginRecord> {
        self.records.get(id)
    }

    /// 按 id 升序列出全部记录（供扩展页消费）。
    pub fn list(&self) -> Vec<&PluginRecord> {
        let mut records: Vec<&PluginRecord> = self.records.values().collect();
        records.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        records
    }
}
