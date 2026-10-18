"""query_enemy 交互测试脚本"""
import os, subprocess

cases = [
    {"name": "浆果磐蟹", "extra": []},              # 默认：级别0 + 共N级变体标注
    # {"name": "源石虫", "extra": ["级别=1"]},       # 级别切换
    # {"name": "火龙", "extra": ["技能文本", "天赋文本"]},  # 附加列
    # {"name": "源石虫", "extra": ["级别=abc"]},     # 非法值报错指路
    # {"name": "源石虫", "extra": ["级别1"]},        # 旧裸写语法报错指路
]

proj = r"F:\Obsidian_notes\60.Dev\prts-terminal\src-tauri"

for case in cases:
    name = case["name"]
    extra = case.get("extra", [])
    env = os.environ.copy()
    env["PRTS_NAME"] = name
    env["PRTS_EXTRA"] = ",".join(extra)
    print(f"{'='*60}")
    print(f"extra={extra}")
    print(f"{'='*60}")
    result = subprocess.run(
        ["cargo", "test", "query_enemy::tests::show_output", "--", "--nocapture"],
        cwd=proj, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        encoding="utf-8", errors="replace"
    )
    in_output = False
    for line in result.stdout.splitlines():
        if "running " in line and "test" in line: in_output = True; continue
        if in_output:
            if line.startswith("test ") or line.startswith("test result"): continue
            if "Running unittests" in line: continue
            print(line)
    print()
