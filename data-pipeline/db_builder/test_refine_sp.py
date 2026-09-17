"""
测试 SP 数据修正逻辑（不修改数据库）
验证：从源数据读取各级 spData → 写入各级详情的 SP 字段 → 删除顶层 SP数据
"""

import json, sqlite3, os, sys

# ─── 路径 ──────────────────────────────

DATA_ROOT = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    '..', '..', 'data-source', 'ArknightsGameData-master', 'zh_CN', 'gamedata'
)
DB_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'arknights.db')

# ─── 源数据 ────────────────────────────

with open(os.path.join(DATA_ROOT, 'excel/skill_table.json'), 'r', encoding='utf-8') as f:
    skill_table = json.load(f)

# ─── 字段映射 ──────────────────────────

SP_KEY_MAP = {
    'spCost': '技力消耗',
    'initSp': '初始技力',
    'maxChargeTime': '可充能次数',
    'increment': '技力回复速度',
}

# ─── 辅助函数 ──────────────────────────

def build_per_level_sp(skill_id, detail_count):
    """从 skill_table 获取指定技能的各等级 SP 数据"""
    sk = skill_table.get(skill_id)
    if not sk:
        return None
    levels = sk.get('levels', [])
    result = []
    for i in range(min(detail_count, len(levels))):
        raw_sp = levels[i].get('spData', {})
        sp = {}
        for raw_key, cn_key in SP_KEY_MAP.items():
            val = raw_sp.get(raw_key)
            if val is not None:
                sp[cn_key] = val
        result.append(sp)
    return result

def fix_skills(skills_json):
    """修正单个干员/召唤物的技能列表 JSON。
    返回 (修正后JSON, 变更摘要列表)"""
    if not skills_json:
        return None, []

    skills = json.loads(skills_json)
    changes = []

    for sk in skills:
        skill_id = sk.get('技能ID', '')
        skill_name = sk.get('技能名', '')
        details = sk.get('各级详情', [])

        # 获取各等级 SP
        per_level_sp = build_per_level_sp(skill_id, len(details))
        if not per_level_sp:
            continue

        # 写入各级详情
        sp_added = 0
        for i, lv in enumerate(details):
            if i < len(per_level_sp):
                lv['SP'] = per_level_sp[i]
                sp_added += 1

        # 删除顶层 SP数据
        had_top_sp = 'SP数据' in sk
        if had_top_sp:
            old_sp = sk.pop('SP数据')
            changes.append({
                '技能': skill_name,
                '技能ID': skill_id,
                '等级数': len(details),
                '写入SP等级': sp_added,
                '删除顶层SP': old_sp,
                '各等级SP': per_level_sp,
            })

    return json.dumps(skills, ensure_ascii=False) if skills else None, changes


# ═══════════════════════════════════════════════
# 测试
# ═══════════════════════════════════════════════

print('=' * 60)
print('测试 1：operators — 推进之王（冲锋号令·γ型，SP 逐级变化）')
print('=' * 60)

db = sqlite3.connect(DB_PATH)
row = db.execute(
    "SELECT 名称, 技能列表 FROM operators WHERE 名称='推进之王'"
).fetchone()
name, skills_json = row
new_json, changes = fix_skills(skills_json)

print(f'\n干员: {name}')
for ch in changes:
    print(f'\n  技能: {ch["技能"]} ({ch["技能ID"]})')
    print(f'  删除的顶层 SP: {ch["删除顶层SP"]}')
    print(f'  各级 SP:')
    for i, sp in enumerate(ch['各等级SP']):
        print(f'    Lv{i+1}: {sp}')


print('\n' + '=' * 60)
print('测试 2：operators — 阿米娅（精神爆发，maxChargeTime 变化）')
print('=' * 60)

row = db.execute(
    "SELECT 名称, 技能列表 FROM operators WHERE 名称='阿米娅'"
).fetchone()
name, skills_json = row
if skills_json:
    skills = json.loads(skills_json)
    for sk in skills:
        sid = sk.get('技能ID', '')
        sk_src = skill_table.get(sid, {})
        levels = sk_src.get('levels', [])
        if len(levels) > 1:
            charges = [lv.get('spData', {}).get('maxChargeTime', 1) for lv in levels]
            if len(set(charges)) > 1:
                print(f'\n  {sk.get("技能名")}: maxChargeTime 变化: {charges}')

new_json, changes = fix_skills(skills_json)
for ch in changes:
    print(f'\n  技能: {ch["技能"]}')
    print(f'  删除的顶层 SP: {ch["删除顶层SP"]}')
    for i, sp in enumerate(ch['各等级SP']):
        print(f'    Lv{i+1}: {sp}')


print('\n' + '=' * 60)
print('测试 3：tokens — 有技能的召唤物')
print('=' * 60)

rows = db.execute(
    "SELECT 名称, 技能列表 FROM tokens WHERE 技能列表 IS NOT NULL LIMIT 3"
).fetchall()
for name, skills_json in rows:
    new_json, changes = fix_skills(skills_json)
    if changes:
        print(f'\n  {name}:')
        for ch in changes:
            print(f'    技能: {ch["技能"]}')
            for i, sp in enumerate(ch['各等级SP'][:4]):
                print(f'      Lv{i+1}: {sp}')
            if len(ch['各等级SP']) > 4:
                print(f'      ...（共 {len(ch["各等级SP"])} 级）')


print('\n' + '=' * 60)
print('测试 4：验证修正后 JSON 不含顶层 SP数据')
print('=' * 60)

row = db.execute(
    "SELECT 名称, 技能列表 FROM operators WHERE 名称='推进之王'"
).fetchone()
_, skills_json = row
new_json, _ = fix_skills(skills_json)
fixed = json.loads(new_json)
for sk in fixed:
    assert 'SP数据' not in sk, f"顶层 SP数据 未被删除: {sk.get('技能名')}"
    for lv in sk.get('各级详情', []):
        assert 'SP' in lv, f"Lv{lv['等级']} 缺少 SP 字段"
        assert isinstance(lv['SP'], dict), f"Lv{lv['等级']} SP 不是 dict: {type(lv['SP'])}"
print('  ✅ 所有技能已删除顶层 SP数据，各级详情均含 SP 字段')


print('\n' + '=' * 60)
print('测试 5：无技能干员不受影响')
print('=' * 60)

row = db.execute(
    "SELECT 名称, 技能列表 FROM operators WHERE 技能列表 IS NULL LIMIT 1"
).fetchone()
if row:
    name, skills_json = row
    new_json, changes = fix_skills(skills_json)
    assert new_json is None
    assert len(changes) == 0
    print(f'  ✅ {name}（无技能）不受影响')

print('\n全部测试通过。')
db.close()
