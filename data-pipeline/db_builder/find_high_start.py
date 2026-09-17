"""找出 level_act6d5_04 中 tile_start 高度=1 的坐标"""
import sqlite3, json

conn = sqlite3.connect(r"F:\Obsidian_notes\60.Dev\prts-terminal\src-tauri\resources\arknights.db")
row = conn.execute(
    "SELECT levelId, 地图数据 FROM levels WHERE levelId='level_act6d5_04'"
).fetchone()
print("levelId:", row[0])
data = json.loads(row[1])

width, height = data["width"], data["height"]
tiles = data["tiles"]
grid = data["grid"]

# 找出高度=1 的 tile_start 的 tiles 索引
start_idx_high = []
for i, t in enumerate(tiles):
    if t[0] == "tile_start" and t[1] == 1:
        start_idx_high.append(i)

print("高度=1 的 tile_start 的 tiles 索引:", start_idx_high)
for idx in start_idx_high:
    print(f"  tiles[{idx}] = {tiles[idx]}")

# 在 grid 中定位这些索引的位置（grid 行从上到下 = 大 row 在上）
# grid[r][c]：r 是行（grid 存储顺序，第 0 行是地图顶部？需确认）
print("\ngrid 尺寸:", len(grid), "x", len(grid[0]))
for r, grid_row in enumerate(grid):
    for c, tile_idx in enumerate(grid_row):
        if tile_idx in start_idx_high:
            # grid 第 0 行对应坐标 row = height-1
            coord_row = height - 1 - r
            print(f"  坐标 (row={coord_row}, col={c})  即 (x={c}, y={coord_row})")
