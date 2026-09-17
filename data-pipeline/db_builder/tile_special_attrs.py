"""统计通行视图特殊符号对应的 5 类地块：通行掩码/高度是否统一"""
import sqlite3, json, collections

conn = sqlite3.connect(r"F:\Obsidian_notes\60.Dev\prts-terminal\src-tauri\resources\arknights.db")
rows = conn.execute("SELECT 地图数据 FROM levels WHERE 地图数据 IS NOT NULL").fetchall()

targets = ["tile_start", "tile_flystart", "tile_end", "tile_telin", "tile_telout"]
attrs = collections.defaultdict(set)
deploy_attrs = collections.defaultdict(set)  # 部署类型
examples = collections.defaultdict(list)

for (raw,) in rows:
    try:
        data = json.loads(raw)
    except Exception:
        continue
    for t in data.get("tiles", []):
        if len(t) < 4:
            continue
        tk, h, deploy, mask = t[0], t[1], t[2], t[3]
        if tk in targets:
            attrs[tk].add((h, mask))
            deploy_attrs[tk].add(deploy)
            if len(examples[tk]) < 5:
                examples[tk].append((h, mask, deploy))

for tk in targets:
    print(f"=== {tk} ===")
    print(f"  (高度, 通行掩码) 组合: {sorted(attrs[tk])}")
    print(f"  部署类型: {sorted(deploy_attrs[tk])}")
    print(f"  示例: {examples[tk]}")
