"""query_level 交互测试脚本

用法：
    1. cases 模式：直接运行，按 cases 列表依次执行
       python test_level.py
    2. 命令行模式：指定关卡与字段
       python test_level.py <levelId> [fields]

fields 为逗号分隔：地图,预置单位,关卡符文,路线,波次（fields 必填）
levelId 如 level_main_01-07、level_act12mini_04
"""
import os, subprocess, sys

proj = os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))), "src-tauri")


def run_case(level_id: str, fields: str = ""):
    """运行一次 query_level show_output 测试"""
    env = os.environ.copy()
    env["PRTS_LEVEL_ID"] = level_id
    if fields:
        env["PRTS_LEVEL_FIELDS"] = fields
    else:
        env.pop("PRTS_LEVEL_FIELDS", None)

    label = f"levelId={level_id}"
    if fields:
        label += f"  fields={fields}"

    print(f"{'='*60}")
    print(label)
    print(f"{'='*60}")

    result = subprocess.run(
        ["cargo", "test", "query_level::tests::show_output", "--", "--nocapture"],
        cwd=proj, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        encoding="utf-8", errors="replace"
    )

    in_output = False
    for line in result.stdout.splitlines():
        if "running " in line and "test" in line:
            in_output = True
            continue
        if in_output:
            if line.startswith("test ") or line.startswith("test result"):
                continue
            if "Running unittests" in line:
                continue
            print(line)
    print()


# ═══════════════════════════════════════════════
# 测试用例列表（按需取消注释）
# ═══════════════════════════════════════════════

cases = [
    # ── 基础关卡 ──
    {"level_id": "level_rogue5_t-10", "fields": "地图,路线,波次"},
    # {"level_id": "level_act6d5_04", "fields": "预置单位"},
    # {"level_id": "level_main_01-07", "fields": "地图"},
    # {"level_id": "level_main_01-07", "fields": "路线"},
    # {"level_id": "level_main_01-07", "fields": "波次"},

    # # ── visitEveryCheckPoint=true 的路线 ──
    # {"level_id": "level_act12mini_04", "fields": "路线"},     # routes[2] 为 true，与 routes[3] 几何相同
    # {"level_id": "level_act11mini_08", "fields": "路线"},     # routes[39] 为 true（含消失重现节点）

    # ── 分支 / 额外路线 ──
    # {"level_id": "level_act12mini_06", "fields": "波次"},   # 含分支信息
    # {"level_id": "level_a001_01", "fields": "路线"},         # 多路线

    # ── 肉鸽关卡 ──
    # {"level_id": "level_rogue5_4-3", "fields": "地图,波次"},
]

if len(sys.argv) > 1:
    # 命令行模式：python test_level.py <levelId> [fields]
    level_id = sys.argv[1]
    fields = sys.argv[2] if len(sys.argv) > 2 else ""
    run_case(level_id, fields)
else:
    for case in cases:
        run_case(**case)
