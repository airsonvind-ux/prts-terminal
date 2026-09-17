"""query_rogue 交互测试脚本"""
import os, subprocess, sys

proj = r"F:\Obsidian_notes\60.Dev\prts-terminal\src-tauri"


def run_case(theme: str, names: list[str] | None = None, ids: list[str] | None = None):
    """运行一次 query_rogue show_output 测试"""
    env = os.environ.copy()
    env["PRTS_ROGUE_THEME"] = theme
    if names:
        env["PRTS_ROGUE_NAME"] = names[0]
    if ids:
        env["PRTS_ROGUE_ID"] = ids[0]

    label = f"theme={theme}"
    if names:
        label += f"  names={names}"
    if ids:
        label += f"  ids={ids}"

    print(f"{'='*60}")
    print(label)
    print(f"{'='*60}")

    result = subprocess.run(
        ["cargo", "test", "query_rogue::tests::show_output", "--", "--nocapture"],
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
    # ── 基本查询 ──
    {"theme": "傀影", "names": ["热水壶"]},
    {"theme": "水月", "names": ["造血障碍"]},
    {"theme": "萨米", "names": ["地形图"]},
    {"theme": "萨卡兹", "names": ["精选兽肉罐头"]},
    {"theme": "界园", "names": ["大炎通宝"]},

    # ── 不同输入方式 ──
    # {"theme": "1", "names": ["热水壶"]},          # 数字
    # {"theme": "rogue3", "names": ["去量化"]},     # rogueN
    # {"theme": "萨米", "ids": ["rogue_3_chaos_1"]},# ID 精确查询

    # ── 各类型条目 ──
    # {"theme": "傀影", "names": ["生存的"]},        # 剧目
    # {"theme": "水月", "names": ["血肉畸变"]},      # 排异反应
    # {"theme": "萨米", "names": ["目空一些"]},      # 坍缩范式（稀有）
    # {"theme": "萨卡兹", "names": ["死仇时代的恨意"]},# 特殊藏品
    # {"theme": "萨卡兹", "names": ["专注"]},         # 年代追忆
    # {"theme": "界园", "names": ["厉-西廉贞"]},      # 通宝
    # {"theme": "界园", "names": ["卯律"]},            # 岁时

    # ── 边界情况 ──
    # {"theme": "萨米", "names": ["地形图-α"]},      # 多元奇物变体
    # {"theme": "傀影", "names": ["不存在的条目"]},   # 未找到
]

if len(sys.argv) > 1:
    # 命令行模式：python test_rogue.py <theme> <name>
    theme = sys.argv[1]
    name = sys.argv[2] if len(sys.argv) > 2 else None
    rid = sys.argv[2] if len(sys.argv) > 2 and sys.argv[2].startswith("rogue_") else None
    run_case(theme, names=[name] if name and not rid else None, ids=[rid] if rid else None)
else:
    for case in cases:
        run_case(**case)
