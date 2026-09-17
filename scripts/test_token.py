"""query_token 交互测试脚本"""
import os, subprocess

cases = [
    # 默认全部
    {"name": "龙腾.F","extra": []},
    # # fields["技能列表"] → 逐级展开
    # {"name": "机械水獭", "fields": ["技能列表"]},
    # # extra 精一 → 属性用精一
    # {"name": "机械水獭", "extra": ["精一"]},
    # # fields["天赋列表"] + extra 精一 → extra 覆盖，不展开
    # {"name": "镜中虚影", "fields": ["天赋列表"], "extra": ["精一"]},
    # # fields["精英化阶段"] → 原始阶段数据
    # {"name": "触手", "fields": ["精英化阶段"]},
    # # extra 指定技能
    # {"name": "镜中虚影", "fields": ["技能列表"], "extra": ["二技能"]},
    # # ID 查询
    # {"id": "token_10007_phatom_twin"},
    # # extra 精零
    # {"name": "机械水獭", "extra": ["精零"]},
    # # 精二1级
    # {"name": "触手", "extra": ["精二1级"]},
]

proj = r"F:\Obsidian_notes\60.Dev\prts-terminal\src-tauri"

for case in cases:
    name = case.get("name")
    tid = case.get("id")
    fields = case.get("fields", [])
    extra = case.get("extra", [])
    env = os.environ.copy()
    if name:
        env["PRTS_TOKEN_NAME"] = name
    if tid:
        env["PRTS_TOKEN_ID"] = tid
    if fields:
        env["PRTS_TOKEN_FIELDS"] = ",".join(fields)
    if extra:
        env["PRTS_TOKEN_EXTRA"] = ",".join(extra)

    label = f"name={name}" if name else f"id={tid}"
    print(f"{'='*60}")
    print(f"{label}  fields={fields}  extra={extra}")
    print(f"{'='*60}")
    result = subprocess.run(
        ["cargo", "test", "query_token::tests::show_output", "--", "--nocapture"],
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
