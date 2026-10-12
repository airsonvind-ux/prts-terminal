//! notices —— 提醒数据源（快照＝值；环境零累积；记忆事件有界）
//!
//! 持久化于 `data_dir()/notices.json`（跨重启）。**后端只做数据源**
//! （状态 + 只读拉取），不渲染、不消费、不注入。
//!
//! ## 数据形状
//!
//! - `seq`：已分配的最大序号；每次变更 +1 后分配给某个 `stamp` 或 event。
//! - `dims`：环境维度**覆写式**快照（值维度：workspace/knowledge/model/maa/emulator/game_pc；
//!   skill_catalog/tool_catalog 的 `value` 为基线清单的 JSON 串）：
//!   - `value`：当前值（空串 = 该维度无值）；
//!   - `stamp`：该维度最近一次变更的序号（从未变更 = 0）；
//!   - `note`：最近一次变更的一句话说明（值类维度 = 新值；目录 = `新增：…；移除：…`）。
//! - `events`：记忆事件（`note_memory_change`），容量 [`EVENTS_CAP`]，
//!   push 与 load 时裁最旧。
//!
//! ## 变更规则
//!
//! - **种子**（从未有记录：`value` 空且 `stamp == 0`）：值入位、`stamp` 保持 0、
//!   `note` 空、**不耗序号**；目录首启只记基线不告知。
//! - **变更**（已有值且不同）：更新 `value`、`stamp = ++seq`、`note` 如上。
//! - 同值 → 不动作。
//!
//! ## 迁移（load 一次性）
//!
//! 无 `notices.json` 而有旧 `env-ledger.json`：各环境 kind 取最新条目 → dims；
//! 其余条目（记忆类）→ events（保序、保 seq/title/detail/ts，截留最后
//! [`EVENTS_CAP`] 条）；`seq` = 旧条目最大序号；写 `notices.json`、删旧文件
//! （删除失败忽略）。两者皆无 → 空状态。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 记忆事件容量（push 与 load 时裁最旧）。
pub const EVENTS_CAP: usize = 200;

/// 单个环境维度的覆写式快照。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DimValue {
    /// 当前值（空串 = 该维度无值）
    #[serde(default)]
    pub value: String,
    /// 最近一次变更的序号（从未变更 = 0）
    #[serde(default)]
    pub stamp: u64,
    /// 最近一次变更的一句话说明
    #[serde(default)]
    pub note: String,
}

/// 环境维度（值维度 6 + 目录维度 2）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dims {
    #[serde(default)]
    pub workspace: DimValue,
    #[serde(default)]
    pub knowledge: DimValue,
    #[serde(default)]
    pub model: DimValue,
    #[serde(default)]
    pub skill_catalog: DimValue,
    #[serde(default)]
    pub tool_catalog: DimValue,
    /// 本机信息（安装态信号；见 `_本机信息-设计.md`）
    #[serde(default)]
    pub maa: DimValue,
    #[serde(default)]
    pub emulator: DimValue,
    #[serde(default)]
    pub game_pc: DimValue,
}

/// 记忆事件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeEvent {
    pub seq: u64,
    pub title: String,
    pub detail: String,
    #[serde(default)]
    pub ts: String,
}

/// 维度类别：值维度（覆写式快照，入首轮快照）／目录维度（基线驱动，不入快照）。
///
/// 供前端按类别筛选（新对话快照只放值维度）——新增维度时声明一次类别即可，
/// 名单不再需要人工维护。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimKind {
    Value,
    Catalog,
}

impl DimKind {
    /// DTO 下发用串（前端按 `"value"` 筛首轮快照）。
    pub fn as_str(self) -> &'static str {
        match self {
            DimKind::Value => "value",
            DimKind::Catalog => "catalog",
        }
    }
}

/// 维度键（固定顺序 + 标题映射）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimKey {
    Workspace,
    Knowledge,
    Model,
    SkillCatalog,
    ToolCatalog,
    Maa,
    Emulator,
    GamePc,
}

impl DimKey {
    pub fn key(self) -> &'static str {
        match self {
            DimKey::Workspace => "workspace",
            DimKey::Knowledge => "knowledge",
            DimKey::Model => "model",
            DimKey::SkillCatalog => "skill_catalog",
            DimKey::ToolCatalog => "tool_catalog",
            DimKey::Maa => "maa",
            DimKey::Emulator => "emulator",
            DimKey::GamePc => "game_pc",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            DimKey::Workspace => "工作空间",
            DimKey::Knowledge => "知识库",
            DimKey::Model => "模型",
            DimKey::SkillCatalog => "技能目录",
            DimKey::ToolCatalog => "工具目录",
            DimKey::Maa => "MAA",
            DimKey::Emulator => "模拟器",
            DimKey::GamePc => "PC 端",
        }
    }

    /// 维度类别（值 / 目录）。**新增维度必须在此声明**——前端首轮快照按此筛。
    pub fn kind(self) -> DimKind {
        match self {
            DimKey::Workspace
            | DimKey::Knowledge
            | DimKey::Model
            | DimKey::Maa
            | DimKey::Emulator
            | DimKey::GamePc => DimKind::Value,
            DimKey::SkillCatalog | DimKey::ToolCatalog => DimKind::Catalog,
        }
    }

    /// 全部维度（固定顺序）。
    pub fn all() -> [DimKey; 8] {
        [
            DimKey::Workspace,
            DimKey::Knowledge,
            DimKey::Model,
            DimKey::SkillCatalog,
            DimKey::ToolCatalog,
            DimKey::Maa,
            DimKey::Emulator,
            DimKey::GamePc,
        ]
    }
}

/// 提醒数据源。`path` 不持久化（load 注入）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Notices {
    /// 已分配的最大序号
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub dims: Dims,
    #[serde(default)]
    pub events: Vec<NoticeEvent>,
    #[serde(skip)]
    path: PathBuf,
}

impl Notices {
    /// 从 data_dir 加载：优先 `notices.json`；缺失则尝试迁移 `env-ledger.json`；
    /// 都无 → 空状态。确保 data_dir 存在。
    pub fn load(data_dir: &Path) -> Self {
        std::fs::create_dir_all(data_dir).ok();
        let path = data_dir.join("notices.json");

        if let Some(mut n) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Notices>(&t).ok())
        {
            n.path = path;
            truncate_events(&mut n.events);
            return n;
        }

        let legacy = data_dir.join("env-ledger.json");
        if let Some(old) = std::fs::read_to_string(&legacy)
            .ok()
            .and_then(|t| serde_json::from_str::<LegacyLedger>(&t).ok())
        {
            let n = migrate(old, path);
            n.persist();
            let _ = std::fs::remove_file(&legacy); // 删除失败忽略
            return n;
        }

        let mut n = Notices::default();
        n.path = path;
        n
    }

    /// 追加一个序号（返回新值）。
    fn alloc_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// 当前时间戳（unix 秒，String；无 chrono 依赖的轻量形态）。
    fn now_ts() -> String {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("{secs}")
    }

    /// 持久化写回 `data_dir()/notices.json`。失败仅 eprintln 告警。
    pub fn persist(&self) {
        if let Err(e) = std::fs::write(
            &self.path,
            serde_json::to_string_pretty(self).unwrap_or_default(),
        ) {
            eprintln!("[notices] 写入失败（{}）：{e}", self.path.display());
        }
    }

    fn dim_ref(&self, k: DimKey) -> &DimValue {
        match k {
            DimKey::Workspace => &self.dims.workspace,
            DimKey::Knowledge => &self.dims.knowledge,
            DimKey::Model => &self.dims.model,
            DimKey::SkillCatalog => &self.dims.skill_catalog,
            DimKey::ToolCatalog => &self.dims.tool_catalog,
            DimKey::Maa => &self.dims.maa,
            DimKey::Emulator => &self.dims.emulator,
            DimKey::GamePc => &self.dims.game_pc,
        }
    }

    fn dim_mut(&mut self, k: DimKey) -> &mut DimValue {
        match k {
            DimKey::Workspace => &mut self.dims.workspace,
            DimKey::Knowledge => &mut self.dims.knowledge,
            DimKey::Model => &mut self.dims.model,
            DimKey::SkillCatalog => &mut self.dims.skill_catalog,
            DimKey::ToolCatalog => &mut self.dims.tool_catalog,
            DimKey::Maa => &mut self.dims.maa,
            DimKey::Emulator => &mut self.dims.emulator,
            DimKey::GamePc => &mut self.dims.game_pc,
        }
    }

    // ── 写入接口 ──

    /// 环境值同步：workspace / knowledge / model 三个值维度。
    pub fn sync_environment_snapshots(&mut self, workspace: &str, model: &str, knowledge: &str) {
        self.sync_value_dim(DimKey::Workspace, workspace);
        self.sync_value_dim(DimKey::Knowledge, knowledge);
        self.sync_value_dim(DimKey::Model, model);
    }

    /// 本机信息同步：maa / emulator / game_pc 三个值维度（探测结果）。
    pub fn sync_machine_snapshots(&mut self, maa: &str, emulator: &str, game_pc: &str) {
        self.sync_value_dim(DimKey::Maa, maa);
        self.sync_value_dim(DimKey::Emulator, emulator);
        self.sync_value_dim(DimKey::GamePc, game_pc);
    }

    /// 单值维度同步：种子（值入位、不耗序号、note 空）／变更（stamp=++seq、note=新值）。
    fn sync_value_dim(&mut self, k: DimKey, current: &str) {
        let (seeded, differs) = {
            let d = self.dim_ref(k);
            (d.value.is_empty() && d.stamp == 0, d.value != current)
        };
        if seeded {
            let d = self.dim_mut(k);
            d.value = current.to_string();
            d.note.clear();
            self.persist();
        } else if differs {
            let s = self.alloc_seq();
            let d = self.dim_mut(k);
            d.value = current.to_string();
            d.stamp = s;
            d.note = current.to_string();
            self.persist();
        }
    }

    /// 模型配置变化（set_settings 检测到 provider/model 变化时调用）：
    /// 等价于对 model 维度的值同步。
    pub fn note_model_change(&mut self, label: &str) {
        self.sync_value_dim(DimKey::Model, label);
    }

    /// 工作空间变化（set_settings 检测到 workspace 变化时调用）：
    /// 等价于对 workspace 维度的值同步。
    ///
    /// **必须调用**：对话前缀冻结，环境变化只能靠 reminder 通知模型——不在这里
    /// 同步，模型就会拿着旧工作目录做事，且新对话（值维度全量）会拿到旧值。
    pub fn note_workspace_change(&mut self, workspace: &str) {
        self.sync_value_dim(DimKey::Workspace, workspace);
    }

    /// 记录记忆事件（成长写入 / 提炼 / 压缩 / 整层清除）：push 一条并裁到容量上限。
    pub fn note_memory_change(&mut self, title: &str, detail: &str) {
        let seq = self.alloc_seq();
        self.events.push(NoticeEvent {
            seq,
            title: title.to_string(),
            detail: detail.to_string(),
            ts: Self::now_ts(),
        });
        truncate_events(&mut self.events);
        self.persist();
    }

    /// 技能目录差异（(name, desc) 对：描述变化可捕捉；文案与目录组装同构）。
    pub fn diff_skill_catalog(&mut self, skills: &[(String, String)]) {
        self.diff_catalog_pairs(DimKey::SkillCatalog, skills);
    }

    /// 技能目录差异：首启只记基线（不告知、不耗序号）；有差异 →
    /// stamp=++seq、note=差异（新增 / 更新条目按 `- 名称：描述` 目录行格式，
    /// 移除列名）。
    fn diff_catalog_pairs(&mut self, k: DimKey, skills: &[(String, String)]) {
        let last = pairs_from_value(&self.dim_ref(k).value);
        let seeded = self.dim_ref(k).value.is_empty() && self.dim_ref(k).stamp == 0;
        let mut current: Vec<(String, String)> = skills.to_vec();
        current.sort();
        current.dedup_by(|a, b| a.0 == b.0);
        if seeded {
            let d = self.dim_mut(k);
            d.value = pairs_to_value(&current);
            d.note.clear();
            self.persist();
            return;
        }
        let (detail, new_base) = catalog_pairs_diff(&current, &last);
        match detail {
            Some(note) => {
                let s = self.alloc_seq();
                let d = self.dim_mut(k);
                d.value = pairs_to_value(&new_base);
                d.stamp = s;
                d.note = note;
                self.persist();
            }
            None => {
                let d = self.dim_mut(k);
                d.value = pairs_to_value(&new_base);
                self.persist();
            }
        }
    }

    /// 工具目录差异（(name, desc) 对，与技能目录同构）：描述变化也能捕捉，
    /// 应用升级带来的工具改动才说得清（只比名字会漏掉“名字未变、描述改了”）。
    pub fn diff_tool_catalog(&mut self, tools: &[(String, String)]) {
        self.diff_catalog_pairs(DimKey::ToolCatalog, tools);
    }

    // ── 只读接口 ──

    /// 已分配的最大序号。
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// 记忆事件（只读）。
    pub fn events(&self) -> &[NoticeEvent] {
        &self.events
    }

    /// 维度视图：固定顺序，`(key, dim)`。调用方经 `DimKey` 取 `key()/title()/kind()`，
    /// 并自行过滤（value 空的不出现）。
    pub fn dim_views(&self) -> Vec<(DimKey, &DimValue)> {
        DimKey::all()
            .into_iter()
            .map(|k| (k, self.dim_ref(k)))
            .collect()
    }
}

/// 裁到 [`EVENTS_CAP`]（保留最新）。
fn truncate_events(events: &mut Vec<NoticeEvent>) {
    if events.len() > EVENTS_CAP {
        let excess = events.len() - EVENTS_CAP;
        events.drain(0..excess);
    }
}

/// 模型配置展示 label（provider/model 拼接；双空 = 未配置）。
pub fn model_label(provider: &str, model: &str) -> String {
    let p = provider.trim();
    let m = model.trim();
    if p.is_empty() && m.is_empty() {
        "未配置".to_string()
    } else {
        format!("{p}/{m}")
    }
}

fn baseline_to_value(baseline: &[String]) -> String {
    serde_json::to_string(baseline).unwrap_or_else(|_| "[]".to_string())
}

/// 技能基线（(name, desc) 对的 JSON）→ 数组；非法/空 → 空。
/// 兼容旧格式（仅名字数组）：desc 视为未知（空串），不参与“更新”判定。
fn pairs_from_value(value: &str) -> Vec<(String, String)> {
    let v = value.trim();
    if v.is_empty() {
        return Vec::new();
    }
    if let Ok(pairs) = serde_json::from_str::<Vec<(String, String)>>(v) {
        return pairs;
    }
    serde_json::from_str::<Vec<String>>(v)
        .unwrap_or_default()
        .into_iter()
        .map(|n| (n, String::new()))
        .collect()
}

fn pairs_to_value(pairs: &[(String, String)]) -> String {
    serde_json::to_string(pairs).unwrap_or_else(|_| "[]".to_string())
}

/// 计算 (name, desc) 目录差异（纯函数）：返回 (变化详情, 更新后基线)。
/// - 首启（last 为空）：不写详情，仅返回新基线
/// - 无差异：None 详情（desc 回填静默进行）
/// - 有差异：新增 / 更新条目按 `- 名称：描述` 目录行格式；移除列名
/// - 旧基线 desc 为空（格式迁移中）不参与“更新”判定，防误报
fn catalog_pairs_diff(
    current: &[(String, String)],
    last: &[(String, String)],
) -> (Option<String>, Vec<(String, String)>) {
    if last.is_empty() {
        return (None, current.to_vec());
    }

    let added: Vec<&(String, String)> = current
        .iter()
        .filter(|(n, _)| !last.iter().any(|(ln, _)| ln == n))
        .collect();
    let removed: Vec<&str> = last
        .iter()
        .filter(|(ln, _)| !current.iter().any(|(n, _)| n == ln))
        .map(|(ln, _)| ln.as_str())
        .collect();
    let updated: Vec<&(String, String)> = current
        .iter()
        .filter(|(n, d)| {
            last.iter()
                .any(|(ln, ld)| ln == n && !ld.is_empty() && ld != d)
        })
        .collect();

    if added.is_empty() && removed.is_empty() && updated.is_empty() {
        return (None, current.to_vec());
    }

    let mut parts: Vec<String> = Vec::new();
    if !added.is_empty() {
        let mut seg = String::from("新增：");
        for (n, d) in &added {
            seg.push_str(&format!("\n- {n}：{d}"));
        }
        parts.push(seg);
    }
    if !updated.is_empty() {
        let mut seg = String::from("更新：");
        for (n, d) in &updated {
            seg.push_str(&format!("\n- {n}：{d}"));
        }
        parts.push(seg);
    }
    if !removed.is_empty() {
        parts.push(format!("移除：{}", removed.join("、")));
    }
    (Some(parts.join("\n")), current.to_vec())
}

// ═══════════════════════════════════════════════════════
// 旧格式（env-ledger.json）——仅迁移读取用
// ═══════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
struct LegacyLedger {
    #[serde(default)]
    entries: Vec<LegacyEntry>,
    #[serde(default)]
    last_skill_catalog: Vec<String>,
    #[serde(default)]
    last_tool_catalog: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct LegacyEntry {
    seq: u64,
    kind: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    detail: String,
    #[serde(default)]
    ts: String,
}

/// 旧台账 → 新结构（各环境 kind 取最新 → dims；其余 → events；seq = 旧最大）。
fn migrate(old: LegacyLedger, path: PathBuf) -> Notices {
    let mut n = Notices::default();
    n.path = path;

    for (kind, key) in [
        ("workspace", DimKey::Workspace),
        ("knowledge", DimKey::Knowledge),
        ("model", DimKey::Model),
    ] {
        if let Some(e) = old.entries.iter().filter(|e| e.kind == kind).max_by_key(|e| e.seq) {
            let d = n.dim_mut(key);
            d.value = e.detail.clone();
            d.stamp = e.seq;
            d.note = e.detail.clone();
        }
    }

    for (kind, key, base) in [
        ("skill_catalog", DimKey::SkillCatalog, &old.last_skill_catalog),
        ("tool_catalog", DimKey::ToolCatalog, &old.last_tool_catalog),
    ] {
        let latest = old.entries.iter().filter(|e| e.kind == kind).max_by_key(|e| e.seq);
        let note = latest.map(|e| e.detail.clone());
        let stamp = latest.map(|e| e.seq);
        let d = n.dim_mut(key);
        d.value = baseline_to_value(base);
        if let Some(s) = stamp {
            d.stamp = s;
        }
        if let Some(v) = note {
            d.note = v;
        }
    }

    let mut events: Vec<NoticeEvent> = old
        .entries
        .iter()
        .filter(|e| {
            !matches!(
                e.kind.as_str(),
                "workspace" | "knowledge" | "model" | "skill_catalog" | "tool_catalog"
            )
        })
        .map(|e| NoticeEvent {
            seq: e.seq,
            title: e.title.clone(),
            detail: e.detail.clone(),
            ts: e.ts.clone(),
        })
        .collect();
    events.sort_by_key(|e| e.seq);
    truncate_events(&mut events);
    n.events = events;
    n.seq = old.entries.iter().map(|e| e.seq).max().unwrap_or(0);
    n
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-notices-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn seed_sets_values_without_consuming_seq() {
        let dir = unique_temp_dir("seed");
        let mut n = Notices::load(&dir);
        n.sync_environment_snapshots("/ws", "m", "/kb");

        assert_eq!(n.seq(), 0, "种子不耗序号");
        assert_eq!(n.dims.workspace.value, "/ws");
        assert_eq!(n.dims.workspace.stamp, 0);
        assert!(n.dims.workspace.note.is_empty());
        assert_eq!(n.dims.knowledge.value, "/kb");
        assert_eq!(n.dims.model.value, "m");
        assert!(dir.join("notices.json").is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn change_updates_value_stamp_note_only_when_different() {
        let dir = unique_temp_dir("change");
        let mut n = Notices::load(&dir);
        n.sync_environment_snapshots("/wsA", "m", "/kb");
        // 同值 → 不动作
        n.sync_environment_snapshots("/wsA", "m", "/kb");
        assert_eq!(n.seq(), 0, "种子 + 同值不耗序号");

        // workspace 变化 → seq=1，stamp/note=新值
        n.sync_environment_snapshots("/wsC", "m", "/kb");
        assert_eq!(n.seq(), 1);
        assert_eq!(n.dims.workspace.value, "/wsC");
        assert_eq!(n.dims.workspace.stamp, 1);
        assert_eq!(n.dims.workspace.note, "/wsC");
        // model 未变 → stamp 仍 0
        assert_eq!(n.dims.model.stamp, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn note_model_change_allocates_seq() {
        let dir = unique_temp_dir("model");
        let mut n = Notices::load(&dir);
        n.sync_environment_snapshots("/ws", "old/model", "/kb");
        n.note_model_change("new/model-v2");
        assert_eq!(n.seq(), 1);
        assert_eq!(n.dims.model.value, "new/model-v2");
        assert_eq!(n.dims.model.stamp, 1);
        assert_eq!(n.dims.model.note, "new/model-v2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn note_workspace_change_allocates_seq_and_is_idempotent() {
        // 工作目录热生效的告知面：改了就必须占一个序号（否则进行中的对话收不到
        // 提醒、新对话会拿到旧值）；同值不耗序号。
        let dir = unique_temp_dir("wschange");
        let mut n = Notices::load(&dir);
        n.sync_environment_snapshots("/wsA", "m", "/kb"); // seed
        n.note_workspace_change("/wsB");
        assert_eq!(n.seq(), 1);
        assert_eq!(n.dims.workspace.value, "/wsB");
        assert_eq!(n.dims.workspace.stamp, 1);
        assert_eq!(n.dims.workspace.note, "/wsB");
        // 其余维度不受影响
        assert_eq!(n.dims.model.stamp, 0);
        assert_eq!(n.dims.knowledge.stamp, 0);
        // 同值幂等
        n.note_workspace_change("/wsB");
        assert_eq!(n.seq(), 1, "同值不应再耗序号");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn machine_snapshots_seed_then_change() {
        let dir = unique_temp_dir("machine");
        let mut n = Notices::load(&dir);
        n.sync_machine_snapshots("已安装", "MuMu 12", "B服");
        assert_eq!(n.seq(), 0, "种子不耗序号");
        assert_eq!(n.dims.maa.value, "已安装");
        assert_eq!(n.dims.emulator.value, "MuMu 12");
        assert_eq!(n.dims.game_pc.value, "B服");

        n.sync_environment_snapshots("/ws", "m", "/kb");
        n.sync_machine_snapshots("已安装", "MuMu 12", "官服；B服");
        assert_eq!(n.seq(), 1);
        assert_eq!(n.dims.game_pc.value, "官服；B服");
        assert_eq!(n.dims.game_pc.stamp, 1);
        assert_eq!(n.dims.game_pc.note, "官服；B服");
        assert_eq!(n.dims.maa.stamp, 0, "未变的维度不动");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dim_kind_classifies_value_and_catalog() {
        assert_eq!(DimKey::Workspace.kind(), DimKind::Value);
        assert_eq!(DimKey::Maa.kind(), DimKind::Value);
        assert_eq!(DimKey::Emulator.kind(), DimKind::Value);
        assert_eq!(DimKey::GamePc.kind(), DimKind::Value);
        assert_eq!(DimKey::SkillCatalog.kind(), DimKind::Catalog);
        assert_eq!(DimKey::ToolCatalog.kind(), DimKind::Catalog);
        assert_eq!(DimKey::all().len(), 8);
        let values = DimKey::all()
            .into_iter()
            .filter(|k| k.kind() == DimKind::Value)
            .count();
        assert_eq!(values, 6, "值维度六个（前端首轮快照即这批）");
    }

    #[test]
    fn catalog_first_run_records_baseline_only() {
        let dir = unique_temp_dir("sk-base");
        let mut n = Notices::load(&dir);
        n.diff_skill_catalog(&[
            ("推演分析".into(), "推演规则".into()),
            ("数值计算".into(), "计算规则".into()),
        ]);
        assert_eq!(n.seq(), 0, "首启不耗序号");
        assert_eq!(n.dims.skill_catalog.stamp, 0);
        assert!(n.dims.skill_catalog.note.is_empty());
        assert_eq!(
            pairs_from_value(&n.dims.skill_catalog.value),
            vec![
                ("推演分析".to_string(), "推演规则".to_string()),
                ("数值计算".to_string(), "计算规则".to_string())
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_diff_second_run_allocates_and_notes() {
        let dir = unique_temp_dir("sk-diff");
        {
            let mut n = Notices::load(&dir);
            n.diff_skill_catalog(&[("A".into(), "da".into()), ("B".into(), "db".into())]);
        }
        {
            let mut n = Notices::load(&dir);
            // 乱序自防御；新增按目录行格式、移除列名
            n.diff_skill_catalog(&[("C".into(), "dc".into()), ("A".into(), "da".into())]);
            assert_eq!(n.seq(), 1);
            assert_eq!(n.dims.skill_catalog.stamp, 1);
            assert_eq!(n.dims.skill_catalog.note, "新增：\n- C：dc\n移除：B");
            assert_eq!(
                pairs_from_value(&n.dims.skill_catalog.value),
                vec![
                    ("A".to_string(), "da".to_string()),
                    ("C".to_string(), "dc".to_string())
                ]
            );
        }
        {
            let mut n = Notices::load(&dir);
            n.diff_skill_catalog(&[("A".into(), "da".into()), ("C".into(), "dc".into())]);
            assert_eq!(n.seq(), 1, "无差异不耗序号");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_desc_change_reports_update_with_new_line() {
        let dir = unique_temp_dir("sk-upd");
        {
            let mut n = Notices::load(&dir);
            n.diff_skill_catalog(&[("A".into(), "旧描述".into())]);
        }
        {
            let mut n = Notices::load(&dir);
            n.diff_skill_catalog(&[("A".into(), "新描述".into())]);
            assert_eq!(n.seq(), 1);
            assert_eq!(n.dims.skill_catalog.note, "更新：\n- A：新描述");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_legacy_names_baseline_migrates_without_false_positives() {
        // 旧格式基线（仅名字数组）→ desc 视为未知：不误报“更新”，基线升级为对
        let dir = unique_temp_dir("sk-legacy");
        let mut n = Notices::load(&dir);
        n.dims.skill_catalog.value = "[\"A\",\"B\"]".to_string();
        n.persist();
        n.diff_skill_catalog(&[("A".into(), "da".into()), ("B".into(), "db".into())]);
        assert_eq!(n.seq(), 0, "仅格式迁移不算差异");
        assert_eq!(
            pairs_from_value(&n.dims.skill_catalog.value),
            vec![
                ("A".to_string(), "da".to_string()),
                ("B".to_string(), "db".to_string())
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_catalog_diff() {
        let dir = unique_temp_dir("tool");
        {
            let mut n = Notices::load(&dir);
            n.diff_tool_catalog(&[("read_file".into(), "读取文件".into())]);
        }
        {
            let mut n = Notices::load(&dir);
            n.diff_tool_catalog(&[
                ("read_file".into(), "读取文件".into()),
                ("write_file".into(), "写入文件".into()),
                ("sim_path".into(), "推演路径".into()),
            ]);
            assert_eq!(n.seq(), 1);
            assert_eq!(
                n.dims.tool_catalog.note,
                "新增：\n- sim_path：推演路径\n- write_file：写入文件"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn events_allocate_seq_and_truncate_to_cap() {
        let dir = unique_temp_dir("events");
        let mut n = Notices::load(&dir);
        for i in 0..(EVENTS_CAP + 5) {
            n.note_memory_change("记忆", &format!("变化{i}"));
        }
        assert_eq!(n.seq(), (EVENTS_CAP + 5) as u64);
        assert_eq!(n.events().len(), EVENTS_CAP, "裁到容量上限");
        // 保留最新（最旧被裁）
        assert_eq!(n.events()[0].detail, "变化5");
        assert_eq!(n.events()[EVENTS_CAP - 1].detail, format!("变化{}", EVENTS_CAP + 4));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seq_is_continuous_across_value_and_event() {
        let dir = unique_temp_dir("seq");
        let mut n = Notices::load(&dir);
        n.sync_environment_snapshots("/ws", "m", "/kb"); // seed
        n.note_memory_change("记忆", "a"); // seq=1
        n.sync_environment_snapshots("/ws2", "m", "/kb"); // seq=2
        n.note_memory_change("记忆", "b"); // seq=3
        assert_eq!(n.seq(), 3);
        assert_eq!(n.events()[0].seq, 1);
        assert_eq!(n.events()[1].seq, 3);
        assert_eq!(n.dims.workspace.stamp, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_roundtrip() {
        let dir = unique_temp_dir("roundtrip");
        {
            let mut n = Notices::load(&dir);
            n.sync_environment_snapshots("/ws", "deepseek/m", "/kb");
            n.diff_tool_catalog(&[("a".into(), "甲工具".into())]);
            n.diff_tool_catalog(&[("a".into(), "甲工具".into()), ("b".into(), "乙工具".into())]);
            n.note_memory_change("记忆", "博士新建了成长页「甲」");
        }
        let n = Notices::load(&dir);
        assert_eq!(n.seq(), 2, "tool diff(1) + event(2)");
        assert_eq!(n.dims.workspace.value, "/ws");
        assert_eq!(n.dims.tool_catalog.note, "新增：\n- b：乙工具");
        assert_eq!(n.events().len(), 1);
        assert_eq!(n.events()[0].title, "记忆");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_truncates_oversized_events() {
        let dir = unique_temp_dir("load-cap");
        // 手工写入超容量 events
        let events: Vec<NoticeEvent> = (1..=(EVENTS_CAP as u64 + 3))
            .map(|s| NoticeEvent {
                seq: s,
                title: "记忆".into(),
                detail: format!("e{s}"),
                ts: "0".into(),
            })
            .collect();
        let json = serde_json::json!({ "seq": EVENTS_CAP + 3, "dims": {}, "events": events });
        std::fs::write(dir.join("notices.json"), json.to_string()).unwrap();

        let n = Notices::load(&dir);
        assert_eq!(n.events().len(), EVENTS_CAP);
        assert_eq!(n.events()[0].seq, 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrate_legacy_ledger() {
        let dir = unique_temp_dir("migrate");
        // 造旧格式：workspace 两条（旧→新）、model 一条、memory_facts 两条、工具基线
        let legacy = serde_json::json!({
            "entries": [
                {"seq":1,"kind":"workspace","title":"工作空间","detail":"/wsA","ts":"1"},
                {"seq":2,"kind":"knowledge","title":"知识库","detail":"/kb","ts":"2"},
                {"seq":3,"kind":"model","title":"模型","detail":"m","ts":"3"},
                {"seq":4,"kind":"workspace","title":"工作空间","detail":"/wsC","ts":"4"},
                {"seq":5,"kind":"memory_facts","title":"记忆","detail":"旧流水1","ts":"5"},
                {"seq":6,"kind":"memory_facts","title":"记忆","detail":"旧流水2","ts":"6"},
                {"seq":7,"kind":"tool_catalog","title":"工具目录","detail":"新增：x","ts":"7"}
            ],
            "last_skill_catalog": ["A","B"],
            "last_tool_catalog": ["x"]
        });
        std::fs::write(dir.join("env-ledger.json"), legacy.to_string()).unwrap();

        let n = Notices::load(&dir);
        // dims：workspace 取最新（/wsC, stamp 4）
        assert_eq!(n.dims.workspace.value, "/wsC");
        assert_eq!(n.dims.workspace.stamp, 4);
        assert_eq!(n.dims.workspace.note, "/wsC");
        assert_eq!(n.dims.model.value, "m");
        assert_eq!(n.dims.model.stamp, 3);
        // 目录：基线数组 + 最新条目 seq/note（旧格式迁移后基线仍可读）
        assert!(
            n.dims.skill_catalog.value.contains("\"A\"")
                && n.dims.skill_catalog.value.contains("\"B\""),
            "迁移后保留技能基线：{}",
            n.dims.skill_catalog.value
        );
        assert_eq!(n.dims.skill_catalog.stamp, 0, "无 skill 条目 → stamp 0");
        assert_eq!(n.dims.tool_catalog.stamp, 7);
        assert_eq!(n.dims.tool_catalog.note, "新增：x");
        // events：仅记忆类，保序保 seq
        assert_eq!(n.events().len(), 2);
        assert_eq!(n.events()[0].seq, 5);
        assert_eq!(n.events()[1].detail, "旧流水2");
        // seq = 旧最大
        assert_eq!(n.seq(), 7);
        // 新文件已写、旧文件已删
        assert!(dir.join("notices.json").is_file());
        assert!(!dir.join("env-ledger.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_when_both_files_absent() {
        let dir = unique_temp_dir("empty");
        let n = Notices::load(&dir);
        assert_eq!(n.seq(), 0);
        assert!(n.events().is_empty());
        assert!(n.dims.workspace.value.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
