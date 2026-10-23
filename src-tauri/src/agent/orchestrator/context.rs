//! 上下文组装 —— 构建发给 LLM 的消息列表（模块 2：注册制组装，P0）
//!
//! 职责：
//!   1. 以 **PromptSection 注册制** 组装系统提示词（替代硬编码 `format!`）
//!   2. 将系统提示词 + 对话历史组装为 `Vec<ChatMessage>`
//!   3. 估算总 token 数（debug 日志用，不触发压缩；双锚兜底接入见模块 8）
//!
//! ## 组装机制（架构升级 · 模块 2 · 2026-08 上下文管理修正）
//!
//! system prompt 只保留静态两层（Base 人格 + Catalog Skill 目录）：
//! system 是请求前缀的最开头，任何动态注入（skill 正文、缓存条目）都会逐字节
//! 改变前缀，破坏 DeepSeek 前缀缓存的命中。动态内容一律以消息形态进历史
//! （激活 skill = 工具结果消息，正文保留在对话中持续生效，不设卸载）。
//!
//! - **静态前缀**：`PromptSection` 注册制，`order` 区间映射，Base 0 / Catalog 100
//! - **历史**：由 session log 派生（`session.rs::derive_messages`），不进 system prompt
//!
//! 已注释（未来记忆系统可能复用）：Active/Reference 动态注入层
//! （ActiveContext / PromptContext / ContextOp 协议、缓存工具通道）。
//!
//! ## request/header 快照
//!
//! `assemble_system_prompt()` 是快照的文本来源——session log 的 request/header
//! 事件记录"模型到底看到了什么"（渲染后 system prompt + 工具 schema），
//! 仅变化时追加（`SessionLog::push_header` 去重）。
//!
//! token 估算基于 DeepSeek 官方文档的换算比例：
//!   英文字符 ≈ 0.3 token  |  中文字符 ≈ 0.6 token
//! 来源：https://api-docs.deepseek.com/quick_start/token_usage
//! DeepSeek 使用 LlamaTokenizerFast（BPE），不同模型的换算比例不同，
//! 这里的估算仅作近似参考，精确 token 数以 API 返回的 usage 为准。

// use std::collections::HashMap; // 已注释（2026-08：ActiveContext 移除）
#[cfg(test)]
use super::prompt::{SYSTEM_PROMPT, UI_PROMPT};
use super::prompt_registry::builtin_registry;
use crate::llm::ChatMessage;

pub use super::prompt_registry::{
    PromptRegistry, PromptSection, ORDER_BASE, ORDER_CATALOG, ORDER_MEMORY, ORDER_UI,
};
pub use super::token_estimate::{estimate_messages, estimate_tokens};

// ═══════════════════════════════════════════════════════
// ActiveContext —— 已注释（2026-08 上下文管理修正：动态层移除，
// 无生产者后整段注释；未来记忆系统复用时可恢复）。
// ═══════════════════════════════════════════════════════
/*

/// Active/Reference 两层状态容器：五层上下文模型中 Skill 激活与缓存条目的
/// **在场层**。
///
/// - **Active 层**：Skill 激活状态（name → 正文）。生命周期与 Reference 层
///   相同——每 turn 随本容器重建（轮内激活、轮末自动卸载，不设停用工具）
/// - **Reference 层**：缓存条目载入状态（key → content）
///
/// 设计决策 A.4：工具不直接修改本容器——执行后通过 `ToolOutput.context_ops`
/// 返回上下文变更指令，由调度中枢统一调用 [`Self::apply_ops`] 应用。
/// 每个 step 起点重组 system prompt 时消费本容器内容（`assemble_system_prompt`）。
///
/// 缓存语义（接口兼容模块 C，本模块不实现）：静态缓存始终载入、
/// 动态缓存载入状态每轮卸载而桶内容跨轮保留——本容器仅承载
/// "载入状态"（在场性），持久化桶由模块 C 的 cache 工具负责。
#[derive(Debug, Default)]
pub struct ActiveContext {
    /// key → content 的缓存条目映射。HashMap 迭代顺序不确定，
    /// 输出必须经 `reference_entries` 排序，保证组装结果逐字节可复现
    /// （request/header 去重以文本比较为准，乱序会误判为变化）。
    reference: HashMap<String, String>,
    /// name → body 的 Skill 激活映射（Active 层）。排序理由同 reference。
    skills: HashMap<String, String>,
}

impl ActiveContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// 载入/更新 Reference 条目：同名覆盖（缓存条目以最新内容为准）。
    pub fn reference_load(&mut self, key: impl Into<String>, content: impl Into<String>) {
        self.reference.insert(key.into(), content.into());
    }

    /// 移除单条。返回是否存在该条目。
    pub fn reference_remove(&mut self, key: &str) -> bool {
        self.reference.remove(key).is_some()
    }

    /// 清空 Reference 层载入状态（桶内容跨轮保留由模块 C 缓存工具负责）。
    pub fn reference_clear(&mut self) {
        self.reference.clear();
    }

    /// 当前在场条目数。
    #[allow(dead_code)] // 模块 C 缓存工具 / 压缩决策消费（当前仅测试使用）
    pub fn reference_len(&self) -> usize {
        self.reference.len()
    }

    /// 按 key 排序输出全部条目（key, content）。
    ///
    /// 排序是组装可复现性的前提：同一份 `ActiveContext` 每次组装
    /// 输出逐字节一致，request/header 去重机制（文本比较）不误判。
    pub fn reference_entries(&self) -> impl Iterator<Item = (&str, &str)> {
        let mut keys: Vec<&String> = self.reference.keys().collect();
        keys.sort();
        keys.into_iter()
            .map(|k| (k.as_str(), self.reference[k].as_str()))
    }

    /// 载入/更新 Skill 激活条目：同名覆盖（最新正文为准）。
    ///
    /// Active 层生命周期与 Reference 层相同（每 turn 随容器重建），
    /// 覆盖语义对齐 reference_load——同名 Skill 重复激活以最新正文生效。
    pub fn skill_load(&mut self, name: impl Into<String>, body: impl Into<String>) {
        self.skills.insert(name.into(), body.into());
    }

    /// 按 name 排序输出全部已激活 Skill（name, body）。
    ///
    /// 排序是组装可复现性的前提：与 `reference_entries` 同理——
    /// 同一份 `ActiveContext` 每次组装输出逐字节一致，
    /// request/header 去重机制（文本比较）不误判。
    pub fn skill_entries(&self) -> impl Iterator<Item = (&str, &str)> {
        let mut names: Vec<&String> = self.skills.keys().collect();
        names.sort();
        names.into_iter()
            .map(|n| (n.as_str(), self.skills[n].as_str()))
    }

    /// 应用工具返回的上下文变更指令（四种指令分别映射到
    /// reference load/remove/clear 与 active load）。
    ///
    /// 标记按调用顺序应用——与消息/事件顺序保持一致，保证确定性。
    pub fn apply_ops(&mut self, ops: &[crate::tools::ContextOp]) {
        for op in ops {
            match op {
                crate::tools::ContextOp::ReferenceLoad { key, content } => {
                    self.reference_load(key, content);
                }
                crate::tools::ContextOp::ReferenceRemove { key } => {
                    self.reference_remove(key);
                }
                crate::tools::ContextOp::ReferenceClear => {
                    self.reference_clear();
                }
                crate::tools::ContextOp::ActiveLoad { name, content } => {
                    self.skill_load(name, content);
                }
            }
        }
    }
}
*/

// ═══════════════════════════════════════════════════════
// 公开接口
// ═══════════════════════════════════════════════════════

/// 装配 system prompt：Base（人格 prts.md + 界面 ui.md）→ Catalog（Skill 目录）。
///
/// `skill_catalog` 为**对话冻结的目录文本**（`SkillIndex::catalog_text` 的
/// 快照原文，由命令层经 `SkillSnapshotStore` 取；空串显示（无））——目录随
/// 对话冻结、逐字不变，是前缀缓存的硬约束。
///
/// 知识库使用方式不在此注入：路径属工具实现细节（KnowledgeIndex 内部消化），
/// 结构认知由 list_knowledge 框架模式承担，条目阅读由 read_knowledge 承担，
/// 工具描述是唯一通道。Skill 目录注册 Catalog 层 section
/// （`prts.skill-catalog`，渐进披露——只给 name + description）。
///
/// 静态两层，逐字节可复现：system 是请求前缀的最开头，任何动态注入（skill
/// 正文、缓存条目）都会逐字节改变前缀、破坏 DeepSeek 前缀缓存命中；
/// 动态内容一律以消息形态进历史（激活 skill = 工具结果消息）。
#[allow(dead_code)] // 仅测试与文档引用：生产走 assemble_system_prompt_with_memory
pub fn assemble_system_prompt(skill_catalog: &str) -> String {
    assemble_system_prompt_with_memory(skill_catalog, "")
}

/// 装配 system prompt：Base（人格）→ Catalog（Skill 目录）→ **记忆区（尾部）**。
///
/// `skill_catalog` 为对话冻结目录文本（见 [`assemble_system_prompt`]）；
/// `memory_block` 为对话冻结快照原文（由命令层经 SnapshotStore 取）；
/// **空块时不注册记忆 section**——无记忆态输出与旧实现逐字节一致。
/// 记忆区不做二次改写（四节内容均为存量原文，装配不产生新内容）。
pub fn assemble_system_prompt_with_memory(skill_catalog: &str, memory_block: &str) -> String {
    let mut reg = builtin_registry().clone();
    // Catalog 层：Skill 目录（渐进披露——只给 name + description，正文按需
    // activate_skill 加载）。空目录显示（无）。
    reg.register_section(PromptSection::new(
        "prts.skill-catalog",
        ORDER_CATALOG,
        if skill_catalog.is_empty() {
            "## Skill 目录\n\n（无）".to_string()
        } else {
            format!("## Skill 目录\n\n{skill_catalog}")
        },
    ))
    .expect("builtin section 注册冲突");
    // 记忆区（Phase 4 任务二 · 批 1）：冻结快照，置 system 尾部。
    // 空块不注册（无记忆态逐字节兼容）。
    if !memory_block.trim().is_empty() {
        reg.register_section(PromptSection::new(
            "prts.memory",
            ORDER_MEMORY,
            memory_block,
        ))
        .expect("builtin section 注册冲突");
    }
    reg.assemble()
}

/// 组装 LLM 调用所需的完整消息列表。
///
/// 系统提示词由注册制装配（prts.md 角色人格 + 冻结 Skill 目录 Catalog + 记忆区）。
///
/// 参数：
///   `history` — 已转换为 llm::ChatMessage 格式的对话历史（不含 system，
///               由 session log 的 derive_messages 派生）
///   `tool_count` — 工具数量（暂不使用，为后续 token 估算预留）
///   `skill_catalog` — 对话冻结目录文本（Catalog 内容来源；空串显示（无））
///
/// 返回：
///   `Vec<ChatMessage>` — system 消息 + 历史消息
#[allow(dead_code)] // 测试用无记忆薄包装；生产走 build_context_with_memory
pub fn build_context(
    history: &[ChatMessage],
    tool_count: usize,
    skill_catalog: &str,
) -> Vec<ChatMessage> {
    build_context_with_memory(history, tool_count, skill_catalog, "")
}

/// 同 [`build_context`]，附加冻结记忆区（空块 = 无记忆态）。
pub fn build_context_with_memory(
    history: &[ChatMessage],
    _tool_count: usize,
    skill_catalog: &str,
    memory_block: &str,
) -> Vec<ChatMessage> {
    // 系统提示词：人格 + 冻结 Skill 目录 + 记忆区（尾部）
    build_context_with_system(
        history,
        assemble_system_prompt_with_memory(skill_catalog, memory_block),
    )
}

/// 以**已渲染的 system 全文**组装上下文（不重新装配）。
///
/// 生产路径用这条：system 来自对话冻结的完整前缀快照（`prefix_snapshot.rs`）——
/// 人格/界面段（编译期常量）与工具定义也会随应用升级改变，故必须读存下来的
/// 那份，不能现拼（前缀缓存硬约束）。装配口径的唯一出口仍是
/// [`assemble_system_prompt_with_memory`]（快照首次冻结时用它渲染）。
pub fn build_context_with_system(
    history: &[ChatMessage],
    system_prompt: String,
) -> Vec<ChatMessage> {
    let mut messages = Vec::with_capacity(1 + history.len());
    messages.push(ChatMessage::system(system_prompt));
    // 对话历史（含本轮用户消息）
    for msg in history {
        messages.push(msg.clone());
    }
    messages
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── estimate_tokens ──

    #[test]
    fn empty_string_returns_one() {
        assert_eq!(estimate_tokens(""), 1);
    }

    #[test]
    fn whitespace_only_returns_one() {
        assert_eq!(estimate_tokens("   \n\t  "), 1);
    }

    #[test]
    fn pure_ascii() {
        // "Hello World" = 10 chars (不含空格) → 10 * 0.3 = 3.0
        let t = estimate_tokens("Hello World");
        assert_eq!(t, 3);
    }

    #[test]
    fn pure_chinese() {
        // "你好世界" = 4 chars → 4 * 0.6 = 2.4 → ceil = 3
        let t = estimate_tokens("你好世界");
        assert_eq!(t, 3);
    }

    #[test]
    fn mixed_chinese_ascii() {
        // "源石虫DEF为0" = 4 CJK + 4 ASCII = 4*0.6 + 4*0.3 = 2.4 + 1.2 = 3.6 → ceil = 4
        let t = estimate_tokens("源石虫DEF为0");
        assert_eq!(t, 4);
    }

    #[test]
    fn long_text_approximation() {
        // 100 个中文字符 ≈ 60 token
        let text = "测".repeat(100);
        assert_eq!(estimate_tokens(&text), 60);

        // 100 个英文字母 ≈ 30 token
        let text = "a".repeat(100);
        assert_eq!(estimate_tokens(&text), 30);
    }

    #[test]
    fn whitespace_not_counted() {
        // "a b c" = 3 ASCII chars → 3 * 0.3 = 0.9 → ceil = 1
        let t = estimate_tokens("a b c");
        assert_eq!(t, 1);
    }

    // ── 注册制组装 ──

    #[test]
    fn base_prompt_orders_persona_before_ui_before_catalog() {
        // 承诺：模型看到的先后 = 人格 → 界面认知 → Skill 目录。
        // （不守"注册表里恰好几段"这类形态快照——加节 / 重排 order 时它只会白响。）
        let prompt = assemble_system_prompt("");
        let persona = prompt.find(SYSTEM_PROMPT).expect("人格段应在");
        let ui = prompt.find(UI_PROMPT).expect("界面段应在");
        let catalog = prompt.find("## Skill 目录").expect("目录段应在");
        assert!(persona < ui && ui < catalog, "顺序应为 人格 → 界面 → 目录");
    }

    #[test]
    fn memory_block_appended_at_tail_and_empty_is_legacy_identical() {
        let legacy = assemble_system_prompt("");
        // 空块 / 空白块 → 与旧实现逐字节一致（无记忆态兼容）
        assert_eq!(assemble_system_prompt_with_memory("", ""), legacy);
        assert_eq!(assemble_system_prompt_with_memory("", "   "), legacy);
        // 非空块 → 追加在 system 尾部（前缀不变）
        let block = "## 记忆\n\n### 关于博士\n博士偏好简洁";
        let with_mem = assemble_system_prompt_with_memory("", block);
        assert!(with_mem.starts_with(&legacy), "记忆区应在尾部（前缀不变）");
        assert!(with_mem.ends_with(block));
        // build_context 同样在 system 头携带记忆区
        let msgs = build_context_with_memory(&[], 0, "", block);
        assert!(msgs[0].content.ends_with(block));
    }

    #[test]
    fn assemble_sorts_by_order() {
        let mut reg = PromptRegistry::default();
        // 乱序注册 → 按 order 升序装配
        reg.register_section(PromptSection::new("c", 200, "C段"))
            .unwrap();
        reg.register_section(PromptSection::new("a", 0, "A段"))
            .unwrap();
        reg.register_section(PromptSection::new("b", 100, "B段"))
            .unwrap();
        assert_eq!(reg.assemble(), "A段\n\nB段\n\nC段");
    }

    #[test]
    fn duplicate_section_name_rejected() {
        let mut reg = PromptRegistry::default();
        reg.register_section(PromptSection::new("a", 0, "A段"))
            .unwrap();
        let err = reg
            .register_section(PromptSection::new("a", 10, "重复"))
            .unwrap_err();
        assert!(err.contains("重复注册"));
        assert_eq!(reg.sections.len(), 1);
    }

    // ── 动态层注入测试已注释（2026-08 上下文管理修正：Active/Reference
    //    动态层移除，对应测试随之停用；未来记忆系统复用时可恢复）──

    #[test]
    fn empty_registry_assembles_empty() {
        assert_eq!(PromptRegistry::default().assemble(), "");
    }

    // ── build_context ──

    #[test]
    fn builds_with_system_prompt() {
        let messages = build_context(&[], 0, "");
        assert!(!messages.is_empty());
        assert_eq!(messages[0].role, crate::llm::MessageRole::System);
        // system 消息应包含角色人格
        assert!(messages[0].content.contains("PRTS"));
    }

    #[test]
    fn assemble_matches_build_context_system() {
        // 等价性回归（关键）：assemble_system_prompt 的输出与 build_context
        // 的 system 头一致，且内容以 SYSTEM_PROMPT 开头。
        // 知识库路径不再注入（路径属工具实现细节，list_knowledge 框架模式
        // 承担结构认知）——断言 prompt 不含任何文件系统痕迹。
        let direct = assemble_system_prompt("");
        let via_build = &build_context(&[], 0, "")[0].content;
        assert_eq!(direct, *via_build);
        assert!(direct.starts_with(SYSTEM_PROMPT));
        // 知识库路径/索引文件/死指令不再注入（工具名含 knowledge 子串，
        // 故以路径形态与文件名为锚断言）
        assert!(!direct.contains("content/knowledge"));
        assert!(!direct.contains("_index.md"));
        assert!(!direct.contains("list_dir"));
    }

    #[test]
    fn includes_history() {
        let history = vec![
            ChatMessage::user("你好"),
            ChatMessage::assistant("你好，博士"),
        ];
        let messages = build_context(&history, 0, "");
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1].role, crate::llm::MessageRole::User);
        assert_eq!(messages[2].role, crate::llm::MessageRole::Assistant);
    }

    #[test]
    fn tool_count_reserved() {
        // tool_count 参数当前不使用，但保留接口以保证向后兼容
        let msgs_0 = build_context(&[], 0, "");
        let msgs_5 = build_context(&[], 5, "");
        assert_eq!(msgs_0.len(), msgs_5.len());
    }

    // ── assemble_system_prompt ──

    #[test]
    fn assemble_prompt_never_injects_knowledge_path() {
        // 路径属工具实现细节（KnowledgeIndex 内部消化），不在 system prompt
        // 出现；结构认知由 list_knowledge 框架模式承担，工具描述是唯一通道。
        let prompt = assemble_system_prompt("");
        assert!(!prompt.contains("content/knowledge"));
        assert!(!prompt.contains("_index.md"));
        assert!(!prompt.contains("list_dir"));
        // Base 层 persona 段仍在前
        assert!(prompt.starts_with(SYSTEM_PROMPT));
    }

    // ── Reference 注入测试已注释（2026-08：动态层移除）──

    // ── Skill 目录（Catalog 层）注入 ──

    /// 假 skills 目录：两个有效条目（文件名字母序与 name 序相反，
    /// 验证目录文本按 name 排序而非按文件名）。
    fn skill_fixture(root: &std::path::Path) {
        std::fs::write(
            root.join("alpha.md"),
            "---\nname: 贝塔\ndescription: 条目 B\n---\n\n## 正文 B",
        )
        .unwrap();
        std::fs::write(
            root.join("gamma.md"),
            "---\nname: 阿尔法\ndescription: 条目 A\n---\n\n## 正文 A",
        )
        .unwrap();
    }

    /// 创建唯一临时目录（不引入 tempfile crate，模式照 skills_index.rs）。
    fn skill_temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("prts-context-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn assemble_injects_catalog_after_persona() {
        let root = skill_temp_dir("catalog");
        skill_fixture(&root);
        let idx = crate::skills_index::SkillIndex::load(&root).unwrap();
        // 目录文本即系统侧来源（冻结快照与活体目录在此同构；批 2）
        let prompt = assemble_system_prompt(&idx.catalog_text());

        // 层序：Base 人格 → Skill 目录（Catalog）→（无 Active/Reference）
        assert!(
            prompt.contains("## Skill 目录\n\n- 贝塔［用户］：条目 B\n- 阿尔法［用户］：条目 A")
        );
        let persona_pos = prompt.find(SYSTEM_PROMPT).unwrap();
        let catalog_pos = prompt.find("## Skill 目录").unwrap();
        assert!(catalog_pos > persona_pos, "Catalog 应在 Base 人格之后");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn assemble_empty_catalog_shows_no_skills() {
        // 空目录文本（空索引 / 未冻结）：目录节仍在，显示（无）
        let prompt = assemble_system_prompt("");
        assert!(prompt.contains("## Skill 目录\n\n（无）"));
    }

    // ── Active/Reference 动态层测试已注释（2026-08：动态层移除）──

    // ── estimate_messages（模块 8 双锚的 estimate 兜底锚）──

    #[test]
    fn estimate_messages_counts_text_and_overhead() {
        let messages = vec![
            ChatMessage::system("system text"),
            ChatMessage::user("你好"),
        ];
        let est = estimate_messages(&messages);
        // system 4 ascii*0.3=1.2→2 + 你好 2 cjk*0.6=1.2→2 + 2 条消息模板开销 8 = 12
        assert_eq!(
            est,
            estimate_tokens("system text") + estimate_tokens("你好") + 8
        );
        assert!(est >= 10);
    }

    #[test]
    fn estimate_messages_includes_reasoning_and_tool_args() {
        let mut msg = ChatMessage::user("查数据");
        msg.reasoning_content = Some("思考".into());
        msg.tool_calls = Some(vec![crate::llm::ToolCall {
            id: "c1".into(),
            call_type: "function".into(),
            function: crate::llm::ToolCallFunction {
                name: "query_enemy".into(),
                arguments: r#"{"names":["源石虫"]}"#.into(),
            },
        }]);
        let messages = vec![msg];
        let est = estimate_messages(&messages);
        assert!(
            est > estimate_tokens("查数据") + 4,
            "应计入推理链与工具参数: {est}"
        );
    }

    #[test]
    fn estimate_messages_empty() {
        assert_eq!(estimate_messages(&[]), 0);
    }
}
