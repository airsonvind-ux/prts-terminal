// PRTS 终端 - Tauri 后端入口
// 启动时装配 State 与命令，将各模块接入 Tauri 运行时
//
// 新架构（2026-07）：中枢调度 + 外围组件
// 旧代码已归档至 src_old/，仅作实现参考

// 模块声明
mod approval;
mod attachments;
mod cache;
mod config;
mod db;
mod db_seed;
mod machine;
mod notices;
mod events;
mod exec;
mod exec_settings;
mod knowledge;
pub mod maa;
mod memory;
mod observability;
mod orchestrator;
mod path_guard;
mod policy;
mod session;
mod session_store;
mod settings;
mod steer_queue;
mod skills;
mod skills_index;
mod skills_install;
mod skills_snapshot;
mod tools;
mod llm;
mod commands;
mod turn_registry;

use std::sync::{Arc, Mutex};

use tauri::Manager;

use crate::approval::ApprovalRegistry;
use crate::db::Database;
use crate::notices::Notices;
use crate::knowledge::index::{knowledge_dir, KnowledgeIndex};
use crate::memory::{MemoryStore, SnapshotStore};
use crate::llm::{ClientRegistry, ModelRegistry};
use crate::policy::Policy;
use crate::settings::SettingsStore;
use crate::skills_snapshot::SkillSnapshotStore;
use crate::tools::ToolRegistry;
use crate::steer_queue::SteerQueue;
use crate::turn_registry::TurnRegistry;
use crate::llm::anthropic::AnthropicMessagesClient;
use crate::llm::openai_compatible::OpenAiCompatibleClient;
use crate::llm::openai_responses::OpenAIResponsesClient;

// ═══════════════════════════════════════════════════════
// 启动前自检与启动失败通道（仅发布构建）
// ═══════════════════════════════════════════════════════

/// 启动诊断日志路径（数据根 `logs/` 下）。
#[cfg(not(debug_assertions))]
fn boot_log_path() -> std::path::PathBuf {
    config::data_dir().join("logs").join("boot.log")
}

/// 追加一条启动诊断（best-effort：写不上不影响弹窗与退出）。
#[cfg(not(debug_assertions))]
fn append_boot_log(text: &str) {
    use std::io::Write;
    let path = boot_log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = write!(f, "[{stamp}] {text}\n");
    }
}

/// 原生错误弹窗（Windows）；非 Windows 只落日志。
#[cfg(not(debug_assertions))]
fn show_native_error(title: &str, body: &str) {
    #[cfg(windows)]
    {
        use windows::core::HSTRING;
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
        let text = HSTRING::from(body);
        let caption = HSTRING::from(title);
        // SAFETY: 纯消息框调用，两个 HSTRING 均在本次调用期间有效。
        unsafe {
            let _ = MessageBoxW(None, &text, &caption, MB_ICONERROR | MB_OK);
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("[boot] {title}\n{body}");
    }
}

/// 启动失败：落一条诊断、弹原生弹窗、退出。
#[cfg(not(debug_assertions))]
fn fatal_startup(title: &str, body: &str) -> ! {
    append_boot_log(&format!("{title}\n{body}"));
    show_native_error(title, body);
    std::process::exit(1);
}

/// 启动前自检（仅发布构建）：安装面关键文件是否齐全可读。
///
/// 判据与资源解析同源：不是"哪条路径存在"，而是"按当前构建类型该在的那份
/// 在不在"。缺任何一样即弹原生弹窗（列出缺失项 + 诊断文件位置）并退出——
/// 此时 Tauri 尚未起来，界面还不存在，只能由原生弹窗承担。
#[cfg(not(debug_assertions))]
fn precheck_install_surface() {
    let mut missing: Vec<String> = Vec::new();

    let knowledge = config::bundled_knowledge_dir();
    if !knowledge.is_dir() {
        missing.push(format!("知识库目录：{}", knowledge.display()));
    }
    let skills = config::bundled_skills_dir();
    if !skills.is_dir() {
        missing.push(format!("内置技能源：{}", skills.display()));
    }
    let seed = config::db_seed_path();
    if !seed.is_file() {
        missing.push(format!("数据库初始副本：{}", seed.display()));
    }
    if crate::maa::bridge::resolve_exe().is_err() {
        missing.push("MAA 桥可执行文件：prts-maa-bridge.exe".to_string());
    }

    if missing.is_empty() {
        return;
    }

    let mut body = String::from("缺失或不可读的文件：\n");
    for item in &missing {
        body.push_str(&format!("- {item}\n"));
    }
    body.push_str(&format!("\n诊断文件：{}", boot_log_path().display()));
    fatal_startup("PRTS 安装不完整", &body);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // ── 启动前自检（仅发布构建）：安装面完整性 ──
    #[cfg(not(debug_assertions))]
    precheck_install_surface();

    // ── 数据库种子同步：随包初始副本 → 数据根运行时副本 ──
    // 首启与后续启动同一条路径：缺失则复制、落后于程序版本则替换。失败意味着
    // 数据根写不进去（磁盘满 / 权限 / 被拦截），发布构建走启动失败弹窗，
    // 开发构建只记日志继续（开发态另有源码库可依）。
    match db_seed::sync_on_boot() {
        Ok(db_seed::SyncOutcome::UpToDate) => {}
        Ok(outcome) => eprintln!("[lib] 数据库种子同步：{outcome:?}"),
        Err(e) => {
            eprintln!("[lib] 数据库种子同步失败：{e}");
            #[cfg(not(debug_assertions))]
            fatal_startup("PRTS 无法启动", &e);
        }
    }

    // ── 数据库 ──
    // M1 批 3：db 路径解析收口于 config::resolve_db_path（原 get_db_path 删除）
    // 打不开不再 panic：降级成“不可用态”——启动自检会把它报出来，之后的查库
    // 调用返回明确错误。发布形态下 db 落位异常时，用户拿到的是可解释的终端，
    // 而不是一个起不来的应用。
    let db_path = config::resolve_db_path();
    let db = Arc::new(match Database::open(db_path.to_string_lossy().as_ref()) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("[lib] 数据库打开失败（降级为不可用态）：{e}");
            Database::unavailable(e)
        }
    });

    // ── 模型注册表（从 models.json 加载）──
    let model_registry = {
        let json = include_str!("../resources/models.json");
        Arc::new(ModelRegistry::from_json(json).expect("models.json 格式错误"))
    };

    // ── 设置存储（2026-09 设置页扩展批 A：单一 settings.json 持久化层）──
    // 装配语义：文件存在用文件；文件缺失用默认值（不落盘，首次 set_settings
    // 成功保存时才建文件）；**env 完全不参与配置装配**（PRTS_LLM_* /
    // PRTS_WORKSPACE 已废弃）。
    let settings_store = Arc::new(Mutex::new(SettingsStore::load(&config::data_dir())));

    // ── 工作目录（文件工具的工作根：read_file/list_dir 的相对路径基准）──
    // 来源 = settings.workspace.workspace_root（空串 = 回退 data_dir）；
    // 修改需重启生效（装配期消费一次，无热切换）。
    let workspace_root = {
        let st = settings_store.lock().unwrap();
        let root = st.workspace_root();
        std::fs::create_dir_all(&root).expect("无法创建工作目录");
        root
    };

    // ── 知识库索引（read_knowledge 的查询底表；加载失败降级为空索引）──
    // 启动自检要报的是“索引到底建起来没有”，故结果（条目数 / 失败原因）留住。
    let (knowledge_index, knowledge_count, knowledge_error) = {
        let root = knowledge_dir();
        match KnowledgeIndex::load(&root) {
            Ok(idx) => {
                let n = idx.entry_count();
                (Arc::new(idx), n, None)
            }
            Err(e) => {
                eprintln!("[lib] 知识库索引加载失败（read_knowledge 将不可用）：{e}");
                (Arc::new(KnowledgeIndex::empty_for_boot_failure()), 0, Some(e))
            }
        }
    };

    // ── 记忆存储（Phase 4 任务二 · 批 1）：落于 data_dir()/memory/ ──
    // 目录创建失败时降级为空存储（记忆工具仍可注册，只读为空），不阻断启动；
    // 失败原因留住，启动自检会报出来。
    let memory_error: Option<String>;
    let memory_store = {
        let dir = config::data_dir();
        match MemoryStore::load(&dir) {
            Ok(s) => {
                memory_error = None;
                Arc::new(Mutex::new(s))
            }
            Err(e) => {
                eprintln!("[lib] 记忆存储加载失败（降级为空）：{e}");
                memory_error = Some(e);
                Arc::new(Mutex::new(MemoryStore::empty(dir.join("memory"))))
            }
        }
    };

    // ── 启动自检事实（get_boot_status 消费）──
    // 只报事实、不参与业务判定：启动演出据此在载入时翻牌。数据库的可用性
    // 从 Database 自身读，这里带的是装配期一次性得到的知识库 / 记忆结果。
    let boot_facts = Arc::new(commands::BootFacts {
        knowledge_count,
        knowledge_error,
        memory_error,
    });

    // ── 提醒数据源（notices：快照＝值、环境零累积、事件有界）━━
    // 提前创建：记忆工具（memory_growth）写成功需向数据源发记忆事件，
    // 故须先于 register_builtin；值同步与目录 diff 在工具注册后补做。
    let ledger = Arc::new(Mutex::new(Notices::load(&config::data_dir())));

    // ── 工具注册表（注册来源收口：tools::register_builtin，模块 9 P1）──
    // M1 批 2：以 workspace 根构造沙箱策略注入注册（read_file/sim_path
    // 经策略持有守卫与工作区基准；workspace_root 区块批 3 接 config）
    let tool_registry = {
        let mut reg = ToolRegistry::new();
        let policy = Arc::new(Policy::new(workspace_root.clone()));
        // 内置工具在此统一注册；每个工具注册时校验 JSON Schema，
        // 非法 schema 在此暴露（拒绝注册）
        tools::register_builtin(
            &mut reg,
            policy,
            knowledge_index,
            Arc::clone(&memory_store),
            Arc::clone(&ledger),
        )
            .expect("内置工具注册失败（schema 校验未通过）");
        Arc::new(reg)
    };

    // ── 记忆区冻结快照（随对话档案落盘 sessions/{cid}.memory.txt）──
    // 首次组装即写；同一对话读回原文，唯一刷新点＝新对话（跨重启逐字不变）。
    let snapshot_store = Arc::new(Mutex::new(SnapshotStore::new(
        &crate::session_store::sessions_dir(&config::data_dir()),
    )));

    // ── 技能目录冻结快照（sessions/{cid}.skills.txt；批 2）──
    // 首次组装即冻结（活体目录文本入 sidecar）；同一对话读回原文
    // （system 前缀缓存硬约束），刷新点＝新对话（+ 压缩，批 4）。
    let skills_snapshot_store = Arc::new(Mutex::new(SkillSnapshotStore::new(
        &crate::session_store::sessions_dir(&config::data_dir()),
    )));

    // ── 适配器注册表 ──
    // 按 ApiType 路由到对应 LlmClient 实现。
    // 未来添加新协议适配器只需在此改一行 register。
    let client_registry = {
        let mut reg = ClientRegistry::new();
        reg.register(
            crate::llm::ApiType::OpenAiCompletions,
            Arc::new(OpenAiCompatibleClient::new(Arc::clone(&model_registry))),
        );
        reg.register(
            crate::llm::ApiType::AnthropicMessages,
            Arc::new(AnthropicMessagesClient::new(Arc::clone(&model_registry))),
        );
        reg.register(
            crate::llm::ApiType::OpenAIResponses,
            Arc::new(OpenAIResponsesClient::new(Arc::clone(&model_registry))),
        );
        // GoogleGemini 走 OpenAI 兼容端点，由 OpenAiCompletions 处理，不需要独立适配器
        Arc::new(reg)
    };

    // ── LLM 配置（Mutex 保护写入，set_settings 命令更新） ──
    // 装配自 settings.llm：active 供应商 resolve 成请求用 LlmConfig
    // （provider = active 名；文件缺失 → 默认空配置）。
    let llm_config = {
        let st = settings_store.lock().unwrap();
        Arc::new(Mutex::new(st.settings.llm.resolve_active()))
    };

    // ── 内置技能同步（运行根 = data_dir()/skills；源为准；用户条目只增不删，内置退役清理）──
    // 顺序约束：先同步 → 再建索引（下方 notices diff 与后续全部消费方）。
    // 失败不阻断启动：记日志继续；技能目录缺失时索引侧自会降级。
    match crate::skills_index::sync_bundled_skills(
        &crate::skills_index::bundled_skills_dir(),
        &crate::skills_index::skills_dir(),
    ) {
        Ok(r) => {
            if !r.changed.is_empty() || !r.removed.is_empty() {
                let mut parts: Vec<String> = Vec::new();
                if !r.changed.is_empty() {
                    parts.push(format!("更新 {} 个（{}）", r.changed.len(), r.changed.join("、")));
                }
                if !r.removed.is_empty() {
                    parts.push(format!("退役 {} 个（{}）", r.removed.len(), r.removed.join("、")));
                }
                parts.push(format!("未变 {} 个", r.unchanged));
                eprintln!("[lib] 技能同步：{}", parts.join(" / "));
            }
        }
        Err(e) => eprintln!("[lib] 内置技能同步失败（继续启动）：{e}"),
    }

    // ── 提醒数据源值同步 / 目录 diff（notices 已提前创建并在注册工具时注入）──
    // 值覆写：workspace/knowledge/model 三维度与当前值比对，不同才更新
    // （stamp＝++seq）；目录 diff（技能/工具）首启只记基线。前端 sendMessage 经
    // get_reminder_data 拉取（快照＝值；增量＝stamp/seq 超游标）。
    {
        // llm_config 装配后取当前生效 model label（env 注入已废，来自 settings 文件）
        let model_label = {
            let c = llm_config.lock().unwrap(); // 刚构造，无竞争
            crate::notices::model_label(&c.provider, &c.model)
        };
        // 本机信息：首次启动全量探测；之后启动只做存活检查（失活才升级全量）。
        // 路径持久化到 data_dir/machine.json（工具层据此解析，不入值维度）。
        let machine_info = {
            let dir = config::data_dir();
            match crate::machine::MachineInfo::load(&dir) {
                Some(info) if info.all_alive() => info,
                _ => {
                    let info = crate::machine::MachineInfo::detect(None);
                    info.save(&dir);
                    info
                }
            }
        };

        let mut ledger = ledger.lock().unwrap();
        ledger.sync_environment_snapshots(
            &workspace_root.to_string_lossy(),
            &model_label,
            &knowledge_dir().to_string_lossy(),
        );
        let (maa, emulator, game_pc) = machine_info.dim_values();
        ledger.sync_machine_snapshots(&maa, &emulator, &game_pc);
        // 技能目录：扫描失败 → 跳过 diff（保留既有基线；首启失败则下次
        // 运行仍按首启处理——空清单不覆盖真实基线）
        match crate::skills_index::SkillIndex::load(&crate::skills_index::skills_dir()) {
            Ok(idx) => {
                // (name, desc) 对：描述变化也可捕捉（批 3）
                let pairs = idx.catalog_pairs();
                ledger.diff_skill_catalog(&pairs);
            }
            Err(e) => {
                eprintln!("[lib] Skill 索引加载失败（Reminder 技能目录差异跳过）：{e}");
            }
        }
        // 工具目录：register_builtin 之后快照（list_defs 名字集合）
        let tool_names: Vec<String> = tool_registry
            .list_defs()
            .iter()
            .map(|d| d.function.name.clone())
            .collect();
        ledger.diff_tool_catalog(&tool_names);
    }

    // ── 会话存储（对话持久化 · Phase 4：data_dir/sessions/{conversationId}.jsonl
    // + index.json 原子索引）──
    // 后端纯存储层：写盘触发 = 前端轮末归档（append_turn）；删除对话 = 删持久文件。
    // Mutex 串行化全部命令（单 Agent 悬浮窗 + 单前端，无并发需求，消灭写竞态）。
    let session_store = Arc::new(Mutex::new(
        crate::session_store::SessionStore::new(&config::data_dir()),
    ));

    // ── 缓存区（工作记忆）State 已注释（2026-08：缓存工具取消注册，
    // 缓存系统停用，待升级为记忆系统；未来复用时可恢复）──
    // let cache_stores = Arc::new(Mutex::new(crate::cache::CacheStores::new()));
    //
    // ── 上轮卸载摘要（模块 B/C）已注释（2026-08：卸载机制移除）──
    // let last_unloads = Arc::new(Mutex::new(std::collections::HashMap::<String, String>::new()));

    // ── 命令执行模式（M3-A：全局声明层设置，默认操作前询问）──
    // 装配自 settings.exec 段（2026-09：持久化兑现，落盘 settings.json）；
    // 运行 State 由 set_settings 的 exec patch 同步更新。
    let exec_settings = {
        let st = settings_store.lock().unwrap();
        Arc::new(Mutex::new(st.settings.exec))
    };

    // ── MAA 设置（settings.maa 段，2026-10）──
    // 运行期权威在桥管理器（连接类改动即作废现有连接），装配时推入初值。
    let maa_settings = {
        let st = settings_store.lock().unwrap();
        let section = st.settings.maa.clone();
        crate::maa::manager::set_settings(section.clone());
        Arc::new(Mutex::new(section))
    };

    // ── 审批注册表（M3-A：run_command 确认等待的决策通道）──
    // 内部 Mutex 串行化 register/resolve；外部 Arc 共享（orchestrator 等待端
    // 与 resolve_tool_approval 命令端）。
    let approval_registry = Arc::new(ApprovalRegistry::new());

    // ── 活动 turn 注册表（M3-B：中止通道 + busy 口径）──
    // 命令层 agent_process 在 turn 起点 register / 返回后无条件 unregister；
    // abort_turn 命令置位中止；设置闸（set_settings）消费 active_count()。
    let turn_registry = Arc::new(TurnRegistry::new());

    // ── 插话待注入队列（2026-09-25 语义修订：步边界领取 + 保留交付）──
    // 键 = session_id（与 turn_registry 同键）；命令层 steer_message 入队 /
    // cancel_steer 移除；dispatch 每个 step 请求前领取注入；中止不清队列——
    // 未交付条目由 agent_process 收尾 preserve、同对话下一轮 adopt 领取。
    let steer_queue = Arc::new(SteerQueue::new());

    // ── 装配 Tauri Builder ──
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        // 键盘治理（发布质量）：拦截浏览器默认行为（刷新/打印/源码/下载/
        // 查找/焦点移动等；右键菜单为保险层——前端自绘体系照常工作）。
        // debug() 语义随构建自动分档：dev 保留「刷新 + 开发者工具 + 右键」，
        // release 全部拦截（见 tauri-plugin-prevent-default 的 Flags::debug）。
        .plugin(tauri_plugin_prevent_default::debug())
        .manage(db)
        .manage(tool_registry)
        .manage(client_registry)
        .manage(llm_config)
        .manage(model_registry)
        .manage(ledger)
        .manage(session_store)
        .manage(memory_store)
        .manage(snapshot_store)
        .manage(skills_snapshot_store)
        .manage(exec_settings)
        .manage(maa_settings)
        .manage(approval_registry)
        .manage(turn_registry)
        .manage(steer_queue)
        .manage(settings_store)
        // .manage(cache_stores)
        // .manage(last_unloads)
        .manage(Arc::clone(&boot_facts))
        // 置顶初值：tauri.conf.json 静态 alwaysOnTop=true 为窗口创建兜底；
        // settings.window.always_on_top=false 时启动即关闭（文件值优先）。
        .setup(|app| {
            let store = app.state::<Arc<Mutex<SettingsStore>>>();
            let atop = store.lock().unwrap().settings.window.always_on_top;
            if !atop {
                if let Some(win) = app.get_webview_window("main") {
                    if let Err(e) = win.set_always_on_top(false) {
                        eprintln!("[lib] 启动应用置顶初值失败（file=false）: {e}");
                    }
                }
            }
            // 孤儿冻结文件清扫（记忆：对话已删、残留的死 sidecar；与 ledger diff 同区）
            if let Ok(snaps) = app.state::<Arc<Mutex<SnapshotStore>>>().lock() {
                let n = snaps.sweep_orphans();
                if n > 0 {
                    eprintln!("[lib] 清扫孤儿记忆快照 {n} 个");
                }
            }
            // 技能目录快照同区清扫（批 2）
            if let Ok(snaps) = app.state::<Arc<Mutex<SkillSnapshotStore>>>().lock() {
                let n = snaps.sweep_orphans();
                if n > 0 {
                    eprintln!("[lib] 清扫孤儿技能目录快照 {n} 个");
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::agent_process,
            commands::get_settings,
            commands::set_settings,
            commands::reveal_api_key,
            commands::get_app_meta,
            commands::get_boot_status,
            commands::maa_status,
            commands::maa_test_connection,
            commands::pick_emulator_directory,
            commands::pick_adb_file,
            commands::list_skills,
            commands::pick_skill_file,
            commands::pick_skill_folder,
            commands::install_skill,
            commands::remove_skill,
            commands::open_path,
            commands::open_url,
            commands::set_always_on_top,
            commands::choose_workspace_directory,
            commands::list_workspace_dir,
            commands::search_workspace_files,
            commands::open_workspace_in_explorer,
            commands::get_reminder_data,
            commands::list_conversations,
            commands::get_conversation,
            commands::append_turn,
            commands::update_conversation_meta,
            commands::generate_conversation_title,
            commands::compact_conversation,
            commands::delete_conversation,
            commands::get_local_model_info,
            commands::quit_app,
            commands::verify_connection,
            commands::list_providers,
            commands::list_models,
            commands::resolve_tool_approval,
            commands::abort_turn,
            commands::steer_message,
            commands::cancel_steer,
            commands::finalize_memory,
            commands::memory_catch_up,
            commands::memory_overview,
            commands::memory_get,
            commands::memory_create,
            commands::memory_update,
            commands::memory_delete,
            commands::memory_experience_list,
            commands::memory_experience_clear,
            commands::memory_set_longterm,
            commands::memory_tidy,
            commands::upload_attachment,
            commands::upload_attachment_blob,
            // commands::clear_conversation_cache, // 已注释（2026-08：缓存系统停用）
        ])
        .run(tauri::generate_context!())
        .expect("启动 Tauri 应用失败");
}
