"""导出 tileInfo 全部内容，并对比 DB 中的 106 种 tileKey"""
import json

p = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata\excel\stage_table.json"
with open(p, encoding="utf-8") as f:
    data = json.load(f)

tile_info = data.get("tileInfo", {})
print(f"tileInfo 共 {len(tile_info)} 种：")
print("=" * 100)
for k in sorted(tile_info.keys()):
    t = tile_info[k]
    fn = t.get("isFunctional")
    print(f"{k:32s} 功能={str(fn):5s}  {t.get('name',''):12s} | {t.get('description','')}")
