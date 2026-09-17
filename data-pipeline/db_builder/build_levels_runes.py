"""
PRTS 终端 — levels 表 · 关卡符文入库
在 build_levels.py 之后运行，为 levels 表添加关卡符文列并填充。
"""

import json
import sqlite3
import os

DATA_ROOT = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata"
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB = os.path.join(SCRIPT_DIR, "arknights.db")


def collect_json_files():
    files = {}
    def walk(root):
        for dirpath, _, fnames in os.walk(root):
            for fname in fnames:
                if fname.endswith(".json"):
                    lid = fname.replace(".json", "")
                    if lid not in files:
                        files[lid] = os.path.join(dirpath, fname)
    walk(os.path.join(DATA_ROOT, "levels", "obt"))
    act_root = os.path.join(DATA_ROOT, "levels", "activities")
    if os.path.isdir(act_root):
        walk(act_root)
    return files


def process_runes(level_data):
    """合并 runes 和 optionalRunes，按 difficultyMask 分组。
    optionalRunes 为 dict 时存入顶层"可选"键"""
    runes = level_data.get("runes") or []
    opt = level_data.get("optionalRunes")

    if isinstance(opt, list):
        runes = runes + opt
        opt = None

    if not runes and not isinstance(opt, dict):
        return "{}"

    result = {}
    for r in runes:
        dm = r.get("difficultyMask")
        if dm is None:
            continue
        dm = str(dm)
        entry = {
            "key": r.get("key"),
            "professionMask": r.get("professionMask"),
            "buildableMask": r.get("buildableMask"),
            "blackboard": [
                {
                    "key": b.get("key"),
                    "value": b.get("value"),
                    "valueStr": b.get("valueStr"),
                }
                for b in (r.get("blackboard") or [])
            ],
        }
        result.setdefault(dm, []).append(entry)

    if isinstance(opt, dict):
        opt_clean = {}
        for group_name, group_runes in opt.items():
            opt_clean[group_name] = []
            for r in group_runes:
                opt_clean[group_name].append({
                    "key": r.get("key"),
                    "professionMask": r.get("professionMask"),
                    "buildableMask": r.get("buildableMask"),
                    "blackboard": [
                        {
                            "key": b.get("key"),
                            "value": b.get("value"),
                            "valueStr": b.get("valueStr"),
                        }
                        for b in (r.get("blackboard") or [])
                    ],
                })
        result["可选"] = opt_clean

    return json.dumps(result, ensure_ascii=False)


def build():
    print("====== 关卡符文入库 ======")
    print(f"数据源: {DATA_ROOT}")
    print(f"目标 DB: {OUTPUT_DB}")

    conn = sqlite3.connect(OUTPUT_DB)
    conn.execute("PRAGMA journal_mode=OFF")
    conn.execute("PRAGMA synchronous=OFF")

    # 检查 levels 表是否已存在
    cur = conn.cursor()
    cur.execute("SELECT name FROM sqlite_master WHERE type='table' AND name='levels'")
    if not cur.fetchone():
        print("错误：levels 表不存在，请先运行 build_levels.py")
        conn.close()
        return

    # 检查关卡符文列是否已存在
    cur.execute("PRAGMA table_info(levels)")
    cols = [r[1] for r in cur.fetchall()]
    if "关卡符文" not in cols:
        print("添加关卡符文列...")
        conn.execute("ALTER TABLE levels ADD COLUMN 关卡符文 TEXT NOT NULL DEFAULT '{}'")
    else:
        print("关卡符文列已存在")

    # 收集 JSON 文件
    all_files = collect_json_files()
    print(f"JSON 文件总数: {len(all_files)}")

    # 逐关处理 runes 并更新
    print("处理 runes...")
    empty_count = 0
    error_count = 0
    updated = 0

    conn.execute("BEGIN")
    for lid, filepath in sorted(all_files.items()):
        try:
            with open(filepath, "r", encoding="utf-8") as f:
                data = json.load(f)
        except Exception:
            error_count += 1
            continue

        runes_json = process_runes(data)
        if runes_json == "{}":
            empty_count += 1
        else:
            updated += 1

        cur.execute("UPDATE levels SET 关卡符文 = ? WHERE levelId = ?", (runes_json, lid))
    conn.commit()

    # 验证
    total = cur.execute("SELECT COUNT(*) FROM levels").fetchone()[0]
    with_runes = cur.execute("SELECT COUNT(*) FROM levels WHERE 关卡符文 != '{}'").fetchone()[0]
    rune_size = cur.execute("SELECT SUM(LENGTH(关卡符文)) FROM levels").fetchone()[0]

    print(f"\n====== 完成 ======")
    print(f"  levels 总行数:      {total}")
    print(f"  有 runes 的关卡:    {with_runes}")
    print(f"  无 runes 的关卡:    {empty_count}")
    if error_count:
        print(f"  解析失败:           {error_count}")
    print(f"  关卡符文总大小:      {rune_size/1024:,.1f} KB")

    conn.close()


if __name__ == "__main__":
    build()
