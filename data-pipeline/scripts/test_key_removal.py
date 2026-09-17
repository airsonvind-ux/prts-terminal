"""去键化 —— 逐条对照测试
只展示单条数据的前后 JSON，不做统计。
"""
import sqlite3, json

DB = r'F:\Obsidian_notes\60.Dev\prts-terminal\data-pipeline\db_builder\arknights.db'

HEIGHT_MAP   = {'LOWLAND': 0, 'HIGHLAND': 1}
BUILD_MAP    = {'NONE': 0, 'MELEE': 1, 'RANGED': 2, 'ALL': 3}
PASSABLE_MAP = {'NONE': 0, 'FLY_ONLY': 2, 'ALL': 3}


def show(label, old, new):
    print(f'--- {label} ---')
    print(f'旧: {json.dumps(old, ensure_ascii=False)}')
    print(f'新: {json.dumps(new, ensure_ascii=False)}')
    print()


conn = sqlite3.connect(DB)

# ── 1. tiles：字符串格式 → 归一化 + 去键 ──
lid = 'level_a001_01'
row = conn.execute('SELECT 地图数据 FROM levels WHERE levelId = ?', (lid,)).fetchone()
tiles = json.loads(row[0])['tiles']
for i in range(min(3, len(tiles))):
    t = tiles[i]
    ht = t['heightType']
    bt = t['buildableType']
    pm = t['passableMask']
    ht2 = HEIGHT_MAP[ht] if isinstance(ht, str) else ht
    bt2 = BUILD_MAP[bt] if isinstance(bt, str) else bt
    pm2 = PASSABLE_MAP[pm] if isinstance(pm, str) else pm
    show(f'{lid} tiles[{i}] (字符串→int)',
        t,
        [t['tileKey'], ht2, bt2, pm2])

# ── 2. tiles：int 格式 → 去键 ──
lid = 'level_act10d5_01'
row = conn.execute('SELECT 地图数据 FROM levels WHERE levelId = ?', (lid,)).fetchone()
tiles = json.loads(row[0])['tiles']
for i in range(min(3, len(tiles))):
    t = tiles[i]
    show(f'{lid} tiles[{i}] (int直通)',
        t,
        [t['tileKey'], t['heightType'], t['buildableType'], t['passableMask']])

# ── 3. 路线：正常 ──
lid = 'level_a001_01'
row = conn.execute('SELECT 路线 FROM levels WHERE levelId = ?', (lid,)).fetchone()
routes = json.loads(row[0])
r = routes[0]
show(f'{lid} 路线[0] (正常)',
    r,
    [r['motionMode'], r['startPosition'], r['endPosition'],
     r['spawnRandomRange'], r['spawnOffset'], r['checkpoints']])

# ── 4. 路线：null 占位 ──
lid = 'level_act10mini_10'
row = conn.execute('SELECT 路线 FROM levels WHERE levelId = ?', (lid,)).fetchone()
routes = json.loads(row[0])
# 显示前3个元素（包含 null）
old_arr = routes[:3]
new_arr = []
for r in routes[:3]:
    if r is None:
        new_arr.append(None)
    else:
        new_arr.append([r['motionMode'], r['startPosition'], r['endPosition'],
                        r['spawnRandomRange'], r['spawnOffset'], r['checkpoints']])
show(f'{lid} 路线[0:3] (含null占位)',
    old_arr,
    new_arr)

# ── 5. 预置单位：characterInsts ──
lid = 'level_a001_01'
row = conn.execute('SELECT 预置单位 FROM levels WHERE levelId = ?', (lid,)).fetchone()
pre = json.loads(row[0])
for key, idx in [('characterInsts', 1)]:
    arr = pre.get(key)
    if arr:
        item = arr[0]
        show(f'{lid} {key}[0] (9位)',
            item,
            [item.get('alias'), item['characterKey'], item['level'], item['phase'],
             item['skillIndex'], item['mainSkillLvl'], item.get('uniEquipIds'),
             [item['position']['row'], item['position']['col']], item['direction']])

# ── 6. 预置单位：tokenCards ──
lid = 'level_act10d5_06'
row = conn.execute('SELECT 预置单位 FROM levels WHERE levelId = ?', (lid,)).fetchone()
pre = json.loads(row[0])
for key, idx in [('tokenCards', 2)]:
    arr = pre.get(key)
    if arr:
        item = arr[0]
        show(f'{lid} {key}[0] (8位)',
            item,
            [item.get('alias'), item['characterKey'], item['level'], item['phase'],
             item['skillIndex'], item['mainSkillLvl'], item.get('uniEquipIds'),
             item['initialCnt']])

# ── 7. 预置单位：characterCards ──
lid = 'level_act10mini_10'
row = conn.execute('SELECT 预置单位 FROM levels WHERE levelId = ?', (lid,)).fetchone()
pre = json.loads(row[0])
for key, idx in [('characterCards', 3)]:
    arr = pre.get(key)
    if arr:
        item = arr[0]
        show(f'{lid} {key}[0] (7位)',
            item,
            [item.get('alias'), item['characterKey'], item['level'], item['phase'],
             item['skillIndex'], item['mainSkillLvl'], item.get('uniEquipIds')])

# ── 8. 额外路线 ──
lid = 'level_act12d5_ex06'
row = conn.execute('SELECT 额外路线 FROM levels WHERE levelId = ?', (lid,)).fetchone()
if row and row[0]:
    extra = json.loads(row[0])
    if extra:
        e = extra[0]
        show(f'{lid} 额外路线[0]',
            e,
            [e['motionMode'], e['startPosition'], e['endPosition'],
             e['spawnRandomRange'], e['spawnOffset'], e['checkpoints']])
else:
    # 退而求其次
    for lid2, raw in conn.execute(
        'SELECT levelId, 额外路线 FROM levels WHERE 额外路线 IS NOT NULL AND 额外路线 != "[]" LIMIT 1'
    ):
        extra = json.loads(raw)
        e = extra[0]
        show(f'{lid2} 额外路线[0]',
            e,
            [e['motionMode'], e['startPosition'], e['endPosition'],
             e['spawnRandomRange'], e['spawnOffset'], e['checkpoints']])
        break

conn.close()
print('✓ 逐条对照完成')
