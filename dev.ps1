# PRTS 终端 · dev 启动脚本
#
# 设置页扩展批 A（2026-09）后：配置装配只认 data_dir/settings.json——
# 文件存在用文件、缺失用默认值。**env 不再参与配置装配**：
# PRTS_LLM_*（provider/api_key/model/base_url）与 PRTS_WORKSPACE 已废弃，
# 配置请在应用内「设置 → 模型」页填写（首次保存生成 settings.json）。
#
# 保留的调试开关（env 直读，不进 settings.json）：
#   PRTS_DEBUG / PRTS_TRACE        observability 日志输出开关
#   PRTS_EXEC_UNRESTRICTED         M4 降权执行的排障旁路（默认不设）

$env:PRTS_DEBUG="1"
npx tauri dev
