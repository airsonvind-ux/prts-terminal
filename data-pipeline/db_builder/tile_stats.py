"""统计全库地图 tileKey 分布：哪些是基础地块（高频/通用），哪些是特殊地块（低频/特定模式）"""
import sqlite3, json, collections

conn = sqlite3.connect(r"F:\Obsidian_notes\60.Dev\prts-terminal\src-tauri\resources\arknights.db")
rows = conn.execute(
    "SELECT levelId, 地图数据 FROM levels WHERE 地图数据 IS NOT NULL"
).fetchall()

tile_counter = collections.Counter()          # tileKey -> 出现次数
tile_levels = collections.defaultdict(set)    # tileKey -> 出现过的关卡
level_tile_set = collections.defaultdict(set) # levelId -> tileKey 集合

for lid, raw in rows:
    try:
        data = json.loads(raw)
    except Exception:
        continue
    tiles = data.get("tiles", [])
    if not tiles:
        continue
    grid = data.get("grid", [])
    # 收集该关实际使用的地块索引
    used = set()
    for row in grid:
        for idx in row:
            used.add(idx)
    for idx in used:
        if idx < len(tiles):
            tk = tiles[idx][0]
            tile_counter[tk] += 1
            tile_levels[tk].add(lid)
            level_tile_set[lid].add(tk)

print(f"全库关卡数（有地图）: {len(level_tile_set)}")
print(f"不同 tileKey 总数: {len(tile_counter)}")
print()
print("=== 按出现关卡数排序（含关卡名示例） ===")
for tk, cnt in tile_counter.most_common():
    n_lev = len(tile_levels[tk])
    sample = sorted(tile_levels[tk])[:3]
    print(f"{tk:32s} 地块次数={cnt:8d} 关卡数={n_lev:5d}  例: {', '.join(sample)}")
