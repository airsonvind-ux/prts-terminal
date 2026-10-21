"""query_stage 交互测试脚本"""
import os, subprocess

cases = [
    {"name": "暴君", "extra": []},                      # 默认：7 列 + levelId（多难度多条返回）
    {"name": "1-7", "extra": []},                       # 关卡编号检索
    {"name": "暴君", "extra": ["敌人列表", "部署上限"]},   # 附加列
    {"name": "暴君", "extra": ["等级=1"]},              # 含 = 报错指路（关卡无状态键）
    {"name": "暴君", "extra": ["不存在的列"]},           # 未知列报错指路
]

proj = os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))), "src-tauri")

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
        ["cargo", "test", "query_stage::tests::show_output", "--", "--nocapture"],
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
