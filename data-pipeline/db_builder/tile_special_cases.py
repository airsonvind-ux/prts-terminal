"""找出 tile_start 高度=1 与 tile_flystart 掩码=3 的具体关卡实例"""
import sqlite3, json, collections

conn = sqlite3.connect(r"F:\Obsidian_notes\60.Dev\prts-terminal\src-tauri\resources\arknights.db")
rows = conn.execute("SELECT levelId, 地图数据 FROM levels WHERE 地图数据 IS NOT NULL").fetchall()

# tileKey -> levelId -> set(属性组合)
lev_attrs = collections.defaultdict(lambda: collections.defaultdict(set))

for lid, raw in rows:
    try:
        data = json.loads(raw)
    except Exception:
        continue
    for t in data.get("tiles", []):
        if len(t) < 4:
            continue
        tk, h, deploy, mask = t[0], t[1], t[2], t[3]
        if tk in ("tile_start", "tile_flystart"):
            lev_attrs[tk][lid].add((h, mask, deploy))

print("=== tile_start 高度=1 的关卡 ===")
for lid, s in sorted(lev_attrs["tile_start"].items()):
    for (h, m, d) in sorted(s):
        if h == 1:
            print(f"  {lid}: 高度={h} 掩码={m} 部署={d}")
print()

print("=== tile_flystart 掩码=3 的关卡 ===")
for lid, s in sorted(lev_attrs["tile_flystart"].items()):
    for (h, m, d) in sorted(s):
        if m == 3:
            print(f"  {lid}: 高度={h} 掩码={m} 部署={d}")
print()

# 补充：tile_start 的所有关卡数统计
n_start = len(lev_attrs["tile_start"])
n_start_h1 = sum(1 for s in lev_attrs["tile_start"].values() if any(h == 1 for h, m, d in s))
n_fly = len(lev_attrs["tile_flystart"])
n_fly_m3 = sum(1 for s in lev_attrs["tile_flystart"].values() if any(m == 3 for h, m, d in s))
print(f"tile_start 关卡数: {n_start}, 其中含高度1的: {n_start_h1}")
print(f"tile_flystart 关卡数: {n_fly}, 其中含掩码3的: {n_fly_m3}")
