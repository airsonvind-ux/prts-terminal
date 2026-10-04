//! 工具系统 —— 可注册、可查询、可执行的能力组件
//!
//! 工具是 Agent 的"手"。LLM 决定调用哪个工具后，orchestrator 通过
//! ToolRegistry 查找对应工具并执行，结果作为 tool_result 消息回传给 LLM
//! 参与后续推理。每个工具实现 Tool trait，在应用启动时注册到 Registry。

// mod cache; // 已注释（2026-08：缓存工具取消注册；未来记忆系统复用时可恢复）
mod display;
mod fs;
mod fs_grep;
mod fs_write;
mod knowledge;
mod list_knowledge;
mod memory;
mod query_enemy;
mod query_level;
mod query_operator;
mod query_rogue;
mod query_stage;
mod query_table;
mod query_token;
mod query_trap;
mod run_command;
mod schema;
mod search_entities;
mod sim_path;
mod skill;
mod time;
mod web_common;
mod web_fetch;
mod web_search;
pub mod wiki;
pub(crate) mod common;
pub(crate) mod common_fmt;

// pub use cache::{CacheListTool, CacheRemoveTool, CacheRetrieveTool, CacheStoreTool};
pub use fs::{ListDirTool, ReadFileTool};
pub use fs_grep::GrepTool;
pub use fs_write::{EditTool, WriteFileTool};
pub use knowledge::ReadKnowledgeTool;
pub use list_knowledge::ListKnowledgeTool;
pub use memory::{MemoryExperienceTool, MemoryGrowthTool};
pub use query_enemy::QueryEnemy;
pub use query_level::QueryLevel;
pub use query_operator::QueryOperator;
pub use query_rogue::QueryRogue;
pub use query_stage::QueryStage;
pub use query_table::QueryTable;
pub use query_token::QueryToken;
pub use query_trap::QueryTrap;
pub use search_entities::SearchEntities;
pub use run_command::RunCommandTool;
pub use display::{DisplayLink, ToolDisplay};
pub use sim_path::SimPath;
pub use skill::ActivateSkillTool;
pub use time::TimeTool;
pub use web_fetch::WebFetchTool;
pub use web_search::WebSearchTool;

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::db::Database;
use crate::llm::ToolDef;
use crate::policy::Policy;
use crate::turn_registry::AbortSignal;

// ═══════════════════════════════════════════════════════
// ToolContext —— 工具执行时的外部依赖容器
// ═══════════════════════════════════════════════════════

/// 工具执行时的上下文，携带工具可能需要的所有外部依赖。
///
/// `db` 为 None 时对应 Mock 测试场景（不连数据库也能跑）；
/// 生产环境通过 `with_db()` 注入。`Database` 内部已处理线程安全，
/// 外部用 `Arc` 共享只读连接。
///
/// `skill_index` 为 Skill 通道（turn 级索引快照）：Skill 激活工具
/// 查询目录与正文，索引由 orchestrator 在 process 入口重建后经本通道注入。
/// 缓存通道（cache_stores / conversation_id）已注释（2026-08：
/// 缓存工具取消注册；未来记忆系统复用时可恢复）。
///
/// `abort` 为中止信号通道（M3-B）：orchestrator 在 dispatch_turn 注入
/// （与 skill_index 同模式）；执行类工具（run_command）读入自身执行选项
/// （RunOptions.cancel → runner 轮询 kill）。None = 无中止能力（Mock/
/// 独立直调场景），执行内核按无取消处理。
#[derive(Debug)]
pub struct ToolContext {
    pub(crate) db: Option<Arc<Database>>,
    // /// 对话分区缓存集合（进程级共享状态，std::sync::Mutex 保护）。
    // /// 工具通过 conversation_id 定位本对话的分区。
    // pub(crate) cache_stores: Option<Arc<Mutex<crate::cache::CacheStores>>>,
    // /// 当前对话标识（与后端 agent_process 的 conversationId 一致）。
    // pub(crate) conversation_id: Option<String>,
    /// turn 级 Skill 索引快照（process 入口重建，Arc 共享零拷贝）。
    /// None = Mock 测试场景未注入（激活工具报「Skill 目录不可用」）。
    pub(crate) skill_index: Option<Arc<crate::skills_index::SkillIndex>>,
    /// 中止信号（M3-B；默认 None，dispatch_turn 注入）。
    pub(crate) abort: Option<AbortSignal>,
}

impl ToolContext {
    pub fn new() -> Self {
        Self {
            db: None,
            // cache_stores: None,
            // conversation_id: None,
            skill_index: None,
            abort: None,
        }
    }

    /// 注入数据库连接（生产环境）。
    pub fn with_db(db: Arc<Database>) -> Self {
        Self {
            db: Some(db),
            // cache_stores: None,
            // conversation_id: None,
            skill_index: None,
            abort: None,
        }
    }

    // /// 注入缓存区通道（对话分区 + 对话标识；db 为 None）。
    // ///
    // /// 供 orchestrator 在 db 为 None（Mock 测试场景）时构造；
    // /// 生产路径（有 db）用 `with_db()` 后再补 `cache_stores` /
    // /// `conversation_id` 字段，两者语义等价。
    // pub(crate) fn with_cache(
    //     cache_stores: Arc<Mutex<crate::cache::CacheStores>>,
    //     conversation_id: String,
    // ) -> Self {
    //     Self {
    //         db: None,
    //         cache_stores: Some(cache_stores),
    //         conversation_id: Some(conversation_id),
    //         skill_index: None,
    //     }
    // }
}

impl Default for ToolContext {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════
// ToolError
// ═══════════════════════════════════════════════════════

#[derive(Debug, Clone)]
pub enum ToolError {
    /// 请求的工具名在 Registry 中不存在
    NotFound(String),
    /// 工具执行过程中出错（数据库查询失败、数据异常等）
    Execution(String),
    /// 参数 JSON 解析失败
    Parse(String),
    /// 命令执行失败（run_command：非零退出码 / 超时 / 进程树杀 / spawn 失败）。
    /// 消息 = 面向 LLM 的完整文案（含退出码/输出尾部），Display 直接输出。
    CommandFailed(String),
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(name) => write!(f, "工具 '{}' 未注册", name),
            Self::Execution(msg) => write!(f, "工具执行失败：{}", msg),
            Self::Parse(msg) => write!(f, "参数解析失败：{}", msg),
            Self::CommandFailed(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for ToolError {}

// ═══════════════════════════════════════════════════════
// ContextOp —— 工具→中枢的上下文变更指令（Reference 层标记协议）
// ═══════════════════════════════════════════════════════

/// 工具执行后携带的上下文变更指令（设计决策 A.4：工具**不**直接修改
/// ActiveContext，返回标记由调度中枢统一应用）。
///
/// 纯数据定义：仅含 String 字段，tools 模块不依赖 orchestrator 的任何类型
/// （依赖方向单向——orchestrator 消费本类型），应用逻辑在
/// orchestrator 侧 `ActiveContext::apply_ops`。
/// 序列化形态为 tag = "op" 的 discriminated union，变体名 kebab-case，
/// 与日志/回放词汇一致（reference-load / reference-remove / reference-clear /
/// active-load）。
///
/// 变体名的 Reference/Active 前缀是接口契约（kebab-case op 值由变体名派生，
/// 协议定型后不可更名）；Reference* 由缓存工具构造，ActiveLoad 由
/// Skill 激活工具（activate_skill）构造。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
#[allow(clippy::enum_variant_names)] // 前缀为接口契约，见上方说明
#[allow(dead_code)] // 2026-08：动态层移除、缓存工具取消注册，无生产者；未来记忆系统复用时可恢复
pub enum ContextOp {
    /// 载入/更新 Reference 缓存条目（同名覆盖，key → content）
    ReferenceLoad { key: String, content: String },
    /// 移除单条 Reference 缓存条目
    ReferenceRemove { key: String },
    /// 清空 Reference 层载入状态
    #[allow(dead_code)] // reset_workspace 工具接线时构造（当前仅测试使用）
    ReferenceClear,
    /// 载入/更新 Active 层条目（Skill 激活，name → 正文）。
    /// 属 Active 层（Skill 激活），与 Reference 层变体共用同一标记协议——
    /// 生命周期相同（每 turn 重建，轮末自动卸载）。
    ActiveLoad { name: String, content: String },
}

// ═══════════════════════════════════════════════════════
// CacheEventMark —— 缓存操作标记（工具→中枢，模块 C）
// ═══════════════════════════════════════════════════════

/// 缓存操作标记（工具→中枢）：orchestrator 统一转为 session log 的 cache/* 事件。
/// 与 ContextOp 同哲学：工具不改 log，只陈述操作事实。
///
/// 纯数据定义（无需序列化）：工具执行后随 ToolOutput 携带，由调度中枢在
/// `execute_tools_and_append` 中按调用顺序统一落账（sink.log_only）——
/// 缓存区与 log 分层（session.rs）：log 只记录 `cache_*` 操作可回放，
/// 条目内容只活在 CacheStore 中。
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)] // 2026-08：缓存工具取消注册，无生产者；未来记忆系统复用时可恢复
pub enum CacheEventMark {
    /// 写入/覆盖桶条目
    Store {
        key: String,
        bucket: crate::session::CacheBucket,
        source_entry: Option<String>,
    },
    /// 读取桶条目（命中则载入 Reference 层）
    Load {
        key: String,
        bucket: crate::session::CacheBucket,
        hit: bool,
    },
    /// 移除桶条目
    Remove {
        key: String,
        bucket: crate::session::CacheBucket,
        existed: bool,
    },
}

// ═══════════════════════════════════════════════════════
// SkillEventMark —— Skill 激活标记（工具→中枢，模块 B）
// ═══════════════════════════════════════════════════════

/// Skill 激活标记（工具→中枢）：orchestrator 统一转为 session log 的
/// skill/activate 事件。与 ContextOp / CacheEventMark 同哲学：工具不改 log，
/// 只陈述操作事实。
///
/// 纯数据定义（无需序列化）：工具执行后随 ToolOutput 携带，由调度中枢在
/// `execute_tools_and_append` 中按调用顺序统一落账（sink.log_only）——
/// 激活状态本身由 ActiveContext 每 turn 重建（天然卸载），log 只记录
/// 何时激活了哪个 Skill（可回放）。
#[derive(Debug, Clone, PartialEq)]
pub enum SkillEventMark {
    /// 激活指定 Skill
    Activate { name: String },
}

// ═══════════════════════════════════════════════════════
// ToolOutput —— 结构化输出契约（canonical 值 + 渲染文本分离）
// ═══════════════════════════════════════════════════════

/// 工具执行的结构化输出：canonical 值 + presentation（渲染文本）分离。
///
/// 架构升级提案 · 模块 3（P0）：`QueryResult{rows, dropped_fields}`
/// 显式化为 canonical 值，渲染文本（LLM 与前端可见的 summary）与其解耦。
/// canonical 供后续消费者使用（缓存/记忆/统计/回放重建），
/// 文本型工具退化为 `{"text": summary}`。
///
/// `context_ops` 为上下文变更指令（Active/Reference 层标记协议）：工具不直接改
/// 状态，通过输出携带指令，由调度中枢统一应用。只读查询工具恒为空。
///
/// `cache_events` 为缓存操作标记（模块 C）：缓存工具陈述操作事实，
/// 由调度中枢统一转为 session log 的 cache/* 事件（与 ContextOp 同哲学）。
///
/// `skill_events` 为 Skill 激活标记（模块 B）：激活工具陈述操作事实，
/// 由调度中枢统一转为 session log 的 skill/activate 事件。
#[derive(Debug, Clone, Serialize)]
pub struct ToolOutput {
    /// 结构化 canonical 值（JSON）。执行产生的无损失数据，
    /// 不依赖渲染上下文即可重建。
    pub canonical: serde_json::Value,
    /// 渲染后的展示文本（presentation），LLM 与前端可见。
    pub summary: String,
    /// 上下文变更指令（按调用顺序由中枢统一应用，详见 [`ContextOp`]）。
    pub context_ops: Vec<ContextOp>,
    /// 缓存操作标记（按调用顺序由中枢统一落账为 cache/* 事件）。
    /// 纯内部通道，不序列化（skip：前端/日志不消费，日志形态为 cache/* 事件）。
    /// 2026-08：缓存工具取消注册后无生产者，保留供未来记忆系统复用。
    #[serde(skip)]
    #[allow(dead_code)]
    pub cache_events: Vec<CacheEventMark>,
    /// Skill 激活标记（按调用顺序由中枢统一落账为 skill/activate 事件）。
    /// 纯内部通道，不序列化（skip：日志形态为 skill/activate 事件）。
    #[serde(skip)]
    pub skill_events: Vec<SkillEventMark>,
}

impl ToolOutput {
    /// 纯文本输出：canonical 退化为 `{"text": summary}`。
    pub fn text(summary: impl Into<String>) -> Self {
        let summary = summary.into();
        Self {
            canonical: serde_json::json!({ "text": summary }),
            summary,
            context_ops: Vec::new(),
            cache_events: Vec::new(),
            skill_events: Vec::new(),
        }
    }

    /// 结构化输出：携带 canonical 值 + 渲染文本。
    pub fn json(canonical: serde_json::Value, summary: impl Into<String>) -> Self {
        Self {
            canonical,
            summary: summary.into(),
            context_ops: Vec::new(),
            cache_events: Vec::new(),
            skill_events: Vec::new(),
        }
    }
}

// ═══════════════════════════════════════════════════════
// Tool trait —— 每个工具必须实现的能力契约
// ═══════════════════════════════════════════════════════

/// 工具的能力契约。
///
/// 任何工具只需实现四个方法即可接入 Agent：
/// name + description + parameters 用于生成发给 LLM 的工具描述，
/// execute 是 LLM 决定调用后实际执行的逻辑。
///
/// Send + Sync 约束使得工具实例可以安全存放在 Registry 中
/// 跨异步任务访问（Registry 被 orchestrator 通过不可变引用访问）。
pub trait Tool: Send + Sync {
    /// 工具名，LLM function calling 时使用。
    /// 需与 ToolDef.function.name 保持一致。
    fn name(&self) -> &str;

    /// 工具描述，LLM 据此决定是否调用。
    /// 应描述"做什么"而非"怎么用"——参数格式由 parameters() 的 JSON Schema 表达。
    fn description(&self) -> &str;

    /// 参数的 JSON Schema 对象（不含最外层 type/function 包装）。
    ///
    /// 示例：
    /// ```json
    /// { "type": "object", "properties": { "name": { "type": "string" } }, "required": ["name"] }
    /// ```
    fn parameters(&self) -> serde_json::Value;

    /// 执行工具，返回结构化输出（canonical 值 + 渲染文本）。
    ///
    /// `params` 是 LLM 传入的 JSON 字符串（arguments 字段原样），
    /// 工具自行解析。`ctx` 提供外部依赖（数据库等）。
    fn execute(&self, ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError>;

    /// 工具执行超时（None = 不限制）。
    ///
    /// 契约（模块 3 P0）：执行类工具的默认超时，消费点在执行内核
    /// （exec/runner）内——run_command 覆写为 120s，调用级 timeout
    /// 参数覆盖此值；查询类工具保持 None（registry 不做全域包装）。
    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    /// 是否可并发执行（模块 3 P2）。
    ///
    /// 默认 true：PRTS 当前全部内置工具为只读查询（线程安全）；
    /// **写工具（cache_store、破坏性操作等）必须覆写为 false** 走串行——
    /// 未来破坏性工具落地（模块 10 沙箱）时以 guard 管线强制检查此项。
    fn is_concurrency_safe(&self) -> bool {
        true
    }

    /// finalizeContent 钩子：执行完成后对结果做最终加工，恰好调用一次。
    /// 默认原样返回；覆写可用于结果后处理（如脱敏、截断、格式归一）。
    fn finalize_content(&self, output: ToolOutput) -> ToolOutput {
        output
    }

    /// 声明本次调用的展示切片（文本信息 + 文件/URL 链接），供 tool/call 卡片
    /// 在**调用进行中**透出给用户。默认空切片（不附加任何信息）。
    ///
    /// 解析发生在 tool/call 事件发出之前（orchestrator 经 Registry::display_for
    /// 调用），因此只依赖参数与工具自持依赖（policy / 知识索引 / Skill 索引），
    /// 不得执行副作用或真实工具逻辑；解析失败降级为空切片，绝不阻断调用。
    fn display(&self, _ctx: &ToolContext, _params: &str) -> ToolDisplay {
        ToolDisplay::default()
    }

    /// 生成发送给 LLM 的 ToolDef。
    ///
    /// 一般不需要覆写。默认实现将 name / description / parameters
    /// 组装为 LLM API 所需的格式。
    fn to_def(&self) -> ToolDef {
        ToolDef::new(self.name(), self.description(), self.parameters())
    }
}

// ═══════════════════════════════════════════════════════
// ToolRegistry —— 工具的集中登记处
// ═══════════════════════════════════════════════════════

/// 工具注册表。应用启动时注册所有可用工具，
/// orchestrator 通过 Registry 查找和执行工具。
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// 注册一个工具。同名工具后注册的覆盖先注册的。
    ///
    /// 注册时校验 `parameters()` 的 JSON Schema 合法性（模块 3 P1：
    /// schema 校验——注册时校验 + 执行前校验参数），非法 schema 拒绝注册。
    pub fn register(&mut self, tool: Box<dyn Tool>) -> Result<(), String> {
        schema::validate_tool_schema(&tool.parameters())?;
        let name = tool.name().to_string();
        self.tools.insert(name, tool);
        Ok(())
    }

    /// 按名查找工具。
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    /// 解析指定工具本次调用的展示切片（tool/call 卡片信息）。
    ///
    /// 未注册工具 → 空切片（防御性：不因展示而报错）。
    pub fn display_for(&self, name: &str, params: &str, ctx: &ToolContext) -> ToolDisplay {
        self.get(name)
            .map(|t| t.display(ctx, params))
            .unwrap_or_default()
    }

    /// 生成所有工具的 ToolDef 列表，供 orchestrator 传给 LLM。
    ///
    /// **按工具名升序排序**（2026-10-03 缓存前缀修复）：`tools` 为 HashMap，
    /// 迭代序由 RandomState 决定、**随进程重启变化**——直接 `values()` 输出
    /// 会让请求前缀在工具区断裂（DeepSeek 前缀缓存整轮全未命中）。
    /// 排序后输出跨重启逐字节稳定，是前缀缓存的硬约束。
    pub fn list_defs(&self) -> Vec<ToolDef> {
        let mut defs: Vec<ToolDef> = self.tools.values().map(|t| t.to_def()).collect();
        defs.sort_by(|a, b| a.function.name.cmp(&b.function.name));
        defs
    }

    /// 执行指定工具，统一走执行管线：
    /// `pre → guard → execute → post → finalize → result`。
    ///
    /// 管线结构就位（模块 3 P0），当前 pre/guard/post 为空实现：
    /// 破坏性工具的批准策略（guard/approval）随模块 10 沙箱落地时在此注册。
    pub fn execute(
        &self,
        name: &str,
        ctx: &ToolContext,
        params: &str,
    ) -> Result<ToolOutput, ToolError> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::NotFound(name.to_string()))?;

        // ── pre-execute：执行前策略检查（当前空实现）──
        self.pre_execute(name, ctx, params)?;

        // ── guard：单调守卫（只能拒绝，不能撤销放行；当前恒放行）──
        self.guard(name, ctx, params)?;

        // ── execute：实际执行 ──
        let output = tool.execute(ctx, params)?;

        // ── post-execute：执行后处理（当前空实现）──
        self.post_execute(name, ctx, params, &output)?;

        // ── finalizeContent → result ──
        Ok(self.finalize_result(tool, output))
    }

    /// 执行管线 · pre-execute。当前空实现（结构就位）。
    fn pre_execute(&self, _name: &str, _ctx: &ToolContext, _params: &str) -> Result<(), ToolError> {
        Ok(())
    }

    /// 执行管线 · guard。单调守卫：一旦放行不可撤销；当前恒放行。
    ///
    /// 已落地守卫（模块 3 P1）：执行前 JSON Schema 参数校验
    /// （必填字段 + 顶层类型），违反转为 `ToolError::Parse`。
    /// 破坏性工具的批准策略（approval）随模块 10 沙箱落地时在此追加。
    fn guard(&self, name: &str, _ctx: &ToolContext, params: &str) -> Result<(), ToolError> {
        // 参数 schema 校验（LLM 传参错误在此拦截，统一错误信息）
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::NotFound(name.to_string()))?;
        let args: serde_json::Value =
            serde_json::from_str(params).map_err(|e| ToolError::Parse(e.to_string()))?;
        schema::validate_arguments(&tool.parameters(), &args).map_err(ToolError::Parse)
    }

    /// 执行管线 · post-execute。当前空实现（结构就位，可用于结果审计）。
    fn post_execute(
        &self,
        _name: &str,
        _ctx: &ToolContext,
        _params: &str,
        _output: &ToolOutput,
    ) -> Result<(), ToolError> {
        Ok(())
    }

    /// 执行管线 · finalizeContent → 最终结果。
    fn finalize_result(&self, tool: &dyn Tool, output: ToolOutput) -> ToolOutput {
        tool.finalize_content(output)
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════
// 内置工具注册（注册来源收口，模块 9 P1）
// ═══════════════════════════════════════════════════════

/// 注册全部内置工具（注册来源收口）。
///
/// lib.rs 装配时调用一次；每个工具注册时校验 JSON Schema（模块 3 P1），
/// 非法 schema 在此暴露（拒绝注册）。第三轮声明式扩展
/// （manifest + 目录扫描，仿 knowledge/）在此函数内接入，
/// 作为"中央控制 + 外接模块"地基的最后一棒。
///
/// `policy` 为沙箱策略（M1）：文件类工具的路径守卫与权限判定源
/// （workspace_root 内含——SimPath log 落点、read_file 相对路径基准）；
/// `knowledge` 为知识库索引（read_knowledge 的查询底表）；
/// `memory` 为记忆存储（memory_experience 只读 / memory_growth 读写通道）；
/// `ledger` 为提醒数据源（记忆变化 → 事件）。
pub fn register_builtin(
    reg: &mut ToolRegistry,
    policy: Arc<Policy>,
    knowledge: std::sync::Arc<crate::knowledge::index::KnowledgeIndex>,
    memory: Arc<Mutex<crate::memory::MemoryStore>>,
    ledger: Arc<Mutex<crate::notices::Notices>>,
) -> Result<(), String> {
    // echo 为测试辅助 dummy（不注册生产，2026-08-31；代码保留供测试）
    reg.register(Box::new(SearchEntities))?;
    reg.register(Box::new(SimPath::new(policy.workspace_root.clone())))?;
    reg.register(Box::new(QueryEnemy))?;
    reg.register(Box::new(QueryLevel))?;
    reg.register(Box::new(QueryOperator))?;
    reg.register(Box::new(QueryStage))?;
    reg.register(Box::new(QueryToken))?;
    reg.register(Box::new(QueryTrap))?;
    reg.register(Box::new(QueryTable::new(
        include_str!("../../resources/ranges.json"),
        include_str!("../../resources/tile_info.json"),
        include_str!("../../resources/term.json"),
        include_str!("../../resources/abnormal_effects.json"),
        include_str!("../../resources/slang.json"),
    )))?;
    reg.register(Box::new(QueryRogue))?;
    // 时间参照（Phase4 任务 1.3）：零注入方案下的唯一时间源，服务记忆系统时间戳
    reg.register(Box::new(TimeTool))?;
    // 文件域（M2 批 1 + 批 2 全部注册）：read_file / list_dir / write_file /
    // edit / grep，与 policy 守卫与 10MB 上限常量共用；知识库导航（模块 A）
    // 在文件工具之后注册
    reg.register(Box::new(ReadFileTool::new(Arc::clone(&policy))))?;
    reg.register(Box::new(ListDirTool::new(Arc::clone(&policy))))?;
    reg.register(Box::new(WriteFileTool::new(Arc::clone(&policy))))?;
    reg.register(Box::new(EditTool::new(Arc::clone(&policy))))?;
    reg.register(Box::new(GrepTool::new(Arc::clone(&policy))))?;
    // 命令执行（M3-A）：通用 shell 命令，执行内核 + 危险度分类 + 审批预检
    // （审批在 orchestrator 预检通道消费，execute 为纯执行路径）
    reg.register(Box::new(RunCommandTool::new(policy.workspace_root.clone())))?;
    // 网络域（M5）：web_search（免费匿名 anysearch_free）/ web_fetch（正文
    // 提取 + SSRF 逐跳防护）只读查询类工具。preflight 非 run_command 一律
    // ToolVerdict::Run 直放（零审批改动）；超时/体积/协议白名单自约束在
    // 工具内部；is_concurrency_safe 保持默认 true（并行组裸线程执行，
    // 内部 reqwest blocking 自建 runtime，无 async 内联 panic 风险）
    reg.register(Box::new(WebSearchTool::anysearch_default()))?;
    reg.register(Box::new(WebFetchTool::default()))?;
    reg.register(Box::new(ReadKnowledgeTool::new(knowledge.clone())))?;
    // 模块 A 追加优化：知识库导航（目录清单 + 节级阅读的目录侧能力）
    reg.register(Box::new(ListKnowledgeTool::new(knowledge)))?;
    // 模块 C 缓存工具已取消注册（2026-08：缓存系统停用，待升级为记忆系统；
    // 代码保留在 tools/cache.rs，未来复用时可恢复注册）
    // reg.register(Box::new(CacheStoreTool))?;
    // reg.register(Box::new(CacheListTool))?;
    // reg.register(Box::new(CacheRetrieveTool))?;
    // reg.register(Box::new(CacheRemoveTool))?;
    // 模块 B：Skill 激活工具（行为规则渐进披露的正文加载通道）。
    // 激活不改状态——经 ContextOp::ActiveLoad 标记由中枢统一应用
    // （同缓存工具哲学）；is_concurrency_safe=false（激活时序确定）
    reg.register(Box::new(ActivateSkillTool))?;
    // 记忆系统（Phase 4 任务二）：成长记忆读写（写成功经台账发记忆变化通知）
    // + 经历记忆只读（经历写入只走后台管线，memory_experience 无写参数面）。
    reg.register(Box::new(MemoryExperienceTool::new(Arc::clone(&memory))))?;
    reg.register(Box::new(MemoryGrowthTool::new(Arc::clone(&memory), ledger)))?;
    Ok(())
}

// ═══════════════════════════════════════════════════════
// EchoTool —— 测试辅助 dummy 工具（2026-08-31：不再注册生产，
// 保留供 registry / orchestrator 管线测试使用）
// ═══════════════════════════════════════════════════════

/// 测试辅助 Dummy 工具：收到什么参数就原样返回。
///
/// 用于验证 tool_call → execute → tool_result 整条路径，
/// 不依赖数据库或任何外部资源。仅测试构建编译（#[cfg(test)]）。
#[cfg(test)]
pub struct EchoTool;

#[cfg(test)]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "原样返回输入的参数，用于测试工具调用链路"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "任意文本，将被原样回显"
                }
            }
        })
    }

    fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text(format!("Echo: {}", params)))
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_failed_display_is_verbatim() {
        // 失败文案由工具自包含（含退出码/分节）；Display 不再叠加前缀
        let e = ToolError::CommandFailed("命令执行失败（退出码 7）：boom".into());
        assert_eq!(e.to_string(), "命令执行失败（退出码 7）：boom");
    }

    #[test]
    fn echo_tool_basic() {
        let tool = EchoTool;
        assert_eq!(tool.name(), "echo");

        let ctx = ToolContext::new();
        let result = tool.execute(&ctx, r#"{"message":"hello"}"#).unwrap();
        assert_eq!(result.summary, r#"Echo: {"message":"hello"}"#);
        // 纯文本输出 canonical 退化为 {"text": summary}
        assert_eq!(result.canonical["text"], r#"Echo: {"message":"hello"}"#);
    }

    #[test]
    fn echo_tool_empty_params() {
        let tool = EchoTool;
        let ctx = ToolContext::new();
        let result = tool.execute(&ctx, "{}").unwrap();
        assert_eq!(result.summary, "Echo: {}");
    }

    #[test]
    fn echo_tool_to_def() {
        let tool = EchoTool;
        let def = tool.to_def();
        assert_eq!(def.tool_type, "function");
        assert_eq!(def.function.name, "echo");
        assert!(def.function.description.contains("原样返回"));
    }

    #[test]
    fn registry_register_and_get() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(EchoTool)).unwrap();

        let tool = reg.get("echo");
        assert!(tool.is_some());
        assert_eq!(tool.unwrap().name(), "echo");
    }

    #[test]
    fn registry_not_found() {
        let reg = ToolRegistry::new();
        let ctx = ToolContext::new();
        let result = reg.execute("nonexistent", &ctx, "{}");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ToolError::NotFound(_)));
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn registry_execute() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(EchoTool)).unwrap();

        let ctx = ToolContext::new();
        let result = reg.execute("echo", &ctx, r#"{"x":1}"#).unwrap();
        assert_eq!(result.summary, r#"Echo: {"x":1}"#);
    }

    #[test]
    fn tool_output_json_carries_canonical_and_summary() {
        let out = ToolOutput::json(
            serde_json::json!({ "rows": [{ "名称": "源石虫" }], "dropped_fields": ["未知列"] }),
            "渲染文本",
        );
        assert_eq!(out.summary, "渲染文本");
        assert_eq!(out.canonical["rows"][0]["名称"], "源石虫");
        assert_eq!(out.canonical["dropped_fields"][0], "未知列");
    }

    // ── ContextOp 标记协议（Reference 层）──

    #[test]
    fn context_op_serializes_with_op_tag_and_kebab_case() {
        // 序列化形态：tag = "op"，变体名 kebab-case
        let load = serde_json::to_value(ContextOp::ReferenceLoad {
            key: "enemy.unit_1001".into(),
            content: "源石虫".into(),
        })
        .unwrap();
        assert_eq!(load["op"], "reference-load");
        assert_eq!(load["key"], "enemy.unit_1001");
        assert_eq!(load["content"], "源石虫");

        let remove = serde_json::to_value(ContextOp::ReferenceRemove {
            key: "k".into(),
        })
        .unwrap();
        assert_eq!(remove["op"], "reference-remove");
        assert_eq!(remove["key"], "k");

        let clear = serde_json::to_value(ContextOp::ReferenceClear).unwrap();
        assert_eq!(clear["op"], "reference-clear");
        assert!(clear.get("key").is_none(), "无字段变体不应携带多余字段");

        // Active 层（Skill 激活）：与 Reference 层变体共用同一标记协议
        let active_load = serde_json::to_value(ContextOp::ActiveLoad {
            name: "推演分析".into(),
            content: "## 正文".into(),
        })
        .unwrap();
        assert_eq!(active_load["op"], "active-load");
        assert_eq!(active_load["name"], "推演分析");
        assert_eq!(active_load["content"], "## 正文");
    }

    #[test]
    fn tool_output_constructors_default_empty_context_ops() {
        // 已注册工具零改动的前提：两个构造函数默认不携带变更指令
        let text = ToolOutput::text("纯文本");
        assert!(text.context_ops.is_empty());
        assert!(text.cache_events.is_empty());
        assert!(text.skill_events.is_empty());

        let json = ToolOutput::json(serde_json::json!({ "rows": [] }), "结构化");
        assert!(json.context_ops.is_empty());
        assert!(json.cache_events.is_empty());
        assert!(json.skill_events.is_empty());
    }

    #[test]
    fn registry_list_defs() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(EchoTool)).unwrap();

        let defs = reg.list_defs();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].function.name, "echo");
    }

    /// 缓存前缀约束（2026-10-03）：list_defs 按工具名升序输出且跨调用稳定——
    /// tools 为 HashMap（迭代序随进程变化），未排序会破坏请求前缀缓存命中。
    #[test]
    fn registry_list_defs_sorted_and_stable() {
        struct StubTool(&'static str);
        impl Tool for StubTool {
            fn name(&self) -> &str {
                self.0
            }
            fn description(&self) -> &str {
                "排序测试桩"
            }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({ "type": "object", "properties": {} })
            }
            fn execute(&self, _ctx: &ToolContext, _params: &str) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::text("stub"))
            }
        }

        let mut reg = ToolRegistry::new();
        // 注册序与名字序不同（若按注册序 / 迭代序输出，本用例可识别）
        reg.register(Box::new(StubTool("zeta"))).unwrap();
        reg.register(Box::new(StubTool("alpha"))).unwrap();
        reg.register(Box::new(StubTool("mike"))).unwrap();

        let names: Vec<String> = reg
            .list_defs()
            .iter()
            .map(|d| d.function.name.clone())
            .collect();
        assert_eq!(names, vec!["alpha", "mike", "zeta"], "list_defs 应按工具名升序");
        let names_again: Vec<String> = reg
            .list_defs()
            .iter()
            .map(|d| d.function.name.clone())
            .collect();
        assert_eq!(names, names_again, "两次调用输出顺序一致");
    }

    /// 展示探针工具：声明展示切片，验证 Registry::display_for 分发。
    struct DisplayProbeTool;

    impl Tool for DisplayProbeTool {
        fn name(&self) -> &str {
            "display_probe"
        }
        fn description(&self) -> &str {
            "展示探针"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({ "type": "object" })
        }
        fn execute(&self, _ctx: &ToolContext, _params: &str) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("ok"))
        }
        fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
            if params.contains("probe") {
                ToolDisplay::info("探针对象")
            } else {
                ToolDisplay::default()
            }
        }
    }

    #[test]
    fn registry_display_for_dispatches_and_defaults_empty() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(DisplayProbeTool)).unwrap();
        let ctx = ToolContext::new();
        assert_eq!(
            reg.display_for("display_probe", r#"{"probe":1}"#, &ctx).text,
            "探针对象"
        );
        // 工具自身判定为空 → 空切片
        assert!(reg.display_for("display_probe", "{}", &ctx).is_empty());
        // 未注册工具 → 空切片（防御性，不 panic）
        assert!(reg.display_for("not_registered", "{}", &ctx).is_empty());
    }

    #[test]
    fn tool_error_display() {
        let e = ToolError::NotFound("test_tool".into());
        assert!(e.to_string().contains("test_tool"));

        let e = ToolError::Execution("db error".into());
        assert!(e.to_string().contains("db error"));
    }

    // ── 注册收口 + schema 校验（模块 3 P1 / 模块 9 P1）──

    #[test]
    fn register_builtin_registers_all_tools_with_valid_schemas() {
        use crate::knowledge::index::KnowledgeIndex;

        let mut reg = ToolRegistry::new();
        // 临时 workspace_root 与临时知识库索引
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let workspace_root =
            std::env::temp_dir().join(format!("prts-reg-ws-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&workspace_root).unwrap();
        let kb_root =
            std::env::temp_dir().join(format!("prts-reg-kb-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&kb_root).unwrap();
        std::fs::write(
            kb_root.join("test.md"),
            "---\ntitle: 测试条目\nbrief: b\n---\n内容",
        )
        .unwrap();
        let knowledge = Arc::new(KnowledgeIndex::load(&kb_root).unwrap());
        // 临时记忆存储（memory_experience / memory_growth 注入）
        let mem_root =
            std::env::temp_dir().join(format!("prts-reg-mem-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&mem_root).unwrap();
        let memory = Arc::new(Mutex::new(crate::memory::MemoryStore::load(&mem_root).unwrap()));
        let ledger = Arc::new(Mutex::new(crate::notices::Notices::load(&mem_root)));

        register_builtin(
            &mut reg,
            Arc::new(Policy::new(workspace_root.clone())),
            knowledge,
            memory,
            ledger,
        )
        .expect("所有内置工具 schema 应合法");
        // 文件域 5 工具（M2 批 1 write_file/edit + 批 2 list_dir 恢复/grep）
        // + 既有 11 查询/知识工具 + 1 Skill 激活 + 1 SimPath + 1 run_command（M3-A）
        // + 网络域 2 工具（M5：web_search/web_fetch）+ 时间 1 工具（Phase4 1.3）
        // + 记忆 2 工具（Phase4 任务二 · 批 1：memory_experience/memory_growth）= 24；
        // echo 为测试辅助不注册（2026-08-31）；4 个缓存工具已取消注册（2026-08）。
        assert_eq!(reg.list_defs().len(), 24);

        // 记忆工具确已注册且名字稳定
        assert!(reg.get("memory_experience").is_some());
        assert!(reg.get("memory_growth").is_some());

        let _ = std::fs::remove_dir_all(&workspace_root);
        let _ = std::fs::remove_dir_all(&kb_root);
        let _ = std::fs::remove_dir_all(&mem_root);
    }

    #[test]
    fn register_rejects_invalid_schema() {
        struct BadSchemaTool;
        impl Tool for BadSchemaTool {
            fn name(&self) -> &str { "bad_schema" }
            fn description(&self) -> &str { "schema 非法的工具" }
            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({ "type": "string" }) // 顶层必须是 object
            }
            fn execute(&self, _ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::text(params))
            }
        }
        let mut reg = ToolRegistry::new();
        let err = reg.register(Box::new(BadSchemaTool)).unwrap_err();
        assert!(err.contains("type"), "got: {err}");
        assert!(reg.get("bad_schema").is_none());
    }

    #[test]
    fn guard_rejects_missing_required_arg_before_execution() {
        // 参数校验在管线 guard 阶段拦截（工具内部解析之前，统一错误信息）
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(QueryLevel)).unwrap();
        let ctx = ToolContext::new(); // 无 db——若 guard 不拦截会走到 Execution("数据库未连接")
        let err = reg.execute("query_level", &ctx, r#"{"levelId":"x"}"#).unwrap_err();
        assert!(
            matches!(&err, ToolError::Parse(msg) if msg.contains("缺少必填参数: fields")),
            "got: {err}"
        );
    }

    #[test]
    fn guard_accepts_valid_args_and_rejects_wrong_type() {
        let mut reg = ToolRegistry::new();
        reg.register(Box::new(QueryEnemy)).unwrap();
        let ctx = ToolContext::new();
        // names 类型错误（应为数组）→ guard 拦截
        let err = reg
            .execute("query_enemy", &ctx, r#"{"names": "源石虫"}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Parse(msg) if msg.contains("类型错误")),
            "got: {err}"
        );
        // 合法参数通过 guard：错误来自工具内部解析（无查询目标），
        // 而非 guard 的 schema 校验——证明 guard 已放行
        let err = reg
            .execute("query_enemy", &ctx, r#"{"names": []}"#)
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Parse(msg) if !msg.contains("缺少必填参数") && !msg.contains("类型错误")),
            "guard 应放行合法参数，got: {err}"
        );
    }
}
