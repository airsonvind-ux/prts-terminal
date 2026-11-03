//! `plugin_check`：插件目录的离线校验（M4 规格 §七）。
//!
//! 为什么是独立二进制而不是 `cargo run --example`：插件开发者拿到的交付包里
//! 只有 exe、手册与模板，没有本仓库；校验器必须能单独跑起来，并且与宿主用
//! **同一份**判据（manifest 十二条、`entry.exe` 三级判定、分帧、错误码），
//! 所以它只经 `prts_terminal_lib::sdk` 取协议实现，不另写一套。
//!
//! 输出格式由设计文档定死（手册 09-排错.md 与这里逐字一致）：
//!
//! ```text
//! [1] PASS  目录与 plugin.json
//! [3] FAIL  entry.exe 解析
//!           插件目录里没有 hello.py，PATH 里也找不到 hello
//!           确认文件名拼写；解释器要先能用 `python --version` 跑通
//! [4] --    进程启动（前一项失败，跳过）
//! ```
//!
//! 失败行下方缩进两行：原因、修法。跳过用 `--` 而不是 `SKIP`——状态列按
//! 4 字符对齐，`--` 与 `PASS`/`FAIL` 同宽，行首才是直的。

use std::collections::HashSet;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitCode, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use prts_terminal_lib::sdk::{
    is_valid_tool_name, read_manifest, resolve_program, write_frame, FrameReader, PluginKind,
    PluginManifest, ProgramResolution, INVALID_PARAMS, SUPPORTED_PROTOCOL,
};

/// 检查项顺序与标签（下标 0 对应编号 1）。
const LABELS: [&str; 8] = [
    "目录与 plugin.json",
    "manifest 十二条校验",
    "entry.exe 解析",
    "进程启动",
    "initialize 往返",
    "未知工具名",
    "shutdown 收场",
    "stdout 只有帧",
];

/// 进程启动后要活过的时间（4a）。
const START_ALIVE: Duration = Duration::from_secs(1);
/// 写入 initialize 之后等首帧的上限（4b）。
const FIRST_FRAME: Duration = Duration::from_secs(5);
/// 一次调用的上限（未知工具名要在这之内回沉默的 -32602）。
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// shutdown 之后等退出的上限。
const EXIT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Fail,
    Skip,
}

impl Status {
    fn text(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Skip => "--",
        }
    }

    fn json(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skip => "skip",
        }
    }
}

#[derive(Debug, Clone)]
struct Check {
    id: usize,
    status: Status,
    /// 通过时是「看到了什么」，失败时是原因。
    detail: String,
    /// 失败时的修法；通过/跳过为空。
    fix: String,
    /// 跳过时的括注，接在标签后面（`进程启动（前一项失败，跳过）`）。
    note: String,
}

impl Check {
    fn pass(id: usize, detail: impl Into<String>) -> Self {
        Self {
            id,
            status: Status::Pass,
            detail: detail.into(),
            fix: String::new(),
            note: String::new(),
        }
    }

    fn fail(id: usize, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            id,
            status: Status::Fail,
            detail: detail.into(),
            fix: fix.into(),
            note: String::new(),
        }
    }

    fn skip(id: usize) -> Self {
        Self {
            id,
            status: Status::Skip,
            detail: String::new(),
            fix: String::new(),
            note: String::new(),
        }
    }

    /// 跳过并说明原因：原因接在标签后面，不另起一行（否则与失败项的缩进混淆）。
    fn skip_with(id: usize, note: &str) -> Self {
        Self {
            id,
            status: Status::Skip,
            detail: String::new(),
            fix: String::new(),
            note: note.to_string(),
        }
    }

    fn label(&self) -> &'static str {
        LABELS[self.id - 1]
    }

    /// 人读输出里的整行标签（跳过项带括注）。
    fn titled(&self) -> String {
        if self.status == Status::Skip && !self.note.is_empty() {
            format!("{}（{}）", self.label(), self.note)
        } else {
            self.label().to_string()
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_help();
        return ExitCode::SUCCESS;
    }

    let json = args.iter().any(|arg| arg == "--json");
    let dump = args.iter().any(|arg| arg == "--dump-capabilities");
    let target = args
        .iter()
        .find(|arg| !arg.starts_with("--"))
        .cloned();

    if dump {
        return dump_capabilities(json);
    }

    let Some(target) = target else {
        eprintln!("用法：plugin_check <插件目录> [--json] [--dump-capabilities]");
        return ExitCode::from(2);
    };

    let path = PathBuf::from(&target);
    let checks = run(&path, &target, json);

    if json {
        print_json(&target, &checks);
    } else {
        print_text(&checks);
    }

    if checks.iter().any(|check| check.status == Status::Fail) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn print_help() {
    println!("plugin_check —— PRTS 插件目录的离线校验");
    println!();
    println!("用法：plugin_check <插件目录> [--json] [--dump-capabilities]");
    println!();
    println!("  --json               以 JSON 输出检查结果");
    println!("  --dump-capabilities  打印宿主当前的能力表（JSON），供生成手册用");
    println!();
    println!("退出码：0 全部通过或跳过；1 有失败项；2 参数错误。");
}

fn dump_capabilities(_json: bool) -> ExitCode {
    let capabilities: Vec<Value> = prts_terminal_lib::sdk::known_capabilities()
        .into_iter()
        .map(|key| json!({ "key": key }))
        .collect();
    let document = json!({
        "protocol": SUPPORTED_PROTOCOL,
        "capabilities": capabilities,
    });
    match serde_json::to_string_pretty(&document) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("序列化能力表失败：{error}");
            ExitCode::from(2)
        }
    }
}

fn print_text(checks: &[Check]) {
    print!("{}", render_text(checks));
}

/// 人读输出（单独提出来是为了能被单测逐字钉住——手册 09-排错.md 直接引用它）。
fn render_text(checks: &[Check]) -> String {
    let mut out = String::new();
    for check in checks {
        out.push_str(&format!(
            "[{}] {:<4}  {}\n",
            check.id,
            check.status.text(),
            check.titled()
        ));
        if check.status == Status::Fail {
            for line in [&check.detail, &check.fix] {
                if !line.is_empty() {
                    // 缩进 10 列：与标签首字对齐（"[3] FAIL  " 正好 10 个字符）。
                    out.push_str(&format!("          {line}\n"));
                }
            }
        }
    }
    out
}

fn print_json(target: &str, checks: &[Check]) {
    let ok = checks.iter().all(|check| check.status != Status::Fail);
    let checks: Vec<Value> = checks
        .iter()
        .map(|check| {
            json!({
                "id": check.id,
                "status": check.status.json(),
                "detail": check.detail,
                "fix": check.fix,
            })
        })
        .collect();
    let document = json!({ "plugin": target, "ok": ok, "checks": checks });
    match serde_json::to_string_pretty(&document) {
        Ok(text) => println!("{text}"),
        Err(error) => eprintln!("序列化结果失败：{error}"),
    }
}

/// 跑完整套检查。`quiet_json` 只用来压制 JSON 模式下的人读提示。
fn run(dir: &Path, target: &str, json: bool) -> Vec<Check> {
    let _ = (target, json);
    let mut checks = Vec::with_capacity(LABELS.len());

    // ── 1. 目录与 plugin.json ───────────────────────────────
    if !dir.is_dir() {
        checks.push(Check::fail(
            1,
            format!("{} 不是一个目录", dir.display()),
            "传插件文件夹（含 plugin.json 的那一层），不是 plugin.json 本身",
        ));
        for id in 2..=LABELS.len() {
            checks.push(Check::skip(id));
        }
        return checks;
    }

    let manifest = match read_manifest(dir) {
        Ok(manifest) => {
            checks.push(Check::pass(
                1,
                format!("读到 plugin.json（id = {}）", manifest.id),
            ));
            manifest
        }
        Err(error) => {
            checks.push(Check::fail(
                1,
                error.to_string(),
                "确认这一层有 plugin.json；没有就先从模板复制一份",
            ));
            for id in 2..=LABELS.len() {
                checks.push(Check::skip(id));
            }
            return checks;
        }
    };

    // ── 2. manifest 十二条校验 ─────────────────────────────
    let dir_name = dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let known = prts_terminal_lib::sdk::known_capabilities();
    match prts_terminal_lib::sdk::validate_with(&manifest, &dir_name, &mut seen, &known) {
        Ok(()) => checks.push(Check::pass(2, "十二条全部通过")),
        Err(error) => {
            checks.push(Check::fail(
                2,
                error.to_string(),
                "按提示改 plugin.json；字段名与取值见手册 02-manifest",
            ));
            // 校验没过就不必再往下跑了：后面每条都会因为拿不到可信 manifest 而失真。
            for id in 3..=LABELS.len() {
                checks.push(Check::skip_with(id, "前一项失败，跳过"));
            }
            return checks;
        }
    }

    // 内置插件只验到 manifest 为止（C4/C5：外部开发者只有 process 一条路）。
    if manifest.kind == PluginKind::Builtin {
        for id in 3..=LABELS.len() {
            checks.push(Check::skip_with(id, "内置插件：校验到 manifest 为止"));
        }
        return checks;
    }

    // ── 3. entry.exe 解析 ──────────────────────────────────
    let entry = match manifest.entry.as_ref() {
        Some(entry) => entry,
        None => {
            // 十二条里的规则 5 已经拦过，这里是防御性写法。
            checks.push(Check::fail(
                3,
                "process 插件没有 entry",
                "在 plugin.json 里写 entry.exe 与 entry.args",
            ));
            for id in 4..=LABELS.len() {
                checks.push(Check::skip_with(id, "前一项失败，跳过"));
            }
            return checks;
        }
    };

    let program = match resolve_program(dir, entry) {
        Ok(ProgramResolution::InDir(path)) => {
            checks.push(Check::pass(3, format!("用插件目录里的 {}", path.display())));
            path
        }
        Ok(ProgramResolution::OnPath(name)) => match which_on_path(&name) {
            Some(found) => {
                checks.push(Check::pass(
                    3,
                    format!("插件目录里没有同名文件，按 PATH 找到 {name}（{}）", found.display()),
                ));
                PathBuf::from(name)
            }
            None => {
                checks.push(Check::fail(
                    3,
                    format!("插件目录里没有 {}，PATH 里也找不到 {name}", entry.exe),
                    format!("确认文件名拼写；解释器要先能用 `{name} --version` 跑通"),
                ));
                for id in 4..=LABELS.len() {
                    checks.push(Check::skip_with(id, "前一项失败，跳过"));
                }
                return checks;
            }
        },
        Err(detail) => {
            checks.push(Check::fail(
                3,
                detail,
                "entry.exe 写相对插件目录的路径；想用解释器就写裸名（如 python）",
            ));
            for id in 4..=LABELS.len() {
                checks.push(Check::skip_with(id, "前一项失败，跳过"));
            }
            return checks;
        }
    };

    // ── 4–8. 真起一个进程 ─────────────────────────────────
    let mut session = match Session::start(&program, &entry.args, dir) {
        Ok(session) => session,
        Err(error) => {
            checks.push(Check::fail(
                4,
                error,
                "先在同一个目录下手动跑一次 entry.exe，看它自己报什么错",
            ));
            for id in 5..=LABELS.len() {
                checks.push(Check::skip_with(id, "前一项失败，跳过"));
            }
            return checks;
        }
    };

    // 4a：启动后 1 秒仍活着（插件在 initialize 之前不说话，所以只能看进程）。
    std::thread::sleep(START_ALIVE);
    if let Some(status) = session.exited() {
        checks.push(Check::fail(
            4,
            format!("进程启动后 1 秒内就退出了（{status}）"),
            "手动跑一次，先修到它能常驻；入口是解释器时确认脚本路径对",
        ));
        for id in 5..=LABELS.len() {
            checks.push(Check::skip_with(id, "前一项失败，跳过"));
        }
        return checks;
    }

    // 4b + 5：initialize 之后 5 秒内读到首帧。
    let handshake_request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocol": SUPPORTED_PROTOCOL,
            "hostVersion": env!("CARGO_PKG_VERSION"),
            "pluginId": manifest.id,
        },
    });
    if let Err(error) = session.send(&handshake_request) {
        checks.push(Check::fail(
            4,
            format!("写入 initialize 失败：{error}"),
            "插件进程的 stdin 可能已经关闭；手动跑一次确认它会读 stdin",
        ));
        for id in 5..=LABELS.len() {
            checks.push(Check::skip_with(id, "前一项失败，跳过"));
        }
        return checks;
    }

    match session.wait_reply(1, FIRST_FRAME) {
        Ok(frame) => {
            checks.push(Check::pass(4, "进程存活，initialize 得到应答"));
            checks.push(check_handshake(5, &manifest, &frame));
        }
        Err(Failure::Timeout) => {
            checks.push(Check::fail(
                4,
                format!(
                    "写入 initialize 后 {} 秒内没有读到任何帧",
                    FIRST_FRAME.as_secs()
                ),
                "确认插件在读到 initialize 后会立刻回一帧；stdout 只写帧，别 print 调试信息",
            ));
            for id in 5..=LABELS.len() {
                checks.push(Check::skip_with(id, "前一项失败，跳过"));
            }
            return checks;
        }
        Err(Failure::Broken(detail)) => {
            checks.push(Check::fail(
                4,
                format!("读 stdout 时出错：{detail}"),
                "stdout 上只能有 Content-Length 分帧；调试输出走 stderr 或 log 通知",
            ));
            for id in 5..=LABELS.len() {
                checks.push(Check::skip_with(id, "前一项失败，跳过"));
            }
            return checks;
        }
        Err(Failure::Exited) => {
            checks.push(Check::fail(
                4,
                "进程在回 initialize 之前退出了",
                "手动跑一次看它自己的报错；确认它是常驻进程而不是跑完就退",
            ));
            for id in 5..=LABELS.len() {
                checks.push(Check::skip_with(id, "前一项失败，跳过"));
            }
            return checks;
        }
    }

    // ── 6. 未知工具名 ─────────────────────────────────────
    let unknown = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tool.call",
        "params": { "name": "plugin_check_no_such_tool", "args": {}, "deadlineMs": 5000 },
    });
    match session.request(&unknown, 2, CALL_TIMEOUT) {
        Ok(frame) => {
            let code = frame
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_i64);
            if code == Some(INVALID_PARAMS) {
                checks.push(Check::pass(
                    6,
                    format!("未知工具名回了 {INVALID_PARAMS}（工具名属于参数）"),
                ));
            } else {
                checks.push(Check::fail(
                    6,
                    format!(
                        "未知工具名应回 {INVALID_PARAMS}，实际回了 {}",
                        code.map(|code| code.to_string())
                            .unwrap_or_else(|| "成功或没有 error 字段".to_string())
                    ),
                    "工具名不认识时回 -32602；-32601 只用于 method 不认识",
                ));
            }
        }
        Err(Failure::Timeout) => checks.push(Check::fail(
            6,
            "未知工具名的调用没有在时限内回任何帧",
            "即使不认识工具名也要回一帧 error，不能沉默",
        )),
        Err(Failure::Broken(detail)) => checks.push(Check::fail(
            6,
            format!("读 stdout 时出错：{detail}"),
            "stdout 上只能有分帧",
        )),
        Err(Failure::Exited) => checks.push(Check::fail(
            6,
            "进程在回未知工具名之前退出了",
            "不认识的工具名要回 -32602，不能直接退出",
        )),
    }

    // ── 7. shutdown 收场 ──────────────────────────────────
    // 与宿主一致：`shutdown` 是带 id 的请求（进程收到后回一帧再退出）。
    let shutdown = json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": {} });
    if let Err(error) = session.send(&shutdown) {
        checks.push(Check::fail(
            7,
            format!("写入 shutdown 失败：{error}"),
            "确认进程还活着",
        ));
    } else {
        match session.wait_exit(EXIT_TIMEOUT) {
            Some(Ok(status)) if status_ok(&status) => {
                checks.push(Check::pass(7, format!("收到 shutdown 后退出（{status}）")));
            }
            Some(Ok(status)) => checks.push(Check::fail(
                7,
                format!("收到 shutdown 后退出码是 {status}，应为 0"),
                "shutdown 是正常收场，退出码给 0",
            )),
            Some(Err(detail)) => checks.push(Check::fail(
                7,
                format!("等退出时出错：{detail}"),
                "确认进程会自己退出，不需要被强杀",
            )),
            None => {
                checks.push(Check::fail(
                    7,
                    format!("shutdown 后 {} 秒内没有退出", EXIT_TIMEOUT.as_secs()),
                    "收到 shutdown 后清理并退出（退出码 0），不要留着等",
                ));
                session.kill();
            }
        }
    }

    // ── 8. stdout 只有帧 ──────────────────────────────────
    match session.poll_broken() {
        Some(detail) => checks.push(Check::fail(
            8,
            format!("stdout 上出现了非帧内容：{detail}"),
            "调试输出走 stderr；stdout 上只写 Content-Length 分帧",
        )),
        None => checks.push(Check::pass(8, "读取期间没有出现非帧内容")),
    }

    session.reap();
    checks
}

fn status_ok(status: &std::process::ExitStatus) -> bool {
    status.code() == Some(0)
}

fn check_handshake(id: usize, manifest: &PluginManifest, frame: &Value) -> Check {
    let Some(result) = frame.get("result") else {
        return Check::fail(
            id,
            format!("initialize 回了 error：{}", frame.get("error").cloned().unwrap_or(Value::Null)),
            "initialize 成功时回 result；失败也要回一帧说明原因",
        );
    };

    let protocol = result.get("protocol").and_then(Value::as_u64);
    if protocol != Some(SUPPORTED_PROTOCOL as u64) {
        return Check::fail(
            id,
            format!(
                "initialize 回报的 protocol 是 {}，宿主只支持 {SUPPORTED_PROTOCOL}",
                protocol
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "缺失".to_string())
            ),
            format!("initialize 的 result 里写 \"protocol\": {SUPPORTED_PROTOCOL}"),
        );
    }

    let Some(tools) = result.get("tools").and_then(Value::as_array) else {
        return Check::fail(
            id,
            "initialize 的 result 里没有 tools 数组",
            "没有工具就写 \"tools\": []；工具面是唯一来源，不读别的字段",
        );
    };

    let prefix = manifest.id.replace('-', "_");
    for tool in tools {
        let Some(name) = tool.get("name").and_then(Value::as_str) else {
            return Check::fail(
                id,
                "tools 里有一项没有 name",
                "每条工具都要有 name、description、parameters",
            );
        };
        let full = format!("{prefix}_{name}");
        if !is_valid_tool_name(&full) {
            return Check::fail(
                id,
                format!("工具名 {full} 不符合 ^[a-z][a-z0-9_]{{0,63}}$"),
                "工具名用小写字母开头，只含 a-z 0-9 _，加上前缀后不超过 64 字符",
            );
        }
        if !tool.get("parameters").map(Value::is_object).unwrap_or(false) {
            return Check::fail(
                id,
                format!("工具 {name} 的 parameters 不是对象"),
                "parameters 写 JSON Schema：{\"type\": \"object\", \"properties\": {}}",
            );
        }
    }

    Check::pass(
        id,
        format!("protocol = {SUPPORTED_PROTOCOL}，声明了 {} 个工具", tools.len()),
    )
}

/// 在 PATH 里找裸命令名（Windows 按 PATHEXT 试后缀）。
fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .filter(|ext| !ext.is_empty())
            .map(|ext| ext.to_ascii_lowercase())
            .collect()
    } else {
        Vec::new()
    };

    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let direct = dir.join(name);
        if direct.is_file() {
            return Some(direct);
        }
        for extension in &extensions {
            let candidate = dir.join(format!("{name}{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[derive(Debug)]
enum Failure {
    /// 时限内没有读到应答帧。
    Timeout,
    /// stdout 上出现了非帧内容。
    Broken(String),
    /// 进程退出、管道关闭。
    Exited,
}

/// 一个已经跑起来的插件进程：只做分帧读写与超时，不掺宿主的生命周期策略
/// （校验器要的是「协议通不通」，不是「宿主会怎么兜底」）。
struct Session {
    child: Child,
    stdin: ChildStdin,
    frames: Receiver<Result<Value, String>>,
}

impl Session {
    fn start(program: &Path, args: &[String], dir: &Path) -> Result<Self, String> {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }

        let mut child = command
            .spawn()
            .map_err(|error| format!("起进程失败（{}）：{error}", program.display()))?;
        let stdin = child.stdin.take().ok_or("拿不到 stdin")?;
        let stdout = child.stdout.take().ok_or("拿不到 stdout")?;
        // stderr 直接转给校验器的 stderr：插件自己的报错是排查的一半线索。
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                use std::io::BufRead;
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    eprintln!("[plugin] {line}");
                }
            });
        }

        let (sender, frames) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = FrameReader::new(BufReader::new(stdout));
            loop {
                match reader.read_frame() {
                    Ok(Some(frame)) => {
                        if sender.send(Ok(frame)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = sender.send(Err(error.to_string()));
                        break;
                    }
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            frames,
        })
    }

    fn send(&mut self, value: &Value) -> Result<(), String> {
        write_frame(&mut self.stdin, value)
            .map_err(|error| error.to_string())?;
        self.stdin.flush().map_err(|error| error.to_string())
    }

    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status),
            _ => None,
        }
    }

    /// 等到 `id` 对应的应答帧；通知（`log`）顺手跳过。
    fn wait_reply(&mut self, id: u64, timeout: Duration) -> Result<Value, Failure> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Failure::Timeout);
            }
            match self.frames.recv_timeout(remaining) {
                Ok(Ok(frame)) => {
                    if frame.get("id").and_then(Value::as_u64) == Some(id) {
                        return Ok(frame);
                    }
                    continue;
                }
                Ok(Err(detail)) => return Err(Failure::Broken(detail)),
                Err(RecvTimeoutError::Timeout) => return Err(Failure::Timeout),
                Err(RecvTimeoutError::Disconnected) => return Err(Failure::Exited),
            }
        }
    }

    fn request(&mut self, value: &Value, id: u64, timeout: Duration) -> Result<Value, Failure> {
        self.send(value).map_err(Failure::Broken)?;
        self.wait_reply(id, timeout)
    }

    fn wait_exit(&mut self, timeout: Duration) -> Option<Result<std::process::ExitStatus, String>> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(Ok(status)),
                Ok(None) => {}
                Err(error) => return Some(Err(error.to_string())),
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn poll_broken(&mut self) -> Option<String> {
        let mut broken = None;
        while let Ok(message) = self.frames.try_recv() {
            if let Err(detail) = message {
                broken = Some(detail);
            }
        }
        broken
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn reap(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            self.kill();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.reap();
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// 手册 09-排错.md 直接抄这段输出，所以逐字钉住。
    #[test]
    fn text_output_matches_the_documented_shape() {
        let checks = vec![
            Check::pass(1, "读到 plugin.json（id = demo）"),
            Check::pass(2, "十二条全部通过"),
            Check::fail(
                3,
                "插件目录里没有 hello.exe，PATH 里也找不到 hello.exe",
                "确认文件名拼写；解释器要先能用 `hello.exe --version` 跑通",
            ),
            Check::skip_with(4, "前一项失败，跳过"),
        ];

        let expected = "\
[1] PASS  目录与 plugin.json
[2] PASS  manifest 十二条校验
[3] FAIL  entry.exe 解析
          插件目录里没有 hello.exe，PATH 里也找不到 hello.exe
          确认文件名拼写；解释器要先能用 `hello.exe --version` 跑通
[4] --    进程启动（前一项失败，跳过）
";
        assert_eq!(render_text(&checks), expected);
    }

    #[test]
    fn skip_without_note_keeps_the_label_bare() {
        let checks = vec![Check::skip(3)];
        assert_eq!(render_text(&checks), "[3] --    entry.exe 解析\n");
    }

    #[test]
    fn pass_never_prints_detail_lines() {
        let checks = vec![Check::pass(1, "读到了")];
        assert_eq!(render_text(&checks), "[1] PASS  目录与 plugin.json\n");
    }
}
