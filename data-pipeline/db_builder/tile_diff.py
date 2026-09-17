"""对比 DB 中出现的 106 种 tileKey 与 tileInfo 的 84 种，找出差异"""
import sqlite3, json

# 1. DB 中出现的 tileKey
conn = sqlite3.connect(r"F:\Obsidian_notes\60.Dev\prts-terminal\src-tauri\resources\arknights.db")
rows = conn.execute("SELECT 地图数据 FROM levels WHERE 地图数据 IS NOT NULL").fetchall()
db_keys = set()
for (raw,) in rows:
    try:
        data = json.loads(raw)
    except Exception:
        continue
    for t in data.get("tiles", []):
        db_keys.add(t[0])

# 2. tileInfo 中的 tileKey
p = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata\excel\stage_table.json"
with open(p, encoding="utf-8") as f:
    data = json.load(f)
info_keys = set(data.get("tileInfo", {}).keys())

only_db = sorted(db_keys - info_keys)
only_info = sorted(info_keys - db_keys)
print(f"DB 出现: {len(db_keys)} 种, tileInfo: {len(info_keys)} 种")
print(f"\n仅 DB 有（{len(only_db)} 种）:")
for k in only_db:
    print("  ", k)
print(f"\n仅 tileInfo 有（{len(only_info)} 种）:")
for k in only_info:
    print("  ", k, "=", data["tileInfo"][k]["name"])
