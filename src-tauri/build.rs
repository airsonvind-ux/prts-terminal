use std::path::Path;

fn main() {
    // Tauri 在编译期校验 bundle.resources 的存在性。测试/开发构建不应被
    // 本地 arknights.db 阻断，因此 debug 下缺库时创建一个空占位文件，
    // 让 tauri-build 完成资源校验，随后立即删除。
    //
    // 这不是运行时数据库：debug 的种子路径仍指向 resources/arknights.db，
    // 缺库时启动自检会明确报错。release/安装包不会走这条路径，缺库仍然
    // 在打包阶段失败。
    let db = Path::new("resources/arknights.db");
    let db_placeholder = std::env::var("PROFILE").as_deref() == Ok("debug") && !db.exists();
    if db_placeholder {
        std::fs::write(db, b"").expect("创建构建期占位数据库失败");
    }

    // 数据工件清单同理：干净克隆没有 DB 和清单，debug 测试不应被阻断。
    // 运行时读取会明确报错；release 构建仍要求真实清单。
    let manifest = Path::new("resources/DATA_MANIFEST.json");
    let manifest_placeholder =
        std::env::var("PROFILE").as_deref() == Ok("debug") && !manifest.exists();
    if manifest_placeholder {
        std::fs::write(manifest, b"{}").expect("创建构建期占位数据清单失败");
    }

    let result = tauri_build::try_build(tauri_build::Attributes::default());

    if db_placeholder {
        let _ = std::fs::remove_file(db);
    }
    if manifest_placeholder {
        let _ = std::fs::remove_file(manifest);
    }
    if let Err(error) = result {
        panic!("{error:#}");
    }
}
