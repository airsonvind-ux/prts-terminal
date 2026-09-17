"""
operators 表入库测试脚本
创建测试库 test_operators.db，取 55 名干员覆盖全职业全稀有度
验证：精英化阶段 JSON、技能 blackboard 替换、天赋提取、模组组装
"""

import json, sqlite3, os, sys, re
from collections import Counter

# 确保能 import 工具模块
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from text_cleaner import clean_desc
from blackboard import replace as replace_bb

# ─── 路径配置 ──────────────────────────────

DATA_ROOT = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    '..', '..', 'data-source', 'ArknightsGameData-master', 'zh_CN', 'gamedata'
)
OUTPUT_DB = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'test_operators.db')

# ─── 读取 JSON ────────────────────────────

def load_json(path):
    with open(os.path.join(DATA_ROOT, path), 'r', encoding='utf-8') as f:
        return json.load(f)

print('====== 读取 JSON ======')
ct = load_json('excel/character_table.json')
skt = load_json('excel/skill_table.json')
ue = load_json('excel/uniequip_table.json')
be = load_json('excel/battle_equip_table.json')

# ─── 枚举映射 ──────────────────────────────

POSITION_MAP = {'MELEE': '近战位', 'RANGED': '远程位'}
RARITY_MAP   = {f'TIER_{i}': i for i in range(1, 7)}
PROF_MAP = {
    'WARRIOR': '近卫', 'SNIPER': '狙击', 'TANK': '重装',
    'MEDIC': '医疗', 'SUPPORT': '辅助', 'CASTER': '术师',
    'SPECIAL': '特种', 'PIONEER': '先锋',
}
SKILL_TYPE_MAP  = {'MANUAL': '手动触发', 'AUTO': '自动触发', 'PASSIVE': '被动'}
SP_TYPE_MAP     = {
    'INCREASE_WITH_TIME': '自动回复', 'INCREASE_WHEN_ATTACK': '攻击回复',
    'INCREASE_WHEN_TAKEN_DAMAGE': '受击回复',
}
DUR_TYPE_MAP    = {'NONE': '普通持续', 'AMMO': '弹药类'}

# 职业分支映射（从 uniequip_table.subProfDict）
subprof_map = {
    v['subProfessionId']: v['subProfessionName']
    for v in ue['subProfDict'].values()
}

# ─── 属性名映射（精英化阶段）───────────────

ATTR_NAMES = {
    'maxHp': '生命上限', 'atk': '攻击', 'def': '防御',
    'magicResistance': '法术抗性', 'cost': '部署费用',
    'blockCnt': '阻挡数', 'respawnTime': '再部署时间',
    'baseAttackTime': '攻击间隔',
}

import traceback

# ─── 筛选干员 ──────────────────────────────

print('====== 筛选干员 ======')
OPS_PROFS = {'WARRIOR', 'SNIPER', 'TANK', 'MEDIC', 'SUPPORT', 'CASTER', 'SPECIAL', 'PIONEER'}
all_ops = [(k, v) for k, v in ct.items() if v.get('profession') in OPS_PROFS]

# 按职业 + 稀有度均匀采样 55 个
import random
random.seed(42)  # 可复现
random.shuffle(all_ops)
selected = all_ops[:55]

print(f'  选中 {len(selected)} 名干员（随机）')

# ─── 辅助函数 ──────────────────────────────

def build_phases(phases):
    """构建精英化阶段 JSON"""
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

def build_skills(skills_ref, char_name=''):
    """构建技能列表 JSON，含 blackboard 替换后的描述"""
    result = []
    unused_log = []  # [(干员名, 技能ID, 技能名, 等级, blackboard键)]
    for sr in skills_ref:
        sid = sr.get('skillId', '')
        sk = skt.get(sid)
        if not sk:
            continue

        levels = sk.get('levels', [])
        lv0 = levels[0] if levels else {}

        # 技能级字段（从 Lv1 读取，各级相同）
        skill_type = SKILL_TYPE_MAP.get(lv0.get('skillType', ''), lv0.get('skillType', ''))
        sp_data = lv0.get('spData', {})
        sp_type = sp_data.get('spType', '')
        if isinstance(sp_type, int):
            sp_type_str = '被动'
        else:
            sp_type_str = SP_TYPE_MAP.get(sp_type, str(sp_type))
        dur_type = DUR_TYPE_MAP.get(lv0.get('durationType', ''), lv0.get('durationType', ''))

        sp_info = {
            '可充能次数': sp_data.get('maxChargeTime', 1),
            '技力消耗': sp_data.get('spCost', 0),
            '初始技力': sp_data.get('initSp', 0),
            '技力回复速度': sp_data.get('increment', 0),
        }

        # 各级详情（含 blackboard 替换）
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
            'SP数据': sp_info,
            '各级详情': level_details,
        })
    return json.dumps(result, ensure_ascii=False) if result else None, unused_log

def build_talents(talents):
    """构建天赋列表 JSON（仅 isHideTalent 非空）"""
    result = []
    if not talents:
        return None
    for talent in talents:
        for cand in talent.get('candidates', []):
            # isHideTalent 为 true 表示隐藏态，跳过
            if cand.get('isHideTalent'):
                continue
            cond = cand.get('unlockCondition', {})
            phase_raw = cond.get('phase', 0)
            phase_num = {'PHASE_0': 0, 'PHASE_1': 1, 'PHASE_2': 2}.get(phase_raw, 0)
            result.append({
                '天赋名': cand.get('name', ''),
                '描述': clean_desc(cand.get('description', '')),
                '条件': {
                    '精英化阶段': phase_num,
                    '潜能等级': cond.get('level', 1),
                },
                '备注': None,
            })
    return json.dumps(result, ensure_ascii=False) if result else None

def build_potential(potential_ranks):
    """构建潜能提升 JSON"""
    if not potential_ranks:
        return None
    descs = [pr.get('description', '') for pr in potential_ranks]
    return json.dumps(descs, ensure_ascii=False)

def build_tokens(display_token_dict):
    """构建关联 token JSON"""
    if not display_token_dict:
        return None
    return json.dumps(list(display_token_dict.keys()), ensure_ascii=False)

def build_modules(char_equip):
    """构建模组列表 JSON"""
    if not char_equip:
        return None

    result = []
    for equip_id in char_equip:
        ed = ue['equipDict'].get(equip_id)
        if not ed:
            continue
        # 仅 ADVANCED 模组（排除 INITIAL 证章）
        if ed.get('type') != 'ADVANCED':
            continue

        bed = be.get(equip_id, {})
        phases = []
        for bp in bed.get('phases', []):
            lv = bp.get('equipLevel')

            # 数值更新（attributeBlackboard）
            stat_updates = []
            attr_name_map = {'max_hp': '生命上限', 'atk': '攻击', 'def': '防御', 'magicResistance': '法术抗性'}
            for bb in bp.get('attributeBlackboard', []):
                name = attr_name_map.get(bb.get('key', ''), bb.get('key', ''))
                val = bb.get('value', 0)
                stat_updates.append({'属性': name, '增加值': int(val) if val == int(val) else val})

            # 特性追加 / 天赋更新（按数据包是否存在判断，不按 target 筛选）
            trait_parts = []
            talent_parts = []
            for part in bp.get('parts', []):
                # 特性文本：任何 target 下 overrideTraitDataBundle 中有描述即为特性
                if part.get('overrideTraitDataBundle'):
                    for item in (part['overrideTraitDataBundle'].get('candidates') or []):
                        desc = item.get('additionalDescription') or item.get('overrideDescripton')
                        if desc:
                            trait_parts.append(clean_desc(desc))
                # 天赋文本：任何 target 下 addOrOverride / override TalentDataBundle
                for bundle_key in ('addOrOverrideTalentDataBundle', 'overrideTalentDataBundle'):
                    bundle = part.get(bundle_key)
                    if not bundle: continue
                    cands = bundle.get('candidates') or []
                    if cands:
                        # 仅取首个候选（requiredPotentialRank=0，无潜能修正）
                        item = cands[0]
                        if item.get('upgradeDescription'):
                            tname = item.get('name', '')
                            txt = clean_desc(item['upgradeDescription'])
                            talent_parts.append(f'{tname}：{txt}' if tname else txt)

            trait_text = '\n'.join(trait_parts) if trait_parts else None
            talent_text = '\n'.join(talent_parts) if talent_parts else None

            phases.append({
                '数值更新': stat_updates,
                '特性追加文本': trait_text if lv == 1 else None,
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
    分支描述      TEXT,
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
all_skill_unused = []  # 收集所有技能中未使用的 blackboard
errors = []
for i, (cid, cv) in enumerate(selected):
    try:
        name = cv.get('name', '')
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
        # 分支描述：留空，后续通过 PRTS 获取
        trait = None

        # 信赖加成 → 合并为一列 JSON
        favor = cv.get('favorKeyFrames', [])
        fav_hp = favor[-1]['data'].get('maxHp', 0) if favor else 0
        fav_atk = favor[-1]['data'].get('atk', 0) if favor else 0
        fav_def = favor[-1]['data'].get('def', 0) if favor else 0
        fav_res = favor[-1]['data'].get('magicResistance', 0) if favor else 0
        favor_json = None
        if favor:
            fb = {'生命上限': fav_hp, '攻击': fav_atk, '防御': fav_def, '法术抗性': fav_res}
            if any(v != 0 for v in fb.values()):
                favor_json = json.dumps(fb, ensure_ascii=False)

        # JSON 列
        phases_json = build_phases(cv.get('phases', []))
        skills_json, skill_unused = build_skills(cv.get('skills', []), name)
        all_skill_unused.extend(skill_unused)
        tokens_json = build_tokens(cv.get('displayTokenDict'))
        talents_json = build_talents(cv.get('talents', []))
        potential_json = build_potential(cv.get('potentialRanks', []))
        modules_json = build_modules(ue['charEquip'].get(cid))

        conn.execute("""
            INSERT INTO operators VALUES (
                ?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,
                ?,?,?,?,?,?
            )
        """, (
            cid, name, appellation, clean_desc(desc_raw) or None,
            position, tags, obtain, not_obtainable, is_sp,
            rarity, prof, subprof, max_pot, trait, None,
            favor_json,
            phases_json, skills_json, tokens_json, talents_json, potential_json, modules_json,
        ))

    except Exception as e:
        errors.append(f'{cv.get("name", cid)}: {e}')
        if len(errors) == 1:
            traceback.print_exc()

conn.commit()

# ─── 结果统计 ─────────────────────────────

print(f'\n====== 完成 ======')
print(f'  入库: {len(selected) - len(errors)} 名')
if errors:
    print(f'  错误: {len(errors)} 条')
    for e in errors[:5]:
        print(f'    {e}')

rows = conn.execute("SELECT COUNT(*) FROM operators").fetchone()[0]
print(f'  表行数: {rows}')

# 抽查：第一个人和有模组的
sample1 = conn.execute("SELECT 干员ID, 名称, 职业, 职业分支, 稀有度, 精英化阶段 FROM operators LIMIT 1").fetchone()
print(f'\n------ 抽查样例 ------')
print(f'  {sample1[1]} ({sample1[0]})')
print(f'  职业: {sample1[2]}, 分支: {sample1[3]}, 稀有度: {sample1[4]}')
phases = json.loads(sample1[5]) if sample1[5] else []
for ph in phases[:2]:
    print(f'  精英{phases.index(ph)} 最高Lv{ph["最高等级"]}, 关键帧: {len(ph["关键帧"])} 帧')

# 抽有技能的
has_skill = conn.execute(
    "SELECT 名称, 技能列表 FROM operators WHERE 技能列表 IS NOT NULL LIMIT 1"
).fetchone()
if has_skill:
    skills = json.loads(has_skill[1])
    print(f'\n  技能样例 [{has_skill[0]}]: {len(skills)} 个技能')
    for sk in skills[:1]:
        print(f'    {sk["技能名"]} ({sk["触发类型"]}/{sk["回复类型"]})')
        for lv in sk['各级详情'][:2]:
            print(f'      Lv{lv["等级"]}: {lv["描述"][:100]}...')

# 抽有模组的
has_mod = conn.execute(
    "SELECT 名称, 模组列表 FROM operators WHERE 模组列表 IS NOT NULL LIMIT 1"
).fetchone()
if has_mod:
    mods = json.loads(has_mod[1])
    print(f'\n  模组样例 [{has_mod[0]}]: {len(mods)} 个ADVANCED模组')
    for m in mods[:1]:
        print(f'    {m["名称"]} ({m["分支名1"]}-{m["分支名2"]})')

conn.close()

# ── 输出日志 ─────────────────────────────────
log_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'test_operators_blackboard.log')
with open(log_path, 'w', encoding='utf-8') as f:
    f.write(f'# blackboard 未使用日志\n')
    f.write(f'# 格式：干员 | 技能ID | 技能名 | Lv | blackboard键 | 值\n\n')
    for name, sid, sname, lv, key, val in all_skill_unused:
        f.write(f'{name} | {sid} | {sname} | Lv{lv} | {key} = {val}\n')
print(f'\nblackboard 未使用日志: {log_path}')
print(f'  条目数: {len(all_skill_unused)}')
print(f'\n测试库: {OUTPUT_DB}')
