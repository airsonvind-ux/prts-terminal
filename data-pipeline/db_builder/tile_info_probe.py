"""读取 stage_table.json 的 tileInfo 顶层字段"""
import json

p = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata\excel\stage_table.json"
with open(p, encoding="utf-8") as f:
    data = json.load(f)

print("顶层字段:", list(data.keys()))

tile_info = data.get("tileInfo")
print("\ntileInfo 类型:", type(tile_info))
if isinstance(tile_info, dict):
    print("tileInfo 键数量:", len(tile_info))
    keys = list(tile_info.keys())
    print("前 20 个键:", keys[:20])
    # 打印前几个的完整内容
    for k in keys[:5]:
        print(f"\n=== {k} ===")
        print(json.dumps(tile_info[k], ensure_ascii=False, indent=1))
