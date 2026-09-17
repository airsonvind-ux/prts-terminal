"""
PRTS 终端 — tokens 表入库
从 character_table / skill_table 构建召唤物数据

用法：python build_tokens.py
"""

import json, sqlite3, os, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from text_cleaner import clean_desc
from blackboard import replace as replace_bb

# ─── 路径配置 ──────────────────────────────

DATA_ROOT = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    '..', '..', 'data-source', 'ArknightsGameData-master', 'zh_CN', 'gamedata'
)
OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB  = os.path.join(OUTPUT_DIR, "arknights.db")

# ─── 读取 JSON ────────────────────────────

def load_json(path):
    with open(os.path.join(DATA_ROOT, path), 'r', encoding='utf-8') as f:
        return json.load(f)

print('====== 读取 JSON ======')
ct  = load_json('excel/character_table.json')
skt = load_json('excel/skill_table.json')

# ─── 枚举映射 ──────────────────────────────

POSITION_MAP  = {'MELEE': '近战位', 'RANGED': '远程位', 'ALL': '全部位', 'NONE': '无'}
SKILL_TYPE_MAP = {'MANUAL': '手动触发', 'AUTO': '自动触发', 'PASSIVE': '被动'}
SP_TYPE_MAP = {
    'INCREASE_WITH_TIME': '自动回复', 'INCREASE_WHEN_ATTACK': '攻击回复',
    'INCREASE_WHEN_TAKEN_DAMAGE': '受击回复',
}
DUR_TYPE_MAP = {'NONE': '普通持续', 'AMMO': '弹药类'}

ATTR_NAMES = {
    'maxHp': '生命上限', 'atk': '攻击', 'def': '防御',
    'magicResistance': '法术抗性',
}

# ─── 筛选 + 持有者反查 ──────────────────

tokens_raw = {k: v for k, v in ct.items() if v.get('profession') == 'TOKEN'}
print(f'  token 数: {len(tokens_raw)}')

# 反查持有者：从 operators 的 displayTokenDict 建立映射
token_owner = {}
ops = {k: v for k, v in ct.items() if v.get('profession') in {
    'WARRIOR','SNIPER','TANK','MEDIC','SUPPORT','CASTER','SPECIAL','PIONEER'
}}
for cid, cv in ops.items():
    for tid in (cv.get('displayTokenDict') or {}):
        token_owner[tid] = cv.get('name', cid)

# ─── 辅助函数 ──────────────────────────────

def build_phases(phases):
    result = []
    for ph in phases:
        frames = []
        for af in ph['attributesKeyFrames']:
            attrs = {}
            for raw_key, cn_name in ATTR_NAMES.items():
                attrs[cn_name] = af['data'].get(raw_key, 0)
            frames.append({'等级': af['level'], '属性': attrs})
        result.append({
            '范围ID': ph.get('rangeId', ''),
            '最高等级': ph['maxLevel'],
            '关键帧': frames,
        })
    return json.dumps(result, ensure_ascii=False)

def build_skills(skills_ref):
    result = []
    for sr in skills_ref:
        sid = sr.get('skillId', '')
        sk = skt.get(sid)
        if not sk:
            continue
        levels = sk.get('levels', [])
        lv0 = levels[0] if levels else {}

        skill_type = SKILL_TYPE_MAP.get(lv0.get('skillType', ''), lv0.get('skillType', ''))
        sp_data = lv0.get('spData', {})
        sp_type = sp_data.get('spType', '')
        sp_type_str = '被动' if isinstance(sp_type, int) else SP_TYPE_MAP.get(sp_type, str(sp_type))
        dur_type = DUR_TYPE_MAP.get(lv0.get('durationType', ''), lv0.get('durationType', ''))

        level_details = []
        for li, lv in enumerate(levels):
            bb = {b['key']: b['value'] for b in lv.get('blackboard', [])}
            desc_clean, _ = replace_bb(lv.get('description', '') or '', bb)
            level_details.append({
                '等级': li + 1,
                '范围ID': lv.get('rangeId'),
                '描述': desc_clean,
                '持续时间': lv.get('duration'),
            })

        result.append({
            '技能ID': sid,
            '技能名': lv0.get('name', ''),
            '触发类型': skill_type,
            '回复类型': sp_type_str,
            '持续类型': dur_type,
            'SP数据': {
                '可充能次数': sp_data.get('maxChargeTime', 1),
                '技力消耗': sp_data.get('spCost', 0),
                '初始技力': sp_data.get('initSp', 0),
                '技力回复速度': sp_data.get('increment', 0),
            },
            '各级详情': level_details,
        })
    return json.dumps(result, ensure_ascii=False) if result else None

def build_talents(talents):
    """构建天赋列表 JSON，与 operators 结构一致"""
    if not talents:
        return None
    groups = {}
    for talent in (talents or []):
        for cand in (talent.get('candidates') or []):
            if cand.get('isHideTalent'):
                continue
            pk = cand.get('prefabKey', '')
            if pk not in groups:
                groups[pk] = {'name': cand.get('name', ''), 'candidates': []}
            cond = cand.get('unlockCondition', {})
            phase_raw = cond.get('phase', 0)
            phase_num = {'PHASE_0': 0, 'PHASE_1': 1, 'PHASE_2': 2}.get(phase_raw, 0)
            groups[pk]['candidates'].append({
                '描述': clean_desc(cand.get('description', '')),
                '条件': {
                    '精英化阶段': phase_num,
                    '潜能等级': cand.get('requiredPotentialRank', 0),
                },
            })
    result = []
    for pk in sorted(groups.keys()):
        g = groups[pk]
        result.append({
            '序号': pk,
            '天赋名': g['name'],
            '备注': None,
            '备选': g['candidates'],
        })
    return json.dumps(result, ensure_ascii=False) if result else None

# ─── 建库 ─────────────────────────────────

print('\n====== 建库 ======')
conn = sqlite3.connect(OUTPUT_DB)
conn.execute("PRAGMA journal_mode=OFF")
conn.execute("DROP TABLE IF EXISTS tokens")

conn.execute("""
CREATE TABLE tokens (
    tokenID    TEXT PRIMARY KEY,
    名称       TEXT,
    代号       TEXT,
    特性       TEXT,
    持有者     TEXT,
    部署位置   TEXT,
    部署费用   INTEGER,
    阻挡数     INTEGER,
    嘲讽等级   INTEGER,
    再部署时间 INTEGER,
    攻击速度   REAL,
    攻击间隔   REAL,
    精英化阶段 TEXT,
    技能列表   TEXT,
    天赋列表   TEXT,
    备注       TEXT
)
""")

# ─── 入库 ─────────────────────────────────

print('====== 入库 ======')
errors = []
success = 0

for tid, cv in tokens_raw.items():
    try:
        lv0_data = cv['phases'][0]['attributesKeyFrames'][0]['data']

        conn.execute("""
            INSERT INTO tokens VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
        """, (
            tid,
            cv.get('name', ''),
            cv.get('appellation', ''),
            clean_desc(cv.get('description') or '') or None,
            token_owner.get(tid),
            POSITION_MAP.get(cv.get('position', ''), cv.get('position', '')),
            lv0_data.get('cost', 0),
            lv0_data.get('blockCnt', 0),
            lv0_data.get('tauntLevel', 0),
            lv0_data.get('respawnTime', 0),
            lv0_data.get('attackSpeed', 0),
            lv0_data.get('baseAttackTime', 0),
            build_phases(cv.get('phases', [])),
            build_skills(cv.get('skills', [])),
            build_talents(cv.get('talents')),
            None,  # 备注，后续 PRTS 爬取
        ))
        success += 1

    except Exception as e:
        errors.append(f'{cv.get("name", tid)}: {e}')

conn.commit()

# ─── 统计 ─────────────────────────────────

print(f'\n====== 完成 ======')
print(f'  入库: {success}/{len(tokens_raw)}')
if errors:
    print(f'  错误: {len(errors)}')
    for e in errors[:5]:
        print(f'    {e}')

for col in ['精英化阶段', '技能列表', '持有者']:
    cnt = conn.execute(f'SELECT COUNT(*) FROM tokens WHERE [{col}] IS NOT NULL').fetchone()[0]
    print(f'  {col}: {cnt}/{success}')

conn.close()
