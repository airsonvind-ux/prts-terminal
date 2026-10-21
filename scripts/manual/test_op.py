"""query_operator 交互测试脚本"""
import os, subprocess

cases = [
    # fields["天赋列表"] + 精一满级 → 只显示精1以下备选
    {"name": "空弦", "extra": ["潜能=满潜","模组=特限模组","二技能"]},
    # # fields["天赋列表"] + 五潜 → 显示含潜能要求的备选
    # {"name": "阿米娅", "fields": ["天赋列表"], "extra": ["五潜"]},
    # # fields["天赋列表"] + 一天赋
    # {"name": "阿米娅", "fields": ["天赋列表"], "extra": ["一天赋"]},
    # # 默认 + 满潜
    # {"name": "阿米娅", "extra": ["满潜"]},
]

proj = os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))), "src-tauri")

for case in cases:
    name = case["name"]
    fields = case.get("fields", [])
    extra = case.get("extra", [])
    env = os.environ.copy()
    env["PRTS_NAME"] = name
    env["PRTS_FIELDS"] = ",".join(fields)
    env["PRTS_EXTRA"] = ",".join(extra)
    print(f"{'='*60}")
    print(f"fields={fields}  extra={extra}")
    print(f"{'='*60}")
    result = subprocess.run(
        ["cargo", "test", "query_operator::tests::show_output", "--", "--nocapture"],
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
