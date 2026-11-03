//! 模板 fixture：校验器必须能把随包的 Python 模板直接跑通。
//!
//! 这条测试守的是「交付物之间没有互相漂移」：模板改了协议而校验器没跟上、
//! 或者校验器的判据比宿主严/松，都会在这里红。
//!
//! Rust 模板要现编译，跑得慢、且离线环境拉不到 `serde_json`，所以标了
//! `#[ignore]`；发版前手动跑一次：
//!
//! ```text
//! cargo test --test plugin_check_templates -- --ignored
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

/// `src-tauri/tests/` → 仓库根 → `plugin-templates/`
fn template_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../plugin-templates")
        .join(name)
}

fn check_binary() -> &'static str {
    env!("CARGO_BIN_EXE_plugin_check")
}

/// 跑校验器，失败时把它的输出原样带出来（断言里看得到第几项红）。
fn run_check(dir: &Path) -> Result<(), String> {
    let output = Command::new(check_binary())
        .arg(dir)
        .output()
        .map_err(|error| format!("起校验器失败：{error}"))?;

    if output.status.success() {
        return Ok(());
    }

    let mut report = String::from_utf8_lossy(&output.stdout).to_string();
    report.push_str(&String::from_utf8_lossy(&output.stderr));
    Err(report)
}

/// 本机有没有能用的 `python`（模板的 `entry.exe` 就是它）。
fn python_available() -> bool {
    Command::new("python")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[test]
fn python_template_passes_the_validator() {
    if !python_available() {
        eprintln!("跳过：PATH 里没有 python");
        return;
    }

    let dir = template_dir("python-hello");
    assert!(dir.is_dir(), "找不到模板目录：{}", dir.display());

    if let Err(report) = run_check(&dir) {
        panic!("Python 模板没通过校验：\n{report}");
    }
}

#[test]
#[ignore = "要现编译 Rust 模板（拉 serde_json），离线环境跳过"]
fn rust_template_passes_the_validator() {
    let source = template_dir("rust-hello");
    assert!(source.is_dir(), "找不到模板目录：{}", source.display());

    // 在临时目录里构建，别把 target/ 和 hello.exe 落进仓库。
    let scratch = std::env::temp_dir().join(format!("prts-rust-template-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    copy_tree(&source, &scratch).expect("复制模板失败");
    // 仓库里那份 lock 可能钉着本机缓存没有的版本；模板是示例，让它自己解析。
    let _ = std::fs::remove_file(scratch.join("Cargo.lock"));

    let mut build = Command::new(env!("CARGO"))
        .args(["build", "--release", "--offline"])
        .current_dir(&scratch)
        .output()
        .expect("起 cargo 失败");
    if !build.status.success() {
        // 没有本地缓存时会拉网络；再试一次联网构建。
        build = Command::new(env!("CARGO"))
            .args(["build", "--release"])
            .current_dir(&scratch)
            .output()
            .expect("起 cargo 失败");
    }
    assert!(
        build.status.success(),
        "Rust 模板编译失败：\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let exe = scratch.join("target/release").join(if cfg!(windows) {
        "hello.exe"
    } else {
        "hello"
    });
    std::fs::copy(&exe, scratch.join(exe.file_name().expect("有文件名"))).expect("拷贝产物失败");

    let result = run_check(&scratch);
    let _ = std::fs::remove_dir_all(&scratch);

    if let Err(report) = result {
        panic!("Rust 模板没通过校验：\n{report}");
    }
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            // 只跳过构建产物，其余目录照拷。
            if entry.file_name() == "target" {
                continue;
            }
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
