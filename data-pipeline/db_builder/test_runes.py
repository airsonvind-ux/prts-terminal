"""
测试脚本：从现有 DB 复制 levels 表结构+数据，新增关卡符文列并填充
输出到独立测试库 test_runes.db
"""

import json
import sqlite3
import os

DATA_ROOT = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata"
REAL_DB = r"F:\Obsidian_notes\60.Dev\prts-terminal\src-tauri\resources\arknights.db"
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB = os.path.join(SCRIPT_DIR, "test_runes.db")


def process_runes(level_data):
    """合并 runes 和 optionalRunes，按 difficultyMask 分组"""
    runes = level_data.get("runes") or []
    opt = level_data.get("optionalRunes")

    if isinstance(opt, list):
        runes = runes + opt

    if not runes:
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

    return json.dumps(result, ensure_ascii=False)


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


def build():
    print("====== 测试：levels + 关卡符文 ======")
    print(f"数据源: {DATA_ROOT}")
    print(f"现有 DB: {REAL_DB}")
    print(f"输出: {OUTPUT_DB}")

    # 读取现有 levels 表
    src = sqlite3.connect(REAL_DB)
    src_cur = src.cursor()
    src_cur.execute("PRAGMA table_info(levels)")
    src_cols = [r[1] for r in src_cur.fetchall()]
    print(f"现有列: {src_cols}")

    src_cur.execute("SELECT COUNT(*) FROM levels")
    src_total = src_cur.fetchone()[0]
    print(f"现有行数: {src_total}")

    # 创建测试库 + 复制表
    dst = sqlite3.connect(OUTPUT_DB)
    dst.execute("PRAGMA journal_mode=OFF")
    dst.execute("PRAGMA synchronous=OFF")
    dst.execute("DROP TABLE IF EXISTS levels")

    dst.execute("""
    CREATE TABLE levels (
        levelId     TEXT PRIMARY KEY,
        地图数据     TEXT NOT NULL,
        路线        TEXT NOT NULL,
        额外路线     TEXT,
        波次        TEXT NOT NULL,
        分支        TEXT,
        预置单位     TEXT,
        关卡符文     TEXT NOT NULL DEFAULT '{}'
    )
    """)

    # 复制原有数据
    src_cur.execute("SELECT levelId, 地图数据, 路线, 额外路线, 波次, 分支, 预置单位 FROM levels")
    dst.execute("BEGIN")
    for row in src_cur:
        dst.execute(
            "INSERT INTO levels (levelId, 地图数据, 路线, 额外路线, 波次, 分支, 预置单位) VALUES (?, ?, ?, ?, ?, ?, ?)",
            row
        )
    dst.commit()
    print(f"复制完成: {dst.execute('SELECT COUNT(*) FROM levels').fetchone()[0]} 行")

    # 收集 level JSON 文件
    all_files = collect_json_files()
    print(f"JSON 文件总数: {len(all_files)}")

    # 逐关更新 关卡符文 列
    print("处理 runes...")
    empty_count = 0
    error_count = 0
    updated = 0

    dst.execute("BEGIN")
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

        dst.execute("UPDATE levels SET 关卡符文 = ? WHERE levelId = ?", (runes_json, lid))
    dst.commit()

    # 验证
    total = dst.execute("SELECT COUNT(*) FROM levels").fetchone()[0]
    with_runes = dst.execute("SELECT COUNT(*) FROM levels WHERE 关卡符文 != '{}'").fetchone()[0]

    # 统计
    dm_stats = {}
    key_counts = {}
    for row in dst.execute("SELECT 关卡符文 FROM levels WHERE 关卡符文 != '{}'"):
        runes = json.loads(row[0])
        for dm, rlist in runes.items():
            dm_stats[dm] = dm_stats.get(dm, 0) + 1
            for r in rlist:
                k = r.get("key", "?")
                key_counts[k] = key_counts.get(k, 0) + 1

    rune_size = dst.execute("SELECT SUM(LENGTH(关卡符文)) FROM levels").fetchone()[0]

    src.close()
    dst.close()

    print(f"\n====== 完成 ======")
    print(f"  总行数:              {total}")
    print(f"  有 runes 的关卡:     {with_runes}")
    print(f"  无 runes 的关卡:     {empty_count}")
    if error_count:
        print(f"  解析失败:            {error_count}")
    print(f"  关卡符文总大小:       {rune_size/1024:,.1f} KB")

    print(f"\ndifficultyMask 分布:")
    for dm, cnt in sorted(dm_stats.items(), key=lambda x: -x[1]):
        print(f"  {dm:12s} {cnt} 关")

    print(f"\nrune key 种类: {len(key_counts)}")
    print("前 20:")
    for k, cnt in sorted(key_counts.items(), key=lambda x: -x[1])[:20]:
        print(f"  {k:40s} {cnt}")


if __name__ == "__main__":
    build()
