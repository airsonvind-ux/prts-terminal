"""
PRTS 终端 — operators 表文本 refine 脚本
基于现有数据库修正构建期遗留的文本问题，不重跑构建：
  1. 技能描述：{-key} 前缀负号取反、占位符大小写不敏感匹配、{duration} 从 level 数据兜底、孤立 </> 清理
  2. 干员描述：占位符解析；多候选（武者类按精英化阶段）合并为紧凑格式

用法：python refine_operators.py
输出：arknights.db 更新 + refine_operators.log（变更明细）+ refine_errors.log（异常）
"""
import sqlite3, json, os, sys, re, shutil, datetime

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import blackboard

BASE = os.path.dirname(os.path.abspath(__file__))
DB = os.path.join(BASE, 'arknights.db')
ROOT = os.path.join(BASE, '..', '..', 'data-source', 'ArknightsGameData-master', 'zh_CN', 'gamedata')
SKT = json.load(open(os.path.join(ROOT, 'excel', 'skill_table.json'), encoding='utf-8'))
CT = json.load(open(os.path.join(ROOT, 'excel', 'character_table.json'), encoding='utf-8'))
LOG = os.path.join(BASE, 'refine_operators.log')
ERR_LOG = os.path.join(BASE, 'refine_errors.log')
DATE_TAG = datetime.datetime.now().strftime('%Y%m%d_%H%M%S')
BACKUP = os.path.join(BASE, f'arknights_backup_{DATE_TAG}.db')

_PAT = re.compile(r'\{(-?)([^}:]+)(?::([^}]+))?\}')
_PH_MAP = {'PHASE_0': 0, 'PHASE_1': 1, 'PHASE_2': 2}

# ─── 增强替换 ──────────────────────────────

def replace_enhanced(text, bb, duration=None):
    """取反 + 大小写不敏感 + duration 兜底"""
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

def merge_phases(desc_raw, items):
    """多候选合并为紧凑格式：变化数值用 / 连接，末尾标注精英化阶段。

    items: [(phase_num, bb)] 按 phase 升序
    """
    texts = [replace_enhanced(desc_raw, bb)[0] for _, bb in items]
    if len(set(texts)) == 1:
        return texts[0]
    n = len(texts[0])
    if any(len(t) != n for t in texts):
        return None
    diff = {i for i in range(n) if len({t[i] for t in texts}) > 1}
    if not diff:
        return texts[0]
    spans, start, prev = [], None, None
    for i in range(n):
        if i in diff:
            if start is None:
                start = i
            prev = i
        elif start is not None:
            spans.append((start, prev))
            start = None
    if start is not None:
        spans.append((start, prev))
    # 扩展区间到完整数值 token（数字/小数点/百分号/前导负号）
    num_chars = set('0123456789.%')
    spans2 = []
    for s, e in spans:
        while s - 1 >= 0 and texts[0][s - 1] in num_chars | {'-'}:
            s -= 1
        while e + 1 < n and texts[0][e + 1] in num_chars:
            e += 1
        spans2.append((s, e))
    merged = []
    for s, e in spans2:
        if merged and s <= merged[-1][1] + 1:
            merged[-1] = (merged[-1][0], max(merged[-1][1], e))
        else:
            merged.append((s, e))
    spans = merged
    out = texts[0][:spans[0][0]]
    for si, (s, e) in enumerate(spans):
        out += '/'.join(t[s:e + 1] for t in texts)
        nxt = spans[si + 1][0] if si + 1 < len(spans) else n
        out += texts[0][e + 1:nxt]
    phases = '/'.join(str(p) for p, _ in items)
    out += f'（精英化{phases}）'
    return out

# ─── 备份 ──────────────────────────────────

shutil.copy2(DB, BACKUP)
print(f'备份: {BACKUP}')

conn = sqlite3.connect(DB)
cur = conn.cursor()
log_lines = []
err_lines = []

# ─── 无损性验证：重序列化不改变 JSON 结构 ──

print('验证 JSON 重序列化无损性 ...')
cur.execute('SELECT 干员ID, 技能列表 FROM operators WHERE 技能列表 IS NOT NULL')
lossless_fail = []
for cid, sk_json in cur.fetchall():
    try:
        if json.dumps(json.loads(sk_json), ensure_ascii=False) != sk_json:
            lossless_fail.append(cid)
    except Exception as e:
        lossless_fail.append(f'{cid}: {e}')
if lossless_fail:
    print(f'无损性验证失败 {len(lossless_fail)} 行，中止：')
    for f in lossless_fail[:20]:
        print('  ', f)
    sys.exit(1)
print('  通过')

# ─── 技能描述更新 ──────────────────────────

print('更新技能描述 ...')
cur.execute('SELECT 干员ID, 名称, 技能列表 FROM operators WHERE 技能列表 IS NOT NULL')
skill_updates = []
skill_changes = 0
for cid, name, sk_json in cur.fetchall():
    try:
        skills = json.loads(sk_json)
    except Exception as e:
        err_lines.append(f'{name} 技能列表 JSON 解析失败: {e}')
        continue
    changed = False
    for sk in skills:
        sid = sk.get('技能ID', '')
        sk_data = SKT.get(sid)
        if not sk_data:
            continue
        for lv in sk.get('各级详情', []):
            li = lv.get('等级', 1) - 1
            if li < 0 or li >= len(sk_data['levels']):
                continue
            lv_data = sk_data['levels'][li]
            raw = lv_data.get('description', '') or ''
            bb = {b['key']: b['value'] for b in lv_data.get('blackboard', [])}
            dur = lv_data.get('duration')
            cur_text = lv.get('描述')
            enhanced, _ = replace_enhanced(raw, bb, dur)
            if cur_text != enhanced:
                if re.search(r'\{[^{}]*\}|</>', enhanced):
                    err_lines.append(f'{name} {sk.get("技能名", "")} Lv{li + 1}: 修复后仍有残留: {enhanced[:60]}')
                    continue
                lv['描述'] = enhanced
                changed = True
                skill_changes += 1
                log_lines.append(f'技能 | {name} | {sk.get("技能名", "")} | Lv{li + 1} | {cur_text} | {enhanced}')
    if changed:
        skill_updates.append((cid, json.dumps(skills, ensure_ascii=False)))
print(f'  技能描述变更: {skill_changes} 条，涉及 {len(skill_updates)} 行')

# ─── 干员描述更新 ──────────────────────────

print('更新干员描述 ...')
cur.execute('SELECT 干员ID, 名称, 描述 FROM operators WHERE 描述 IS NOT NULL')
desc_updates = []
desc_changes = 0
for cid, name, cur_desc in cur.fetchall():
    cv = CT.get(cid)
    if not cv:
        continue
    raw = cv.get('description', '') or ''
    if not re.search(r'\{[^{}]*\}', raw):
        continue
    trait = cv.get('trait') or {}
    items = []
    for cand in (trait.get('candidates') or []):
        ph_raw = (cand.get('unlockCondition') or {}).get('phase', 'PHASE_0')
        ph = _PH_MAP.get(ph_raw, 0)
        bb = {b['key']: b['value'] for b in cand.get('blackboard', [])}
        items.append((ph, bb))
    items.sort(key=lambda x: x[0])
    if not items:
        err_lines.append(f'{name}: trait 无候选，无法解析描述占位符')
        continue
    if len(items) > 1:
        new_desc = merge_phases(raw, items)
        if new_desc is None:
            err_lines.append(f'{name}: 多候选文本长度不一致，无法合并')
            continue
    else:
        new_desc, _ = replace_enhanced(raw, items[0][1])
    if new_desc != cur_desc:
        desc_updates.append((cid, new_desc))
        desc_changes += 1
        log_lines.append(f'描述 | {name} | {cur_desc} | {new_desc}')
print(f'  干员描述变更: {desc_changes} 条')

# ─── 写入 ──────────────────────────────────

print('写入数据库 ...')
conn.execute('BEGIN')
try:
    for cid, new_json in skill_updates:
        cur.execute('UPDATE operators SET 技能列表 = ? WHERE 干员ID = ?', (new_json, cid))
    for cid, new_desc in desc_updates:
        cur.execute('UPDATE operators SET 描述 = ? WHERE 干员ID = ?', (new_desc, cid))
    conn.commit()
    print('  已提交')
except Exception as e:
    conn.rollback()
    print(f'  写入失败，已回滚: {e}')
    sys.exit(1)

# ─── 复核 ──────────────────────────────────

print('复核残留 ...')
pat_res = re.compile(r'\{[^{}]*\}|</>')
cur.execute('SELECT 名称, 技能列表 FROM operators WHERE 技能列表 IS NOT NULL')
skill_left = 0
for name, sk_json in cur.fetchall():
    try:
        skills = json.loads(sk_json)
    except Exception:
        continue
    for sk in skills:
        for lv in sk.get('各级详情', []):
            d = lv.get('描述') or ''
            if pat_res.search(d):
                skill_left += 1
                err_lines.append(f'复核残留 | 技能 | {name} | {sk.get("技能名", "")} | Lv{lv.get("等级")} | {d[:60]}')
cur.execute('SELECT 名称, 描述 FROM operators WHERE 描述 IS NOT NULL')
desc_left = [(r[0], r[1]) for r in cur.fetchall() if pat_res.search(r[1] or '')]
print(f'  技能描述残留: {skill_left}，干员描述残留: {len(desc_left)}')

# ─── 日志 ──────────────────────────────────

with open(LOG, 'w', encoding='utf-8') as f:
    f.write(f'# refine 日志 {DATE_TAG}\n# 技能描述 {skill_changes} 条，干员描述 {desc_changes} 条\n\n')
    for line in log_lines:
        f.write(line + '\n')
with open(ERR_LOG, 'w', encoding='utf-8') as f:
    for line in err_lines:
        f.write(line + '\n')

print(f'完成。变更日志: {LOG}')
conn.close()
