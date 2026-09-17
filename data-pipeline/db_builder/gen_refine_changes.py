"""
生成 operators 技能/干员描述修正清单（只读，不修改数据库）
输出：refine_changes_20260801.md
覆盖：
  1. 技能描述 负号取反（{-key} 前缀负号语义）
  2. 技能描述 占位符大小写不匹配（描述大写 vs blackboard 小写）
  3. 技能描述 {duration} 缺失（从 level.duration 补）
  4. 干员描述 占位符（值来自 trait blackboard，多候选取值需用户定夺）
"""
import sqlite3, json, os, re, sys, datetime

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import blackboard

DB = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'arknights.db')
ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..',
                    'data-source', 'ArknightsGameData-master', 'zh_CN', 'gamedata')
SKT = json.load(open(os.path.join(ROOT, 'excel', 'skill_table.json'), encoding='utf-8'))
CT = json.load(open(os.path.join(ROOT, 'excel', 'character_table.json'), encoding='utf-8'))
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'refine_changes_20260801.md')

_PAT = re.compile(r'\{(-?)([^}:]+)(?::([^}]+))?\}')

def replace_enhanced(text, bb, duration=None):
    """增强版替换：取反 + 大小写不敏感 + duration 兜底"""
    if not text:
        return text, []
    text = blackboard.clean_desc(text)
    bb_lower = {k.lower(): (k, v) for k, v in bb.items()}
    used = set()
    def repl(m):
        sign, key, fmt = m.group(1), m.group(2), m.group(3)
        kk = key.lower()
        if kk in bb_lower:
            used.add(bb_lower[kk][0])
            val = bb_lower[kk][1]
        elif kk == 'duration' and duration is not None:
            used.add('__duration__')
            val = duration
        else:
            return m.group(0)
        if sign == '-':
            val = -val
        return blackboard.format_value(val, fmt)
    return _PAT.sub(repl, text), [k for k in bb if k not in used]

def replace_orig(text, bb):
    return blackboard.replace(text, bb)

def classify(raw, bb, duration, cur_text):
    """判定修复类型"""
    types = set()
    if '</>' in (cur_text or ''):
        types.add('孤立标签')
    bb_keys_lower = {k.lower() for k in bb}
    for m in _PAT.finditer(raw):
        sign, key, fmt = m.group(1), m.group(2), m.group(3)
        kk = key.lower()
        if sign == '-':
            types.add('负号取反')
        elif key not in bb and kk in bb_keys_lower:
            types.add('大小写')
        elif kk == 'duration' and kk not in bb_keys_lower:
            types.add('补duration')
    return '+'.join(sorted(types)) or '其他'

conn = sqlite3.connect(DB)
cur = conn.cursor()

# ── 技能描述 ──
cur.execute('SELECT 干员ID, 名称, 技能列表 FROM operators WHERE 技能列表 IS NOT NULL')
skill_rows = []
for cid, name, sk_json in cur.fetchall():
    try:
        skills = json.loads(sk_json)
    except Exception:
        continue
    for sk in skills:
        sid = sk.get('技能ID')
        sk_data = SKT.get(sid)
        if not sk_data:
            continue
        for lv in sk.get('各级详情', []):
            li = lv.get('等级', 1) - 1
            if li < 0 or li >= len(sk_data['levels']):
                continue
            raw = sk_data['levels'][li].get('description', '') or ''
            bb = {b['key']: b['value'] for b in sk_data['levels'][li].get('blackboard', [])}
            dur = sk_data['levels'][li].get('duration')
            cur_text = lv.get('描述')
            enhanced, _ = replace_enhanced(raw, bb, dur)
            if cur_text != enhanced:
                skill_rows.append((name, sk.get('技能名', ''), lv.get('等级', 1),
                                   classify(raw, bb, dur, cur_text), cur_text, enhanced))

# ── 干员描述 ──
cur.execute('SELECT 干员ID, 名称, 描述 FROM operators WHERE 描述 IS NOT NULL')
desc_rows = []
pat_ph = re.compile(r'\{[^{}]*\}')
for cid, name, desc in cur.fetchall():
    if not pat_ph.search(desc or ''):
        continue
    cv = CT.get(cid, {})
    trait = cv.get('trait') or {}
    cands = trait.get('candidates') or []
    for cand in cands:
        bb = {b['key']: b['value'] for b in cand.get('blackboard', [])}
        enhanced, _ = replace_enhanced(desc, bb)
        if enhanced != desc:
            phase = (cand.get('unlockCondition') or {}).get('phase', '?')
            desc_rows.append((name, phase, desc, enhanced))

# ── 输出 ──
lines = []
lines.append('# operators 文本修正清单')
lines.append('')
lines.append(f'- 生成时间：{datetime.datetime.now().strftime("%Y-%m-%d %H:%M")}')
lines.append(f'- 技能描述变更：{len(skill_rows)} 条')
lines.append(f'- 干员描述变更：{len(desc_rows)} 条（按 trait 候选展开）')
lines.append('')
lines.append('## 一、技能描述')
lines.append('')
lines.append('| 干员 | 技能 | 等级 | 类型 | 旧文本 | 新文本 |')
lines.append('|:--|:--|:--|:--|:--|:--|')
for name, sname, lv, typ, old, new in skill_rows:
    lines.append(f'| {name} | {sname} | {lv} | {typ} | {old} | {new} |')
lines.append('')
lines.append('## 二、干员描述')
lines.append('')
lines.append('注：多候选取值按 trait 解锁条件展开，最终取哪个阶段的值需确认。')
lines.append('')
lines.append('| 干员 | trait阶段 | 旧文本 | 新文本 |')
lines.append('|:--|:--|:--|:--|')
for name, phase, old, new in desc_rows:
    lines.append(f'| {name} | {phase} | {old} | {new} |')

with open(OUT, 'w', encoding='utf-8') as f:
    f.write('\n'.join(lines))

print(f'技能描述: {len(skill_rows)} 条')
print(f'干员描述: {len(desc_rows)} 条（含多候选展开）')
print(f'输出: {OUT}')
