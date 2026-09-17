"""
PRTS 终端 — enemies 关联敌人回填
读取 enemies 表，从 handbook.linkEnemies 转为中文名后写入
"""

import json, sqlite3, os

OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB = os.path.join(OUTPUT_DIR, "arknights.db")

DATA_ROOT = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata"

# ─── 读取 handbook ─────────────────────────

with open(os.path.join(DATA_ROOT, "excel/enemy_handbook_table.json"), "r", encoding="utf-8") as f:
    handbook = json.load(f)
hb_data = handbook.get("enemyData", {})

# ─── 连接数据库 ────────────────────────────

conn = sqlite3.connect(OUTPUT_DB)
conn.execute("PRAGMA journal_mode=OFF")
conn.execute("PRAGMA synchronous=OFF")

# 构建 敌人ID → 中文名 查找表（取 level 0 的名称）
name_map = {}
for row in conn.execute("SELECT 敌人ID, 名称 FROM enemies WHERE 级别=0"):
    name_map[row[0]] = row[1]

# ─── 逐条更新 ─────────────────────────────

print("====== 回填 关联敌人 ======")
updated = 0
no_link = 0
missing_name = 0

for row in conn.execute("SELECT 敌人ID FROM enemies WHERE 级别=0"):
    enemy_id = row[0]
    entry = hb_data.get(enemy_id.lower(), {})
    link_ids = entry.get("linkEnemies", [])

    if not link_ids:
        no_link += 1
        continue

    # ID → 中文名
    link_names = []
    for lid in link_ids:
        name = name_map.get(lid.lower(), lid)
        link_names.append(name)
        if name == lid:
            missing_name += 1

    data = json.dumps(link_names, ensure_ascii=False)

    # 更新该 enemy 的所有 level 变体
    conn.execute(
        "UPDATE enemies SET 关联敌人=? WHERE 敌人ID=?",
        (data, enemy_id)
    )
    updated += 1

conn.commit()

# ─── 验证 ─────────────────────────────────

print(f"====== 完成 ======")
print(f"  更新: {updated} 个敌人")
print(f"  无关联: {no_link} 个")
print(f"  未查到中文名: {missing_name} 次")

# 抽样
for r in conn.execute("SELECT 敌人ID, 名称, 关联敌人 FROM enemies WHERE 关联敌人 IS NOT NULL LIMIT 3"):
    print(f"  抽样: {r[0]} ({r[1]}) → {r[2]}")

conn.close()
print(f"  数据库: {OUTPUT_DB}")
