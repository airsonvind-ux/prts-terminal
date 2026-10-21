//! memory::store —— 记忆存储（盘上加载 + 内存索引 + 读写）
//!
//! ## 目录布局（`data_dir()/memory/`）
//!
//! ```text
//! memory/
//!   profile.md            # 「关于博士」页（成长层，与专题页同构）
//!   topics/{slug}.md      # 专题页
//!   experience.jsonl      # 经历条目（append-only）
//!   longterm.md           # 「先前」：压缩后的长期经历摘要
//!   state.json            # 水位 / 压缩游标 / 冷却
//! ```
//!
//! ## 单一写者边界（对齐方案 §4）
//!
//! 成长层（profile / topics）由 `growth_*` 方法写（工具与用户）；经历层
//! （experience.jsonl）由后台经历管线经 `apply_ops` / `mark_absorbed` 写。
//!
//! ## 页路径归属（批 1 审查修复 🟡2）
//!
//! 页的实际文件路径在**加载时记录**（`topic_files`）、**创建时决策**并记入映射；
//! 此后读/改/删一律以映射为准，不再按"磁盘当前状态"反推——避免清洗撞名
//! （如 `A/B`→`A_B.md` 与 `A_B`→`A_B-<hash>.md`）导致指向错误路径。
//!
//! ## 失败/自愈
//!
//! `profile.md` 存在但解析失败 → 先备份 `profile.md.bak` 再降级为空页
//! （对齐 settings 损坏备份惯例；防下次写入覆盖原正文）。`load` 对缺失文件
//! 宽容；单页/单行解析失败跳过并告警。写盘一律 tmp + rename 原子替换。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::memory::schema::{
    now_ms, CompressionState, ExperienceEntry, GrowthPage, MemoryState, PROFILE_NAME,
};

/// 目录内固定文件名。
pub const PROFILE_FILE: &str = "profile.md";
pub const TOPICS_DIR: &str = "topics";
pub const EXPERIENCE_FILE: &str = "experience.jsonl";
pub const LONGTERM_FILE: &str = "longterm.md";
pub const STATE_FILE: &str = "state.json";

// ═══════════════════════════════════════════════════════
// 经历操作（提炼输出 → 存储指令）
// ═══════════════════════════════════════════════════════

/// 条目指纹（重写守卫）：写入前比对"读取后是否被改动"（按现字段计算）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    pub ts: u64,
    pub digest: u64,
}

/// 指纹摘要：按现字段（标题 / 状态句 / 标签）计算。
fn digest_of(title: &str, summary: &str, tags: &[String]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let step = |h: &mut u64, bytes: &[u8]| {
        for b in bytes {
            *h ^= *b as u64;
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        *h ^= 0u64;
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    step(&mut h, title.as_bytes());
    step(&mut h, summary.as_bytes());
    for t in tags {
        step(&mut h, t.as_bytes());
    }
    h
}

fn fingerprint_of_entry(e: &ExperienceEntry) -> Fingerprint {
    Fingerprint {
        ts: e.ts,
        digest: digest_of(&e.title, &e.summary, &e.tags),
    }
}

/// 提炼操作（管线产出 → 存储应用）。无正文层。
#[derive(Debug, Clone, PartialEq)]
pub enum MemoryOp {
    /// 新建条目
    Create {
        title: String,
        summary: String,
        tags: Vec<String>,
    },
    /// 重写条目：**覆盖**标题/状态句/标签、重写计数 +1、延长时间范围（带指纹守卫）
    Rewrite {
        target_id: String,
        expected: Fingerprint,
        title: String,
        summary: String,
        tags: Vec<String>,
    },
}

/// `apply_ops` 结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub created: usize,
    pub rewritten: usize,
    /// 重写守卫命中（目标已改动/已吸收/已封存）→ 降级为新建的条数
    pub degraded: usize,
    /// 本次所涉条目 id（新建 + 重写，处理顺序）——提醒内容据此列出条目行
    pub affected: Vec<String>,
}

// ═══════════════════════════════════════════════════════
// MemoryStore
// ═══════════════════════════════════════════════════════

/// 记忆存储：盘上加载后的内存权威副本 + 写路径。
pub struct MemoryStore {
    root: PathBuf,
    profile: GrowthPage,
    /// 专题页，按 name 字典序（注入顺序确定 → 提示词可复现）
    topics: Vec<GrowthPage>,
    /// 专题页实际文件路径（name → path）；加载时记录、创建时决策
    topic_files: HashMap<String, PathBuf>,
    experiences: Vec<ExperienceEntry>,
    state: MemoryState,
    longterm: String,
}

impl MemoryStore {
    /// 从 `data_dir()` 加载（建 `memory/` 与 `topics/`）。
    pub fn load(data_dir: &Path) -> Result<Self, String> {
        let root = data_dir.join("memory");
        std::fs::create_dir_all(&root)
            .map_err(|e| format!("无法创建记忆目录（{}）：{e}", root.display()))?;
        std::fs::create_dir_all(root.join(TOPICS_DIR)).ok();

        // profile：存在但解析失败 → 备份 .bak 后降级为空页
        let profile_path = root.join(PROFILE_FILE);
        let profile = match read_page(&profile_path) {
            Some(p) => p,
            None => {
                if profile_path.exists() {
                    let bak = root.join("profile.md.bak");
                    if let Err(e) = std::fs::copy(&profile_path, &bak) {
                        eprintln!("[memory] profile 损坏备份失败（{}）：{e}", bak.display());
                    } else {
                        eprintln!(
                            "[memory] profile.md 解析失败，已备份为 {} 并降级为空页",
                            bak.display()
                        );
                    }
                }
                GrowthPage::empty_profile(now_ms())
            }
        };

        let mut topics: Vec<GrowthPage> = Vec::new();
        let mut topic_files: HashMap<String, PathBuf> = HashMap::new();
        if let Ok(rd) = std::fs::read_dir(root.join(TOPICS_DIR)) {
            let mut paths: Vec<PathBuf> = rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false))
                .collect();
            paths.sort();
            for p in paths {
                match read_page(&p) {
                    Some(page) => {
                        if topic_files.contains_key(&page.name) {
                            eprintln!("[memory] 重名专题页已忽略（保留先注册的）：{}", p.display());
                            continue;
                        }
                        topic_files.insert(page.name.clone(), p);
                        topics.push(page);
                    }
                    None => eprintln!("[memory] 跳过无法解析的专题页：{}", p.display()),
                }
            }
        }
        topics.sort_by(|a, b| a.name.cmp(&b.name));

        let experiences = read_experiences(&root.join(EXPERIENCE_FILE));
        let state = read_state(&root.join(STATE_FILE));
        let longterm = std::fs::read_to_string(root.join(LONGTERM_FILE)).unwrap_or_default();

        Ok(Self {
            root,
            profile,
            topics,
            topic_files,
            experiences,
            state,
            longterm,
        })
    }

    /// 启动降级用：目录不可用时的空存储（不落盘、不 panic）。
    pub fn empty(root: PathBuf) -> Self {
        Self {
            root,
            profile: GrowthPage::empty_profile(now_ms()),
            topics: Vec::new(),
            topic_files: HashMap::new(),
            experiences: Vec::new(),
            state: MemoryState::default(),
            longterm: String::new(),
        }
    }

    /// 记忆根目录（观测/诊断用）。
    #[allow(dead_code)] // 诊断/任务四消费
    pub fn root(&self) -> &Path {
        &self.root
    }

    // ── 只读访问 ──

    pub fn profile(&self) -> &GrowthPage {
        &self.profile
    }

    pub fn topics(&self) -> &[GrowthPage] {
        &self.topics
    }

    #[allow(dead_code)] // 任务四总览/统计消费
    pub fn experiences(&self) -> &[ExperienceEntry] {
        &self.experiences
    }

    /// 全部经历条目 id（清除后核对等用）。
    #[allow(dead_code)]
    pub fn experience_ids(&self) -> Vec<String> {
        self.experiences.iter().map(|e| e.id.clone()).collect()
    }

    pub fn longterm(&self) -> &str {
        &self.longterm
    }

    /// 长期记忆版本号（观测/任务四消费）。
    #[allow(dead_code)]
    pub fn longterm_version(&self) -> u64 {
        self.state.compression.longterm_version
    }

    /// 未吸收短期条目数（"到 10 压 5"计数口径）。
    pub fn recent_line_count(&self) -> u64 {
        self.experiences
            .iter()
            .filter(|e| e.is_recent_candidate())
            .count() as u64
    }

    /// 按名取成长页（「关于博士」 → profile；其余 → 专题页）。
    pub fn growth_get(&self, name: &str) -> Option<&GrowthPage> {
        if name == PROFILE_NAME {
            return Some(&self.profile);
        }
        self.topics.iter().find(|p| p.name == name)
    }

    /// 未吸收的短期经历，按时间升序取最近 k 条（提示词「近期」素材）。
    pub fn recent_unabsorbed(&self, k: usize) -> Vec<&ExperienceEntry> {
        let mut v: Vec<&ExperienceEntry> = self
            .experiences
            .iter()
            .filter(|e| e.is_recent_candidate())
            .collect();
        v.sort_by_key(|e| e.ts);
        if v.len() > k {
            v.drain(0..v.len() - k);
        }
        v
    }

    /// 未吸收短期经历，全部按时间升序（保留：近期计数/旧调用）。
    pub fn unabsorbed_short_asc(&self) -> Vec<&ExperienceEntry> {
        let mut v: Vec<&ExperienceEntry> = self
            .experiences
            .iter()
            .filter(|e| e.is_recent_candidate())
            .collect();
        v.sort_by_key(|e| e.ts);
        v
    }

    /// **可重写组**（未吸收且未封存），按时间升序（提炼材料编号组）。
    pub fn unabsorbed_rewritable(&self) -> Vec<&ExperienceEntry> {
        let mut v: Vec<&ExperienceEntry> = self
            .experiences
            .iter()
            .filter(|e| e.is_rewritable())
            .collect();
        v.sort_by_key(|e| e.ts);
        v
    }

    /// **已封存组**（未吸收但 `revisionCount ≥ 封存线`），按时间升序（不编号、不可重写）。
    pub fn unabsorbed_sealed(&self) -> Vec<&ExperienceEntry> {
        let mut v: Vec<&ExperienceEntry> = self
            .experiences
            .iter()
            .filter(|e| !e.is_absorbed() && e.level == "short" && !e.is_rewritable())
            .collect();
        v.sort_by_key(|e| e.ts);
        v
    }

    /// 最老的 n 条未吸收短期经历（压缩素材）。
    pub fn oldest_unabsorbed(&self, n: usize) -> Vec<&ExperienceEntry> {
        let mut v = self.unabsorbed_short_asc();
        if v.len() > n {
            v.truncate(n);
        }
        v
    }

    pub fn experience_get(&self, id: &str) -> Option<&ExperienceEntry> {
        self.experiences.iter().find(|e| e.id == id)
    }

    /// 条目指纹（续写守卫的读取侧快照）。
    pub fn experience_fingerprint(&self, id: &str) -> Option<Fingerprint> {
        self.experience_get(id).map(fingerprint_of_entry)
    }

    pub fn experience_by_title(&self, title: &str) -> Vec<&ExperienceEntry> {
        self.experiences
            .iter()
            .filter(|e| e.title == title)
            .collect()
    }

    /// 关键词 / 时段检索（按 ts 降序）。
    pub fn experience_search(
        &self,
        query: Option<&str>,
        from: Option<u64>,
        to: Option<u64>,
        limit: usize,
    ) -> Vec<&ExperienceEntry> {
        let needle = query.map(|q| q.to_lowercase()).filter(|q| !q.is_empty());
        let mut hits: Vec<&ExperienceEntry> = self
            .experiences
            .iter()
            .filter(|e| {
                if let Some(n) = &needle {
                    let hay = format!("{}\n{}", e.title, e.summary).to_lowercase();
                    if !hay.contains(n.as_str()) {
                        return false;
                    }
                }
                if let Some(f) = from {
                    if e.ts < f {
                        return false;
                    }
                }
                if let Some(t) = to {
                    if e.ts > t {
                        return false;
                    }
                }
                true
            })
            .collect();
        hits.sort_by_key(|e| std::cmp::Reverse(e.ts));
        if limit > 0 && hits.len() > limit {
            hits.truncate(limit);
        }
        hits
    }

    // ── 水位 / 状态 ──

    /// 某对话已提炼到的轮数（水位）。
    pub fn watermark(&self, conversation_id: &str) -> u64 {
        self.state
            .conversations
            .get(conversation_id)
            .map(|w| w.processed_turns)
            .unwrap_or(0)
    }

    /// 冷却截止（unix 毫秒；None = 无冷却）。
    pub fn cooldown_until(&self, conversation_id: &str) -> Option<u64> {
        self.state
            .conversations
            .get(conversation_id)
            .and_then(|w| w.cooldown_until)
    }

    /// 推进水位并落盘（提炼成功后调用）。
    pub fn advance_watermark(
        &mut self,
        conversation_id: &str,
        processed_turns: u64,
        now: u64,
    ) -> Result<(), String> {
        let w = self
            .state
            .conversations
            .entry(conversation_id.to_string())
            .or_default();
        w.processed_turns = processed_turns;
        w.last_run_at = now;
        w.last_error = None;
        w.cooldown_until = None;
        self.recompute_totals();
        self.persist_state()
    }

    /// 记录整轮失败并设冷却（水位不前移）。
    pub fn mark_failure(
        &mut self,
        conversation_id: &str,
        err: &str,
        cooldown_until: u64,
        now: u64,
    ) -> Result<(), String> {
        let w = self
            .state
            .conversations
            .entry(conversation_id.to_string())
            .or_default();
        w.last_run_at = now;
        w.last_error = Some(err.to_string());
        w.cooldown_until = Some(cooldown_until);
        self.persist_state()
    }

    fn recompute_totals(&mut self) {
        self.state.totals.recent_lines = self.recent_line_count();
        self.state.totals.long_chars = self.longterm.chars().count() as u64;
    }

    // ── 经历写路径（管线专用） ──

    /// 应用提炼操作清单（新建 / 重写）。
    ///
    /// 重写守卫：目标条目存在、未被吸收、未封存（`revisionCount < SEALED`）、
    /// 且指纹与 `expected` 一致才重写；否则**降级为新建**（保内容、防丢更新）。
    /// 重写为**覆盖**（非追加）；落盘 experience.jsonl + state.json。
    pub fn apply_ops(
        &mut self,
        ops: &[MemoryOp],
        conversation_id: &str,
        turn_from: u64,
        turn_to: u64,
        now: u64,
    ) -> Result<ApplyReport, String> {
        let mut report = ApplyReport::default();
        for op in ops {
            match op {
                MemoryOp::Create {
                    title,
                    summary,
                    tags,
                } => {
                    let id = self.next_exp_id(now);
                    report.affected.push(id.clone());
                    self.experiences.push(ExperienceEntry {
                        id,
                        ts: now,
                        conversation_id: conversation_id.to_string(),
                        line_from: String::new(),
                        turn_from,
                        turn_to,
                        level: "short".into(),
                        title: title.clone(),
                        summary: summary.clone(),
                        tags: tags.clone(),
                        revision_count: 0,
                        absorbed_in: None,
                    });
                    report.created += 1;
                }
                MemoryOp::Rewrite {
                    target_id,
                    expected,
                    title,
                    summary,
                    tags,
                } => {
                    let ok = self
                        .experiences
                        .iter_mut()
                        .find(|e| e.id == *target_id)
                        .map(|e| {
                            if !e.is_rewritable() || fingerprint_of_entry(e) != *expected {
                                return false;
                            }
                            // 覆盖语义（非追加）
                            e.summary = summary.clone();
                            if !title.is_empty() {
                                e.title = title.clone();
                            }
                            e.tags = tags.clone();
                            e.revision_count += 1;
                            e.turn_to = turn_to;
                            e.ts = now; // 活跃线保持"最近"，避免被优先压缩
                            true
                        })
                        .unwrap_or(false);
                    if ok {
                        report.affected.push(target_id.clone());
                        report.rewritten += 1;
                    } else {
                        // 守卫失败/目标丢失 → 降级为新建
                        let id = self.next_exp_id(now);
                        report.affected.push(id.clone());
                        self.experiences.push(ExperienceEntry {
                            id,
                            ts: now,
                            conversation_id: conversation_id.to_string(),
                            line_from: String::new(),
                            turn_from,
                            turn_to,
                            level: "short".into(),
                            title: title.clone(),
                            summary: summary.clone(),
                            tags: tags.clone(),
                            revision_count: 0,
                            absorbed_in: None,
                        });
                        report.created += 1;
                        report.degraded += 1;
                    }
                }
            }
        }
        self.persist_experiences()?;
        self.recompute_totals();
        self.persist_state()?;
        Ok(report)
    }

    /// 标记条目已蒸进长期（压缩后）：`absorbed_in = version`、`level = long`。
    pub fn mark_absorbed(&mut self, ids: &[String], version: u64) -> Result<(), String> {
        for e in self.experiences.iter_mut() {
            if ids.contains(&e.id) {
                e.absorbed_in = Some(version);
                e.level = "long".into();
            }
        }
        self.persist_experiences()?;
        self.recompute_totals();
        self.persist_state()
    }

    /// 清空经历本体：全部条目 + 长期梗概 + 压缩游标（供「清除经历记忆」命令）。
    ///
    /// 不碰冻结 sidecar（sessions/*.memory.txt 保持原样——冻结优先）。
    pub fn clear_experiences(&mut self) -> Result<(), String> {
        self.experiences.clear();
        self.longterm.clear();
        self.state.compression = CompressionState::default();
        self.recompute_totals();
        self.persist_experiences()?;
        atomic_write(&self.root.join(LONGTERM_FILE), "")?;
        self.persist_state()
    }

    /// 水位抬平到不低于 `turns`（防清除后被管线把旧轮次挖回库）。
    pub fn raise_watermark(
        &mut self,
        conversation_id: &str,
        turns: u64,
        now: u64,
    ) -> Result<(), String> {
        let w = self
            .state
            .conversations
            .entry(conversation_id.to_string())
            .or_default();
        if turns > w.processed_turns {
            w.processed_turns = turns;
        }
        w.last_run_at = now;
        w.last_error = None;
        w.cooldown_until = None;
        self.persist_state()
    }

    /// 覆盖写入 longterm.md 并推进版本号。
    pub fn set_longterm(&mut self, text: String, through_ts: u64, now: u64) -> Result<u64, String> {
        atomic_write(&self.root.join(LONGTERM_FILE), &text)?;
        self.longterm = text;
        let c = &mut self.state.compression;
        c.longterm_version += 1;
        c.compressed_through_ts = through_ts;
        c.last_run_at = now;
        self.recompute_totals();
        self.persist_state()?;
        Ok(self.state.compression.longterm_version)
    }

    /// 人工重写长期记忆（设置页入口）：与 [`Self::set_longterm`] 同构，
    /// 但**无改动不落盘**——正文逐字相同 → 不写文件、不推进版本、不动水位，
    /// 返回 `(当前版本, false)`；调用方据此跳过提醒。
    pub fn set_longterm_manual(&mut self, text: String, now: u64) -> Result<(u64, bool), String> {
        if self.longterm == text {
            return Ok((self.longterm_version(), false));
        }
        let v = self.set_longterm(text, now, now)?;
        Ok((v, true))
    }

    fn persist_experiences(&self) -> Result<(), String> {
        let mut out = String::new();
        for e in &self.experiences {
            let line =
                serde_json::to_string(e).map_err(|err| format!("经历条目序列化失败: {err}"))?;
            out.push_str(&line);
            out.push('\n');
        }
        atomic_write(&self.root.join(EXPERIENCE_FILE), &out)
    }

    fn next_exp_id(&self, now: u64) -> String {
        let mut n = self.experiences.len();
        loop {
            let id = format!("exp_{now}_{n}");
            if !self.experiences.iter().any(|e| e.id == id) {
                return id;
            }
            n += 1;
        }
    }

    // ── 成长层写路径 ──

    /// 建页；重名（含「关于博士」）→ Err。
    pub fn growth_create(
        &mut self,
        name: &str,
        brief: &str,
        body: &str,
        tags: Vec<String>,
        source: &str,
    ) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("页名不能为空".into());
        }
        if self.growth_get(name).is_some() {
            return Err(format!("成长页「{name}」已存在；改用 update 更新"));
        }
        let page = GrowthPage::new(name, brief, body, tags, source, now_ms());
        if name == PROFILE_NAME {
            // 理论不可达（profile 恒存在），防御处理
            atomic_write(&self.root.join(PROFILE_FILE), &page.render())?;
            self.profile = page;
            return Ok(());
        }
        // 路径决策在创建期一次性完成并记入映射
        let path = self.allocate_topic_path(name);
        atomic_write(&path, &page.render())?;
        self.topic_files.insert(name.to_string(), path);
        self.topics.push(page);
        self.topics.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(())
    }

    /// 改页（字段缺席 = 不动）；不存在 → Err。
    ///
    /// `source` 为本次写入方（调用点集中常量：工具＝「工具」/ 设置页＝「博士」），
    /// 与 `updated_at` 同期刷新。
    ///
    /// 返回是否有实际改动：三字段与现值逐字相同 → 不写盘、不刷新
    /// `updated_at`/`source`，返回 `false`；调用方据此跳过提醒
    /// （「点了保存但没改」不应产生任何变化）。
    pub fn growth_update(
        &mut self,
        name: &str,
        body: Option<&str>,
        brief: Option<&str>,
        tags: Option<Vec<String>>,
        source: &str,
    ) -> Result<bool, String> {
        let name = name.trim();
        let target = self
            .growth_get_mut(name)
            .ok_or_else(|| format!("成长页「{name}」不存在；改用 create 新建"))?;
        let changed = body.is_some_and(|b| b != target.body)
            || brief.is_some_and(|b| b != target.brief)
            || tags.as_ref().is_some_and(|t| *t != target.tags);
        if !changed {
            return Ok(false);
        }
        if let Some(b) = body {
            target.body = b.to_string();
        }
        if let Some(b) = brief {
            target.brief = b.to_string();
        }
        if let Some(t) = tags {
            target.tags = t;
        }
        // 写入方盖章：与 updated_at 同期刷新（create 由调用方传 source）
        target.source = source.to_string();
        target.updated_at = now_ms();
        let page = target.clone();
        self.write_page(&page)?;
        Ok(true)
    }

    /// 删页；不存在 → Err。
    ///
    /// 「关于博士」为固定键：删除＝复位为空页（保持页恒在）。
    pub fn growth_delete(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if self.growth_get(name).is_none() {
            return Err(format!("成长页「{name}」不存在"));
        }
        if name == PROFILE_NAME {
            let blank = GrowthPage::empty_profile(now_ms());
            atomic_write(&self.root.join(PROFILE_FILE), &blank.render())?;
            self.profile = blank;
            return Ok(());
        }
        if let Some(pos) = self.topics.iter().position(|p| p.name == name) {
            self.topics.remove(pos);
        }
        if let Some(path) = self.topic_files.remove(name) {
            if let Err(e) = std::fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("[memory] 专题页删除失败（{}）：{e}", path.display());
                }
            }
        }
        Ok(())
    }

    fn growth_get_mut(&mut self, name: &str) -> Option<&mut GrowthPage> {
        if name == PROFILE_NAME {
            return Some(&mut self.profile);
        }
        self.topics.iter_mut().find(|p| p.name == name)
    }

    /// 写页（更新路径）：以映射记录的实际路径为准。
    fn write_page(&self, page: &GrowthPage) -> Result<(), String> {
        let path = if page.name == PROFILE_NAME {
            self.root.join(PROFILE_FILE)
        } else {
            self.topic_files
                .get(&page.name)
                .cloned()
                .ok_or_else(|| format!("成长页「{}」路径未登记（内部状态异常）", page.name))?
        };
        atomic_write(&path, &page.render())
    }

    /// 创建期路径决策：基于页名净化；已被**其它页**占用或磁盘已存在 → 加哈希后缀。
    fn allocate_topic_path(&self, name: &str) -> PathBuf {
        let base = sanitize_filename(name);
        let dir = self.root.join(TOPICS_DIR);
        let mut cand = dir.join(format!("{base}.md"));
        if !self.path_taken(&cand, name) {
            return cand;
        }
        let mut i = 0u32;
        loop {
            let suffix = if i == 0 {
                format!("{:08x}", fnv1a(name))
            } else {
                format!("{:08x}-{i}", fnv1a(name))
            };
            cand = dir.join(format!("{base}-{suffix}.md"));
            if !self.path_taken(&cand, name) {
                return cand;
            }
            i += 1;
        }
    }

    fn path_taken(&self, p: &Path, name: &str) -> bool {
        self.topic_files
            .iter()
            .any(|(n, path)| n != name && path == p)
            || p.exists()
    }

    /// 写回 state.json（测试/兼容入口）。
    pub fn persist_state(&self) -> Result<(), String> {
        let path = self.root.join(STATE_FILE);
        let body = serde_json::to_string_pretty(&self.state)
            .map_err(|e| format!("state.json 序列化失败: {e}"))?;
        atomic_write(&path, &body)
    }

    /// 测试专用：注入一条经历条目（批 1/工具测试）。
    #[cfg(test)]
    pub fn inject_experience_for_test(&mut self, entry: ExperienceEntry) {
        self.experiences.push(entry);
    }
}

// ═══════════════════════════════════════════════════════
// 辅助
// ═══════════════════════════════════════════════════════

/// 原子写（tmp + rename；失败上抛）。
pub fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content).map_err(|e| format!("写入失败（{}）：{e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("替换失败（{}）：{e}", path.display())
    })
}

fn read_page(path: &Path) -> Option<GrowthPage> {
    let text = std::fs::read_to_string(path).ok()?;
    GrowthPage::parse(&text)
}

fn read_experiences(path: &Path) -> Vec<ExperienceEntry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<ExperienceEntry>(line) {
            Ok(e) => out.push(e),
            Err(e) => eprintln!("[memory] 跳过损坏的经历条目行：{e}"),
        }
    }
    out
}

fn read_state(path: &Path) -> MemoryState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<MemoryState>(&s).ok())
        .unwrap_or_default()
}

/// 页名 → 文件名（保留 CJK；屏蔽路径分隔与 Windows 非法字符）。
fn sanitize_filename(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    s = s.trim().trim_end_matches(['.', ' ']).to_string();
    if s.is_empty() {
        s = "unnamed".to_string();
    }
    if s.chars().count() > 60 {
        s = s.chars().take(60).collect();
    }
    s
}

/// FNV-1a（文件名冲突后缀；无外部依赖）。
fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::schema::{SOURCE_SYSTEM, SOURCE_TOOL, SOURCE_USER};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("prts-mem-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_missing_files_yields_empty_profile() {
        let dir = temp_dir("empty");
        let s = MemoryStore::load(&dir).unwrap();
        assert_eq!(s.profile().name, PROFILE_NAME);
        assert!(s.profile().body.is_empty());
        assert!(s.topics().is_empty());
        assert!(s.experiences().is_empty());
        assert!(s.longterm().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn growth_create_update_delete_roundtrip_and_persist() {
        let dir = temp_dir("growth");
        {
            let mut s = MemoryStore::load(&dir).unwrap();
            s.growth_create(
                "伤害计算",
                "暴击与减伤",
                "## 正文",
                vec!["数值".into()],
                SOURCE_SYSTEM,
            )
            .unwrap();
            s.growth_update(
                "伤害计算",
                Some("## 新正文"),
                Some("新简述"),
                None,
                SOURCE_SYSTEM,
            )
            .unwrap();
            s.growth_create("索敌", "仇恨规则", "body", vec![], SOURCE_SYSTEM)
                .unwrap();
        }
        let s2 = MemoryStore::load(&dir).unwrap();
        let p = s2.growth_get("伤害计算").unwrap();
        assert_eq!(p.body, "## 新正文");
        assert_eq!(p.brief, "新简述");
        assert_eq!(s2.topics().len(), 2);
        assert_eq!(s2.topics()[0].name, "伤害计算", "按名排序（Unicode 码点）");
        assert_eq!(s2.topics()[1].name, "索敌");

        let mut s3 = MemoryStore::load(&dir).unwrap();
        s3.growth_delete("索敌").unwrap();
        let s4 = MemoryStore::load(&dir).unwrap();
        assert!(s4.growth_get("索敌").is_none());
        assert_eq!(s4.topics().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🟡2 回归：清洗撞名页往返后不残留/不复活、指向正确路径。
    #[test]
    fn growth_cleaned_name_collision_paths_are_stable() {
        let dir = temp_dir("collide");
        let mut s = MemoryStore::load(&dir).unwrap();
        s.growth_create("A/B", "斜杠名", "b1", vec![], SOURCE_SYSTEM)
            .unwrap(); // → A_B.md
        s.growth_create("A_B", "下划线名", "b2", vec![], SOURCE_SYSTEM)
            .unwrap(); // 撞名 → A_B-<hash>.md
        assert_eq!(s.topics().len(), 2);

        // 删前者，改后者 → 后者仍在、内容正确、重启不复活前者
        s.growth_delete("A/B").unwrap();
        s.growth_update("A_B", Some("b2-new"), None, None, SOURCE_SYSTEM)
            .unwrap();
        drop(s);
        let s2 = MemoryStore::load(&dir).unwrap();
        assert!(s2.growth_get("A/B").is_none(), "前者不应复活");
        assert_eq!(s2.growth_get("A_B").unwrap().body, "b2-new");
        assert_eq!(s2.topics().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn growth_create_duplicate_errors_including_profile() {
        let dir = temp_dir("dup");
        let mut s = MemoryStore::load(&dir).unwrap();
        s.growth_create("甲", "b", "x", vec![], SOURCE_SYSTEM)
            .unwrap();
        let err = s
            .growth_create("甲", "b", "x", vec![], SOURCE_SYSTEM)
            .unwrap_err();
        assert!(err.contains("已存在"), "got: {err}");
        let err = s
            .growth_create(PROFILE_NAME, "b", "x", vec![], SOURCE_SYSTEM)
            .unwrap_err();
        assert!(err.contains("已存在"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn growth_update_or_delete_missing_errors() {
        let dir = temp_dir("missing");
        let mut s = MemoryStore::load(&dir).unwrap();
        assert!(s
            .growth_update("不存在", Some("x"), None, None, SOURCE_SYSTEM)
            .is_err());
        assert!(s.growth_delete("不存在").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn growth_update_noop_is_not_a_write() {
        let dir = temp_dir("noop");
        let mut s = MemoryStore::load(&dir).unwrap();
        s.growth_create("甲", "b", "x", vec!["t".into()], SOURCE_SYSTEM)
            .unwrap();
        let before = s.growth_get("甲").unwrap().clone();
        let changed = s
            .growth_update(
                "甲",
                Some("x"),
                Some("b"),
                Some(vec!["t".into()]),
                SOURCE_USER,
            )
            .unwrap();
        assert!(!changed, "同内容应返回无改动");
        let after = s.growth_get("甲").unwrap();
        assert_eq!(after.updated_at, before.updated_at, "无改动不应刷新时间");
        assert_eq!(after.source, SOURCE_SYSTEM, "无改动不应盖章写入方");
        let changed = s
            .growth_update("甲", Some("x2"), None, None, SOURCE_USER)
            .unwrap();
        assert!(changed, "有改动返回 true");
        assert_eq!(s.growth_get("甲").unwrap().source, SOURCE_USER);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_longterm_manual_noop_keeps_version() {
        let dir = temp_dir("lt-noop");
        let mut s = MemoryStore::load(&dir).unwrap();
        let (v, changed) = s.set_longterm_manual("老".into(), 1).unwrap();
        assert!(changed);
        assert_eq!(v, 1);
        let (v2, changed2) = s.set_longterm_manual("老".into(), 2).unwrap();
        assert!(!changed2, "同内容应返回无改动");
        assert_eq!(v2, 1, "版本不应推进");
        assert_eq!(s.longterm(), "老");
        let (v3, changed3) = s.set_longterm_manual("新".into(), 3).unwrap();
        assert!(changed3);
        assert_eq!(v3, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn growth_update_profile_and_delete_resets() {
        let dir = temp_dir("profile");
        let mut s = MemoryStore::load(&dir).unwrap();
        s.growth_update(PROFILE_NAME, Some("博士偏好简洁"), None, None, SOURCE_USER)
            .unwrap();
        assert_eq!(s.profile().body, "博士偏好简洁");
        assert_eq!(s.profile().source, SOURCE_USER, "更新盖章写入方");
        s.growth_delete(PROFILE_NAME).unwrap();
        assert!(s.profile().body.is_empty(), "profile 删除＝复位为空页");
        assert_eq!(s.profile().source, SOURCE_SYSTEM, "复位＝初始化（系统）");
        let s2 = MemoryStore::load(&dir).unwrap();
        assert!(s2.profile().body.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn growth_source_stamps_writer_and_refreshes_on_update() {
        let dir = temp_dir("source");
        let mut s = MemoryStore::load(&dir).unwrap();
        // 工具写入 → 「工具」
        s.growth_create("甲", "b", "x", vec![], SOURCE_TOOL)
            .unwrap();
        assert_eq!(s.growth_get("甲").unwrap().source, SOURCE_TOOL);
        // 设置页写入 → 「博士」
        s.growth_create("乙", "b", "x", vec![], SOURCE_USER)
            .unwrap();
        assert_eq!(s.growth_get("乙").unwrap().source, SOURCE_USER);
        // 更新刷新写入方（工具 → 博士）
        s.growth_update("甲", Some("x2"), None, None, SOURCE_USER)
            .unwrap();
        assert_eq!(s.growth_get("甲").unwrap().source, SOURCE_USER);
        // 初始化 → 「系统」
        assert_eq!(GrowthPage::empty_profile(0).source, SOURCE_SYSTEM);
        // 重启后保持
        let s2 = MemoryStore::load(&dir).unwrap();
        assert_eq!(s2.growth_get("甲").unwrap().source, SOURCE_USER);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 🔵9：profile 损坏 → 备份 .bak + 降级空页（不丢原正文）。
    #[test]
    fn corrupted_profile_is_backed_up_and_degraded() {
        let dir = temp_dir("corrupt");
        let root = dir.join("memory");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(PROFILE_FILE), "这不是 frontmatter 的正文").unwrap();
        let s = MemoryStore::load(&dir).unwrap();
        assert!(s.profile().body.is_empty(), "降级为空页");
        assert!(root.join("profile.md.bak").exists(), "损坏件应备份");
        assert_eq!(
            std::fs::read_to_string(root.join("profile.md.bak")).unwrap(),
            "这不是 frontmatter 的正文"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn experience_jsonl_load_skip_bad_lines_and_query() {
        let dir = temp_dir("exp");
        let root = dir.join("memory");
        std::fs::create_dir_all(&root).unwrap();
        let e1 = ExperienceEntry {
            id: "e1".into(),
            ts: 100,
            conversation_id: "c1".into(),
            line_from: "L1".into(),
            turn_from: 0,
            turn_to: 2,
            level: "short".into(),
            title: "伤害".into(),
            summary: "讨论了暴击".into(),
            tags: vec![],
            revision_count: 0,
            absorbed_in: None,
        };
        let mut e2 = e1.clone();
        e2.id = "e2".into();
        e2.ts = 200;
        e2.level = "long".into();
        e2.absorbed_in = Some(1);
        e2.title = "索敌".into();
        e2.summary = "仇恨".into();
        let mut e3 = e1.clone();
        e3.id = "e3".into();
        e3.ts = 300;
        e3.title = "Buff".into();
        e3.summary = "刷新机制".into();
        let content = format!(
            "{}\n{}\n  坏行 not json\n{}\n",
            serde_json::to_string(&e1).unwrap(),
            serde_json::to_string(&e2).unwrap(),
            serde_json::to_string(&e3).unwrap()
        );
        std::fs::write(root.join(EXPERIENCE_FILE), content).unwrap();

        let s = MemoryStore::load(&dir).unwrap();
        assert_eq!(s.experiences().len(), 3, "坏行跳过");
        let recent = s.recent_unabsorbed(5);
        assert_eq!(
            recent.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["e1", "e3"]
        );
        assert_eq!(s.recent_line_count(), 2, "已吸收的 e2 不计");
        let hits = s.experience_search(Some("暴击"), None, None, 0);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "e1");
        let hits = s.experience_search(None, Some(150), Some(250), 0);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "e2");
        let hits = s.experience_search(None, None, None, 2);
        assert_eq!(
            hits.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            vec!["e3", "e2"]
        );
        assert_eq!(s.experience_by_title("Buff").len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_ops_create_rewrite_and_guards() {
        let dir = temp_dir("ops");
        let mut s = MemoryStore::load(&dir).unwrap();
        // 新建
        let r = s
            .apply_ops(
                &[MemoryOp::Create {
                    title: "伤害公式".into(),
                    summary: "第一版".into(),
                    tags: vec!["数值".into()],
                }],
                "c1",
                0,
                3,
                1000,
            )
            .unwrap();
        assert_eq!((r.created, r.rewritten, r.degraded), (1, 0, 0));
        let id = s.unabsorbed_rewritable()[0].id.clone();
        assert_eq!(s.experience_get(&id).unwrap().revision_count, 0);

        // 合法重写：覆盖 summary/title/tags（非追加），revisionCount 1
        let fp = fingerprint_of_entry(s.experience_get(&id).unwrap());
        let r = s
            .apply_ops(
                &[MemoryOp::Rewrite {
                    target_id: id.clone(),
                    expected: fp,
                    title: "伤害公式（修订）".into(),
                    summary: "第二版".into(),
                    tags: vec!["数值".into(), "机制".into()],
                }],
                "c1",
                0,
                6,
                2000,
            )
            .unwrap();
        assert_eq!((r.created, r.rewritten, r.degraded), (0, 1, 0));
        let e = s.experience_get(&id).unwrap();
        assert_eq!(e.summary, "第二版");
        assert_eq!(e.title, "伤害公式（修订）");
        assert_eq!(
            e.tags,
            vec!["数值".to_string(), "机制".to_string()],
            "标签覆盖"
        );
        assert_eq!(e.revision_count, 1);
        assert_eq!(e.turn_to, 6);
        assert_eq!(e.ts, 2000);

        // 指纹失配（陈旧）→ 降级为新建
        let r = s
            .apply_ops(
                &[MemoryOp::Rewrite {
                    target_id: id.clone(),
                    expected: fp, // 旧指纹
                    title: "伤害公式（修订）".into(),
                    summary: "第三版".into(),
                    tags: vec![],
                }],
                "c1",
                0,
                9,
                3000,
            )
            .unwrap();
        assert_eq!(
            (r.created, r.rewritten, r.degraded),
            (1, 0, 1),
            "陈旧指纹 → 新建"
        );
        assert_eq!(s.recent_line_count(), 2);

        // 再重写一次至封存线（1 → 2）
        let fp2 = fingerprint_of_entry(s.experience_get(&id).unwrap());
        let r = s
            .apply_ops(
                &[MemoryOp::Rewrite {
                    target_id: id.clone(),
                    expected: fp2,
                    title: "伤害公式（修订）".into(),
                    summary: "第四版".into(),
                    tags: vec![],
                }],
                "c1",
                0,
                12,
                4000,
            )
            .unwrap();
        assert_eq!((r.created, r.rewritten, r.degraded), (0, 1, 0));
        assert_eq!(s.experience_get(&id).unwrap().revision_count, 2, "达封存线");
        assert!(!s.experience_get(&id).unwrap().is_rewritable());

        // 封存后同题再有进展 → 一律新建
        let fp3 = fingerprint_of_entry(s.experience_get(&id).unwrap());
        let r = s
            .apply_ops(
                &[MemoryOp::Rewrite {
                    target_id: id.clone(),
                    expected: fp3,
                    title: "伤害公式（修订）".into(),
                    summary: "第五版".into(),
                    tags: vec![],
                }],
                "c1",
                0,
                15,
                5000,
            )
            .unwrap();
        assert_eq!(
            (r.created, r.rewritten, r.degraded),
            (1, 0, 1),
            "已封存 → 新建"
        );
        assert_eq!(s.recent_line_count(), 3);
        assert!(!s.unabsorbed_rewritable().iter().any(|e| e.id == id));
        assert!(s.unabsorbed_sealed().iter().any(|e| e.id == id));

        // 重新加载验证落盘
        let s2 = MemoryStore::load(&dir).unwrap();
        assert_eq!(s2.recent_line_count(), 3);
        assert_eq!(s2.experience_get(&id).unwrap().revision_count, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mark_absorbed_and_longterm_version() {
        let dir = temp_dir("absorb");
        let mut s = MemoryStore::load(&dir).unwrap();
        s.apply_ops(
            &[MemoryOp::Create {
                title: "A".into(),
                summary: "s".into(),
                tags: vec![],
            }],
            "c1",
            0,
            1,
            100,
        )
        .unwrap();
        let id = s.unabsorbed_short_asc()[0].id.clone();
        assert_eq!(s.longterm_version(), 0);
        let v = s.set_longterm("## 先前\n老事".into(), 100, 200).unwrap();
        assert_eq!(v, 1);
        s.mark_absorbed(&[id.clone()], v).unwrap();
        assert_eq!(s.recent_line_count(), 0, "已吸收不计近期");
        assert_eq!(s.longterm(), "## 先前\n老事");

        let s2 = MemoryStore::load(&dir).unwrap();
        assert_eq!(s2.longterm_version(), 1);
        assert_eq!(s2.experience_get(&id).unwrap().level, "long");
        assert_eq!(s2.experience_get(&id).unwrap().absorbed_in, Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watermark_and_failure_state_persist() {
        let dir = temp_dir("watermark");
        {
            let mut s = MemoryStore::load(&dir).unwrap();
            s.advance_watermark("c1", 7, 1000).unwrap();
            s.mark_failure("c2", "坏件", 5000, 1200).unwrap();
        }
        let s2 = MemoryStore::load(&dir).unwrap();
        assert_eq!(s2.watermark("c1"), 7);
        assert_eq!(s2.cooldown_until("c2"), Some(5000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitize_and_fnv_are_stable() {
        assert_eq!(sanitize_filename("a/b:c"), "a_b_c");
        assert_eq!(sanitize_filename("  ..  "), "unnamed");
        assert_eq!(fnv1a("x"), fnv1a("x"));
        assert_ne!(fnv1a("x"), fnv1a("y"));
    }

    #[test]
    fn clear_experiences_wipes_body_and_longterm() {
        let dir = temp_dir("clear");
        let mut s = MemoryStore::load(&dir).unwrap();
        s.apply_ops(
            &[MemoryOp::Create {
                title: "A".into(),
                summary: "s".into(),
                tags: vec![],
            }],
            "c1",
            0,
            1,
            100,
        )
        .unwrap();
        s.set_longterm("## 先前\n事".into(), 100, 200).unwrap();
        s.clear_experiences().unwrap();
        assert_eq!(s.recent_line_count(), 0);
        assert!(s.longterm().is_empty());
        assert_eq!(s.longterm_version(), 0, "压缩游标随清");
        let s2 = MemoryStore::load(&dir).unwrap();
        assert!(s2.experiences().is_empty());
        assert!(s2.longterm().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn raise_watermark_is_monotonic() {
        let dir = temp_dir("raise");
        let mut s = MemoryStore::load(&dir).unwrap();
        s.advance_watermark("c1", 5, 1).unwrap();
        s.raise_watermark("c1", 3, 2).unwrap();
        assert_eq!(s.watermark("c1"), 5, "不降");
        s.raise_watermark("c1", 9, 3).unwrap();
        assert_eq!(s.watermark("c1"), 9, "抬平");
        let s2 = MemoryStore::load(&dir).unwrap();
        assert_eq!(s2.watermark("c1"), 9);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
