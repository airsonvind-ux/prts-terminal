// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // env_logger::init();  // 网络恢复后恢复
    prts_terminal_lib::run()
}
