"""query_trap 交互测试脚本"""
import os, subprocess

cases = [
    # {"name": "障碍物"},
    {"name": "碎石", "extra": ["等级=20级"]}
]

proj = r"F:\Obsidian_notes\60.Dev\prts-terminal\src-tauri"

for case in cases:
    name = case.get("name")
    tid = case.get("id")
    fields = case.get("fields", [])
    extra = case.get("extra", [])
    env = os.environ.copy()
    if name:
        env["PRTS_TRAP_NAME"] = name
    if tid:
        env["PRTS_TRAP_ID"] = tid
    if fields:
        env["PRTS_TRAP_FIELDS"] = ",".join(fields)
    if extra:
        env["PRTS_TRAP_EXTRA"] = ",".join(extra)

    label = f"name={name}" if name else f"id={tid}"
    print(f"{'='*60}")
    print(f"{label}  fields={fields}  extra={extra}")
    print(f"{'='*60}")
    result = subprocess.run(
        ["cargo", "test", "query_trap::tests::show_output", "--", "--nocapture"],
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
