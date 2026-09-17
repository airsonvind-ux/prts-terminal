//! session_store —— 对话持久化存储层（内容文件 jsonl + 元数据索引 index.json）
//!
//! 布局（`data_dir()/sessions/`）：
//! - `{conversationId}.jsonl` 内容文件：首行 header + 每轮一行（该轮 SessionEvent
//!   数组的 JSON **原文**整行，不做每事件一行，**不重新序列化**——落盘内容与前端
//!   归档序列化逐字节一致，仅写前做一次合法性校验）。
//! - `index.json` 元数据索引：全部会话 id/title/createdAt/modelName/contextLength/
//!   contextTokens/lastNoticeSeq/updatedAt，**整写 + tmp+rename 原子替换**
//!   （不用 SQLite：单 Agent 悬浮窗规模，元数据每条 ~300B，整写成本可忽略）。
//!
//! 职责边界（架构升级 · 模块 1 · 持久化 seam，Phase 4 落地）：
//! 后端**纯存储层**——只落盘/读取，不解析业务、不做标题生成、不做压缩。
//! 内容文件是事实源（header 承载归属与创建快照），index.json 是可重建索引
//! （缺失/损坏 → 扫 jsonl header 自愈重建）。
//!
//! 写盘触发 = 前端轮末归档（append_turn：turn/end 与错误轮都提交）：
//! - 首轮（内容文件不存在）→ 写 header + 轮行 + index 插入条目
//! - 后续轮 → 校验 header 归属（防串写）→ 追加轮行 → index 更新 updatedAt，
//!   并覆盖随轮提交的非 None contextTokens / lastNoticeSeq（Reminder 游标随会话
//!   持久，接口见前端 advanceNoticeSeq 随轮提交决策）
//! - **空对话不落盘**：前端 createConversation 不触发任何 IPC，文件在首轮归档才诞生
//!
//! 损坏处理：
//! - 轮行坏行（parse 失败/半行）→ 读取时跳过，其余行照常（get 不整体失败）
//! - header 损坏 / conversationId 与文件名不符 / schemaVersion 过高 → 会话视为
//!   无效：list 移除对应 index 条目（**保留 jsonl 文件**），get/append 返回错误
//! - index.json 缺失/parse 失败 → 自愈重建（扫 jsonl header；title=创建快照、
//!   updatedAt=createdAt、其余字段 null）
//! - index.json schemaVersion > 当前 → **只读降级**：list 返回空、写操作拒绝
//!   （绝不覆盖写高版本数据）
//!
//! 写失败策略：内容行写失败 → Err 上抛（调用方前端静默降级）；index 写失败 →
//! eprintln 告警不抛（内容已落；条目缺失会在下次 append/get 由 header 补回，
//! 或整索引损坏时由 list 自愈重建——内容文件为事实源）。

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::llm::TokenUsage;

/// 当前 schema 版本（内容文件 header 与 index.json 共用；升版走迁移，见方案 §1.3）。
pub const SCHEMA_VERSION: u32 = 1;

const INDEX_FILE: &str = "index.json";
const INDEX_TMP: &str = "index.json.tmp";
const HEADER_KIND: &str = "header";
const HEADER_FORMAT: &str = "prts-conversation-jsonl";
const DEFAULT_TITLE: &str = "新对话";

// ═══════════════════════════════════════════════════════
// 公开类型（IPC 载荷 + 命令参数契约）
// ═══════════════════════════════════════════════════════

/// 会话元数据（index.json 条目 = list_conversations 载荷；前端 Conversation 的
/// 持久化投影）。可缺省字段以 null 表达（index 中存 null，前端恢复时转 undefined）。
/// 恢复后的导航决策（选哪个会话为 activeId 等）由前端恢复 hook 单点负责——
/// 本字段集保证该决策所需信息齐全（含 updatedAt）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMeta {
    pub id: String,
    pub title: String,
    /// 创建时间（unix 毫秒，前端 Date.now() 语义）
    pub created_at: u64,
    pub model_name: Option<String>,
    pub context_length: Option<u64>,
    /// 最近一轮归档的上下文 Token 用量快照（随轮覆盖提交）
    pub context_tokens: Option<TokenUsage>,
    /// 环境通知已告知游标（M1 Reminder；随轮覆盖提交，单调递增）
    pub last_notice_seq: Option<u64>,
    /// 压缩记录（模块 D；None = 未压缩过；旧 index 缺字段兼容）
    pub compaction: Option<CompactionMeta>,
    /// 最近一次轮提交/元数据更新的时间（unix 毫秒）
    pub updated_at: u64,
}

/// 压缩记录（模块 D · 上下文衰减）：会话元数据扩展字段。
///
/// 语义：压缩 = 前端发起的一次对话级状态转换。`retained_from` 为压缩后保留段
/// 第一轮在 turns 的下标（turns 本身全量保留——旧轮是回看资产，永不回灌 LLM）；
/// `summary` 为压缩时生成的 LLM checkpoint 文本（仅存最新一份，重压缩覆盖）；
/// `tokens_before` 为折叠区压缩前估算（信息用）。
///
/// Option 字段天然容忍旧 index 缺字段（serde Option 缺省 None）——新增本字段
/// **不升 schemaVersion**：旧程序读新 index 忽略未知字段；新程序读旧 index 缺省 None。
/// index 损坏自愈重建（meta_from_header）时本记录丢失——接受：turns 仍在，
/// 仅丢摘要起点（回退到未压缩态，无数据损失）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionMeta {
    /// 保留段第一轮在 turns 的下标（前端事件数组下标语义）
    pub retained_from: u64,
    /// 结构化 checkpoint 摘要文本（LLM 产出正文，不含定界块）
    pub summary: String,
    /// 折叠区压缩前估算 token（信息用；压缩消息形态定界块不计）
    pub tokens_before: Option<u64>,
    /// 压缩发生时间（unix 毫秒）
    pub at: u64,
}

/// get_conversation 返回：meta + 轮行原文列表（每行 = 该轮 SessionEvent 数组 JSON，
/// 保持落盘原文——前端 JSON.parse 后直接重建 turns，零改写）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRecord {
    pub meta: ConversationMeta,
    pub turns: Vec<String>,
}

/// append_turn 请求（命令层组装；Deserialize 供 IPC 契约测试）。
/// 可缺省字段：缺失与 null 均视为缺省（Option + serde default / null → None）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppendTurnReq {
    pub conversation_id: String,
    /// 整轮事件数组的 JSON **原文**（前端 JSON.stringify(events)）
    pub events_json: String,
    /// 仅首轮创建时使用（title 缺省回退「新对话」；createdAt 缺省用当前时间）
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub created_at: Option<u64>,
    #[serde(default)]
    pub model_name: Option<String>,
    #[serde(default)]
    pub context_length: Option<u64>,
    /// 每轮随轮覆盖提交（非 None 才更新 index）
    #[serde(default)]
    pub context_tokens: Option<TokenUsage>,
    #[serde(default)]
    pub last_notice_seq: Option<u64>,
}

/// update_conversation_meta 补丁（只更新非 None 字段；文件必须存在）。
#[derive(Debug, Clone, Default)]
pub struct MetaPatch {
    pub title: Option<String>,
    /// 标题写入的 CAS 守卫（生成标题专用）：Some 时**仅当** index 当前 title
    /// 等于该值才应用 `title`（用户已改名 → 放弃写入，不报错）。None = 无条件。
    pub expected_title: Option<String>,
    pub context_tokens: Option<TokenUsage>,
    pub last_notice_seq: Option<u64>,
    /// 压缩记录（模块 D）：Some(Some) = 覆盖写入新记录。None = 不更新。
    /// （不支持置 None 清除——压缩不可逆，回退态只出现在 index 自愈重建）
    pub compaction: Option<CompactionMeta>,
    /// 显式清空 contextTokens（模块 D：压缩归零的持久面——context_tokens=None
    /// 表达"不更新"，无法表达"清空"，故单独布尔。true = 置 None 写盘）
    pub context_tokens_clear: bool,
}

/// 生成标题的 CAS 写入结果（`try_set_title_if_expected`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleWriteOutcome {
    /// 当前 title == expected → 已写入新标题
    Written,
    /// 会话文件尚未落盘（首轮流式期，append_turn 尚未创建）→ 未写入；
    /// 调用方仍应推送事件（由前端携带至后续 append_turn / 补丁）
    NotExists,
    /// 当前 title != expected（用户已改名或已生成过）→ 放弃写入
    TitleChanged,
}

/// append_turn 结果：created=true 表示本次创建了新会话文件（首轮归档）。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppendTurnOutcome {
    pub created: bool,
}

// ═══════════════════════════════════════════════════════
// 内部盘上结构
// ═══════════════════════════════════════════════════════

/// 内容文件首行。title 为**创建时**快照（rename 不回写 header——
/// 仅 index 自愈重建时用作兜底标题，见方案 §7.5 取舍）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionHeader {
    #[serde(rename = "type")]
    kind: String,
    format: String,
    schema_version: u32,
    conversation_id: String,
    created_at: u64,
    title: String,
}

/// index.json 顶层容器。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionIndex {
    schema_version: u32,
    sessions: Vec<ConversationMeta>,
}

/// 会话存储。目录布局在 `new` 时创建；所有写路径经 Mutex 由命令层串行化。
pub struct SessionStore {
    /// `data_dir()/sessions`
    dir: PathBuf,
}

/// 会话目录（`data_dir()/sessions`）。SessionStore 与记忆快照 sidecar
/// （`memory::SnapshotStore`）共用同一布局，避免两处硬编码漂移。
pub fn sessions_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("sessions")
}

impl SessionStore {
    /// 在 data_dir 下建 sessions/ 子目录（create_dir_all；失败忽略——
    /// 后续写操作会以具体 IO 错误暴露）。
    pub fn new(data_dir: &Path) -> Self {
        let dir = sessions_dir(data_dir);
        std::fs::create_dir_all(&dir).ok();
        Self { dir }
    }

    // ── 写路径 ──

    /// 追加一轮事件。
    ///
    /// 语义（方案 §2.2）：
    /// - 内容文件不存在 = 该对话首轮归档 → 一次性写 header + 轮行，index 插入条目
    /// - 内容文件已存在 → 校验 header 归属（conversationId 匹配，防 id 碰撞串写）
    ///   → 追加轮行 → index 更新 updatedAt + 非 None 覆盖 contextTokens/lastNoticeSeq
    /// - 只读降级（index schemaVersion 过高）→ Err 拒绝（不向高版本索引域写数据）
    pub fn append_turn(&mut self, req: &AppendTurnReq) -> Result<AppendTurnOutcome, String> {
        // 事件内容只做合法性校验（不解析业务）；坏内容拒绝落盘
        if serde_json::from_str::<Vec<serde_json::Value>>(&req.events_json).is_err() {
            return Err("eventsJson 不是合法 JSON 数组（拒绝落盘）".into());
        }
        let path = self.file_path(&req.conversation_id)?;
        // index 状态检查（触发只读降级检测与缺失自愈）先于任何内容写入
        let mut index = self.load_or_rebuild()?;
        // 轮行 = 前端原文 + 单一尾换行（trim 仅去除意外行终止符，不重排内容）
        let events_line = format!("{}\n", req.events_json.trim_end_matches(['\r', '\n']));

        if !path.exists() {
            // ── 首轮：创建会话文件（一次性写 header + 轮行，比两次追加更抗中断）──
            let title = req
                .title
                .clone()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_TITLE.to_string());
            let created_at = req.created_at.unwrap_or_else(now_ms);
            let header = SessionHeader {
                kind: HEADER_KIND.to_string(),
                format: HEADER_FORMAT.to_string(),
                schema_version: SCHEMA_VERSION,
                conversation_id: req.conversation_id.clone(),
                created_at,
                title: title.clone(),
            };
            let header_json = serde_json::to_string(&header)
                .map_err(|e| format!("header 序列化失败: {e}"))?;
            let content = format!("{header_json}\n{events_line}");
            std::fs::write(&path, content)
                .map_err(|e| format!("会话文件创建失败（{}）：{e}", path.display()))?;

            index.push(ConversationMeta {
                id: req.conversation_id.clone(),
                title,
                created_at,
                updated_at: now_ms(),
                model_name: req.model_name.clone(),
                context_length: req.context_length,
                context_tokens: req.context_tokens,
                last_notice_seq: req.last_notice_seq,
                compaction: None,
            });
            self.write_index(&index);
            return Ok(AppendTurnOutcome { created: true });
        }

        // ── 追加轮：归属校验 → 追加 → index 更新 ──
        let header = self
            .read_header(&path)
            .map_err(|e| format!("会话文件损坏（{}）：{e}", path.display()))?;
        if header.conversation_id != req.conversation_id {
            return Err(format!(
                "会话文件归属不符：文件 {}.jsonl 的 header conversationId 不一致（防串写）",
                req.conversation_id
            ));
        }
        if header.schema_version > SCHEMA_VERSION {
            return Err(format!(
                "会话文件 schemaVersion {} 高于当前 {SCHEMA_VERSION}，拒绝追加",
                header.schema_version
            ));
        }
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(|e| format!("打开会话文件追加失败（{}）：{e}", path.display()))?;
        // 内容行写失败 → Err 上抛（内容不落，调用方静默降级）
        file.write_all(events_line.as_bytes())
            .map_err(|e| format!("会话文件追加失败（{}）：{e}", path.display()))?;

        match index.iter_mut().find(|m| m.id == req.conversation_id) {
            Some(meta) => {
                meta.updated_at = now_ms();
                if let Some(t) = req.context_tokens {
                    meta.context_tokens = Some(t);
                }
                if let Some(s) = req.last_notice_seq {
                    meta.last_notice_seq = Some(s);
                }
            }
            None => {
                // index 缺条目（此前 index 写失败丢失）：从 header 补回——
                // 内容文件为事实源；modelName/contextLength 无法从 header 恢复，置空
                eprintln!(
                    "[session_store] index 缺会话条目 {}，从 header 补回（modelName/contextLength 置空）",
                    req.conversation_id
                );
                let mut meta = meta_from_header(header);
                meta.updated_at = now_ms();
                if let Some(t) = req.context_tokens {
                    meta.context_tokens = Some(t);
                }
                if let Some(s) = req.last_notice_seq {
                    meta.last_notice_seq = Some(s);
                }
                index.push(meta);
            }
        }
        self.write_index(&index);
        Ok(AppendTurnOutcome { created: false })
    }

    /// 更新会话元数据（rename / contextTokens / lastNoticeSeq；只更新非 None 字段，
    /// 更新 updatedAt）。文件不存在 → Err。index 写失败 eprintln 不抛（下轮 append
    /// 或后续操作会带最新值重写）。
    pub fn update_meta(
        &mut self,
        conversation_id: &str,
        patch: &MetaPatch,
    ) -> Result<(), String> {
        let path = self.file_path(conversation_id)?;
        let mut index = self.load_or_rebuild()?;
        if !path.exists() {
            return Err(format!("会话不存在（{conversation_id}）"));
        }
        let meta = index
            .iter_mut()
            .find(|m| m.id == conversation_id)
            .ok_or_else(|| format!("会话不存在（{conversation_id}）"))?;
        if let Some(t) = &patch.title {
            if !t.trim().is_empty() {
                // CAS 守卫（生成标题专用）：expected_title=Some 时仅当当前
                // title 相等才写入（锁内复检——用户已改名则放弃，不报错）
                let allowed = match &patch.expected_title {
                    Some(exp) => meta.title == *exp,
                    None => true,
                };
                if allowed {
                    meta.title = t.clone();
                }
            }
        }
        if let Some(t) = patch.context_tokens {
            meta.context_tokens = Some(t);
        }
        if let Some(s) = patch.last_notice_seq {
            meta.last_notice_seq = Some(s);
        }
        if let Some(c) = &patch.compaction {
            meta.compaction = Some(c.clone());
        }
        if patch.context_tokens_clear {
            meta.context_tokens = None;
        }
        meta.updated_at = now_ms();
        self.write_index(&index);
        Ok(())
    }

    /// 生成标题专用 CAS 写入：仅当会话当前 title == `expected_title` 时写入。
    ///
    /// 三态返回（见 [`TitleWriteOutcome`]）——调用方据此决定是否推送事件：
    /// - `Written`：已写入（推送事件刷新前端）
    /// - `NotExists`：会话文件尚未落盘（首轮流式期）→ 未写入（仍推送，前端携带）
    /// - `TitleChanged`：用户已改名/已生成过 → 放弃（不推送，避免覆盖用户标题）
    ///
    /// index 锁内比较+写，消灭 TOCTOU（写盘侧复检）。index 缺条目时以 header
    /// 创建快照兜底比较后插入（内容文件为事实源）。
    pub fn try_set_title_if_expected(
        &mut self,
        conversation_id: &str,
        title: &str,
        expected_title: &str,
    ) -> Result<TitleWriteOutcome, String> {
        let path = self.file_path(conversation_id)?;
        let mut index = self.load_or_rebuild()?;
        if !path.exists() {
            return Ok(TitleWriteOutcome::NotExists);
        }
        match index.iter_mut().find(|m| m.id == conversation_id) {
            Some(meta) => {
                if meta.title != expected_title {
                    return Ok(TitleWriteOutcome::TitleChanged);
                }
                meta.title = title.to_string();
                meta.updated_at = now_ms();
            }
            None => {
                // index 缺条目：以 header 创建快照兜底比较后插入
                let header = self.read_header(&path).map_err(|e| {
                    format!("会话文件损坏（{}）：{e}", path.display())
                })?;
                if header.title != expected_title {
                    return Ok(TitleWriteOutcome::TitleChanged);
                }
                let mut meta = meta_from_header(header);
                meta.title = title.to_string();
                meta.updated_at = now_ms();
                index.push(meta);
            }
        }
        self.write_index(&index);
        Ok(TitleWriteOutcome::Written)
    }

    /// 删除会话：删内容文件 + 从 index 移除条目。幂等：文件/条目已不在 → Ok
    /// （重复删除无害）。index 写失败 eprintln 不抛——残留条目由下次 list 的
    /// 孤儿清理移除（文件已删）。
    pub fn delete(&mut self, conversation_id: &str) -> Result<(), String> {
        let path = self.file_path(conversation_id)?;
        let mut index = self.load_or_rebuild()?;
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("删除会话文件失败（{}）：{e}", path.display()))?;
        }
        let before = index.len();
        index.retain(|m| m.id != conversation_id);
        if index.len() != before {
            self.write_index(&index);
        }
        Ok(())
    }

    // ── 读路径 ──

    /// 列出全部会话元数据（仅 index.json；列表绝不扫内容文件）。
    ///
    /// 附带孤儿清理：index 条目对应的 jsonl 缺失或 header 无效 → 移除条目
    /// （**保留 jsonl 文件**，eprintln 留痕），并整写回 index。
    /// index 只读降级（schemaVersion 过高）→ Ok(空列表)，不写任何数据。
    pub fn list(&mut self) -> Result<Vec<ConversationMeta>, String> {
        let mut sessions = match self.load_or_rebuild() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[session_store] list 只读降级（不覆盖写）：{e}");
                return Ok(Vec::new());
            }
        };
        let mut removed = false;
        sessions.retain(|m| {
            let path = self.dir.join(format!("{}.jsonl", m.id));
            let valid = match self.read_header(&path) {
                Ok(h) => {
                    h.conversation_id == m.id && h.schema_version <= SCHEMA_VERSION
                }
                Err(_) => false,
            };
            if !valid {
                eprintln!(
                    "[session_store] index 条目 {} 对应会话文件缺失/损坏，移除条目（保留文件）",
                    m.id
                );
                removed = true;
            }
            valid
        });
        if removed {
            self.write_index(&sessions);
        }
        Ok(sessions)
    }

    /// 读取单个会话：meta + 轮行原文。
    ///
    /// header 无效 → Err（会话不存在或文件损坏）；轮行坏行 → 跳过并 eprintln
    /// （其余行照常返回）。meta 优先取 index（含 rename/updatedAt），index 无
    /// 条目时用 header 构造兜底（内容文件为事实源）。
    pub fn get(&mut self, conversation_id: &str) -> Result<ConversationRecord, String> {
        let path = self.file_path(conversation_id)?;
        let header = self.read_header(&path).map_err(|e| {
            format!("会话不存在或文件损坏（{conversation_id}）：{e}")
        })?;
        if header.conversation_id != conversation_id {
            return Err(format!(
                "会话文件归属不符（{conversation_id}）：header conversationId 不一致"
            ));
        }
        if header.schema_version > SCHEMA_VERSION {
            return Err(format!(
                "会话文件 schemaVersion {} 高于当前 {SCHEMA_VERSION}（需升级应用）",
                header.schema_version
            ));
        }
        let turns = self.read_turns(&path)?;
        // index 只读降级（Err）不影响内容读取——用 header 构造 meta 兜底
        let meta = match self.load_or_rebuild() {
            Ok(list) => list.into_iter().find(|m| m.id == conversation_id),
            Err(e) => {
                eprintln!("[session_store] get 索引降级，用 header meta 兜底：{e}");
                None
            }
        };
        let meta = meta.unwrap_or_else(|| meta_from_header(header));
        Ok(ConversationRecord { meta, turns })
    }

    // ── 内部辅助 ──

    /// conversationId 文件名安全校验（仅 [A-Za-z0-9_-]，排除路径分隔与 "."/".."）。
    fn valid_id(id: &str) -> bool {
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }

    fn file_path(&self, id: &str) -> Result<PathBuf, String> {
        if !Self::valid_id(id) {
            return Err(format!("conversationId 非法（仅允许字母数字与 - _）：{id:?}"));
        }
        Ok(self.dir.join(format!("{id}.jsonl")))
    }

    /// 读内容文件 header（首行）。文件缺失/为空/首行非合法 header 行 → Err。
    fn read_header(&self, path: &Path) -> Result<SessionHeader, String> {
        let file =
            std::fs::File::open(path).map_err(|e| format!("打开会话文件失败: {e}"))?;
        let mut lines = BufReader::new(file).lines();
        let first = lines
            .next()
            .ok_or("会话文件为空（缺 header 行）")?
            .map_err(|e| format!("读取 header 行失败: {e}"))?;
        let h: SessionHeader = serde_json::from_str(first.trim_end_matches(['\r', '\n']))
            .map_err(|e| format!("header 行解析失败: {e}"))?;
        if h.kind != HEADER_KIND {
            return Err(format!("首行不是 header（kind={:?}）", h.kind));
        }
        if h.format != HEADER_FORMAT {
            return Err(format!("不认识的格式标识：{:?}", h.format));
        }
        Ok(h)
    }

    /// 读轮行原文（跳过首行 header 与空行；坏行跳过不整体失败）。
    fn read_turns(&self, path: &Path) -> Result<Vec<String>, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("读取会话文件失败: {e}"))?;
        let mut turns = Vec::new();
        for (i, line) in raw.split('\n').enumerate() {
            if i == 0 {
                continue; // header 行
            }
            let line = line.trim_end_matches('\r'); // '\n' 已被 split 去除
            if line.is_empty() {
                continue;
            }
            if serde_json::from_str::<serde_json::Value>(line).is_err() {
                let preview: String = line.chars().take(80).collect();
                eprintln!(
                    "[session_store] {} 第 {} 行损坏，跳过：{}",
                    path.display(),
                    i + 1,
                    preview
                );
                continue;
            }
            turns.push(line.to_string());
        }
        Ok(turns)
    }

    /// 读 index.json。三态：
    /// - 文件缺失 / 读取失败 / parse 失败 → Ok(None)（调用方触发自愈重建）
    /// - schemaVersion > 当前 → Err（只读降级，绝不覆盖写）
    /// - 正常 → Ok(Some(sessions))
    fn try_load_index(&self) -> Result<Option<Vec<ConversationMeta>>, String> {
        let path = self.dir.join(INDEX_FILE);
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                eprintln!("[session_store] index 读取失败（按损坏重建）：{e}");
                return Ok(None);
            }
        };
        match serde_json::from_str::<SessionIndex>(&raw) {
            Ok(idx) => {
                if idx.schema_version > SCHEMA_VERSION {
                    eprintln!(
                        "[session_store] index schemaVersion {} 高于当前 {SCHEMA_VERSION}：只读降级",
                        idx.schema_version
                    );
                    return Err(format!(
                        "索引 schemaVersion {} 高于当前 {SCHEMA_VERSION}（需升级应用）",
                        idx.schema_version
                    ));
                }
                Ok(Some(idx.sessions))
            }
            Err(e) => {
                eprintln!("[session_store] index 解析失败（按损坏重建）：{e}");
                Ok(None)
            }
        }
    }

    /// index 加载：正常 → 直接返回；缺失/损坏 → 从 jsonl header 自愈重建并写回。
    /// schemaVersion 过高 → Err 透传。
    fn load_or_rebuild(&mut self) -> Result<Vec<ConversationMeta>, String> {
        match self.try_load_index()? {
            Some(sessions) => Ok(sessions),
            None => {
                let rebuilt = self.rebuild_index();
                self.write_index(&rebuilt);
                Ok(rebuilt)
            }
        }
    }

    /// 扫描 sessions/ 下全部 jsonl 的 header 行重建索引。
    /// 跳过：schemaVersion 过高、header conversationId 与文件名不符、header 损坏
    /// 的文件（均 eprintln 留痕，不删文件）。重建条目 title=header 创建快照、
    /// updatedAt=createdAt、其余字段 null。
    fn rebuild_index(&self) -> Vec<ConversationMeta> {
        let mut sessions = Vec::new();
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("[session_store] 扫描 sessions 目录失败（索引重建为空）：{e}");
                return sessions;
            }
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("jsonl"))
            .collect();
        files.sort(); // 确定性顺序
        for path in files {
            match self.read_header(&path) {
                Ok(h) => {
                    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                    if h.schema_version > SCHEMA_VERSION {
                        eprintln!(
                            "[session_store] 跳过 schemaVersion 过高的会话文件：{}",
                            path.display()
                        );
                        continue;
                    }
                    if h.conversation_id != stem {
                        eprintln!(
                            "[session_store] 会话文件与 header 归属不符，跳过：{}",
                            path.display()
                        );
                        continue;
                    }
                    sessions.push(meta_from_header(h));
                }
                Err(e) => {
                    eprintln!(
                        "[session_store] 跳过无效会话文件 {}：{e}",
                        path.display()
                    );
                }
            }
        }
        sessions
    }

    /// index.json 整写 + tmp+rename 原子替换。失败仅 eprintln 告警不抛
    /// （内容文件已落；条目可在后续操作补回或由 list/自愈重建）。
    fn write_index(&self, sessions: &[ConversationMeta]) {
        let idx = SessionIndex {
            schema_version: SCHEMA_VERSION,
            sessions: sessions.to_vec(),
        };
        let json = match serde_json::to_string_pretty(&idx) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("[session_store] index 序列化失败（不写盘）：{e}");
                return;
            }
        };
        let target = self.dir.join(INDEX_FILE);
        let tmp = self.dir.join(INDEX_TMP);
        if let Err(e) = std::fs::write(&tmp, json) {
            eprintln!("[session_store] index 临时文件写入失败（{}）：{e}", tmp.display());
            return;
        }
        if let Err(e) = std::fs::rename(&tmp, &target) {
            eprintln!("[session_store] index 原子替换失败（{}）：{e}", target.display());
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// 从 header 构造兜底 meta（自愈重建 / index 缺条目补回 / 只读降级读路径用）。
/// modelName/contextLength/contextTokens/lastNoticeSeq 无法从 header 恢复 → None。
fn meta_from_header(h: SessionHeader) -> ConversationMeta {
    ConversationMeta {
        id: h.conversation_id,
        title: h.title,
        created_at: h.created_at,
        updated_at: h.created_at,
        model_name: None,
        context_length: None,
        context_tokens: None,
        last_notice_seq: None,
        compaction: None,
    }
}

/// 当前时间（unix 毫秒；clock 异常时回退 0）。
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建唯一临时目录（不引入 tempfile crate，沿用 notices/config 夹具模式）。
    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-sess-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 无 meta 的基础 append 请求（各测试按需填充字段）。
    fn req(conversation_id: &str, events_json: &str) -> AppendTurnReq {
        AppendTurnReq {
            conversation_id: conversation_id.into(),
            events_json: events_json.into(),
            title: None,
            created_at: None,
            model_name: None,
            context_length: None,
            context_tokens: None,
            last_notice_seq: None,
        }
    }

    /// 简单轮事件（user 消息）。
    fn ev_user(content: &str) -> String {
        format!(r#"[{{"type":"user/message","sessionId":"s1","content":"{content}"}}]"#)
    }

    fn read_lines(dir: &Path, id: &str) -> Vec<String> {
        let raw = std::fs::read_to_string(dir.join("sessions").join(format!("{id}.jsonl")))
            .unwrap();
        raw.split('\n')
            .map(|l| l.trim_end_matches('\r').to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }

    fn read_index_value(dir: &Path) -> serde_json::Value {
        let raw =
            std::fs::read_to_string(dir.join("sessions").join(INDEX_FILE)).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    // ── R1：roundtrip（内容原文逐字节 + meta 全字段）──

    #[test]
    fn roundtrip_appends_list_gets_with_byte_exact_turns() {
        let dir = unique_temp_dir("r1-roundtrip");
        let mut s = SessionStore::new(&dir);

        // 含块版本（[prts_reminder] 转义 \n）——冻结历史必须逐字节恢复
        let ev1 = r#"[{"type":"user/message","sessionId":"s1","content":"你好"}]"#;
        let ev2 = r#"[{"type":"user/message","sessionId":"s1","content":"[prts_reminder]\n- 模型：deepseek\n[/prts_reminder]\n\n继续"}]"#;
        let mut r1 = req("c1", ev1);
        r1.title = Some("查询".into());
        r1.created_at = Some(1234);
        r1.model_name = Some("deepseek-chat".into());
        r1.context_length = Some(64000);
        r1.context_tokens = Some(TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
        });
        r1.last_notice_seq = Some(3);
        assert!(s.append_turn(&r1).unwrap().created);

        let r2 = req("c1", ev2);
        let out2 = s.append_turn(&r2).unwrap();
        assert!(!out2.created, "第二轮不应重建文件");

        // list：meta 全字段
        let list = s.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "c1");
        assert_eq!(list[0].title, "查询");
        assert_eq!(list[0].created_at, 1234);
        assert_eq!(list[0].model_name.as_deref(), Some("deepseek-chat"));
        assert_eq!(list[0].context_length, Some(64000));
        assert_eq!(
            list[0].context_tokens,
            Some(TokenUsage {
                input_tokens: 10,
                output_tokens: 5
            })
        );
        assert_eq!(list[0].last_notice_seq, Some(3));
        assert!(list[0].updated_at >= 1234);

        // get：轮行原文逐字节一致（含块版本冻结）
        let rec = s.get("c1").unwrap();
        assert_eq!(rec.meta.id, "c1");
        assert_eq!(rec.turns.len(), 2);
        assert_eq!(rec.turns[0], ev1);
        assert_eq!(rec.turns[1], ev2);

        // 文件结构：1 header + 2 轮行
        let lines = read_lines(&dir, "c1");
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("\"type\":\"header\""));
        assert_eq!(lines[1], ev1);
        assert_eq!(lines[2], ev2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── R2：首轮创建（header 字段 + index 条目）──

    #[test]
    fn first_append_creates_header_and_index_entry() {
        let dir = unique_temp_dir("r2-first");
        let mut s = SessionStore::new(&dir);
        let mut r = req("c2", &ev_user("首问"));
        r.title = Some("首问标题".into());
        r.created_at = Some(555);
        assert!(s.append_turn(&r).unwrap().created);

        let lines = read_lines(&dir, "c2");
        assert_eq!(lines.len(), 2);
        let h: SessionHeader = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(h.kind, "header");
        assert_eq!(h.format, "prts-conversation-jsonl");
        assert_eq!(h.schema_version, SCHEMA_VERSION);
        assert_eq!(h.conversation_id, "c2");
        assert_eq!(h.created_at, 555);
        assert_eq!(h.title, "首问标题");

        // index.json 条目与 header 一致
        let v = read_index_value(&dir);
        assert_eq!(v["schemaVersion"], 1);
        assert_eq!(v["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(v["sessions"][0]["id"], "c2");
        assert_eq!(v["sessions"][0]["title"], "首问标题");
        assert_eq!(v["sessions"][0]["createdAt"], 555);
        assert_eq!(v["sessions"][0]["updatedAt"].as_u64().unwrap() >= 555, true);
        assert_eq!(v["sessions"][0]["modelName"], serde_json::Value::Null);
        assert_eq!(v["sessions"][0]["lastNoticeSeq"], serde_json::Value::Null);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── R3：追加轮不重复 header ──

    #[test]
    fn subsequent_appends_do_not_duplicate_header() {
        let dir = unique_temp_dir("r3-append");
        let mut s = SessionStore::new(&dir);
        let mut r = req("c3", &ev_user("第一问"));
        r.created_at = Some(100);
        r.title = Some("T".into());
        s.append_turn(&r).unwrap();

        s.append_turn(&req("c3", &ev_user("第二问"))).unwrap();
        s.append_turn(&req("c3", &ev_user("第三问"))).unwrap();

        let lines = read_lines(&dir, "c3");
        assert_eq!(lines.len(), 4, "1 header + 3 轮行");
        // 首行仍是原 header（未被覆盖或重复）
        let h: SessionHeader = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(h.kind, "header");
        assert_eq!(h.conversation_id, "c3");
        assert_eq!(h.created_at, 100);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── R4：index 原子替换（tmp 不残留；updatedAt 随轮单调）──

    #[test]
    fn index_atomic_replace_no_tmp_leftover_and_updated_at_monotonic() {
        let dir = unique_temp_dir("r4-atomic");
        let mut s = SessionStore::new(&dir);
        let mut r = req("c4", &ev_user("一"));
        r.created_at = Some(1);
        s.append_turn(&r).unwrap();
        assert!(
            !dir.join("sessions").join(INDEX_TMP).exists(),
            "tmp 文件不应残留"
        );
        let first = read_index_value(&dir);
        let u1 = first["sessions"][0]["updatedAt"].as_u64().unwrap();

        s.append_turn(&req("c4", &ev_user("二"))).unwrap();
        assert!(!dir.join("sessions").join(INDEX_TMP).exists());
        let second = read_index_value(&dir);
        let u2 = second["sessions"][0]["updatedAt"].as_u64().unwrap();
        assert!(u2 >= u1, "updatedAt 应单调不减：{u1} → {u2}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── R5：轮行坏行跳过（get 返回其余行，不整体失败）──

    #[test]
    fn corrupted_turn_line_is_skipped_on_get() {
        let dir = unique_temp_dir("r5-badline");
        let mut s = SessionStore::new(&dir);
        let ev1 = ev_user("好行一");
        let ev2 = ev_user("好行二");
        let mut r = req("b1", &ev1);
        r.created_at = Some(1);
        s.append_turn(&r).unwrap();

        // 手工注入坏行/空行（模拟崩溃半行或外部编辑）
        let path = dir.join("sessions").join("b1.jsonl");
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut content = raw; // header + 好轮 1
        content.push_str("garbage{{{not json at all\n"); // 坏行
        content.push_str(&format!("{ev2}\n")); // 好轮 2
        content.push_str("\n"); // 空行
        std::fs::write(&path, content).unwrap();

        let rec = s.get("b1").unwrap();
        assert_eq!(rec.turns.len(), 2, "坏行与空行应被跳过");
        assert_eq!(rec.turns[0], ev1);
        assert_eq!(rec.turns[1], ev2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── R6：header 损坏 → get Err；list 移除条目且保留文件 ──

    #[test]
    fn corrupted_header_makes_get_err_and_list_prunes_entry() {
        let dir = unique_temp_dir("r6-badheader");
        let mut s = SessionStore::new(&dir);
        let mut r = req("c6", &ev_user("x"));
        r.created_at = Some(1);
        r.title = Some("T".into());
        s.append_turn(&r).unwrap();
        assert_eq!(s.get("c6").unwrap().turns.len(), 1);

        // 破坏 header 行（保留轮行）
        let path = dir.join("sessions").join("c6.jsonl");
        let raw = std::fs::read_to_string(&path).unwrap();
        let rest: Vec<&str> = raw.splitn(2, '\n').collect();
        std::fs::write(&path, format!("this is not a header\n{}", rest[1])).unwrap();

        // get → Err（文件损坏）
        assert!(s.get("c6").is_err());

        // list → 条目被移除（文件保留）
        let list = s.list().unwrap();
        assert_eq!(list.len(), 0);
        assert!(path.exists(), "损坏的 jsonl 文件应保留（不自动删除）");
        let v = read_index_value(&dir);
        assert_eq!(v["sessions"].as_array().unwrap().len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── R7：index 缺失 → 自愈重建（扫 jsonl header）──

    #[test]
    fn missing_index_rebuilds_from_jsonl_headers() {
        let dir = unique_temp_dir("r7-rebuild");
        let mut s = SessionStore::new(&dir);
        let mut r = req("c7", &ev_user("x"));
        r.created_at = Some(777);
        r.title = Some("自愈标题".into());
        s.append_turn(&r).unwrap();

        // 删除 index.json（模拟损坏/丢失）
        std::fs::remove_file(dir.join("sessions").join(INDEX_FILE)).unwrap();

        let list = s.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "c7");
        assert_eq!(list[0].title, "自愈标题", "title 取 header 创建快照");
        assert_eq!(list[0].created_at, 777);
        assert_eq!(list[0].updated_at, 777, "重建后 updatedAt = createdAt");
        assert_eq!(list[0].model_name, None, "其余字段重建为空");

        // index 已写回（新文件 schemaVersion 1）
        let v = read_index_value(&dir);
        assert_eq!(v["schemaVersion"], 1);
        assert_eq!(v["sessions"].as_array().unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── R8：delete（文件 + index 条目；幂等）──

    #[test]
    fn delete_removes_file_and_entry_and_is_idempotent() {
        let dir = unique_temp_dir("r8-delete");
        let mut s = SessionStore::new(&dir);
        s.append_turn(&req("a", &ev_user("1"))).unwrap();
        s.append_turn(&req("b", &ev_user("2"))).unwrap();
        assert_eq!(s.list().unwrap().len(), 2);

        s.delete("a").unwrap();
        assert!(
            !dir.join("sessions").join("a.jsonl").exists(),
            "内容文件应被删除"
        );
        let list = s.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "b");

        // 幂等：重复删除（文件与条目都已不在）→ Ok
        assert!(s.delete("a").is_ok());
        assert!(!dir.join("sessions").join("a.jsonl").exists(), "不应复活文件");

        s.delete("b").unwrap();
        assert!(s.list().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── R9：index schemaVersion 过高 → list 空 + 不覆盖写；写操作拒绝 ──

    #[test]
    fn future_schema_version_readonly_degrades_without_overwrite() {
        let dir = unique_temp_dir("r9-future");
        let mut s = SessionStore::new(&dir);
        s.append_turn(&req("c9", &ev_user("x"))).unwrap();

        // 篡改 index schemaVersion → 2
        let path = dir.join("sessions").join(INDEX_FILE);
        let raw = std::fs::read_to_string(&path).unwrap();
        let tampered = raw.replace("\"schemaVersion\": 1", "\"schemaVersion\": 2");
        assert_ne!(raw, tampered, "篡改应生效（pretty 格式含空格）");
        std::fs::write(&path, tampered).unwrap();

        // list → Ok(空)（只读降级）
        assert!(s.list().unwrap().is_empty());

        // 文件未被覆盖写（仍是 schemaVersion 2）
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("\"schemaVersion\": 2"),
            "降级期间不得覆盖高版本索引"
        );

        // 写操作拒绝
        assert!(s.append_turn(&req("c9b", &ev_user("y"))).is_err());
        assert!(
            s.update_meta("c9", &MetaPatch::default()).is_err(),
            "update_meta 应拒绝"
        );
        assert!(s.delete("c9").is_err(), "delete 应拒绝（不写高版本索引）");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── R10：conversationId 文件名安全校验 ──

    #[test]
    fn invalid_conversation_ids_rejected() {
        let dir = unique_temp_dir("r10-id");
        let mut s = SessionStore::new(&dir);
        for bad in ["", "a/b", "../x", "x.y", "a b", "a\\b", ".hidden"] {
            assert!(
                s.append_turn(&req(bad, &ev_user("x"))).is_err(),
                "非法 id 应拒绝 append：{bad:?}"
            );
            assert!(s.get(bad).is_err(), "非法 id 应拒绝 get：{bad:?}");
            assert!(s.delete(bad).is_err(), "非法 id 应拒绝 delete：{bad:?}");
            assert!(
                s.update_meta(bad, &MetaPatch::default()).is_err(),
                "非法 id 应拒绝 update_meta：{bad:?}"
            );
        }
        // 合法 id（含 - _）正常
        let ok = "a-b_c1";
        assert!(s.append_turn(&req(ok, &ev_user("x"))).unwrap().created);
        assert_eq!(s.get(ok).unwrap().turns.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── R11（补充）：events_json 非合法数组拒绝落盘 ──

    #[test]
    fn invalid_events_json_rejected_before_write() {
        let dir = unique_temp_dir("r11-events");
        let mut s = SessionStore::new(&dir);
        // 非 JSON / 非数组 / 对象：全部拒绝
        for bad in [
            "not json at all",
            r#"{"type":"user/message","sessionId":"s"}"#,
            r#""just a string""#,
            "123",
            "",
        ] {
            let err = s.append_turn(&req("c11", bad)).unwrap_err();
            assert!(
                err.contains("eventsJson"),
                "应报 eventsJson 错误，got: {err}"
            );
        }
        assert!(
            !dir.join("sessions").join("c11.jsonl").exists(),
            "非法内容不应创建文件"
        );
        // 空数组合法（防御性接受，前端正常轮恒 ≥1 事件）
        assert!(s.append_turn(&req("c11", "[]")).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── IPC 契约：AppendTurnReq camelCase 反序列化 + null/缺失 → None ──

    #[test]
    fn append_turn_req_deserializes_camelcase_with_null_and_missing_defaults() {
        // 全字段 camelCase
        let json = r#"{"conversationId":"c1","eventsJson":"[]","title":"T","createdAt":1,"modelName":"m","contextLength":64000,"contextTokens":{"inputTokens":1,"outputTokens":2},"lastNoticeSeq":3}"#;
        let r: AppendTurnReq = serde_json::from_str(json).unwrap();
        assert_eq!(r.conversation_id, "c1");
        assert_eq!(r.events_json, "[]");
        assert_eq!(r.title.as_deref(), Some("T"));
        assert_eq!(r.created_at, Some(1));
        assert_eq!(r.model_name.as_deref(), Some("m"));
        assert_eq!(r.context_length, Some(64000));
        assert_eq!(
            r.context_tokens,
            Some(TokenUsage {
                input_tokens: 1,
                output_tokens: 2
            })
        );
        assert_eq!(r.last_notice_seq, Some(3));

        // null → None；缺失可选字段 → None（serde default）
        let json2 = r#"{"conversationId":"c2","eventsJson":"[]","title":null,"contextTokens":null}"#;
        let r2: AppendTurnReq = serde_json::from_str(json2).unwrap();
        assert_eq!(r2.conversation_id, "c2");
        assert_eq!(r2.title, None);
        assert_eq!(r2.created_at, None);
        assert_eq!(r2.context_tokens, None);
        assert_eq!(r2.last_notice_seq, None);

        // 必填字段缺失 → 报错
        let bad = r#"{"title":"T"}"#;
        assert!(serde_json::from_str::<AppendTurnReq>(bad).is_err());
    }

    // ── 随轮覆盖：contextTokens/lastNoticeSeq 非 None 覆盖、None 不覆盖 ──

    #[test]
    fn per_turn_tokens_and_notice_seq_override_when_some() {
        let dir = unique_temp_dir("r12-override");
        let mut s = SessionStore::new(&dir);
        let mut r1 = req("c12", &ev_user("一"));
        r1.created_at = Some(1);
        r1.context_tokens = Some(TokenUsage {
            input_tokens: 100,
            output_tokens: 10,
        });
        r1.last_notice_seq = Some(5);
        s.append_turn(&r1).unwrap();

        // 第二轮：contextTokens 更新、游标更新
        let mut r2 = req("c12", &ev_user("二"));
        r2.context_tokens = Some(TokenUsage {
            input_tokens: 200,
            output_tokens: 20,
        });
        r2.last_notice_seq = Some(9);
        s.append_turn(&r2).unwrap();

        // 第三轮：均缺省 → 不覆盖（保持上轮值）
        s.append_turn(&req("c12", &ev_user("三"))).unwrap();

        let list = s.list().unwrap();
        assert_eq!(
            list[0].context_tokens,
            Some(TokenUsage {
                input_tokens: 200,
                output_tokens: 20
            }),
            "None 不应覆盖已提交值"
        );
        assert_eq!(list[0].last_notice_seq, Some(9));

        // update_meta 显式 patch 也走同语义
        s.update_meta(
            "c12",
            &MetaPatch {
                title: Some("改名".into()),
                expected_title: None,
                context_tokens: None,
                last_notice_seq: Some(11),
                compaction: None,
                context_tokens_clear: false,
            },
        )
        .unwrap();
        let list = s.list().unwrap();
        assert_eq!(list[0].title, "改名");
        assert_eq!(list[0].last_notice_seq, Some(11));
        assert_eq!(
            list[0].context_tokens,
            Some(TokenUsage {
                input_tokens: 200,
                output_tokens: 20
            }),
            "patch 只更新非 None 字段"
        );
        // header 不改（rename 不回写 header——自愈用创建快照；未传 title 回退默认）
        let lines = read_lines(&dir, "c12");
        let h: SessionHeader = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(h.title, "新对话", "header title 保持创建时回退值，rename 不回写");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 模块 D：compaction 元数据字段 ──

    #[test]
    fn compaction_meta_roundtrips_via_update_meta() {
        let dir = unique_temp_dir("compaction-meta");
        let mut s = SessionStore::new(&dir);
        s.append_turn(&req("c-meta", &ev_user("你好"))).unwrap();

        let record = CompactionMeta {
            retained_from: 3,
            summary: "## Goal\n达成".into(),
            tokens_before: Some(1234),
            at: 456,
        };
        s.update_meta(
            "c-meta",
            &MetaPatch {
                title: None,
                expected_title: None,
                context_tokens: None,
                last_notice_seq: None,
                compaction: Some(record.clone()),
                context_tokens_clear: false,
            },
        )
        .unwrap();

        let list = s.list().unwrap();
        assert_eq!(
            list[0].compaction,
            Some(record.clone()),
            "update_meta 应写入 compaction 记录"
        );

        // patch 不带 compaction（None）→ 已写值保持（非 None 字段语义）
        s.update_meta(
            "c-meta",
            &MetaPatch {
                title: Some("改名".into()),
                expected_title: None,
                context_tokens: None,
                last_notice_seq: None,
                compaction: None,
                context_tokens_clear: false,
            },
        )
        .unwrap();
        let list = s.list().unwrap();
        assert_eq!(
            list[0].compaction,
            Some(record),
            "compaction=None 不覆盖已写记录"
        );
        assert_eq!(list[0].title, "改名");

        // 盘上 index 原文应含 camelCase 键（serde 契约；write_index 用 pretty 格式）
        let raw = std::fs::read_to_string(dir.join("sessions").join("index.json")).unwrap();
        assert!(raw.contains("\"compaction\""), "index 应含 compaction 键");
        assert!(raw.contains("\"retainedFrom\": 3"), "retainedFrom camelCase");
        assert!(raw.contains("\"tokensBefore\": 1234"), "tokensBefore camelCase");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_index_without_compaction_field_loads_as_none() {
        // 旧版本 index.json 无 compaction 键：新程序读取必须兼容（Option 缺省 None，
        // 不升 schemaVersion 的双向兼容承诺——旧程序读新 index 忽略未知字段同理）。
        let dir = unique_temp_dir("compaction-legacy");
        let mut s = SessionStore::new(&dir);
        s.append_turn(&req("c-leg", &ev_user("你好"))).unwrap();

        let index_path = dir.join("sessions").join("index.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&index_path).unwrap()).unwrap();
        let sessions = v["sessions"].as_array_mut().unwrap();
        assert!(
            sessions[0]
                .as_object_mut()
                .unwrap()
                .remove("compaction")
                .is_some(),
            "新写入的 index 应含 compaction——测试前提要求它被手动移除"
        );
        std::fs::write(&index_path, serde_json::to_string(&v).unwrap()).unwrap();

        // 重新打开（内存态重建）→ 旧格式 index 可读且 compaction=None
        let mut s2 = SessionStore::new(&dir);
        let list = s2.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].compaction, None, "缺字段应兜底 None");
        assert_eq!(list[0].id, "c-leg");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn context_tokens_clear_persists_null_override() {
        // 模块 D：压缩归零的持久面——contextTokens 置 None 后重启必须保持归零
        // （此前 MetaPatch.context_tokens=None 表达"不更新"，清空无法写盘，
        // 重启 hydration 读回压缩前的旧用量——本测试锁定显式清空语义）。
        let dir = unique_temp_dir("compaction-clear");
        let mut s = SessionStore::new(&dir);
        let mut r = req("c-clr", &ev_user("你好"));
        r.context_tokens = Some(TokenUsage {
            input_tokens: 22_000,
            output_tokens: 500,
        });
        s.append_turn(&r).unwrap();
        assert_eq!(s.list().unwrap()[0].context_tokens, Some(TokenUsage {
            input_tokens: 22_000,
            output_tokens: 500,
        }));

        // 显式清空（压缩完成时随 compaction 一并提交）
        s.update_meta(
            "c-clr",
            &MetaPatch {
                title: None,
                expected_title: None,
                context_tokens: None,
                last_notice_seq: None,
                compaction: Some(CompactionMeta {
                    retained_from: 3,
                    summary: "## Goal\n摘要".into(),
                    tokens_before: Some(21_000),
                    at: 7,
                }),
                context_tokens_clear: true,
            },
        )
        .unwrap();

        // 重新打开（模拟重启 hydration 读盘）→ contextTokens 保持 null
        let mut s2 = SessionStore::new(&dir);
        let list = s2.list().unwrap();
        assert_eq!(list[0].context_tokens, None, "清空应持久化（重启后环保持归零）");
        assert_eq!(list[0].compaction.as_ref().unwrap().retained_from, 3);

        // 之后普通轮无 usage（None）不得复活旧值（保持 None）
        s2.append_turn(&req("c-clr", &ev_user("继续"))).unwrap();
        let list = s2.list().unwrap();
        assert_eq!(list[0].context_tokens, None, "None 不覆盖语义保持：清空后无新 usage 仍是 null");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── 生成标题 CAS（try_set_title_if_expected / MetaPatch.expected_title）──

    #[test]
    fn title_cas_written_when_expected_matches() {
        let dir = unique_temp_dir("title-cas-written");
        let mut s = SessionStore::new(&dir);
        let mut r = req("t1", &ev_user("查源石虫"));
        r.title = Some("查源石虫".into());
        s.append_turn(&r).unwrap();

        let out = s.try_set_title_if_expected("t1", "源石虫数值", "查源石虫").unwrap();
        assert_eq!(out, TitleWriteOutcome::Written);
        assert_eq!(s.list().unwrap()[0].title, "源石虫数值");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn title_cas_skips_when_title_changed() {
        let dir = unique_temp_dir("title-cas-changed");
        let mut s = SessionStore::new(&dir);
        let mut r = req("t2", &ev_user("查源石虫"));
        r.title = Some("查源石虫".into());
        s.append_turn(&r).unwrap();
        // 用户改名
        s.update_meta(
            "t2",
            &MetaPatch {
                title: Some("我的标题".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let out = s.try_set_title_if_expected("t2", "源石虫数值", "查源石虫").unwrap();
        assert_eq!(out, TitleWriteOutcome::TitleChanged);
        assert_eq!(s.list().unwrap()[0].title, "我的标题", "CAS 失败不得覆盖用户标题");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn title_cas_not_exists_when_session_missing() {
        let dir = unique_temp_dir("title-cas-missing");
        let mut s = SessionStore::new(&dir);
        let out = s.try_set_title_if_expected("nope", "标题", "默认").unwrap();
        assert_eq!(out, TitleWriteOutcome::NotExists);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn title_cas_falls_back_to_header_when_index_entry_missing() {
        let dir = unique_temp_dir("title-cas-header-fallback");
        let mut s = SessionStore::new(&dir);
        let mut r = req("t3", &ev_user("首问"));
        r.title = Some("首问".into());
        s.append_turn(&r).unwrap();
        // 手工删除 index 条目（文件仍在）
        let idx_path = dir.join("sessions").join(INDEX_FILE);
        std::fs::write(&idx_path, r#"{"schemaVersion":1,"sessions":[]}"#).unwrap();

        let out = s.try_set_title_if_expected("t3", "新标题", "首问").unwrap();
        assert_eq!(out, TitleWriteOutcome::Written);
        assert_eq!(s.list().unwrap()[0].title, "新标题");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_meta_expected_title_guard_skips_mismatch() {
        let dir = unique_temp_dir("title-meta-guard");
        let mut s = SessionStore::new(&dir);
        let mut r = req("t4", &ev_user("首问"));
        r.title = Some("首问".into());
        s.append_turn(&r).unwrap();
        s.update_meta(
            "t4",
            &MetaPatch {
                title: Some("用户改的".into()),
                ..Default::default()
            },
        )
        .unwrap();
        // expected_title 不匹配 → 不写入，也不报错
        s.update_meta(
            "t4",
            &MetaPatch {
                title: Some("生成标题".into()),
                expected_title: Some("首问".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.list().unwrap()[0].title, "用户改的");

        // expected_title 匹配 → 写入
        s.update_meta(
            "t4",
            &MetaPatch {
                title: Some("生成标题".into()),
                expected_title: Some("用户改的".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.list().unwrap()[0].title, "生成标题");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
