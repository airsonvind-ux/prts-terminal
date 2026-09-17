"""levels 表去键化脚本
对地图数据 tiles、路线、额外路线、预置单位四项做去键+归一化处理。
其他列（波次、分支、关卡符文）不动。
"""
import sqlite3, json, sys, os

# 数据库路径（写 builder 库，后续同步到 runtime）
DB_PATH = os.path.join(
    os.path.dirname(os.path.dirname(__file__)),
    'db_builder', 'arknights.db'
)

# ── 归一化映射 ──
HEIGHT_MAP   = {'LOWLAND': 0, 'HIGHLAND': 1}
BUILD_MAP    = {'NONE': 0, 'MELEE': 1, 'RANGED': 2, 'ALL': 3}
PASSABLE_MAP = {'NONE': 0, 'FLY_ONLY': 2, 'ALL': 3}


# ═══════════════════════════════════════════
# 1. 地图数据 tiles
# ═══════════════════════════════════════════
def process_mapdata(raw: str) -> str:
    d = json.loads(raw)
    new_tiles = []
    for t in d['tiles']:
        ht = t['heightType']
        bt = t['buildableType']
        pm = t['passableMask']
        if isinstance(ht, str): ht = HEIGHT_MAP[ht]
        if isinstance(bt, str): bt = BUILD_MAP[bt]
        if isinstance(pm, str): pm = PASSABLE_MAP[pm]
        new_tiles.append([t['tileKey'], ht, bt, pm])
    return json.dumps({
        'width': d['width'],
        'height': d['height'],
        'grid': d['grid'],
        'tiles': new_tiles
    }, ensure_ascii=False)


# ═══════════════════════════════════════════
# 2. 路线 / 额外路线
# ═══════════════════════════════════════════
def process_routes(raw: str) -> str:
    if not raw or raw == '[]':
        return '[]'
    routes = json.loads(raw)
    new_routes = []
    for r in routes:
        if r is None:
            new_routes.append(None)
            continue
        new_routes.append([
            r['motionMode'],
            r['startPosition'],
            r['endPosition'],
            r['spawnRandomRange'],
            r['spawnOffset'],
            r['checkpoints']
        ])
    return json.dumps(new_routes, ensure_ascii=False)


# ═══════════════════════════════════════════
# 3. 预置单位
# ═══════════════════════════════════════════
def process_predefines(raw: str) -> str:
    if not raw or raw == '{}':
        return None
    pre = json.loads(raw)

    def xform_inst(item):
        return [
            item.get('alias'),
            item['characterKey'],
            item['level'],
            item['phase'],
            item['skillIndex'],
            item['mainSkillLvl'],
            item.get('uniEquipIds'),
            [item['position']['row'], item['position']['col']],
            item['direction']
        ]

    def xform_token_card(item):
        return [
            item.get('alias'),
            item['characterKey'],
            item['level'],
            item['phase'],
            item['skillIndex'],
            item['mainSkillLvl'],
            item.get('uniEquipIds'),
            item['initialCnt']
        ]

    def xform_char_card(item):
        return [
            item.get('alias'),
            item['characterKey'],
            item['level'],
            item['phase'],
            item['skillIndex'],
            item['mainSkillLvl'],
            item.get('uniEquipIds')
        ]

    result = [
        [xform_inst(i)      for i in (pre.get('tokenInsts')     or [])],
        [xform_inst(i)      for i in (pre.get('characterInsts')  or [])],
        [xform_token_card(i) for i in (pre.get('tokenCards')     or [])],
        [xform_char_card(i)  for i in (pre.get('characterCards')  or [])],
    ]
    return json.dumps(result, ensure_ascii=False)


# ═══════════════════════════════════════════
# 主流程
# ═══════════════════════════════════════════
def main():
    if not os.path.exists(DB_PATH):
        print(f'数据库不存在: {DB_PATH}')
        sys.exit(1)

    conn = sqlite3.connect(DB_PATH)
    conn.execute('PRAGMA journal_mode=WAL')

    total = conn.execute('SELECT COUNT(*) FROM levels').fetchone()[0]
    print(f'levels 表共 {total} 行，开始处理...')

    # 批量读取→转换→写入
    rows = conn.execute(
        'SELECT levelId, 地图数据, 路线, 额外路线, 预置单位 FROM levels'
    ).fetchall()

    conn.execute('BEGIN')
    updated = 0
    for i, (lid, map_raw, route_raw, extra_raw, pre_raw) in enumerate(rows):
        new_map = process_mapdata(map_raw)
        new_route = process_routes(route_raw)
        new_extra = process_routes(extra_raw)
        new_pre = process_predefines(pre_raw)

        conn.execute('''
            UPDATE levels SET
                地图数据 = ?,
                路线 = ?,
                额外路线 = ?,
                预置单位 = ?
            WHERE levelId = ?
        ''', (new_map, new_route, new_extra, new_pre, lid))
        updated += 1

        if (i + 1) % 500 == 0:
            conn.execute('COMMIT')
            conn.execute('BEGIN')
            print(f'  {i+1}/{total}')

    conn.execute('COMMIT')

    # VACUUM 回收空间
    print(f'处理完成 {updated} 行，执行 VACUUM...')
    conn.execute('VACUUM')

    # 验证：抽查几条
    print('\n验证抽查:')
    for lid in ['level_a001_01', 'level_act10d5_01', 'level_act10mini_10']:
        row = conn.execute(
            'SELECT 地图数据, 路线, 额外路线, 预置单位 FROM levels WHERE levelId = ?',
            (lid,)
        ).fetchone()
        if row:
            map_d = json.loads(row[0])
            routes = json.loads(row[1])
            extra = json.loads(row[2])
            pre = json.loads(row[3]) if row[3] else None

            tile = map_d['tiles'][0]
            ok_tile = isinstance(tile, list) and isinstance(tile[1], int) and not isinstance(tile[1], str)
            print(f'  {lid}: tiles={ok_tile}, routes={len(routes)}条, extra={len(extra)}条, pre={"有" if pre else "无"}')
        else:
            print(f'  {lid}: 不存在')

    conn.close()
    print('\n✓ 完成')


if __name__ == '__main__':
    main()
