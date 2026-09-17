"""
PRTS 终端 — operators 表入库
从 character_table / skill_table / uniequip_table / battle_equip_table 构建干员数据

用法：python build_operators.py
输出：arknights.db 新增 operators 表 + operators_blackboard.log
"""

import json, sqlite3, os, sys, re

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
ERROR_LOG  = os.path.join(OUTPUT_DIR, "operators_errors.log")
BB_LOG     = os.path.join(OUTPUT_DIR, "operators_blackboard.log")

# ─── 读取 JSON ────────────────────────────

def load_json(path):
    with open(os.path.join(DATA_ROOT, path), 'r', encoding='utf-8') as f:
        return json.load(f)

print('====== 读取 JSON ======')
ct  = load_json('excel/character_table.json')
skt = load_json('excel/skill_table.json')
ue  = load_json('excel/uniequip_table.json')
be  = load_json('excel/battle_equip_table.json')

# ─── 枚举映射 ──────────────────────────────

POSITION_MAP = {'MELEE': '近战位', 'RANGED': '远程位'}
RARITY_MAP   = {f'TIER_{i}': i for i in range(1, 7)}
PROF_MAP = {
    'WARRIOR': '近卫', 'SNIPER': '狙击', 'TANK': '重装',
    'MEDIC': '医疗', 'SUPPORT': '辅助', 'CASTER': '术师',
    'SPECIAL': '特种', 'PIONEER': '先锋',
}
SKILL_TYPE_MAP = {'MANUAL': '手动触发', 'AUTO': '自动触发', 'PASSIVE': '被动'}
SP_TYPE_MAP = {
    'INCREASE_WITH_TIME': '自动回复', 'INCREASE_WHEN_ATTACK': '攻击回复',
    'INCREASE_WHEN_TAKEN_DAMAGE': '受击回复',
}
DUR_TYPE_MAP = {'NONE': '普通持续', 'AMMO': '弹药类'}

subprof_map = {
    v['subProfessionId']: v['subProfessionName']
    for v in ue['subProfDict'].values()
}

ATTR_NAMES = {
    'maxHp': '生命上限', 'atk': '攻击', 'def': '防御',
    'magicResistance': '法术抗性', 'cost': '部署费用',
    'blockCnt': '阻挡数', 'respawnTime': '再部署时间',
    'baseAttackTime': '攻击间隔',
}

# ─── 筛选 ──────────────────────────────

OPS_PROFS = {'WARRIOR', 'SNIPER', 'TANK', 'MEDIC', 'SUPPORT', 'CASTER', 'SPECIAL', 'PIONEER'}
operators = [(k, v) for k, v in ct.items() if v.get('profession') in OPS_PROFS]

# 合并 char_patch_table 中的升变/异格形态
cpt = load_json('excel/char_patch_table.json')
for cid, cv in cpt.get('patchChars', {}).items():
    if cv.get('profession') in OPS_PROFS:
        operators.append((cid, cv))

# 重名处理：阿米娅升变形态追加职业后缀
PATCH_RENAME = {
    'char_1001_amiya2': '阿米娅（近卫）',
    'char_1037_amiya3': '阿米娅（医疗）',
}

print(f'  干员数: {len(operators)}')

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

def build_skills(skills_ref, char_name):
    result = []
    unused_log = []
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
            desc_clean, unused_bb = replace_bb(lv.get('description', '') or '', bb)
            for uk in unused_bb:
                unused_log.append((char_name, sid, lv0.get('name', ''), li + 1, uk, bb[uk]))
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
    return json.dumps(result, ensure_ascii=False) if result else None, unused_log

def build_talents(talents):
    """构建天赋列表 JSON，按 prefabKey 分序号，同序号下备选按条件不同展开"""
    if not talents:
        return None

    groups = {}  # prefabKey → {name, candidates[]}
    for talent in (talents or []):
        for cand in talent.get('candidates', []):
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

def build_potential(potential_ranks):
    if not potential_ranks:
        return None
    return json.dumps([pr.get('description', '') for pr in potential_ranks], ensure_ascii=False)

def build_tokens(display_token_dict):
    if not display_token_dict:
        return None
    return json.dumps(list(display_token_dict.keys()), ensure_ascii=False)

def build_modules(char_equip):
    if not char_equip:
        return None
    result = []
    for equip_id in char_equip:
        ed = ue['equipDict'].get(equip_id)
        if not ed or ed.get('type') != 'ADVANCED':
            continue

        bed = be.get(equip_id, {})
        phases = []
        for phases_idx, bp in enumerate(bed.get('phases', [])):
            lv = bp.get('equipLevel')

            stat_updates = []
            attr_name_map = {'max_hp': '生命上限', 'atk': '攻击', 'def': '防御', 'magicResistance': '法术抗性'}
            for bb in bp.get('attributeBlackboard', []):
                name = attr_name_map.get(bb.get('key', ''), bb.get('key', ''))
                val = bb.get('value', 0)
                stat_updates.append({'属性': name, '增加值': int(val) if val == int(val) else val})

            # 合并特性文本，区分追加/覆盖
            trait_parts = []
            talent_parts = []
            for part in bp.get('parts', []):
                if part.get('overrideTraitDataBundle'):
                    for item in (part['overrideTraitDataBundle'].get('candidates') or []):
                        add = item.get('additionalDescription')
                        ovr = item.get('overrideDescripton')
                        item_bb = {b['key']: b['value'] for b in (item.get('blackboard') or [])}
                        if add:
                            add_text, _ = replace_bb(add, item_bb)
                            trait_parts.append(f'特性追加：{add_text}')
                        if ovr:
                            ovr_text, _ = replace_bb(ovr, item_bb)
                            trait_parts.append(f'特性覆盖：{ovr_text}')
                for bundle_key in ('addOrOverrideTalentDataBundle', 'overrideTalentDataBundle'):
                    bundle = part.get(bundle_key)
                    if not bundle:
                        continue
                    cands = bundle.get('candidates') or []
                    if cands:
                        item = cands[0]
                        if item.get('upgradeDescription'):
                            tname = item.get('name', '')
                            txt = clean_desc(item['upgradeDescription'])
                            talent_parts.append(f'{tname}：{txt}' if tname else txt)

            trait_text = '\n'.join(trait_parts) if trait_parts else None
            talent_text = '\n'.join(talent_parts) if talent_parts else None

            phases.append({
                '数值更新': stat_updates,
                '特性': trait_text if phases_idx == 0 else None,
                '天赋更新文本': talent_text if lv in (2, 3) else None,
            })

        result.append({
            '模组ID': equip_id,
            '名称': ed.get('uniEquipName', ''),
            '分支名1': ed.get('typeName1', ''),
            '分支名2': ed.get('typeName2', ''),
            '各级效果': phases,
        })

    return json.dumps(result, ensure_ascii=False) if result else None

# ─── 建库 ─────────────────────────────────

print('\n====== 建库 ======')
conn = sqlite3.connect(OUTPUT_DB)
conn.execute("PRAGMA journal_mode=OFF")
conn.execute("PRAGMA synchronous=OFF")
conn.execute("DROP TABLE IF EXISTS operators")

conn.execute("""
CREATE TABLE operators (
    干员ID        TEXT PRIMARY KEY,
    名称          TEXT,
    代号          TEXT,
    描述          TEXT,
    部署位置      TEXT,
    标签          TEXT,
    获取方式      TEXT,
    是否不可获取  INTEGER,
    是否为异格    INTEGER,
    稀有度        INTEGER,
    职业          TEXT,
    职业分支      TEXT,
    最大潜能      INTEGER,
    备注          TEXT,
    信赖加成      TEXT,
    精英化阶段    TEXT,
    技能列表      TEXT,
    关联token     TEXT,
    天赋列表      TEXT,
    潜能提升      TEXT,
    模组列表      TEXT
)
""")

# ─── 入库 ─────────────────────────────────

print('====== 入库 ======')
errors = []
all_bb_unused = []
success = 0
total = len(operators)

for i, (cid, cv) in enumerate(operators):
    try:
        name = PATCH_RENAME.get(cid, cv.get('name', ''))
        appellation = cv.get('appellation', '')
        desc_raw = cv.get('description') or ''
        position = POSITION_MAP.get(cv.get('position', ''), cv.get('position', ''))
        tags = json.dumps(cv.get('tagList'), ensure_ascii=False) if cv.get('tagList') else None
        obtain = cv.get('itemObtainApproach') or None
        not_obtainable = 1 if cv.get('isNotObtainable') else 0
        is_sp = 1 if cv.get('isSpChar') else 0
        rarity = RARITY_MAP.get(cv.get('rarity', ''), 0)
        prof = PROF_MAP.get(cv.get('profession', ''), cv.get('profession', ''))
        subprof = subprof_map.get(cv.get('subProfessionId', ''), cv.get('subProfessionId', ''))
        max_pot = cv.get('maxPotentialLevel', 0)

        favor = cv.get('favorKeyFrames', [])
        if favor:
            fb = {
                '生命上限': favor[-1]['data'].get('maxHp', 0),
                '攻击': favor[-1]['data'].get('atk', 0),
                '防御': favor[-1]['data'].get('def', 0),
                '法术抗性': favor[-1]['data'].get('magicResistance', 0),
            }
            favor_json = json.dumps(fb, ensure_ascii=False) if any(v != 0 for v in fb.values()) else None
        else:
            favor_json = None

        phases_json   = build_phases(cv.get('phases', []))
        skills_json, bb_unused = build_skills(cv.get('skills', []), name)
        all_bb_unused.extend(bb_unused)
        tokens_json   = build_tokens(cv.get('displayTokenDict'))
        talents_json  = build_talents(cv.get('talents'))
        potential_json = build_potential(cv.get('potentialRanks', []))
        modules_json  = build_modules(ue['charEquip'].get(cid))

        conn.execute("""
            INSERT INTO operators VALUES (
                ?,?,?,?,?,?,?,?,?,?,?,?,?,
                ?,?,?,?,?,?,?,?
            )
        """, (
            cid, name, appellation, clean_desc(desc_raw) or None,
            position, tags, obtain, not_obtainable, is_sp,
            rarity, prof, subprof, max_pot, None, favor_json,
            phases_json, skills_json, tokens_json, talents_json, potential_json, modules_json,
        ))
        success += 1

        if (success % 50) == 0:
            print(f'  [{success}/{total}]')

    except Exception as e:
        errors.append(f'{cv.get("name", cid)}: {e}')

conn.commit()

# ─── 统计 ─────────────────────────────────

print(f'\n====== 完成 ======')
print(f'  入库: {success}/{total}')
print(f'  错误: {len(errors)}')
if errors:
    with open(ERROR_LOG, 'w', encoding='utf-8') as f:
        for e in errors:
            f.write(f'{e}\n')
    print(f'  错误日志: {ERROR_LOG}')

# 各列填充率
cols = ['精英化阶段','技能列表','关联token','天赋列表','潜能提升','模组列表','信赖加成']
for col in cols:
    cnt = conn.execute(f'SELECT COUNT(*) FROM operators WHERE [{col}] IS NOT NULL').fetchone()[0]
    print(f'  {col}: {cnt}/{total}')

conn.close()

# ─── blackboard 日志 ───────────────────────

with open(BB_LOG, 'w', encoding='utf-8') as f:
    f.write(f'# blackboard 未使用日志\n')
    f.write(f'# 格式：干员 | 技能ID | 技能名 | Lv | blackboard键 | 值\n')
    f.write(f'# 总数: {len(all_bb_unused)}\n\n')
    for name, sid, sname, lv, key, val in all_bb_unused:
        f.write(f'{name} | {sid} | {sname} | Lv{lv} | {key} = {val}\n')

print(f'\nblackboard 未使用日志: {BB_LOG}')
print(f'  条目数: {len(all_bb_unused)}')
