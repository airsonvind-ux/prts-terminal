//! gate 模块测试：M3-A 设计 §11 用例清单全量程序化落地
//!
//! 覆盖：词法子集 G 扫描器 / 删除门 / 程序分类与目标提取 /
//! 路径规范化与地域比较 / evaluate 集成（优先级链）/ reason 码断言。

use super::*;
use crate::exec::risk;
use std::path::PathBuf;

// ── 夹具 ──

fn unique_temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "prts-gate-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 工作区夹具：ws/ 含 a.txt、script.py、sub/、tools/my.exe、rel.txt。
fn ws_fixture(tag: &str) -> PathBuf {
    let ws = unique_temp_dir(tag);
    std::fs::write(ws.join("a.txt"), "a").unwrap();
    std::fs::write(ws.join("script.py"), "print(1)").unwrap();
    std::fs::write(ws.join("rel.txt"), "r").unwrap();
    std::fs::create_dir_all(ws.join("sub")).unwrap();
    std::fs::write(ws.join("sub").join("x.txt"), "x").unwrap();
    std::fs::create_dir_all(ws.join("tools")).unwrap();
    std::fs::write(ws.join("tools").join("my.exe"), "x").unwrap();
    ws
}

/// 区外目录夹具（真实存在于工作区外）。
fn outside_fixture(tag: &str) -> PathBuf {
    let d = unique_temp_dir(tag);
    std::fs::write(d.join("a.txt"), "outside").unwrap();
    d
}

fn eval(command: &str, cwd: Option<&PathBuf>, workspace: &Path) -> GateVerdict {
    let cwd = cwd.cloned().unwrap_or_else(|| workspace.to_path_buf());
    evaluate(command, &cwd, workspace)
}

/// 断言 Auto + reason。
fn assert_auto(v: &GateVerdict, reason: &str) {
    assert_eq!(v.decision, GateDecision::Auto, "{}: {:?}", reason, v);
    assert_eq!(v.reason, reason, "reason 码不符: {:?}", v);
}

/// 断言 Ask + reason。
fn assert_ask(v: &GateVerdict, reason: &str) {
    assert_eq!(v.decision, GateDecision::Ask, "{}: {:?}", reason, v);
    assert_eq!(v.reason, reason, "reason 码不符: {:?}", v);
}

/// 断言 Blocked（crit rule id）。
fn assert_blocked(v: &GateVerdict, rule: &str) {
    assert_eq!(v.decision, GateDecision::Blocked, "{rule}: {:?}", v);
    assert_eq!(v.reason, rule);
    assert_eq!(v.rule_id, Some(rule), "crit 的 rule_id 应携带原真规则 id");
}

fn scan(command: &str) -> ScanResult {
    scan_tokens(command)
}

// ═══════════════════════════════════════════════════════
// 词法子集 G：AUTO 语法面（§11）
// ═══════════════════════════════════════════════════════

#[test]
fn scan_auto_syntax_forms() {
    for cmd in [
        "echo hi",
        "Get-Content a.txt",
        "python script.py",
        "copy a.txt b.txt",
        "echo hi > a.txt",
        r#"python "my script.py""#,
        r"Get-Content '.\a.txt'",
        r"Get-Content .\a.txt",
        "cmd /c echo hi",
        r#"python -c "print(1)""#,
    ] {
        assert!(scan(cmd).ok, "应 ∈ 子集 G: {cmd}");
    }
}

#[test]
fn scan_redirect_extraction() {
    let s = scan("echo hi > a.txt");
    assert!(s.ok);
    assert_eq!(s.tokens.len(), 2);
    assert_eq!(s.redirects.len(), 1);
    assert_eq!(s.redirects[0].fd, None);
    assert_eq!(s.redirects[0].op, RedirectOp::Out);
    assert_eq!(s.redirects[0].target.as_deref(), Some("a.txt"));
}

#[test]
fn scan_redirect_forms() {
    // >> 追加
    let s = scan("echo hi >> log.txt");
    assert!(s.ok);
    assert_eq!(s.redirects[0].op, RedirectOp::Append);
    // < 输入
    let s = scan("type < in.txt");
    assert!(s.ok);
    assert_eq!(s.redirects[0].op, RedirectOp::In);
    assert_eq!(s.redirects[0].target.as_deref(), Some("in.txt"));
    // 2> 与 2>&1 合并（无目标 OK）
    let s = scan("type a.txt 2> err.txt");
    assert!(s.ok);
    assert_eq!(s.redirects[0].fd, Some(2));
    assert_eq!(s.redirects[0].target.as_deref(), Some("err.txt"));
    let s = scan("type a.txt 2>&1");
    assert!(s.ok);
    assert_eq!(s.redirects[0].fd, Some(2));
    assert_eq!(s.redirects[0].op, RedirectOp::Merge);
    assert_eq!(s.redirects[0].target, None);
    let s = scan("type a.txt 1> out.txt 2>> err.txt");
    assert!(s.ok);
    assert_eq!(s.redirects.len(), 2);
    // 引号目标保留为单 token
    let s = scan(r#"echo hi > "my file.txt""#);
    assert!(s.ok);
    assert_eq!(s.redirects[0].target.as_deref(), Some("my file.txt"));
}

#[test]
fn scan_redirect_limit_and_fd_rules() {
    // 重定向超 3 次 → OUT
    let s = scan("echo a > x > y > z > w");
    assert!(!s.ok, "4 次重定向应 OUT");
    // FD ∉ {1,2}（3>）→ OUT
    let s = scan("echo a 3> x");
    assert!(!s.ok, "FD=3 应 OUT");
    // token 中间 >（hi>a.txt）→ OUT
    let s = scan("echo hi>a.txt");
    assert!(!s.ok, "紧贴 token 的 > 应 OUT");
    // 重定向无目标 → OUT
    let s = scan("echo hi >");
    assert!(!s.ok, "空目标应 OUT");
    // 合并重定向无 fd（裸 >&1）→ OUT
    let s = scan("echo a >&1");
    assert!(!s.ok, "裸 >& 应 OUT");
}

#[test]
fn scan_out_syntax_forms() {
    // §11 ASK 语法面：全部 ∉ 子集 G
    let outs = [
        "echo $HOME",
        r#"Get-Content "$HOME\a.txt""#,
        r"Get-Content %TEMP%\a.txt",
        "ls | Select-Object -First 5",
        "cd x && dir",
        "type a.txt; dir",
        "Write-Output (Get-Item a).Name",
        "echo `a",
        "echo hi > a.txt > b.txt > c.txt > d.txt",
        r#"echo "unclosed"#,
        "echo 'unclosed",
        "a\"b",
        "cd .. | echo x",
        "Get-Process | Out-String",
    ];
    for cmd in outs {
        let s = scan(cmd);
        assert!(!s.ok, "应 ∉ 子集 G: {cmd}");
    }
}

#[test]
fn scan_pipes_and_compound_out() {
    // 单 & 后台、|、;、( 等任意位置 OUT
    for cmd in ["a & b", "a | b", "a; b", "a && b", "a || b", "echo (x)", "echo a)b"] {
        assert!(!scan(cmd).ok, "应 OUT: {cmd}");
    }
}

#[test]
fn scan_single_quoted_literal_content_allowed() {
    // single 引号内容为字面量：$ % ` | 不展开
    let s = scan(r"echo '$HOME\a.txt'");
    assert!(s.ok, "single 引号内字面 $ 应合法");
    assert_eq!(s.tokens[1].value, r"$HOME\a.txt");
    let s = scan(r"echo '%TEMP%\a'");
    assert!(s.ok, "single 引号内字面 % 应合法");
    // single 引号内 | ; 为内容
    let s = scan(r"echo 'a|b;c'");
    assert!(s.ok);
    assert_eq!(s.tokens[1].value, "a|b;c");
}

#[test]
fn scan_double_quoted_expansion_out() {
    // double 内展开构造 → OUT
    for cmd in [r#"echo "a$b""#, r#"echo "a%b""#, r#"echo "a`b""#] {
        assert!(!scan(cmd).ok, "应 OUT: {cmd}");
    }
    // double 内普通字符（含 | ; 等字面）合法
    let s = scan(r#"echo "a|b""#);
    assert!(s.ok, "double 内 | 无展开构造应合法");
    assert_eq!(s.tokens[1].value, "a|b");
}

#[test]
fn scan_psversiontable_single_token_only() {
    // $PSVersionTable 单 token 特例豁免
    let s = scan("$PSVersionTable");
    assert!(s.ok);
    // 其余 $ 形态（含 echo $PSVersionTable）OUT
    assert!(!scan("echo $PSVersionTable").ok);
    assert!(!scan("$env:USERPROFILE\\x").ok);
    assert!(!scan("$PSVersionTable x").ok);
}

#[test]
fn scan_quoted_spaces_keep_token() {
    let s = scan(r#"copy "my file.txt" dest.txt"#);
    assert!(s.ok);
    assert_eq!(s.tokens.len(), 3);
    assert_eq!(s.tokens[1].value, "my file.txt");
    assert_eq!(s.tokens[2].value, "dest.txt");
}

// ═══════════════════════════════════════════════════════
// 删除门（§11）
// ═══════════════════════════════════════════════════════

#[test]
fn delete_gate_always_ask() {
    let ws = ws_fixture("del");
    for cmd in [
        "del C:\\ws\\a.txt",
        "Remove-Item a.txt",
        "rm .\\tmp",
        "rd .\\tmp",
        "erase a.txt",
        "clear-content a.txt",
        "rmdir tmp",
        "Remove-ItemProperty HKLM:\\x -Name y",
    ] {
        assert_ask(&eval(cmd, None, &ws), "delete-always-ask");
    }
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn delete_gate_git_rm_and_robocopy_flags() {
    let ws = ws_fixture("del2");
    assert_ask(&eval("git rm a.txt", None, &ws), "delete-always-ask");
    assert_ask(&eval(r"robocopy ws\src ws\dst /mir", None, &ws), "delete-flag-option");
    assert_ask(&eval(r"robocopy ws\src ws\dst /purge /s", None, &ws), "delete-flag-option");
    // robocopy 无删除开关：区内 AUTO（§5/§11）
    assert_auto(&eval(r"robocopy ws\src ws\dst", None, &ws), "region-in");
    assert_auto(&eval(r"robocopy .\src .\dst /e /nfl", None, &ws), "region-in");
    let _ = std::fs::remove_dir_all(&ws);
}

// ═══════════════════════════════════════════════════════
// 地域判定（§11）
// ═══════════════════════════════════════════════════════

#[test]
fn region_inside_auto() {
    let ws = ws_fixture("in");
    assert_auto(&eval("copy a.txt b.txt", None, &ws), "region-in");
    assert_auto(&eval("type a.txt", None, &ws), "region-in");
    assert_auto(&eval("Get-Content sub\\x.txt", None, &ws), "region-in");
    assert_auto(&eval("echo hi > a.txt", None, &ws), "region-in");
    assert_auto(&eval("echo hi > sub\\new.txt", None, &ws), "region-in");
    assert_auto(&eval("copy x.txt sub\\new.txt", None, &ws), "region-in");
    assert_auto(&eval("mkdir newdir", None, &ws), "region-in");
    assert_auto(&eval("move a.txt sub\\b.txt", None, &ws), "region-in");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn region_outside_ask() {
    let ws = ws_fixture("out");
    let outside = outside_fixture("out-x");
    let o = outside.join("a.txt");
    let o = o.to_string_lossy().into_owned();
    assert_ask(&eval(&format!("copy {o} b.txt"), None, &ws), "region-external");
    assert_ask(&eval(r"type C:\Windows\win.ini", None, &ws), "region-external");
    assert_ask(&eval(r"Get-Content ..\..\etc\x", None, &ws), "region-external");
    assert_ask(&eval(&format!("echo hi > {o}"), None, &ws), "region-external");
    assert_ask(&eval("copy x.txt C:\\outside\\new.txt", None, &ws), "region-external");
    // 区外目标应置 touched_external
    let v = eval(&format!("copy {o} b.txt"), None, &ws);
    assert!(v.touched_external, "区外触及应置位");
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn region_cd_navigation() {
    let ws = ws_fixture("cd");
    // ws 根 cd .. 越出工作区 → ASK
    assert_ask(&eval("cd ..", None, &ws), "region-external");
    // 子目录内 cd .. 回到 ws 根 → AUTO
    let sub = ws.join("sub");
    assert_auto(&eval("cd ..", Some(&sub), &ws), "region-in");
    // cd 到区内目录 → AUTO；区外目录 → ASK
    assert_auto(&eval("cd sub", None, &ws), "region-in");
    let outside = outside_fixture("cd-x");
    let o = outside.to_string_lossy().into_owned();
    assert_ask(&eval(&format!("cd {o}"), None, &ws), "region-external");
    // popd 无参 AUTO（§4）
    assert_auto(&eval("popd", None, &ws), "region-in");
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn region_cwd_outside_whole_command_ask() {
    let ws = ws_fixture("cwdx");
    let outside = outside_fixture("cwd-x");
    // cwd 区外 → 整条 ASK(region-cwd-external)，即使命令本身只读
    assert_ask(&eval("Get-Content a.txt", Some(&outside), &ws), "region-cwd-external");
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn region_cwd_inside_subdir_resolves_relative() {
    let ws = ws_fixture("cwdsub");
    let sub = ws.join("sub");
    // 相对路径按 cwd（sub）解析：sub 下新建 out.txt 区内
    assert_auto(&eval("copy x.txt out.txt", Some(&sub), &ws), "region-in");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn region_case_insensitive_compare() {
    // 大小写不一致的绝对路径写法（Windows 大小写不敏感 FS）
    #[cfg(windows)]
    {
        let ws = ws_fixture("case");
        let canon = ws.canonicalize().unwrap();
        let upper = canon.to_string_lossy().to_uppercase();
        let cmd = format!("copy {upper}\\A.TXT b.txt");
        // A.TXT 若不存在（a.txt 存在，大小写不敏感匹配）→ canonicalize 到真实
        // 路径后比较 → 区内 AUTO；无论如何大小写不同写法不应误判区外
        assert_eq!(
            eval(&cmd, None, &ws).decision,
            GateDecision::Auto,
            "大小写差异不应误判: {cmd}"
        );
        let _ = std::fs::remove_dir_all(&ws);
    }
}

#[cfg(windows)]
#[test]
fn region_symlink_escape_ask() {
    // 符号链接指向区外的区内路径 → canonicalize 解析真实目标 → 区外 ASK
    let ws = ws_fixture("slink");
    let outside = outside_fixture("slink-x");
    let link = ws.join("linkdir");
    let created = std::os::windows::fs::symlink_dir(&outside, &link);
    if let Ok(()) = created {
        let cmd = format!("type {}\\a.txt", link.to_string_lossy());
        let v = eval(&cmd, None, &ws);
        assert_eq!(v.decision, GateDecision::Ask, "symlink 逃逸应 ASK: {:?}", v);
        assert_eq!(v.reason, "region-external");
    }
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn region_write_nonexistent_paths() {
    let ws = ws_fixture("ne");
    let outside = outside_fixture("ne-x");
    // 不存在目标：区内 → AUTO、区外 → ASK（祖先上溯 + 词法拼接）
    assert_auto(&eval("copy a.txt deep\\new\\file.txt", None, &ws), "region-in");
    let o = outside.join("new").join("file.txt").to_string_lossy().into_owned();
    assert_ask(&eval(&format!("copy a.txt {o}"), None, &ws), "region-external");
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&outside);
}

// ═══════════════════════════════════════════════════════
// 程序运行面（§11）
// ═══════════════════════════════════════════════════════

#[test]
fn run_inside_program_auto() {
    let ws = ws_fixture("run");
    // python 脚本（区内）→ AUTO
    assert_auto(&eval("python script.py", None, &ws), "region-in");
    assert_auto(&eval(r#"python "script.py""#, None, &ws), "region-in");
    // 脚本后的选项不影响目标判定
    assert_auto(&eval("python script.py --flag arg", None, &ws), "region-in");
    // 路径形态命令名：区内程序 → AUTO
    let myexe = ws.join("tools").join("my.exe");
    let myexe = myexe.to_string_lossy().into_owned();
    assert_auto(&eval(&format!("{myexe} --go"), None, &ws), "region-in");
    // node 脚本类比（解释器族）
    std::fs::write(ws.join("s.js"), "console.log(1)").unwrap();
    assert_auto(&eval("node s.js", None, &ws), "region-in");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn run_outside_program_ask() {
    let ws = ws_fixture("runx");
    let outside = outside_fixture("runx-o");
    let script = outside.join("script.py").to_string_lossy().into_owned();
    assert_ask(&eval(&format!("python {script}"), None, &ws), "run-external-program");
    let myexe = outside.join("my.exe").to_string_lossy().into_owned();
    assert_ask(&eval(&format!("{myexe}"), None, &ws), "run-external-program");
    let v = eval(&format!("python {script}"), None, &ws);
    assert!(v.touched_external);
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn unknown_program_rules() {
    let ws = ws_fixture("unk");
    // 无参未知程序 → ASK(unknown-program-no-target)
    assert_ask(&eval("some-custom-tool", None, &ws), "unknown-program-no-target");
    // 全选项无位置参数 → 同
    assert_ask(&eval("some-custom-tool --do-thing", None, &ws), "unknown-program-no-target");
    // 首个非选项参数 = 运行目标候选：区内 → AUTO、区外 → ASK
    assert_auto(&eval("some-custom-tool a.txt", None, &ws), "region-in");
    let outside = outside_fixture("unk-o");
    let o = outside.join("a.txt").to_string_lossy().into_owned();
    assert_ask(&eval(&format!("some-custom-tool {o}"), None, &ws), "region-external");
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn code_exec_forms_ask() {
    let ws = ws_fixture("code");
    for cmd in [
        r#"python -c "print(1)""#,
        r#"powershell -Command "Get-Process""#,
        r#"powershell -c "Remove-Item x""#,
        r#"cmd /c echo hi"#,
        r#"cmd /c "echo hi & echo ok""#,
        r#"bash -c echo hi"#,
        r#"sh -c "ls -la""#,
        "python",
        "powershell",
        "cmd",
    ] {
        assert_ask(&eval(cmd, None, &ws), "code-exec");
    }
    // python -m 模块 → 白名单外形态 → unlisted-option
    assert_ask(&eval("python -m http.server", None, &ws), "unlisted-option");
    // powershell 未列开关 → unlisted-option
    assert_ask(&eval(r"powershell -NoProfile -File x.ps1", None, &ws), "unlisted-option");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn network_install_process_config_ask() {
    let ws = ws_fixture("ask");
    assert_ask(&eval("curl -o out.txt https://x", None, &ws), "network-op");
    assert_ask(&eval("curl.exe https://example.com", None, &ws), "network-op");
    assert_ask(&eval("wget https://x/file.zip", None, &ws), "network-op");
    assert_ask(&eval("winget install vlc", None, &ws), "install-op");
    assert_ask(&eval("pip install requests", None, &ws), "install-op");
    assert_ask(&eval("npm install", None, &ws), "install-op");
    assert_ask(&eval("taskkill /IM notepad.exe", None, &ws), "process-control");
    assert_ask(&eval("stop-process -Name x", None, &ws), "process-control");
    assert_ask(&eval("netsh interface set interface eth0 disabled", None, &ws), "config-op");
    assert_ask(&eval("net use Z: \\\\server\\share", None, &ws), "config-op");
    assert_ask(&eval("route add 10.0.0.0 mask 255.0.0.0 10.0.0.1", None, &ws), "config-op");
    // 网络/安装族无区内豁免（即使目标区内）
    assert_ask(&eval("curl -o a.txt https://x", None, &ws), "network-op");
    let _ = std::fs::remove_dir_all(&ws);
}

// ═══════════════════════════════════════════════════════
// 类型放行（§6 / §11）
// ═══════════════════════════════════════════════════════

#[test]
fn type_release_system_queries_auto() {
    let ws = ws_fixture("sys");
    for cmd in [
        "Get-Process",
        "systeminfo",
        "ipconfig",
        "ver",
        "tasklist",
        "get-location",
        "pwd",
        "get-service",
        "echo hello",
        "write-host hello",
        "git status",
        "git log --oneline -5",
        "git diff",
        "git show HEAD",
        "git branch",
        "git branch -v",
        "git remote -v",
        "git --version",
        "python --version",
        "python -V",
        "node -v",
        "cmd /?",
        "reg query HKCU\\Software",
        "$PSVersionTable",
        "git stash list",
    ] {
        assert_auto(&eval(cmd, None, &ws), "system-query");
    }
    let _ = std::fs::remove_dir_all(&ws);
}

// ═══════════════════════════════════════════════════════
// git 子表（§6 / §11）
// ═══════════════════════════════════════════════════════

#[test]
fn git_write_family_region() {
    let ws = ws_fixture("git");
    assert_auto(&eval("git add a.txt", None, &ws), "region-in");
    assert_auto(&eval("git add .", None, &ws), "region-in");
    assert_auto(&eval("git add sub\\x.txt sub\\y.txt", None, &ws), "region-in");
    assert_auto(&eval("git commit -m msg", None, &ws), "region-in");
    assert_auto(&eval("git commit -am msg", None, &ws), "region-in");
    assert_auto(&eval("git checkout main", None, &ws), "region-in");
    assert_auto(&eval("git merge dev", None, &ws), "region-in");
    assert_auto(&eval("git tag v1.0", None, &ws), "region-in");
    assert_auto(&eval("git branch feat", None, &ws), "region-in");
    assert_auto(&eval("git branch -m old new", None, &ws), "region-in");
    assert_auto(&eval("git stash push -m wip", None, &ws), "region-in");
    assert_auto(&eval("git stash pop", None, &ws), "region-in");
    assert_auto(&eval("git mv a.txt b.txt", None, &ws), "region-in");
    // git add 区外路径 → ASK
    let outside = outside_fixture("git-o");
    let o = outside.join("x.txt").to_string_lossy().into_owned();
    assert_ask(&eval(&format!("git add {o}"), None, &ws), "region-external");
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn git_undo_network_ask() {
    let ws = ws_fixture("gitx");
    assert_ask(&eval("git reset --hard HEAD", None, &ws), "git-undo");
    assert_ask(&eval("git clean -f", None, &ws), "git-undo");
    assert_ask(&eval("git clean -fd", None, &ws), "git-undo");
    assert_ask(&eval("git checkout -f main", None, &ws), "git-undo");
    assert_ask(&eval("git branch -d old", None, &ws), "git-undo");
    assert_ask(&eval("git branch -D old", None, &ws), "git-undo");
    assert_ask(&eval("git push", None, &ws), "network-op");
    assert_ask(&eval("git push origin main", None, &ws), "network-op");
    assert_ask(&eval("git fetch origin", None, &ws), "network-op");
    assert_ask(&eval("git pull", None, &ws), "network-op");
    assert_ask(&eval("git clone https://x/repo.git", None, &ws), "network-op");
    assert_ask(&eval("git remote add origin https://x", None, &ws), "network-op");
    assert_ask(&eval("git rebase main", None, &ws), "git-undo");
    assert_ask(&eval("git stash drop", None, &ws), "git-undo");
    let _ = std::fs::remove_dir_all(&ws);
}

// ═══════════════════════════════════════════════════════
// 优先级不变量（§1 / §11）
// ═══════════════════════════════════════════════════════

#[test]
fn priority_crit_before_delete_before_syntax() {
    let ws = ws_fixture("prio");
    // crit 先于删除门：递归删除 → Blocked（即使删除门也会 Ask）
    assert_blocked(&eval("del /s C:\\ws\\x", None, &ws), "crit-recursive-delete");
    // crit 先于语法：含管道等 OUT 形态的 crit 动词仍恒拦
    assert_blocked(&eval("del /s C:\\x | more", None, &ws), "crit-recursive-delete");
    assert_blocked(&eval("format D: /q", None, &ws), "crit-format-drive");
    assert_blocked(&eval("reg delete HKCU\\X /v Y", None, &ws), "crit-registry-write");
    // 删除门先于分类：非递归 del → delete-always-ask（不落 FileDeleteOp 地域面）
    assert_ask(&eval("del C:\\ws\\a.txt", None, &ws), "delete-always-ask");
    // 语法门先于删除/分类：$HOME 形态 del 无删除词命中？—— $ 使整条 OUT
    assert_ask(&eval("del $HOME\\x", None, &ws), "unanalyzable-syntax");
    // crit 命中 → rule_id 原真 id、touched_external false
    let v = eval("del /s C:\\ws\\x", None, &ws);
    assert!(!v.touched_external);
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn priority_crit_set_unchanged_from_risk_migration() {
    // crit 恒拦集合逐条语义不变（risk.rs 迁移面）：
    // 此处抽查 evaluate 透传（全量正则断言在 risk.rs tests）
    let ws = ws_fixture("crit");
    for (cmd, rule) in [
        ("del /S C:\\TEMP", "crit-recursive-delete"),
        ("diskpart", "crit-diskpart"),
        ("takeown /f C:\\x", "crit-takeown"),
        ("icacls C:\\x /grant Users:F", "crit-icacls"),
        ("net user hacker /add", "crit-net-account"),
        ("sc create Evil binPath= C:\\x", "crit-sc-service"),
        ("schtasks /create /tn X /tr Y", "crit-schtasks"),
        ("bcdedit /enum", "crit-bcdedit"),
    ] {
        assert_blocked(&eval(cmd, None, &ws), rule);
    }
    let _ = std::fs::remove_dir_all(&ws);
}

// ═══════════════════════════════════════════════════════
// 白名单选项面
// ═══════════════════════════════════════════════════════

#[test]
fn named_option_whitelist() {
    let ws = ws_fixture("opt");
    // 路径值选项参与判定：区内 → AUTO
    assert_auto(&eval("Get-Content -Path a.txt", None, &ws), "region-in");
    assert_auto(&eval("Get-Content -LiteralPath .\\a.txt", None, &ws), "region-in");
    assert_auto(&eval("copy a.txt -Destination sub\\b.txt", None, &ws), "region-in");
    // 路径值选项值在区外 → ASK
    let outside = outside_fixture("opt-o");
    let o = outside.join("x.txt").to_string_lossy().into_owned();
    assert_ask(&eval(&format!("Get-Content -Path {o}"), None, &ws), "region-external");
    // -filter 值跳过不判
    assert_auto(&eval("Get-Content sub -Filter *.txt", None, &ws), "region-in");
    // 白名单外 -x（strict cmdlet 面）→ ASK(unlisted-option)
    assert_ask(&eval("Get-ChildItem -Recurse", None, &ws), "unlisted-option");
    assert_ask(&eval("Get-Content -Raw a.txt", None, &ws), "unlisted-option");
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&outside);
}

// ═══════════════════════════════════════════════════════
// 分解函数单测
// ═══════════════════════════════════════════════════════

#[test]
fn decompose_scan_is_delete_classify() {
    let ws = ws_fixture("decomp");
    // is_delete_program
    let t = |cmd: &str| scan(cmd);
    let del = |cmd: &str| is_delete_program(&t(cmd).tokens);
    assert_eq!(del("del a.txt"), Some("delete-always-ask"));
    assert_eq!(del("rmdir /q tmp"), Some("delete-always-ask"));
    assert_eq!(del("git rm a.txt"), Some("delete-always-ask"));
    assert_eq!(del(r"robocopy s d /mir"), Some("delete-flag-option"));
    assert_eq!(del("robocopy s d"), None);
    assert_eq!(del("copy a b"), None);
    assert_eq!(del("type a.txt"), None);
    // classify_program：git 面
    let git = |cmd: &str| classify_program(&scan(cmd).tokens);
    assert_eq!(git("git status").kind, OpKind::SystemQueryOp);
    assert_eq!(git("git push").kind, OpKind::NetworkOp);
    assert_eq!(git("git add a.txt").kind, OpKind::FileWriteOp);
    assert_eq!(git("git reset --hard").fixed_ask, Some("git-undo"));
    // 解释器面
    assert_eq!(classify_program(&scan("python s.py").tokens).kind, OpKind::RunProgramOp);
    assert_eq!(classify_program(&scan("python -c x").tokens).fixed_ask, Some("code-exec"));
    assert_eq!(classify_program(&scan("python -m x").tokens).fixed_ask, Some("unlisted-option"));
    assert_eq!(classify_program(&scan("cmd /c x").tokens).fixed_ask, Some("code-exec"));
    assert_eq!(classify_program(&scan("powershell -File x.ps1").tokens).kind, OpKind::RunProgramOp);
    // 网络/安装/进程/配置族
    assert_eq!(classify_program(&scan("curl https://x").tokens).kind, OpKind::NetworkOp);
    assert_eq!(classify_program(&scan("winget install vlc").tokens).kind, OpKind::InstallOp);
    assert_eq!(classify_program(&scan("taskkill /im x").tokens).kind, OpKind::ProcessControlOp);
    assert_eq!(classify_program(&scan("netsh show x").tokens).kind, OpKind::ConfigOp);
    // 路径形态程序
    assert_eq!(
        classify_program(&scan(r"C:\ws\tools\my.exe").tokens).kind,
        OpKind::RunProgramOp
    );
    // 表外未知程序
    assert_eq!(classify_program(&scan("my-tool x").tokens).kind, OpKind::UnknownOp);
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn decompose_extract_targets() {
    let ws = ws_fixture("ext");
    let s = |cmd: &str| scan(cmd);
    let cls = |cmd: &str| classify_program(&s(cmd).tokens);
    // AllPositional：非选项位置
    let c = cls("copy a.txt b.txt");
    assert_eq!(extract_targets(&c, &s("copy a.txt b.txt").tokens).unwrap(), vec!["a.txt", "b.txt"]);
    // 选项跳过：-x 与 -- 后位置
    let c = cls("Get-Content a.txt");
    assert_eq!(
        extract_targets(&c, &s("Get-Content -Path a.txt").tokens).unwrap(),
        vec!["a.txt"]
    );
    // -filter 值跳过
    let c = cls("Get-Content sub -Filter *.txt a.txt");
    assert_eq!(
        extract_targets(&c, &s("Get-Content sub -Filter *.txt a.txt").tokens).unwrap(),
        vec!["sub", "a.txt"]
    );
    // strict 面白名单外 -x → Err(unlisted-option)
    let c = cls("Get-ChildItem -Recurse");
    assert_eq!(
        extract_targets(&c, &s("Get-ChildItem -Recurse a.txt").tokens),
        Err("unlisted-option")
    );
    // robocopy FirstTwo
    let c = cls(r"robocopy s d");
    assert_eq!(
        extract_targets(&c, &s(r"robocopy src dst /e /nfl").tokens).unwrap(),
        vec!["src", "dst"]
    );
    // git add：sub 后路径
    let c = cls("git add a.txt");
    assert_eq!(extract_targets(&c, &s("git add a.txt b.txt").tokens).unwrap(), vec!["a.txt", "b.txt"]);
    let c = cls("git add -- a.txt");
    assert_eq!(extract_targets(&c, &s("git add -- a.txt").tokens).unwrap(), vec!["a.txt"]);
    // git commit 无目标
    let c = cls("git commit -m msg");
    assert_eq!(extract_targets(&c, &s("git commit -m msg").tokens).unwrap(), Vec::<String>::new());
    // Script：命令名即目标（路径形态程序）
    let c = cls(r"C:\ws\tools\my.exe");
    assert_eq!(
        extract_targets(&c, &s(r"C:\ws\tools\my.exe --go").tokens).unwrap(),
        vec![r"C:\ws\tools\my.exe"]
    );
    // Script：解释器首个非选项
    let c = cls("python script.py");
    assert_eq!(
        extract_targets(&c, &s("python script.py --flag").tokens).unwrap(),
        vec!["script.py"]
    );
    // Script：未知程序首个非选项
    let c = cls("my-tool --x data.txt");
    assert_eq!(
        extract_targets(&c, &s("my-tool --x data.txt").tokens).unwrap(),
        vec!["data.txt"]
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn decompose_normalize_and_inside() {
    let ws = ws_fixture("norm");
    let ws_c = ws.canonicalize().unwrap();
    // 存在文件 canonicalize
    let a = normalize_path("a.txt", &ws_c).unwrap();
    assert!(inside_workspace(&a, &ws_c));
    // 相对子目录
    let x = normalize_path("sub\\x.txt", &ws_c).unwrap();
    assert!(inside_workspace(&x, &ws_c));
    // 不存在文件：区内
    let n = normalize_path("deep\\new\\f.txt", &ws_c).unwrap();
    assert!(inside_workspace(&n, &ws_c));
    // .. 越界：真实解析到区外
    let esc = normalize_path("..", &ws_c).unwrap();
    assert!(!inside_workspace(&esc, &ws_c));
    // 绝对区外（不存在文件）：上溯存在祖先 → 区外
    let outside = outside_fixture("norm-x");
    let o = outside.join("no.txt").to_string_lossy().into_owned();
    let o = normalize_path(&o, &ws_c).unwrap();
    assert!(!inside_workspace(&o, &ws_c));
    // 同级命名不相交：ws2 前缀不应命中 ws（组件级）
    let sibling = unique_temp_dir("sib");
    assert!(!inside_workspace(&sibling, &ws_c));
    // workspace 自身（= 前缀包含自身）
    assert!(inside_workspace(&ws_c, &ws_c));
    // 空/不可解析 → None
    assert_eq!(normalize_path("", &ws_c), None);
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&outside);
    let _ = std::fs::remove_dir_all(&sibling);
}

#[test]
fn reason_code_table_complete_for_ask_auto() {
    // §7 reason 码表的可出卡面：逐一能由用例触发（防止拼写漂移）
    let ws = ws_fixture("codes");
    let outside = outside_fixture("codes-x");
    let o = outside.join("a.txt").to_string_lossy().into_owned();
    let region_cmd = format!("copy {o} b.txt");
    let cwd_out_cmd = "Get-Content a.txt".to_string();
    let cases: &[(&str, &str, &str)] = &[
        ("ls | more", "unanalyzable-syntax", "unanalyzable-syntax"),
        ("del a.txt", "delete-always-ask", "delete-always-ask"),
        (r"robocopy s d /mir", "delete-flag-option", "delete-flag-option"),
        (&region_cmd, "region-external", "region-external"),
        (&cwd_out_cmd, "cwd-out", "region-cwd-external"),
        ("python C:\\tools\\s.py", "run-external-program", "run-external-program"),
        (r#"python -c "x""#, "code-exec", "code-exec"),
        ("Get-ChildItem -Recurse", "unlisted-option", "unlisted-option"),
        ("some-custom-tool", "unknown-program-no-target", "unknown-program-no-target"),
        ("curl https://x", "network-op", "network-op"),
        ("winget install vlc", "install-op", "install-op"),
        ("taskkill /im x", "process-control", "process-control"),
        ("netsh show x", "config-op", "config-op"),
        ("git reset --hard", "git-undo", "git-undo"),
        ("git status", "system-query", "system-query"),
        ("copy a.txt b.txt", "region-in", "region-in"),
    ];
    for (cmd, tag, expected) in cases {
        let v = if *tag == "cwd-out" {
            eval(cmd, Some(&outside), &ws)
        } else {
            eval(cmd, None, &ws)
        };
        assert_eq!(v.reason, *expected, "reason 码漂移: {cmd}");
    }
    let _ = std::fs::remove_dir_all(&ws);
    let _ = std::fs::remove_dir_all(&outside);
}

#[test]
fn risk_data_consistency() {
    // 命令表与删除词表数据一致性抽查（risk.rs 数据层与 gate 消费面）
    assert!(risk::classify_program_spec("copy").is_some());
    assert!(risk::classify_program_spec("robocopy").is_some());
    assert_eq!(risk::classify_program_spec("python"), None);
    assert!(risk::is_delete_word("del"));
    assert!(!risk::is_delete_word("move"), "move 不是删除语义（FileWriteOp）");
}
