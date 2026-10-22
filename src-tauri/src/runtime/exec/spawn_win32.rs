//! spawn_win32 —— Windows 受限令牌进程启动（M4 降权执行 · 子步 2-4）
//!
//! 仅 Windows 编译（exec/mod.rs 以 #[cfg(windows)] 门控）。实现「写隔离」模型：
//!
//! - **Token**：`CreateRestrictedToken(DISABLE_MAX_PRIVILEGE | LUA_TOKEN |
//!   WRITE_RESTRICTED)`；限制 SID = Everyone + 当前 logon SID + 每写根派生
//!   SID（S-1-5-21-* 哈希，discriminator `prts-win32-write-root-v1:`）+
//!   S-1-5-33（Write Restricted Code）；特权仅重开 SE_CHANGE_NOTIFY；
//!   TokenDefaultDacl 替换为 Everyone/logon/写根 SID 授 GENERIC_ALL
//!   （受限进程新建对象可回写）。完整性级别**不降**（Medium）。
//! - **Job**：kill-on-close 唯一限制；`PROC_THREAD_ATTRIBUTE_JOB_LIST`
//!   创建即入 Job（无逃逸窗口）；无进程数/内存上限、无 breakaway。
//! - **ACL**：workspace 根 + TEMP/缓存目录**临时** GRANT
//!   （CONTAINER|OBJECT_INHERIT，WRITE_ALLOW_MASK），引用计数——最后一名
//!   活跃会话退出时还原到「不含我方 SID 的干净状态」（崩溃残留由下次运行
//!   的清理步骤收敛）；同 workspace 并发受限命令互不干扰。
//! - **环境**：TEMP/TMP → workspace/.prts-exec-temp；npm/pip/cargo 缓存
//!   变量 → workspace/.prts-exec-cache/*（对应 exe 不在 PATH 时跳过）。
//! - **路径形式**：内部一律用 canonical（`\\?\`）形式做 SID 派生 / ACL /
//!   判定；只有「交给子进程」的路径（工作目录、TEMP/缓存环境变量）剥去
//!   `\\?\` 前缀——PowerShell 5.1 在 verbatim 工作目录下相对路径解析会失效。
//! - **PS 探针**：PowerShell 系首个受限进程先跑探测
//!   （`[Console]::Out.Write('ok')`；验证 %TEMP% 可写 + 无 CLM 回落）；
//!   失败 fail-fast（不静默回落），按 exe 路径缓存。
//! - **失败策略**：fail-closed；受限创建失败 → Err（调用方 SpawnError），
//!   不降级到全令牌自动兜底。
//!
//! ## 完整权限通道（M4 批 2：permission=full）
//!
//! [`spawn_full`] 复用同一启动管道（Job kill-on-close + CREATE_NO_WINDOW +
//! 句柄自持），但身份 = 应用自身主令牌（**无** restricted token、**无** ACL
//! 临时放行、**无** TEMP/缓存重定向、**无** PS 探针——全令牌语义）。full 不是
//! 纯 std spawn：超时/cancel/Job 树回收能力与受限路径一致，仅身份不同。
//!
//! `std::process::Child` 无法由外部 HANDLE 构造，句柄层自持：try_wait =
//! WaitForSingleObject(0) + GetExitCodeProcess；kill = TerminateJobObject +
//! 收敛等待；管道自建（CreatePipe ×3：stdin 空管读端 = EOF、stdout/stderr
//! 写端；HANDLE_LIST 仅这三个句柄进子进程）。

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W,
    GRANT_ACCESS, REVOKE_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows::Win32::Security::{
    AdjustTokenPrivileges, AllocateAndInitializeSid, CreateRestrictedToken, FreeSid, GetAce,
    GetLengthSid, GetTokenInformation, LookupPrivilegeValueW, SetTokenInformation,
    TokenDefaultDacl, TokenGroups, ACE_FLAGS, ACL, CONTAINER_INHERIT_ACE,
    CREATE_RESTRICTED_TOKEN_FLAGS, DACL_SECURITY_INFORMATION, DISABLE_MAX_PRIVILEGE, LUA_TOKEN,
    OBJECT_INHERIT_ACE, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
    SID_AND_ATTRIBUTES, SID_IDENTIFIER_AUTHORITY, TOKEN_ACCESS_MASK, TOKEN_ADJUST_DEFAULT,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_ASSIGN_PRIMARY, TOKEN_DEFAULT_DACL, TOKEN_DUPLICATE,
    TOKEN_GROUPS, TOKEN_IMPERSONATE, TOKEN_PRIVILEGES, TOKEN_QUERY,
    UNPROTECTED_DACL_SECURITY_INFORMATION, WRITE_RESTRICTED,
};
use windows::Win32::System::JobObjects::{
    CreateJobObjectW, JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, OpenProcessToken, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, STARTF_USESTDHANDLES,
    STARTUPINFOEXW,
};

// ═══════════════════════════════════════════════════════
// 常量（winnt 值，不依赖 crate 命名面）
// ═══════════════════════════════════════════════════════

const FILE_GENERIC_READ: u32 = 0x120089;
const FILE_GENERIC_WRITE: u32 = 0x120116;
const FILE_GENERIC_EXECUTE: u32 = 0x1200A0;
const DELETE_RIGHT: u32 = 0x0001_0000;
const FILE_DELETE_CHILD: u32 = 0x0000_0040;
/// 写放行掩码（同既有 WRITE_ALLOW_MASK）。
const WRITE_ALLOW_MASK: u32 = FILE_GENERIC_READ
    | FILE_GENERIC_WRITE
    | FILE_GENERIC_EXECUTE
    | DELETE_RIGHT
    | FILE_DELETE_CHILD;
/// TokenDefaultDacl 授全权（受限进程新建对象可回写）。
const GENERIC_ALL: u32 = 0x1F01FF;
/// 组属性（winnt SE_GROUP_*；TokenGroups 条目 Attributes 为 u32）。
const SE_GROUP_ENABLED: u32 = 0x0000_0004;
const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;
const STILL_ACTIVE: u32 = 259;
const JOB_CONVERGE_GRACE: Duration = Duration::from_millis(5000);
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const EXEC_TEMP_DIR: &str = ".prts-exec-temp";
const EXEC_CACHE_DIR: &str = ".prts-exec-cache";
const WRITE_ROOT_DISCRIMINATOR: &str = "prts-win32-write-root-v1:";

/// 打开自身进程令牌的访问权组合（typed wrapper 数值拼装）。
fn token_access_rights() -> TOKEN_ACCESS_MASK {
    TOKEN_ACCESS_MASK(
        TOKEN_DUPLICATE.0
            | TOKEN_ASSIGN_PRIMARY.0
            | TOKEN_QUERY.0
            | TOKEN_IMPERSONATE.0
            | TOKEN_ADJUST_DEFAULT.0
            | TOKEN_ADJUST_PRIVILEGES.0,
    )
}

/// PS 探测缓存：exe 全路径 → 是否可用。
fn probe_cache() -> &'static Mutex<HashMap<String, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 探针执行串行锁：并发首用风暴时只跑一次真实探针，其余线程等缓存。
fn probe_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

// ═══════════════════════════════════════════════════════
// 宽字符 / 路径 / exe / SID
// ═══════════════════════════════════════════════════════

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn os_wide(os: &std::ffi::OsStr) -> Vec<u16> {
    os.encode_wide().collect()
}

/// 规范化路径键：大写 + 反斜杠 + 剥 \\?\ 前缀（SID 派生 / 引用计数键共用）。
fn path_key(path: &Path) -> String {
    let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut s = p.to_string_lossy().replace('/', "\\");
    if let Some(stripped) = s.strip_prefix("\\\\?\\UNC\\") {
        s = format!("\\\\{stripped}");
    } else if let Some(stripped) = s.strip_prefix("\\\\?\\") {
        s = stripped.to_string();
    }
    let mut upper = s.to_uppercase();
    while upper.len() > 3 && upper.ends_with('\\') {
        upper.pop();
    }
    upper
}

/// 子进程可见路径：剥 `\\?\` verbatim 前缀（`\\?\UNC\server\share` → `\\server\share`）。
///
/// 内部判定（SID 派生 / 引用计数 / ACL / 守卫比较）一律沿用 canonical 形式；
/// 仅在「把路径交给子进程」的边界（工作目录、TEMP/缓存环境变量）转成普通形式。
fn child_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    match s.strip_prefix("\\\\?\\UNC\\") {
        Some(rest) => PathBuf::from(format!("\\\\{rest}")),
        None => match s.strip_prefix("\\\\?\\") {
            Some(rest) => PathBuf::from(rest),
            None => path.to_path_buf(),
        },
    }
}

/// PATH 解析 exe（绝对/带分隔符原样；否则 PATH × PATHEXT 常见扩展）。
fn resolve_exe_in_path(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.is_absolute() || name.contains('\\') || name.contains('/') {
        return p.is_file().then(|| p.to_path_buf());
    }
    let exts = [".exe", ".cmd", ".bat", ".com"];
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        for cand in std::iter::once(dir.join(name))
            .chain(exts.iter().map(|e| dir.join(format!("{name}{e}"))))
        {
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

fn tool_on_path(name: &str) -> bool {
    resolve_exe_in_path(name).is_some()
}

fn is_powershell_like(exe: &Path) -> bool {
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    stem == "powershell" || stem == "pwsh"
}

/// 权威标识 + 子权威 → SID 原始字节（AllocateAndInitializeSid + 拷贝 + FreeSid）。
fn build_sid(authority: [u8; 6], subauths: &[u32]) -> Option<Vec<u8>> {
    if subauths.is_empty() || subauths.len() > 8 {
        return None;
    }
    let auth = SID_IDENTIFIER_AUTHORITY { Value: authority };
    let mut psid: PSID = PSID(std::ptr::null_mut());
    let mut a = [0u32; 8];
    for (i, v) in subauths.iter().enumerate() {
        a[i] = *v;
    }
    unsafe {
        AllocateAndInitializeSid(
            &auth,
            subauths.len() as u8,
            a[0],
            a[1],
            a[2],
            a[3],
            a[4],
            a[5],
            a[6],
            a[7],
            &mut psid,
        )
        .ok()?;
        let len = GetLengthSid(psid) as usize;
        let bytes = std::slice::from_raw_parts(psid.0 as *const u8, len).to_vec();
        let _ = FreeSid(psid);
        Some(bytes)
    }
}

fn everyone_sid() -> Vec<u8> {
    build_sid([0, 0, 0, 0, 0, 1], &[0]).expect("Everyone SID")
}

fn write_restricted_code_sid() -> Vec<u8> {
    build_sid([0, 0, 0, 0, 0, 5], &[33]).expect("Write Restricted Code SID")
}

fn writable_root_sid(root: &Path) -> Vec<u8> {
    let key = format!("{WRITE_ROOT_DISCRIMINATOR}{}", path_key(root));
    let h = hash_root(&key);
    build_sid(
        [0, 0, 0, 0, 0, 5],
        &[21, h[0] | 1, h[1] | 1, h[2] | 1, h[3] | 1],
    )
    .expect("写根 SID 构造失败")
}

fn hash_root(key: &str) -> [u32; 4] {
    let mut h = [
        2166136261u32,
        2166136261 ^ 0x9e3779b9,
        2166136261 ^ 0x85ebca6b,
        2166136261 ^ 0xc2b2ae35,
    ];
    for ch in key.encode_utf16() {
        let value = ch as u32;
        for (i, slot) in h.iter_mut().enumerate() {
            *slot ^= value.wrapping_add((i as u32).wrapping_mul(257));
            *slot = slot.wrapping_mul(16777619);
            *slot ^= *slot >> 13;
        }
    }
    h
}

// ═══════════════════════════════════════════════════════
// ACL 临时放行（workspace 根 / TEMP / 缓存目录）
// ═══════════════════════════════════════════════════════

/// 还原记录：DACL ← 目标状态（无我方 SID）。持有 descriptor 保指针存活。
struct AclRestore {
    path_w: Vec<u16>,
    /// 干净 DACL（原 DACL 含我方 SID 残留时才构建；还原后 LocalFree）。
    clean_dacl: Option<*mut ACL>,
    /// 读取时的原始 DACL 指针（descriptor 内；无残留时的字节级还原目标）。
    orig_dacl: *mut ACL,
    /// GetNamedSecurityInfoW 的 descriptor（还原完成后释放）。
    orig_descriptor: PSECURITY_DESCRIPTOR,
    restore_clean: bool,
    /// 结算标记：已还原（restore）或已弃置（discard）后不再动作（防二次释放/悬垂指针）。
    settled: bool,
}

// SAFETY: 内部裸指针指向 LocalAlloc 分配的进程堆内存（DACL / 安全描述符），
// 无线程归属；对其的全部访问都发生在 acl_entries() 的互斥区内（首个会话登记 /
// 末位会话执行），跨线程移动与释放由该互斥串行化，故可 Send。
unsafe impl Send for AclRestore {}

impl AclRestore {
    /// 释放持有的 descriptor / clean DACL（不触碰目标对象 ACL）。
    fn free_owned(&mut self) {
        unsafe {
            if !self.orig_descriptor.0.is_null() {
                let _ = LocalFree(Some(HLOCAL(self.orig_descriptor.0.cast())));
                self.orig_descriptor = PSECURITY_DESCRIPTOR(std::ptr::null_mut());
            }
            if let Some(dacl) = self.clean_dacl.take() {
                let _ = LocalFree(Some(HLOCAL(dacl.cast())));
            }
            self.orig_dacl = std::ptr::null_mut();
        }
    }

    /// 还原目标对象 ACL 到记录状态（仅末位会话执行），随后释放本地内存。
    fn restore(&mut self) {
        if self.settled {
            return;
        }
        unsafe {
            // 还原目标：残留存在 → 干净状态；否则 → 原始 DACL（字节级一致）
            let target: Option<*const ACL> = if self.restore_clean {
                self.clean_dacl.map(|p| p as *const ACL)
            } else if self.orig_dacl.is_null() {
                None // NULL DACL：还原为无显式 DACL
            } else {
                Some(self.orig_dacl as *const ACL)
            };
            let rc = SetNamedSecurityInfoW(
                PCWSTR(self.path_w.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                target,
                None,
            );
            if rc.0 != 0 {
                eprintln!("[spawn_win32] ACL 还原失败 rc={}", rc.0);
            }
        }
        self.free_owned();
        self.settled = true;
    }

    /// 弃置：不触碰 ACL（放行仍被其它活跃会话使用），只释放本地内存。
    fn discard(&mut self) {
        if self.settled {
            return;
        }
        self.free_owned();
        self.settled = true;
    }
}

impl Drop for AclRestore {
    fn drop(&mut self) {
        // 兜底只释放内存：ACL 还原一律显式执行（末位会话），
        // 任意析构路径都不得改动授权状态。
        self.discard();
    }
}

/// 活跃写根登记项：引用计数 + 本代还原记录（末位会话归还时执行）。
struct GrantEntry {
    refs: usize,
    restores: Vec<AclRestore>,
}

/// 活跃写根登记表（同 workspace 并发受限命令共享放行；末位归零才还原）。
fn acl_entries() -> &'static Mutex<HashMap<String, GrantEntry>> {
    static ENTRIES: OnceLock<Mutex<HashMap<String, GrantEntry>>> = OnceLock::new();
    ENTRIES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 一次 spawn 期间持有的放行租约：只记 key；放行记录挂在登记表上。
/// 协议：首个进入者施加放行，末位退出者执行还原；非末位释放只递减计数。
struct GrantSession {
    key: String,
    released: bool,
}

impl GrantSession {
    fn acquire(ws: &Path) -> io::Result<GrantSession> {
        // TEMP/缓存父目录保证存在（放行对象必须可 GetNamedSecurityInfo）
        std::fs::create_dir_all(ws.join(EXEC_TEMP_DIR)).map_err(io::Error::other)?;
        std::fs::create_dir_all(ws.join(EXEC_CACHE_DIR)).map_err(io::Error::other)?;

        let key = path_key(ws);
        // 施加 / 计数 / 还原共用本锁串行：防「新代施加」与「旧代末位还原」交错。
        let mut entries = acl_entries().lock().unwrap();
        if let Some(entry) = entries.get_mut(&key) {
            // 已有活跃会话：共享其放行，只加引用
            entry.refs += 1;
            return Ok(GrantSession {
                key,
                released: false,
            });
        }
        // 本代首个会话：施加放行；失败原路收回已施加部分（此刻无并发者）
        let sid = writable_root_sid(ws);
        let mut restores = Vec::new();
        for p in [
            ws.to_path_buf(),
            ws.join(EXEC_TEMP_DIR),
            ws.join(EXEC_CACHE_DIR),
        ] {
            if let Err(e) = grant_write_root(&p, &sid, &mut restores) {
                for r in restores.iter_mut().rev() {
                    r.restore();
                }
                return Err(e);
            }
        }
        entries.insert(key.clone(), GrantEntry { refs: 1, restores });
        Ok(GrantSession {
            key,
            released: false,
        })
    }

    /// 归还引用；末位（归零）执行本代还原，其余只递减计数。重复调用无效。
    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;

        let mut entries = acl_entries().lock().unwrap();
        let zero = match entries.get_mut(&self.key) {
            Some(entry) => {
                entry.refs = entry.refs.saturating_sub(1);
                entry.refs == 0
            }
            None => return, // 防御：无对应登记（不应发生）
        };
        if zero {
            if let Some(mut entry) = entries.remove(&self.key) {
                for r in entry.restores.iter_mut().rev() {
                    r.restore();
                }
            }
        }
    }
}

impl Drop for GrantSession {
    fn drop(&mut self) {
        self.release();
    }
}

/// 读取路径当前 DACL（descriptor 所有权交给调用方）。
fn read_dacl(path_w: &[u16]) -> Result<(*mut ACL, PSECURITY_DESCRIPTOR), String> {
    unsafe {
        let mut ppdacl: *mut ACL = std::ptr::null_mut();
        let mut desc = PSECURITY_DESCRIPTOR(std::ptr::null_mut());
        let rc = GetNamedSecurityInfoW(
            PCWSTR(path_w.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut ppdacl),
            None,
            &mut desc,
        );
        if rc.0 != 0 {
            if !desc.0.is_null() {
                let _ = LocalFree(Some(HLOCAL(desc.0.cast())));
            }
            return Err(format!("GetNamedSecurityInfoW rc={}", rc.0));
        }
        Ok((ppdacl, desc))
    }
}

fn explicit_access(
    sid_bytes: &[u8],
    mode: windows::Win32::Security::Authorization::ACCESS_MODE,
    mask: u32,
    inherit: ACE_FLAGS,
) -> EXPLICIT_ACCESS_W {
    let mut trustee: TRUSTEE_W = unsafe { std::mem::zeroed() };
    trustee.TrusteeForm = TRUSTEE_IS_SID;
    trustee.TrusteeType = TRUSTEE_IS_UNKNOWN;
    trustee.ptstrName = PWSTR(sid_bytes.as_ptr().cast::<u16>().cast_mut());
    EXPLICIT_ACCESS_W {
        grfAccessPermissions: mask,
        grfAccessMode: mode,
        grfInheritance: inherit,
        Trustee: trustee,
    }
}

/// 当前 DACL 是否含我方 SID（允许/拒绝 ACE；原始字节比对）。
fn dacl_contains_sid(dacl: *const ACL, sid: &[u8]) -> bool {
    if dacl.is_null() {
        return false;
    }
    unsafe {
        let count = (*dacl).AceCount as usize;
        for i in 0..count {
            let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
            if GetAce(dacl, i as u32, &mut ace).is_err() {
                continue;
            }
            let ty = *(ace as *const u8);
            // ACCESS_ALLOWED_ACE_TYPE=0 / ACCESS_DENIED_ACE_TYPE=1：SID 自偏移 8
            if ty != 0 && ty != 1 {
                continue;
            }
            let ace_sid = (ace as *const u8).add(8) as *const core::ffi::c_void;
            let len = GetLengthSid(PSID(ace_sid as *mut core::ffi::c_void)) as usize;
            if len == sid.len() && std::slice::from_raw_parts(ace_sid as *const u8, len) == sid {
                return true;
            }
        }
    }
    false
}

/// 应用写放行；记录还原。
/// - 原 DACL 无我方 SID：还原目标 = 原始 DACL（字节级一致）；
/// - 有残留（崩溃/并发会话叠加）：先 revoke 再 grant，还原目标 = 干净状态。
fn grant_write_root(path: &Path, sid: &[u8], restores: &mut Vec<AclRestore>) -> io::Result<()> {
    let path_w = wide(&path.to_string_lossy());
    let (orig_dacl, desc) = read_dacl(&path_w).map_err(io::Error::other)?;

    let had_ours = dacl_contains_sid(orig_dacl, sid);

    // base = 原 DACL；有残留时先 revoke（防同会话叠加重复 ACE）
    let mut clean: *mut ACL = std::ptr::null_mut();
    let mut base_dacl: *const ACL = orig_dacl;
    if had_ours {
        let revoke = explicit_access(sid, REVOKE_ACCESS, 0, ACE_FLAGS(0));
        let rc = unsafe { SetEntriesInAclW(Some(&[revoke]), Some(orig_dacl), &mut clean) };
        if rc.0 != 0 {
            unsafe {
                let _ = LocalFree(Some(HLOCAL(desc.0.cast())));
            }
            return Err(io::Error::other(format!(
                "SetEntriesInAclW(revoke) rc={}",
                rc.0
            )));
        }
        base_dacl = clean;
    }

    // grant = base + 我方 SID 写放行（CONTAINER|OBJECT_INHERIT → 子树继承）
    let grant = explicit_access(
        sid,
        GRANT_ACCESS,
        WRITE_ALLOW_MASK,
        CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE,
    );
    let mut granted: *mut ACL = std::ptr::null_mut();
    let rc = unsafe {
        SetEntriesInAclW(
            Some(&[grant]),
            if base_dacl.is_null() {
                None
            } else {
                Some(base_dacl)
            },
            &mut granted,
        )
    };
    if rc.0 != 0 {
        unsafe {
            if !clean.is_null() {
                let _ = LocalFree(Some(HLOCAL(clean.cast())));
            }
            let _ = LocalFree(Some(HLOCAL(desc.0.cast())));
        }
        return Err(io::Error::other(format!(
            "SetEntriesInAclW(grant) rc={}",
            rc.0
        )));
    }

    let rc = unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(path_w.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(granted as *const ACL),
            None,
        )
    };
    unsafe {
        let _ = LocalFree(Some(HLOCAL(granted.cast())));
    }
    if rc.0 != 0 {
        unsafe {
            if !clean.is_null() {
                let _ = LocalFree(Some(HLOCAL(clean.cast())));
            }
            let _ = LocalFree(Some(HLOCAL(desc.0.cast())));
        }
        return Err(io::Error::other(format!(
            "SetNamedSecurityInfoW({}) rc={}",
            path.display(),
            rc.0
        )));
    }

    // clean 与 descriptor 移交 AclRestore（还原后释放）
    restores.push(AclRestore {
        path_w,
        clean_dacl: if had_ours { Some(clean) } else { None },
        orig_dacl,
        orig_descriptor: desc,
        restore_clean: had_ours,
        settled: false,
    });
    Ok(())
}

// ═══════════════════════════════════════════════════════
// 子进程环境（TEMP/缓存重定向）
// ═══════════════════════════════════════════════════════

fn append_env(block: &mut Vec<u16>, k: &std::ffi::OsStr, v: &std::ffi::OsStr) {
    block.extend(os_wide(k));
    block.push(b'=' as u16);
    block.extend(os_wide(v));
    block.push(0);
}

/// 当前环境原样（full 通道：无 TEMP 覆盖、无缓存重定向、不建受限目录）。
fn current_env_block() -> Vec<u16> {
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in std::env::vars_os() {
        append_env(&mut block, &k, &v);
    }
    block.push(0);
    block
}

/// 环境块：当前环境 + 覆盖（UTF-16 双空串结尾）。
fn build_env_block(workspace_root: &Path, overrides: &[(String, String)]) -> Vec<u16> {
    let temp = workspace_root.join(EXEC_TEMP_DIR);
    let cache = workspace_root.join(EXEC_CACHE_DIR);
    let _ = std::fs::create_dir_all(&temp);
    let _ = std::fs::create_dir_all(&cache);
    // 环境值用普通形式（剥 verbatim 前缀）：目录已按 canonical 形式建好，
    // 两种写法指向同一处；子进程解析 `\\?\` 路径不可靠。
    let temp_env = child_path(&temp).to_string_lossy().into_owned();

    let mut eff: Vec<(String, String)> =
        vec![("TEMP".into(), temp_env.clone()), ("TMP".into(), temp_env)];
    eff.extend(
        overrides
            .iter()
            .filter(|(k, _)| k != "TEMP" && k != "TMP")
            .cloned(),
    );

    let mut block: Vec<u16> = Vec::new();
    for (k, v) in std::env::vars_os() {
        let key = k.to_string_lossy();
        if eff.iter().any(|(ok, _)| ok == key.as_ref()) {
            continue;
        }
        append_env(&mut block, &k, &v);
    }
    for (k, v) in &eff {
        append_env(&mut block, std::ffi::OsStr::new(k), std::ffi::OsStr::new(v));
    }
    block.push(0);
    block
}

/// 缓存重定向（放行后调用：子目录创建即继承父目录写 ACE）。
fn cache_overrides(workspace_root: &Path) -> Vec<(String, String)> {
    let cache = workspace_root.join(EXEC_CACHE_DIR);
    let mut out = Vec::new();
    if tool_on_path("npm") {
        let p = child_path(&cache.join("npm"));
        let _ = std::fs::create_dir_all(&p);
        out.push(("npm_config_cache".into(), p.to_string_lossy().into_owned()));
    }
    if tool_on_path("pip") {
        let p = child_path(&cache.join("pip"));
        let _ = std::fs::create_dir_all(&p);
        out.push(("PIP_CACHE_DIR".into(), p.to_string_lossy().into_owned()));
    }
    if tool_on_path("cargo") {
        let p = child_path(&cache.join("cargo"));
        let _ = std::fs::create_dir_all(&p);
        out.push(("CARGO_HOME".into(), p.to_string_lossy().into_owned()));
    }
    out
}

// ═══════════════════════════════════════════════════════
// 受限令牌
// ═══════════════════════════════════════════════════════

/// 创建受限令牌（既有 v3 同构参数）。返回令牌句柄（调用方 CloseHandle）。
fn create_restricted_token(workspace_root: &Path) -> Result<HANDLE, String> {
    unsafe {
        let mut base: HANDLE = HANDLE(std::ptr::null_mut());
        OpenProcessToken(GetCurrentProcess(), token_access_rights(), &mut base)
            .map_err(|e| format!("OpenProcessToken: {e}"))?;

        let everyone = everyone_sid();
        let logon = match current_logon_sid(base) {
            Ok(s) => s,
            Err(e) => {
                let _ = CloseHandle(base);
                return Err(e);
            }
        };
        let root = writable_root_sid(workspace_root);
        let wrc = write_restricted_code_sid();

        let sids: Vec<Vec<u8>> = vec![everyone.clone(), logon.clone(), root.clone(), wrc];
        let mut attr: Vec<SID_AND_ATTRIBUTES> = Vec::with_capacity(sids.len());
        for s in &sids {
            attr.push(SID_AND_ATTRIBUTES {
                Sid: PSID(s.as_ptr() as *mut core::ffi::c_void),
                Attributes: 0,
            });
        }
        let mut restricted: HANDLE = HANDLE(std::ptr::null_mut());
        let flags = CREATE_RESTRICTED_TOKEN_FLAGS(
            DISABLE_MAX_PRIVILEGE.0 | LUA_TOKEN.0 | WRITE_RESTRICTED.0,
        );
        CreateRestrictedToken(base, flags, None, None, Some(&attr), &mut restricted)
            .map_err(|e| format!("CreateRestrictedToken: {e}"))?;
        let _ = CloseHandle(base);

        // TokenDefaultDacl：Everyone + logon + 写根 授 GENERIC_ALL（NO_INHERITANCE）
        let grants = [&everyone, &logon, &root];
        let mut entries: Vec<EXPLICIT_ACCESS_W> = Vec::with_capacity(grants.len());
        for g in &grants {
            entries.push(explicit_access(g, GRANT_ACCESS, GENERIC_ALL, ACE_FLAGS(0)));
        }
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let rc = SetEntriesInAclW(Some(&entries), None, &mut dacl);
        if rc.0 != 0 {
            let _ = CloseHandle(restricted);
            return Err(format!("TokenDefaultDacl SetEntriesInAclW rc={}", rc.0));
        }
        let info = TOKEN_DEFAULT_DACL { DefaultDacl: dacl };
        let applied = SetTokenInformation(
            restricted,
            TokenDefaultDacl,
            (&info as *const TOKEN_DEFAULT_DACL).cast(),
            std::mem::size_of::<TOKEN_DEFAULT_DACL>() as u32,
        );
        let _ = LocalFree(Some(HLOCAL(dacl.cast())));
        if applied.is_err() {
            let _ = CloseHandle(restricted);
            return Err("SetTokenInformation(TokenDefaultDacl) 失败".into());
        }

        // 特权：仅重开 SE_CHANGE_NOTIFY（恢复目录遍历/读文件能力）
        if let Err(e) = enable_change_notify(restricted) {
            let _ = CloseHandle(restricted);
            return Err(e);
        }
        Ok(restricted)
    }
}

fn enable_change_notify(token: HANDLE) -> Result<(), String> {
    unsafe {
        let name = wide("SeChangeNotifyPrivilege");
        let mut luid = windows::Win32::Foundation::LUID::default();
        LookupPrivilegeValueW(PCWSTR(std::ptr::null()), PCWSTR(name.as_ptr()), &mut luid)
            .map_err(|e| format!("LookupPrivilegeValueW: {e}"))?;
        let mut privs: TOKEN_PRIVILEGES = std::mem::zeroed();
        privs.PrivilegeCount = 1;
        privs.Privileges[0].Luid = luid;
        privs.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;
        AdjustTokenPrivileges(
            token,
            false,
            Some((&privs as *const TOKEN_PRIVILEGES).cast()),
            0,
            None,
            None,
        )
        .map_err(|e| format!("AdjustTokenPrivileges: {e}"))
    }
}

/// 当前进程 logon SID（TokenGroups 中 SE_GROUP_LOGON_ID 且已启用者）。
fn current_logon_sid(token: HANDLE) -> Result<Vec<u8>, String> {
    unsafe {
        let mut needed: u32 = 0;
        let _ = GetTokenInformation(token, TokenGroups, None, 0, &mut needed);
        let mut buf = vec![0u8; needed as usize];
        GetTokenInformation(
            token,
            TokenGroups,
            Some(buf.as_mut_ptr().cast()),
            needed,
            &mut needed,
        )
        .map_err(|e| format!("GetTokenInformation(TokenGroups): {e}"))?;
        find_logon_sid_in_groups(&buf)
    }
}

/// 从 TOKEN_GROUPS 原始缓冲中找已启用的 logon SID。
///
/// 关键点：`Groups` 的偏移是 `offset_of!(TOKEN_GROUPS, Groups)`（Windows
/// 布局为 8），不是 `size_of::<TOKEN_GROUPS>()`（24，已包含 Groups[0]）。
/// 旧写法会跳过第一项并在末尾越界读取。
fn find_logon_sid_in_groups(buf: &[u8]) -> Result<Vec<u8>, String> {
    let count = u32::from_ne_bytes(
        buf.get(0..4)
            .ok_or_else(|| "TokenGroups 缓冲不足 4 字节".to_string())?
            .try_into()
            .map_err(|_| "TokenGroups GroupCount 读取失败".to_string())?,
    ) as usize;
    let base = std::mem::offset_of!(TOKEN_GROUPS, Groups);
    let stride = std::mem::size_of::<SID_AND_ATTRIBUTES>();
    let end = count
        .checked_mul(stride)
        .and_then(|groups_len| base.checked_add(groups_len))
        .ok_or_else(|| "TokenGroups 尺寸溢出".to_string())?;
    if buf.len() < end {
        return Err(format!(
            "TokenGroups 缓冲不足：需要 {end} 字节，实际 {} 字节",
            buf.len()
        ));
    }

    unsafe {
        for i in 0..count {
            let offset = base + i * stride;
            let entry =
                std::ptr::read_unaligned(buf.as_ptr().add(offset).cast::<SID_AND_ATTRIBUTES>());
            let attrs = entry.Attributes;
            if attrs & SE_GROUP_LOGON_ID == SE_GROUP_LOGON_ID && attrs & SE_GROUP_ENABLED != 0 {
                let sid_ptr = entry.Sid;
                if sid_ptr.0.is_null() {
                    continue;
                }
                let len = GetLengthSid(sid_ptr) as usize;
                if len == 0 {
                    continue;
                }
                return Ok(std::slice::from_raw_parts(sid_ptr.0 as *const u8, len).to_vec());
            }
        }
    }
    Err("TokenGroups 中未找到已启用 logon SID".into())
}

// ═══════════════════════════════════════════════════════
// Job / 管道 / 进程启动
// ═══════════════════════════════════════════════════════

fn create_job() -> Result<HANDLE, String> {
    unsafe {
        let job = CreateJobObjectW(None, None).map_err(|e| format!("CreateJobObjectW: {e}"))?;
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&mut info as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .map_err(|e| {
            let _ = CloseHandle(job);
            format!("SetInformationJobObject: {e}")
        })?;
        Ok(job)
    }
}

fn make_pipe(sa: &SECURITY_ATTRIBUTES) -> Result<(HANDLE, HANDLE), String> {
    unsafe {
        let mut read: HANDLE = HANDLE(std::ptr::null_mut());
        let mut write: HANDLE = HANDLE(std::ptr::null_mut());
        CreatePipe(
            &mut read,
            &mut write,
            Some(sa as *const SECURITY_ATTRIBUTES),
            0,
        )
        .map_err(|e| format!("CreatePipe: {e}"))?;
        Ok((read, write))
    }
}

/// CommandLineToArgvW 兼容引号（std::process 同算法）。
fn quote_arg(arg: &str) -> String {
    if !arg.is_empty()
        && !arg
            .chars()
            .any(|c| c == ' ' || c == '\t' || c == '\n' || c == '"')
    {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        if ch == '\\' {
            backslashes += 1;
            continue;
        }
        if ch == '"' {
            out.push_str(&"\\".repeat(backslashes * 2 + 1));
            out.push(ch);
        } else {
            out.push_str(&"\\".repeat(backslashes));
            out.push(ch);
        }
        backslashes = 0;
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

/// 受限进程启动结果（管道读端由调用方转 File）。
struct Launched {
    process: HANDLE,
    job: HANDLE,
    stdout_read: HANDLE,
    stderr_read: HANDLE,
}

/// 实际启动（调用前必须已持有 ACL 放行 / 令牌已创建）。
fn launch_restricted(
    token: HANDLE,
    exe: &Path,
    args: &[String],
    cwd: &Path,
    env_block: &[u16],
) -> Result<Launched, String> {
    unsafe {
        let mut cmdline = quote_arg(&exe.to_string_lossy());
        for a in args {
            cmdline.push(' ');
            cmdline.push_str(&quote_arg(a));
        }
        let mut cmdline_w: Vec<u16> = cmdline.encode_utf16().collect();
        cmdline_w.push(0);

        let cwd_w = wide(&child_path(cwd).to_string_lossy());
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: windows::core::BOOL(1),
        };
        let (stdin_read, stdin_write) = make_pipe(&sa)?;
        let (out_read, out_write) = make_pipe(&sa)?;
        let (err_read, err_write) = make_pipe(&sa)?;

        let job = create_job()?;

        let mut attr_list_size: usize = 0;
        let _ = InitializeProcThreadAttributeList(None, 2, None, &mut attr_list_size);
        let mut attr_buf = vec![0usize; attr_list_size.div_ceil(8) + 1];
        let attr_list = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr().cast());
        if let Err(e) =
            InitializeProcThreadAttributeList(Some(attr_list), 2, None, &mut attr_list_size)
        {
            close_handles(&[
                stdin_read,
                stdin_write,
                out_read,
                out_write,
                err_read,
                err_write,
                job,
            ]);
            return Err(format!("InitializeProcThreadAttributeList: {e}"));
        }

        // stdio 三句柄（stdin 空管读端 = EOF；stdout/stderr 写端）
        let handles = [stdin_read, out_write, err_write];
        if let Err(e) = UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            Some(handles.as_ptr().cast()),
            handles.len() * std::mem::size_of::<HANDLE>(),
            None,
            None,
        ) {
            DeleteProcThreadAttributeList(attr_list);
            close_handles(&[
                stdin_read,
                stdin_write,
                out_read,
                out_write,
                err_read,
                err_write,
                job,
            ]);
            return Err(format!("UpdateProcThreadAttribute(HANDLE_LIST): {e}"));
        }
        if let Err(e) = UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
            Some((&job as *const HANDLE).cast()),
            std::mem::size_of::<HANDLE>(),
            None,
            None,
        ) {
            DeleteProcThreadAttributeList(attr_list);
            close_handles(&[
                stdin_read,
                stdin_write,
                out_read,
                out_write,
                err_read,
                err_write,
                job,
            ]);
            return Err(format!("UpdateProcThreadAttribute(JOB_LIST): {e}"));
        }

        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = stdin_read;
        si.StartupInfo.hStdOutput = out_write;
        si.StartupInfo.hStdError = err_write;
        si.lpAttributeList = attr_list;

        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let created = CreateProcessAsUserW(
            Some(token),
            PCWSTR(std::ptr::null()), // lpApplicationName = NULL → 命令行首 token + PATH
            Some(PWSTR(cmdline_w.as_mut_ptr())),
            None,
            None,
            true, // bInheritHandles（配合 HANDLE_LIST 仅继承列表内句柄）
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            Some(env_block.as_ptr().cast()),
            PCWSTR(cwd_w.as_ptr()),
            &si.StartupInfo,
            &mut pi,
        );

        DeleteProcThreadAttributeList(attr_list);
        // 父进程关闭写端与 stdin 空管（子进程独占 → EOF 语义正确）
        close_handles(&[stdin_read, stdin_write, out_write, err_write]);

        match created {
            Ok(()) => {
                let _ = CloseHandle(pi.hThread);
                Ok(Launched {
                    process: pi.hProcess,
                    job,
                    stdout_read: out_read,
                    stderr_read: err_read,
                })
            }
            Err(e) => {
                close_handles(&[job, out_read, err_read]);
                Err(format!("CreateProcessAsUserW: {e}"))
            }
        }
    }
}

unsafe fn close_handles(handles: &[HANDLE]) {
    for h in handles {
        if !h.0.is_null() {
            let _ = CloseHandle(*h);
        }
    }
}

// ═══════════════════════════════════════════════════════
// RestrictedChild —— 受限子进程句柄封装
// ═══════════════════════════════════════════════════════

pub(crate) struct RestrictedChild {
    process: HANDLE,
    job: Option<HANDLE>,
    stdout_file: Option<File>,
    stderr_file: Option<File>,
    /// ACL 放行租约（仅受限路径持有；full 恒 None）：Drop 归还引用，末位归零时还原
    _grant: Option<GrantSession>,
}

impl RestrictedChild {
    pub(crate) fn stdout_file(&mut self) -> Option<File> {
        self.stdout_file.take()
    }

    pub(crate) fn stderr_file(&mut self) -> Option<File> {
        self.stderr_file.take()
    }

    /// 非阻塞轮询退出（WaitForSingleObject(0) + GetExitCodeProcess）。
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<i32>> {
        unsafe {
            let wr = WaitForSingleObject(self.process, 0);
            if wr == windows::Win32::Foundation::WAIT_OBJECT_0 {
                let mut code: u32 = 0;
                GetExitCodeProcess(self.process, &mut code)
                    .map_err(|e| io::Error::other(format!("GetExitCodeProcess: {e}")))?;
                if code == STILL_ACTIVE {
                    return Ok(None);
                }
                return Ok(Some(code as i32));
            }
            if wr == windows::Win32::Foundation::WAIT_TIMEOUT {
                return Ok(None);
            }
            Err(io::Error::other("WaitForSingleObject 失败"))
        }
    }

    /// 进程树回收：TerminateJobObject + 收敛等待（≤5s）；Drop 关 Job
    /// 句柄 → kill-on-close 兜底残余。
    pub(crate) fn kill_tree(&mut self) {
        unsafe {
            if let Some(job) = self.job {
                let _ = TerminateJobObject(job, 1);
                let deadline = std::time::Instant::now() + JOB_CONVERGE_GRACE;
                loop {
                    let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = std::mem::zeroed();
                    let ok = QueryInformationJobObject(
                        Some(job),
                        JobObjectBasicAccountingInformation,
                        (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                        std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                        None,
                    );
                    if ok.is_ok() && info.ActiveProcesses == 0 {
                        break;
                    }
                    if std::time::Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            let _ = WaitForSingleObject(self.process, JOB_CONVERGE_GRACE.as_millis() as u32);
            if !self.process.0.is_null() {
                let _ = CloseHandle(self.process);
                self.process = HANDLE(std::ptr::null_mut());
            }
            if let Some(job) = self.job.take() {
                let _ = CloseHandle(job);
            }
        }
    }
}

impl Drop for RestrictedChild {
    fn drop(&mut self) {
        unsafe {
            if !self.process.0.is_null() {
                let _ = CloseHandle(self.process);
                self.process = HANDLE(std::ptr::null_mut());
            }
            if let Some(job) = self.job.take() {
                // 关 Job = kill-on-close：正常退出后子树亦不泄漏
                let _ = CloseHandle(job);
            }
        }
        // _grant Drop：归还引用；末位（归零）时执行 ACL 还原
    }
}

// ═══════════════════════════════════════════════════════
// spawn_restricted —— 对外入口
// ═══════════════════════════════════════════════════════

/// 受限启动（runner → spawn::spawn_command，Windows + 受限开启时调用）。
pub(crate) fn spawn_restricted(
    program: &str,
    args: &[String],
    cwd: &Path,
    workspace_root: &Path,
) -> io::Result<RestrictedChild> {
    spawn_exec(program, args, cwd, workspace_root, false)
}

/// 完整权限启动（permission=full）：应用自身主令牌；Job/回收/句柄能力保留。
pub(crate) fn spawn_full(
    program: &str,
    args: &[String],
    cwd: &Path,
    workspace_root: &Path,
) -> io::Result<RestrictedChild> {
    spawn_exec(program, args, cwd, workspace_root, true)
}

/// 通用启动：`full=false` 受限身份（restricted token + ACL + 重定向 + 探针）；
/// `full=true` 应用自身令牌（无上述隔离面，仅保留 Job 回收/句柄自持）。
fn spawn_exec(
    program: &str,
    args: &[String],
    cwd: &Path,
    workspace_root: &Path,
    full: bool,
) -> io::Result<RestrictedChild> {
    // 1) exe 解析
    let exe = resolve_exe_in_path(program).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{program} 未在 PATH 中找到"),
        )
    })?;

    // 2) 环境与放行准备：受限 = TEMP/缓存父目录预建 + ACL 放行 + 重定向；
    //    full = 原样当前环境（无覆盖、不建受限目录）
    let grant = if full {
        None
    } else {
        let temp = workspace_root.join(EXEC_TEMP_DIR);
        let cache = workspace_root.join(EXEC_CACHE_DIR);
        std::fs::create_dir_all(&temp).map_err(io::Error::other)?;
        std::fs::create_dir_all(&cache).map_err(io::Error::other)?;
        Some(GrantSession::acquire(workspace_root)?)
    };
    let env_block = if full {
        current_env_block()
    } else {
        let cache_vars = cache_overrides(workspace_root);
        build_env_block(workspace_root, &cache_vars)
    };

    // 3) PS 探针（仅受限路径：验证 %TEMP% 可写 + 无 CLM；full 无此问题）
    if !full && is_powershell_like(&exe) && !probe_ok(&exe, workspace_root, &env_block) {
        return Err(io::Error::other(format!(
            "受限 {program} 探针失败（%TEMP% 写入或 PowerShell CLM 探测未通过）；\
             如确需执行请经完整权限通道（M4 豁免）重试"
        )));
    }

    // 4) 令牌 + 启动（失败路径：grant Drop 归还引用，末位时还原 ACL）
    let token = if full {
        open_own_token().map_err(|e| io::Error::other(format!("自身令牌获取失败：{e}")))?
    } else {
        create_restricted_token(workspace_root)
            .map_err(|e| io::Error::other(format!("受限令牌创建失败：{e}")))?
    };
    let launched = match launch_restricted(token, &exe, args, cwd, &env_block) {
        Ok(l) => l,
        Err(e) => {
            unsafe {
                let _ = CloseHandle(token);
            }
            return Err(io::Error::other(format!("进程启动失败：{e}")));
        }
    };
    unsafe {
        let _ = CloseHandle(token);
    }

    let stdout_file = unsafe { File::from_raw_handle(launched.stdout_read.0) };
    let stderr_file = unsafe { File::from_raw_handle(launched.stderr_read.0) };

    Ok(RestrictedChild {
        process: launched.process,
        job: Some(launched.job),
        stdout_file: Some(stdout_file),
        stderr_file: Some(stderr_file),
        _grant: grant,
    })
}

/// 应用自身主令牌（full 通道身份；CreateProcessAsUserW 用同用户主令牌 = 全令牌语义）。
fn open_own_token() -> Result<HANDLE, String> {
    unsafe {
        let mut token: HANDLE = HANDLE(std::ptr::null_mut());
        OpenProcessToken(GetCurrentProcess(), token_access_rights(), &mut token)
            .map_err(|e| format!("OpenProcessToken: {e}"))?;
        Ok(token)
    }
}

/// PS 探针（幂等缓存）：受限启动 `[Console]::Out.Write('ok')`。
/// 探针在调用方 GrantSession 放行区内运行（不重复 acquire）。
fn probe_ok(exe: &Path, workspace_root: &Path, env_block: &[u16]) -> bool {
    let key = exe.to_string_lossy().to_lowercase();
    if let Some(cached) = probe_cache().lock().unwrap().get(&key).copied() {
        return cached;
    }
    // 串行化真实探针（防并发首用同时跑多次 15s 窗口探针）
    let _g = probe_lock().lock().unwrap();
    if let Some(cached) = probe_cache().lock().unwrap().get(&key).copied() {
        return cached;
    }
    let ok = run_probe(exe, workspace_root, env_block);
    probe_cache().lock().unwrap().insert(key, ok);
    if !ok {
        eprintln!("[spawn_win32] PowerShell 受限探针失败：{}", exe.display());
    }
    ok
}

fn run_probe(exe: &Path, workspace_root: &Path, env_block: &[u16]) -> bool {
    let token = match create_restricted_token(workspace_root) {
        Ok(t) => t,
        Err(_) => return false,
    };
    let probe_args = [
        "-NoProfile".to_string(),
        "-NonInteractive".to_string(),
        "-Command".to_string(),
        "[Console]::Out.Write('ok')".to_string(),
    ];
    let launched = match launch_restricted(token, exe, &probe_args, workspace_root, env_block) {
        Ok(l) => l,
        Err(_) => {
            unsafe {
                let _ = CloseHandle(token);
            }
            return false;
        }
    };
    unsafe {
        let _ = CloseHandle(token);
    }
    let mut child = RestrictedChild {
        process: launched.process,
        job: Some(launched.job),
        stdout_file: Some(unsafe { File::from_raw_handle(launched.stdout_read.0) }),
        stderr_file: Some(unsafe { File::from_raw_handle(launched.stderr_read.0) }),
        _grant: None,
    };

    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut code: Option<i32> = None;
    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    loop {
        if let Some(c) = child.try_wait().unwrap_or(None) {
            code = Some(c);
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if let Some(mut f) = child.stdout_file.take() {
        let _ = f.read_to_string(&mut stdout);
    }
    if let Some(mut f) = child.stderr_file.take() {
        let _ = f.read_to_string(&mut stderr);
    }
    // 失败即收敛（不泄漏探针进程）
    child.kill_tree();

    let clm = stderr.contains("ConstrainedLanguage")
        || stderr.contains("MethodInvocationNotSupportedInConstrainedLanguage")
        || stdout.contains("MethodInvocationNotSupportedInConstrainedLanguage");
    code == Some(0) && stdout.contains("ok") && !clm
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_ws(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("prts-m4-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 与 TOKEN_GROUPS 头部同布局的测试缓冲（u32 + 两个 SID_AND_ATTRIBUTES）。
    #[repr(C)]
    struct TestTokenGroups {
        group_count: u32,
        groups: [SID_AND_ATTRIBUTES; 2],
    }

    fn groups_bytes(groups: &TestTokenGroups) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(
                (groups as *const TestTokenGroups).cast::<u8>(),
                std::mem::size_of::<TestTokenGroups>(),
            )
        }
    }

    #[test]
    fn logon_sid_scan_reads_first_group_and_rejects_short_buffer() {
        // 假 SID 只需满足 GetLengthSid：revision=1、1 个子权威、NT authority。
        let mut sid = [1u8, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];
        let sid_ptr = PSID(sid.as_mut_ptr().cast());
        let groups = TestTokenGroups {
            group_count: 2,
            groups: [
                SID_AND_ATTRIBUTES {
                    Sid: sid_ptr,
                    Attributes: SE_GROUP_LOGON_ID | SE_GROUP_ENABLED,
                },
                SID_AND_ATTRIBUTES {
                    Sid: sid_ptr,
                    Attributes: 0,
                },
            ],
        };

        // 旧实现的 base=size_of::<TOKEN_GROUPS>() 会跳过 Groups[0]，此断言会失败。
        assert_eq!(
            find_logon_sid_in_groups(groups_bytes(&groups)).unwrap(),
            sid
        );

        let short = TestTokenGroups {
            group_count: 3,
            groups: groups.groups,
        };
        assert!(find_logon_sid_in_groups(groups_bytes(&short)).is_err());
    }

    /// 同步执行一次受限命令并收集输出（测试用便捷封装）。
    fn run_ps(ws: &Path, script: &str, timeout: Duration) -> (Option<i32>, String, String) {
        run_ps_identity(ws, script, timeout, false)
    }

    /// 同步执行一次 full（完整权限）命令并收集输出。
    fn run_full_ps(ws: &Path, script: &str, timeout: Duration) -> (Option<i32>, String, String) {
        run_ps_identity(ws, script, timeout, true)
    }

    fn run_ps_identity(
        ws: &Path,
        script: &str,
        timeout: Duration,
        full: bool,
    ) -> (Option<i32>, String, String) {
        let args = [
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            script.to_string(),
        ];
        let mut child = if full {
            spawn_full("powershell", &args, ws, ws).expect("full 启动应成功")
        } else {
            spawn_restricted("powershell", &args, ws, ws).expect("受限启动应成功")
        };
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(code) = child.try_wait().unwrap_or(None) {
                let mut out = String::new();
                if let Some(mut f) = child.stdout_file.take() {
                    let _ = f.read_to_string(&mut out);
                }
                let mut err = String::new();
                if let Some(mut f) = child.stderr_file.take() {
                    let _ = f.read_to_string(&mut err);
                }
                return (Some(code), out, err);
            }
            if std::time::Instant::now() >= deadline {
                child.kill_tree();
                return (None, String::new(), String::new());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn full_can_write_outside_workspace_where_restricted_cannot() {
        let ws = unique_ws("full");
        let outside = unique_ws("full-target");
        let target = outside.join("x.txt");
        // full（应用自身令牌）：区外写入成功
        let script = format!(
            "Set-Content -Path '{}' -Value 'full-write-ok'",
            target.display()
        );
        let (code, _, err) = run_full_ps(&ws, &script, Duration::from_secs(30));
        assert_eq!(code, Some(0), "full 应能写区外 err={err}");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap().trim(),
            "full-write-ok",
            "区外文件内容应落盘（自身令牌产物可由测试进程读）"
        );
        // 对照：受限身份区外写被拒（既有语义）
        let target2 = outside.join("y.txt");
        let script2 = format!("Set-Content -Path '{}' -Value 'x'", target2.display());
        let (code2, _, _) = run_ps(&ws, &script2, Duration::from_secs(30));
        assert_ne!(code2, Some(0), "受限身份区外写应仍被拒");
        assert!(!target2.exists());
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn restricted_echo_and_reads_work() {
        let ws = unique_ws("echo");
        let (code, out, _) = run_ps(
            &ws,
            "Write-Output hello-m4; whoami",
            Duration::from_secs(30),
        );
        assert_eq!(code, Some(0), "受限 PS 应正常退出");
        assert!(out.contains("hello-m4"), "out: {out}");
        assert!(
            out.to_lowercase().contains("\\"),
            "whoami 应输出当前用户: {out}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn workspace_deep_write_allowed_and_existing_file_append() {
        let ws = unique_ws("deep");
        // 预置既有深层目录与文件（放行前已存在 → 验证继承传播到既有对象）
        let deep = ws.join("sub").join("deep");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("old.txt"), "old").unwrap();
        let script = format!(
            "Set-Content -Path '{}' -Value 'new-line'; Add-Content -Path '{}' -Value 'appended'",
            deep.join("new.txt").display(),
            deep.join("old.txt").display()
        );
        let (code, _, err) = run_ps(&ws, &script, Duration::from_secs(30));
        assert_eq!(code, Some(0), "区内深层写应成功 err={err}");
        // 回读验证走受限子进程（Agent 沙箱内测试进程令牌受限，读子进程
        // 新建文件可能被拒；子进程回读与本环境解耦）
        let read_script = format!(
            "Write-Output ('NEW:' + (Get-Content -Raw -Path '{}').Trim()); \
             Write-Output ('OLD:' + (Get-Content -Raw -Path '{}').Trim())",
            deep.join("new.txt").display(),
            deep.join("old.txt").display()
        );
        let (rcode, rout, rerr) = run_ps(&ws, &read_script, Duration::from_secs(30));
        assert_eq!(rcode, Some(0), "回读应成功 err={rerr}");
        assert!(rout.contains("NEW:new-line"), "new 文件内容: {rout}");
        assert!(rout.contains("OLD:oldappended"), "old 追加内容: {rout}");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn outside_workspace_write_denied() {
        let ws = unique_ws("outside");
        // 工作区外的可写目录（测试进程创建；受限 token 无写 ACE → 应被拒）
        let outside = unique_ws("outside-target");
        let target = outside.join("x.txt");
        let script = format!("Set-Content -Path '{}' -Value 'x'", target.display());
        let (code, _, err) = run_ps(&ws, &script, Duration::from_secs(30));
        assert_ne!(code, Some(0), "区外写应被拒（code={code:?} err={err}）");
        assert!(!target.exists(), "区外目标文件不应被创建");
        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn temp_redirect_and_complex_ps_no_clm() {
        let ws = unique_ws("clm");
        // .NET 调用 + 复杂表达式：若 PS 回落 CLM 会 MethodInvocation 报错
        let out_file = ws.join("net-out.txt");
        let script = format!(
            "$env:TEMP | Out-File -FilePath '{}' -Encoding utf8; \
             [System.IO.File]::WriteAllText('{}', ('ab' * 1000))",
            ws.join("temp-value.txt").display(),
            out_file.display()
        );
        let (code, _, err) = run_ps(&ws, &script, Duration::from_secs(30));
        assert_eq!(code, Some(0), "受限复杂 PS 应成功 err={err}");
        assert!(!err.contains("MethodInvocationNotSupportedInConstrainedLanguage"));
        // 子进程回读：文件长度 2000 + TEMP 重定向到 workspace 内
        let read_script = format!(
            "Write-Output ('LEN:' + ([System.IO.File]::ReadAllText('{}').Length)); \
             Write-Output ('TEMP:' + (Get-Content -Raw -Path '{}').Trim())",
            out_file.display(),
            ws.join("temp-value.txt").display()
        );
        let (rcode, rout, rerr) = run_ps(&ws, &read_script, Duration::from_secs(30));
        assert_eq!(rcode, Some(0), "回读应成功 err={rerr}");
        assert!(
            rout.contains("LEN:2000"),
            "WriteAllText 应写入（无 CLM）: {rout}"
        );
        assert!(
            rout.lines()
                .find(|l| l.starts_with("TEMP:"))
                .map(|l| l.ends_with(EXEC_TEMP_DIR))
                .unwrap_or(false),
            "TEMP 应重定向到 workspace 内: {rout}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn child_path_strips_verbatim_prefix() {
        assert_eq!(
            child_path(Path::new("\\\\?\\C:\\ws\\x")),
            PathBuf::from("C:\\ws\\x"),
            "盘符 verbatim 应剥为普通形式"
        );
        assert_eq!(
            child_path(Path::new("\\\\?\\UNC\\srv\\share\\x")),
            PathBuf::from("\\\\srv\\share\\x"),
            "UNC verbatim 应还原为普通 UNC"
        );
        assert_eq!(child_path(Path::new("C:\\ws")), PathBuf::from("C:\\ws"));
        assert_eq!(child_path(Path::new("rel\\x")), PathBuf::from("rel\\x"));
    }

    /// 回归：cwd 为 canonical（verbatim `\\?\`）形式时相对路径 cmdlet 仍应工作。
    /// 此前 cwd 原样带前缀交给子进程，PowerShell 5.1 相对解析失效。
    #[test]
    fn verbatim_cwd_relative_paths_work() {
        let ws = unique_ws("verbatim");
        let ws_canon = ws.canonicalize().expect("canonicalize");
        assert!(
            ws_canon.to_string_lossy().starts_with("\\\\?\\"),
            "canonical 形式应带 verbatim 前缀（否则本用例失去意义）: {ws_canon:?}"
        );
        let script = "Set-Content -Path rel.txt -Value hi -ErrorAction Stop; \
                      Write-Output ('REL:' + (Get-Content -Raw -Path rel.txt).Trim())";
        let (code, out, err) = run_ps(&ws_canon, script, Duration::from_secs(30));
        assert_eq!(code, Some(0), "verbatim cwd 下相对路径应可用 err={err}");
        assert!(out.contains("REL:hi"), "out: {out}");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn job_kill_converges_grandchild_tree() {
        let ws = unique_ws("job");
        // 受限子进程再派生子进程（孙进程 sleep 60s），pid 打到 stdout；
        // kill_tree 后孙进程必须随 Job 终止（kill 后读管道 EOF，再核对 OpenProcess）。
        let script = "$p = Start-Process powershell -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 60' -PassThru; \
                      Write-Output $p.Id; Start-Sleep -Seconds 60";
        let args = [
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-Command".to_string(),
            script.to_string(),
        ];
        let mut child = spawn_restricted("powershell", &args, &ws, &ws).expect("启动");
        // 等主受限进程完成启动并保持运行（受限 PS 冷启动在全量并行回归中
        // 可能超过 1.5s——给足窗口再 kill，避免脚本未及输出 pid）
        if let Some(code) = child.try_wait().unwrap_or(None) {
            panic!("主受限进程提前退出 code={code}（脚本异常）");
        }
        // 额外等待脚本启动孙进程并输出 pid
        std::thread::sleep(Duration::from_millis(6000));
        child.kill_tree();
        // kill 后读管道（EOF）取孙进程 pid
        let mut out = String::new();
        if let Some(mut f) = child.stdout_file.take() {
            let _ = f.read_to_string(&mut out);
        }
        let mut err = String::new();
        if let Some(mut f) = child.stderr_file.take() {
            let _ = f.read_to_string(&mut err);
        }
        let gp: u32 = out.trim().parse().unwrap_or(0);
        assert!(
            gp != 0,
            "孙进程 pid 未从 stdout 捕获 out={out:?} err={err:?}"
        );
        // 孙进程应已随 Job 终止（OpenProcess 应失败）
        let mut gone = false;
        for _ in 0..100 {
            let h = unsafe {
                windows::Win32::System::Threading::OpenProcess(
                    windows::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION,
                    false,
                    gp,
                )
            };
            match h {
                Ok(hh) => {
                    unsafe {
                        let _ = CloseHandle(hh);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => {
                    gone = true;
                    break;
                }
            }
        }
        assert!(gone, "孙进程应已被 Job 终止（pid={gp}）");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn acl_restored_after_child_drop() {
        let ws = unique_ws("acl-restore");
        let before = dacl_raw(&ws);
        {
            let _child = spawn_restricted(
                "powershell",
                &[
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    "Write-Output hi".to_string(),
                ],
                &ws,
                &ws,
            )
            .expect("启动");
            // child drop → ACL 还原
        }
        let after = dacl_raw(&ws);
        assert_eq!(before, after, "运行前后 workspace DACL 应一致（还原）");
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// 放行租约语义（回归：曾因 AclRestore::Drop 无条件还原，先结束的会话会把
    /// 仍在运行会话的写放行收走——非末位释放必须只递减计数，不得触碰 ACL）。
    #[test]
    fn grant_lease_survives_until_last_release() {
        let ws = unique_ws("grant-lease");
        let before = dacl_raw(&ws);

        let g1 = GrantSession::acquire(&ws).expect("首个会话放行");
        let after_first = dacl_raw(&ws);
        assert_ne!(before, after_first, "首个会话应施加放行（DACL 应变化）");

        let g2 = GrantSession::acquire(&ws).expect("第二个会话共享放行");
        drop(g1);
        let after_early_release = dacl_raw(&ws);
        assert_ne!(
            before, after_early_release,
            "非末位释放不得还原 ACL（仍在运行的会话需要放行）"
        );

        drop(g2);
        let after_last_release = dacl_raw(&ws);
        assert_eq!(
            before, after_last_release,
            "末位释放后 DACL 应还原到初始状态"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    /// 读取目录 DACL 原始字节（AclSize 段；测试比对用）。
    fn dacl_raw(path: &Path) -> Vec<u8> {
        let path_w = wide(&path.to_string_lossy());
        let (dacl, desc) = read_dacl(&path_w).expect("read dacl");
        let out = if dacl.is_null() {
            Vec::new()
        } else {
            unsafe {
                let size = (*dacl).AclSize as usize;
                std::slice::from_raw_parts(dacl as *const u8, size).to_vec()
            }
        };
        unsafe {
            let _ = LocalFree(Some(HLOCAL(desc.0.cast())));
        }
        out
    }

    #[test]
    fn cache_env_vars_match_tool_presence() {
        let ws = unique_ws("cache-env");
        let _grant = GrantSession::acquire(&ws).expect("grant");
        let vars = cache_overrides(&ws);
        let has_npm = tool_on_path("npm");
        let has_pip = tool_on_path("pip");
        let has_cargo = tool_on_path("cargo");
        assert_eq!(vars.iter().any(|(k, _)| k == "npm_config_cache"), has_npm);
        assert_eq!(vars.iter().any(|(k, _)| k == "PIP_CACHE_DIR"), has_pip);
        assert_eq!(vars.iter().any(|(k, _)| k == "CARGO_HOME"), has_cargo);
        // TEMP/TMP 恒有
        let env = build_env_block(&ws, &vars);
        let text = String::from_utf16_lossy(&env);
        assert!(text.contains("TEMP="));
        assert!(text.contains("TMP="));
        let _ = std::fs::remove_dir_all(&ws);
    }
}
