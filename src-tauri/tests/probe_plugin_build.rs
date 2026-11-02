//! 探针二进制的构建检查。
//!
//! 这条用例的唯一作用是让 `cargo test` 真的去构建 `src/bin/probe_plugin.rs`：
//! 单元测试（`plugins::process::tests::talks_to_the_probe_over_content_length_frames`）
//! 要靠这个二进制跑真实子进程，而 `cargo test` 只有在有集成测试引用
//! `CARGO_BIN_EXE_*` 时才会构建 bin。通道本身由那条单元测试覆盖。

#[test]
fn probe_binary_is_built() {
    let probe = env!("CARGO_BIN_EXE_probe_plugin");
    assert!(
        std::path::Path::new(probe).is_file(),
        "探针二进制不存在：{probe}"
    );
}
