"""统计每种 tileKey 的通行掩码/高度是否固定（跨关卡一致）"""
import sqlite3, json, collections

conn = sqlite3.connect(r"F:\Obsidian_notes\60.Dev\prts-terminal\src-tauri\resources\arknights.db")
rows = conn.execute("SELECT 地图数据 FROM levels WHERE 地图数据 IS NOT NULL").fetchall()

# tileKey -> set of (高度, 通行掩码)
attrs = collections.defaultdict(set)
for (raw,) in rows:
    try:
        data = json.loads(raw)
    except Exception:
        continue
    for t in data.get("tiles", []):
        if len(t) < 4:
            continue
        tk, h, deploy, mask = t[0], t[1], t[2], t[3]
        attrs[tk].add((h, mask, deploy))

print(f"共 {len(attrs)} 种 tileKey")
fixed, varied = [], []
for tk, s in attrs.items():
    if len(s) == 1:
        fixed.append((tk, s.pop()))
    else:
        varied.append((tk, sorted(s)))

print(f"\n=== 属性固定（{len(fixed)} 种）===")
# 按 (高度,掩码,部署) 分组
by_attr = collections.defaultdict(list)
for tk, a in fixed:
    by_attr[a].append(tk)
for a in sorted(by_attr.keys(), key=lambda x: (x[0], x[1], x[2])):
    print(f"  高度={a[0]} 掩码={a[1]} 部署={a[2]}: {len(by_attr[a])} 种")
    print(f"    例: {sorted(by_attr[a])[:8]}")

print(f"\n=== 属性不固定（{len(varied)} 种）===")
for tk, s in varied:
    print(f"  {tk}: {s}")
