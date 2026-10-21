//! policy —— 沙箱策略层（M1 地基）：读写访问的四级权限判定
//!
//! 四级模型（M1 规格）：
//! - BLOCKED（连读都不行）：**数据根整根**（会话、记忆、设置、提醒台账、密钥、
//!   日志、数据库运行副本全在其中；普通用户与其工具都不需要碰它），
//!   以及尚未搬入数据根的数据库文件 arknights.db（搬入后此条可删）
//! - READ_ONLY（可读不可写）：知识库目录、技能运行根（data_dir()/skills）
//! - WRITABLE（可写）：当前工作根 workspace_root（共享句柄，改设置即跟随；
//!   见 [`crate::workspace`]）
//! - protected（写保护特例）：当前工作根下的 `.git`（可读不可写，防 LLM
//!   破坏仓库；按当前工作根实时派生）
//!
//! 判定约定：`check_read` / `check_write` 接收 **canonicalize 之后的路径**，
//! 边界检查必须发生在 canonicalize 之后（path_guard.rs 保证）。
//!
//! 匹配语义：统一用 `Path::starts_with`（组件级比较，非字符串前缀）——
//! 文件条目（blocked 的 notices.json / env-ledger.json / db）在 `==` 时命中（自身即前缀，
//! `a.json` 与 `a.json2` 组件不同不会误命中）；目录条目（read_only /
//! protected）命中自身与全部子路径。

use std::path::{Path, PathBuf};

use crate::config;
use crate::knowledge::index::knowledge_dir;
use crate::skills_index::skills_dir;
use crate::workspace::{canonicalize_opt, WorkspaceRoot};

/// 访问分类标签（四级的判别枚举）。
///
/// 当前主要用于把路径归类到四级模型的语义标签（错误分级 / 未来工具
/// 描述可用）；判定入口是 [`Policy::check_read`] / [`Policy::check_write`]。
#[allow(dead_code)] // 守卫层经 is_* 分类（错误变体更细）；本枚举供未来错误分级/描述使用
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessKind {
    /// BLOCKED：名单内，连读都不行
    Blocked,
    /// READ_ONLY：可读不可写
    ReadOnly,
    /// WRITABLE：工作空间内可写
    Writable,
    /// protected：写保护特例（工作区 .git）
    Protected,
}

/// 沙箱策略：workspace 根 + 两条路径清单 + 两级判定函数。
///
/// 工作根是**共享句柄**而非快照（见 [`crate::workspace`]）：设置页改路径后
/// 判定立即跟随，无需重启。
#[derive(Debug, Clone)]
pub struct Policy {
    /// 当前工作根（WRITABLE 区）：共享句柄（判定用其 canonical 形态）。
    workspace_root: WorkspaceRoot,
    /// BLOCKED：连读都不行（当前均为**文件**路径）
    pub blocked: Vec<PathBuf>,
    /// READ_ONLY：可读不可写（**目录**路径，前缀语义）
    pub read_only: Vec<PathBuf>,
}

impl Policy {
    /// 构造默认策略：
    /// - BLOCKED = **数据根整根**（`config::data_dir()`；会话 / 记忆 / 设置 /
    ///   提醒台账 / 密钥 / 日志 / 数据库**运行时副本**都在其中）
    ///   + 随包数据库**种子**（`config::db_seed_path()`：与原来同理由，
    ///     不允许绕过工具直读数据库）
    /// - READ_ONLY = knowledge_dir() + skills_dir()（知识库 + 技能运行根
    ///   data_dir()/skills——内置同步副本与用户技能同住其中）
    /// - protected = 当前工作根下的 `.git`（见 [`Self::is_protected`]：实时派生
    ///   ——工作根可变，快照会过期；用户把工作区指向一个仓库时必须立刻受保护）
    ///
    /// `workspace_root` 接受路径或已有句柄：传路径 = 建**独立**句柄（单测 /
    /// 独立使用）；传句柄 = 与设置共享同一份值（生产装配经 lib.rs 注入）。
    pub fn new(workspace_root: impl Into<WorkspaceRoot>) -> Self {
        let ws = workspace_root.into();
        // 数据根整根拦截：此项按组件级前缀匹配，覆盖全部子树，
        // 因此模型**读不到**数据根内的任何文件（含 MAA 识别明细）。
        let blocked = vec![
            canonicalize_opt(config::data_dir()),
            // 随包种子另列（安装在程序目录，不在数据根内）。
            canonicalize_opt(config::db_seed_path()),
        ];
        let read_only = vec![
            canonicalize_opt(knowledge_dir()),
            canonicalize_opt(skills_dir()),
        ];
        Self {
            workspace_root: ws,
            blocked,
            read_only,
        }
    }

    /// 当前工作根的共享句柄（工具构造时注入；与设置同源，改设置即跟随）。
    pub fn workspace_root(&self) -> WorkspaceRoot {
        self.workspace_root.clone()
    }

    /// canonical 路径是否命中任一名单条目（组件级前缀比较，
    /// 文件条目命中 ==，目录条目命中自身与子路径）。
    ///
    /// 名单条目**动态 canonicalize**：Windows 下 canonicalize 返回 verbatim
    /// 路径（`\\?\` 前缀），而 Policy 构造时条目可能尚不存在（notices.json
    /// 未落盘、新工作区无 .git）只能存普通路径——检查时对存在条目实时
    /// canonicalize，保证与传入的 canonical 路径同基准（`\\?\`）比较；
    /// 条目不存在时传入路径必不等于它（同一路径不可能一边存在一边不存在），
    /// 保留原样不影响判定。
    fn matches_any(list: &[PathBuf], canonical: &Path) -> bool {
        list.iter().any(|b| {
            let b = b.canonicalize().unwrap_or_else(|_| b.clone());
            canonical.starts_with(&b)
        })
    }

    /// 读判定：默认放行全盘（check_read 仅查 BLOCKED），名单内拒绝。
    #[allow(dead_code)] // 守卫走 is_blocked（区分错误变体）；bool 聚合接口供未来直接判定场景
    pub fn check_read(&self, canonical: &Path) -> bool {
        !Self::matches_any(&self.blocked, canonical)
    }

    /// 写判定：工作区内（含根自身——写 workspace_root 本身 = 在其中
    /// 创建条目，放行）**且**不在 BLOCKED / READ_ONLY / protected 前缀内。
    /// （M2 #B：写同读拒 BLOCKED——默认工作区 = data_dir 时 db / 台账位于
    /// 可写区内，bool 聚合与守卫（path_guard::check_write_bounds）同语义。）
    #[allow(dead_code)] // 守卫走 is_in_workspace/is_*（区分错误变体）；bool 聚合接口供未来直接判定场景
    pub fn check_write(&self, canonical: &Path) -> bool {
        self.is_in_workspace(canonical)
            && !Self::matches_any(&self.blocked, canonical)
            && !Self::matches_any(&self.read_only, canonical)
            && !self.is_protected(canonical)
    }

    /// 是否在工作区内（== 或前缀；根自身含）。按**当前**工作根判定。
    pub fn is_in_workspace(&self, p: &Path) -> bool {
        p.starts_with(self.workspace_root.canonical())
    }

    /// 是否命中 BLOCKED 名单。
    pub fn is_blocked(&self, p: &Path) -> bool {
        Self::matches_any(&self.blocked, p)
    }

    /// 是否命中 READ_ONLY 名单。
    pub fn is_read_only(&self, p: &Path) -> bool {
        Self::matches_any(&self.read_only, p)
    }

    /// 是否命中写保护（当前工作根下的 `.git`，按当前根实时派生）。
    pub fn is_protected(&self, p: &Path) -> bool {
        p.starts_with(canonicalize_opt(
            self.workspace_root.canonical().join(".git"),
        ))
    }
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建唯一临时目录（不引入 tempfile crate，沿用 fs.rs 夹具模式）。
    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("prts-pol-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 工作区夹具：ws/ 含 .git/、sub/ 与 a.txt；ro/ 为工作区内
    /// 追加的 READ_ONLY 演示目录（知识库 dev 下在 workspace 外，
    /// check_write 的 read_only 真分支需构造工作区内只读目录）。
    fn ws_fixture(tag: &str) -> PathBuf {
        let ws = unique_temp_dir(tag);
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        std::fs::create_dir_all(ws.join("sub")).unwrap();
        std::fs::create_dir_all(ws.join("ro")).unwrap();
        std::fs::write(ws.join("a.txt"), "hello").unwrap();
        ws
    }

    fn make_policy(ws: &Path) -> Policy {
        let mut p = Policy::new(ws.to_path_buf());
        // READ_ONLY 真分支：知识库 dev 下位于工作区外（命中 OutsideWorkspace
        // 而非 read_only），此处在工作区内补一个只读目录验证 read_only 拦截
        p.read_only.push(ws.join("ro"));
        p
    }

    // ── check_read ──

    #[test]
    fn read_blocked_entries_rejected() {
        // 自反断言：Policy::new 存的 BLOCKED 条目自身必须拒读
        // （不重新求值 data_dir——避免与 env 修改测试并发的取值窗口）
        let ws = ws_fixture("rd-blocked");
        let policy = Policy::new(ws.clone());
        for b in &policy.blocked {
            assert!(!policy.check_read(b), "BLOCKED 条目应拒读：{b:?}");
        }
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn read_workspace_file_allowed() {
        let ws = ws_fixture("rd-ws");
        let policy = Policy::new(ws.clone());
        let canon = ws.join("a.txt").canonicalize().unwrap();
        assert!(policy.check_read(&canon), "工作区内普通文件应可读");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn read_knowledge_dir_allowed() {
        // 知识库不在 BLOCKED 名单 → READ_ONLY 区可读（check_read 仅查 blocked）
        let ws = ws_fixture("rd-kb");
        let policy = Policy::new(ws.clone());
        let kb = knowledge_dir().canonicalize().unwrap();
        assert!(policy.check_read(&kb), "知识库应可读（READ_ONLY 仅限写）");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn read_outside_workspace_allowed() {
        // 读默认放行全盘：工作区外普通文件不命中 BLOCKED 即可读
        let ws = ws_fixture("rd-outside");
        let policy = Policy::new(ws.clone());
        let other = unique_temp_dir("rd-other");
        std::fs::write(other.join("x.txt"), "x").unwrap();
        let canon = other.join("x.txt").canonicalize().unwrap();
        assert!(policy.check_read(&canon), "工作区外普通文件默认可读");
        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── check_write ──

    #[test]
    fn write_workspace_allowed() {
        let ws = ws_fixture("wr-ws");
        let policy = make_policy(&ws);
        let canon_ws = ws.canonicalize().unwrap();
        // 工作区内新文件（不存在也放行——check_write 只看路径形态）
        assert!(policy.check_write(&canon_ws.join("new.txt")));
        assert!(policy.check_write(&canon_ws.join("sub").join("new.txt")));
        // 工作区根自身（写根 = 在其中创建条目）
        assert!(policy.check_write(&canon_ws), "workspace_root 自身应可写");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_blocked_entry_rejected() {
        // M2 #B：写判定含 BLOCKED——blocked 文件位于可写区（默认工作区 =
        // data_dir 时 db / notices.json 即此形态）时仍拒绝
        let ws = ws_fixture("wr-blocked");
        let mut policy = Policy::new(ws.clone());
        // 注入的工作区内 blocked 演示文件必须真实存在：matches_any 动态
        // canonicalize 名单条目（Windows verbatim 同基准），不存在则失配
        std::fs::write(ws.join("secret.txt"), "secret").unwrap();
        policy.blocked.push(ws.join("secret.txt"));
        let canon = ws.canonicalize().unwrap();

        assert!(!policy.check_write(&canon.join("secret.txt")));
        // 不误伤：同目录普通文件仍可写
        assert!(policy.check_write(&canon.join("normal.txt")));
        assert!(policy.check_write(&canon));

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_git_dir_rejected() {
        let ws = ws_fixture("wr-git");
        let policy = Policy::new(ws.clone());
        let canon = ws.canonicalize().unwrap();
        // .git 自身与子路径都拒绝
        assert!(!policy.check_write(&canon.join(".git")));
        assert!(!policy.check_write(&canon.join(".git").join("config")));
        assert!(!policy.check_write(&canon.join(".git").join("objects")));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_read_only_dir_rejected() {
        let ws = ws_fixture("wr-ro");
        let policy = make_policy(&ws); // ws/ro 已注入 read_only
        let canon = ws.canonicalize().unwrap();
        assert!(!policy.check_write(&canon.join("ro").join("x.txt")));
        assert!(!policy.check_write(&canon.join("ro")));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn read_only_covers_skills_runtime_root() {
        // 技能运行根（data_dir()/skills）应入 READ_ONLY 名单（可读不可写）；
        // 目录可不存在——两侧同用 canonicalize 兜底口径比较
        let ws = ws_fixture("ro-skills");
        let policy = Policy::new(ws.clone());
        let sk = skills_dir();
        let sk_c = sk.canonicalize().unwrap_or(sk);
        assert!(
            policy.read_only.iter().any(|b| b == &sk_c),
            "read_only 应含技能运行根：{sk_c:?}"
        );
        assert!(
            policy.is_read_only(&sk_c.join("数值计算.md")),
            "运行根子路径应命中 READ_ONLY"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_knowledge_dir_rejected() {
        // 知识库在工作区外 → is_in_workspace false → 拒写
        let ws = ws_fixture("wr-kb");
        let policy = Policy::new(ws.clone());
        let kb = knowledge_dir().canonicalize().unwrap();
        assert!(!policy.check_write(&kb), "知识库不可写（工作区外）");
        assert!(!policy.check_write(&kb.join("x.md")));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn write_outside_workspace_rejected() {
        let ws = ws_fixture("wr-outside");
        let policy = Policy::new(ws.clone());
        let other = unique_temp_dir("wr-other");
        let canon = other.canonicalize().unwrap();
        assert!(!policy.check_write(&canon.join("x.txt")));
        assert!(!policy.check_write(&canon));
        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&ws);
    }

    // ── 工作根热更新（共享句柄）──

    #[test]
    fn workspace_root_follows_live_update() {
        // 改设置后判定必须立即跟随（工作根是句柄不是快照）：新根立刻可写、
        // 旧根立刻不可写、`.git` 保护随之迁移。
        let ws_a = ws_fixture("live-a");
        let ws_b = ws_fixture("live-b");
        let policy = Policy::new(ws_a.clone());
        let a = ws_a.canonicalize().unwrap();
        let b = ws_b.canonicalize().unwrap();

        assert!(policy.is_in_workspace(&a.join("x.txt")), "初始根应可写");
        assert!(
            !policy.is_in_workspace(&b.join("x.txt")),
            "换根前新根不在工作区"
        );

        policy.workspace_root().set(ws_b.clone());

        assert!(
            policy.is_in_workspace(&b.join("x.txt")),
            "换根后新根应立即生效"
        );
        assert!(policy.check_write(&b.join("new.txt")));
        assert!(
            !policy.is_in_workspace(&a.join("x.txt")),
            "换根后旧根应失效"
        );
        assert!(!policy.check_write(&a.join("new.txt")));
        // `.git` 保护随根迁移（若按构造时快照实现，新根的 .git 会漏保护）
        assert!(policy.is_protected(&b.join(".git").join("config")));
        assert!(!policy.is_protected(&a.join(".git").join("config")));

        let _ = std::fs::remove_dir_all(&ws_a);
        let _ = std::fs::remove_dir_all(&ws_b);
    }

    // ── 名单内容形态 ──

    #[test]
    fn policy_entries_shape() {
        // blocked 含 db 文件（dev 下真实存在 → canonicalize 成功）
        let ws = ws_fixture("shape");
        let policy = Policy::new(ws.clone());
        let db = config::db_seed_path().canonicalize().unwrap();
        assert!(
            policy.blocked.iter().any(|b| b == &db),
            "blocked 应含种子路径"
        );
        // read_only 含知识库与技能运行根（运行根可不存在，兜底口径比较）
        let kb = knowledge_dir().canonicalize().unwrap();
        assert!(policy.read_only.iter().any(|b| b == &kb));
        let sk = skills_dir();
        let sk_c = sk.canonicalize().unwrap_or(sk);
        assert!(policy.read_only.iter().any(|b| b == &sk_c));
        // protected 由当前工作根派生（工作根可变，故不是列表快照）
        let canon_ws = ws.canonicalize().unwrap();
        let git = canon_ws.join(".git");
        assert!(policy.is_protected(&git), ".git 自身应写保护");
        assert!(
            policy.is_protected(&git.join("config")),
            ".git 子路径应写保护"
        );
        assert!(
            !policy.is_protected(&canon_ws.join("sub")),
            "非 .git 不写保护"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
}
