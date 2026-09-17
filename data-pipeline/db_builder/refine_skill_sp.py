"""
PRTS 终端 — 技能 SP 数据修正
将技能 JSON 中顶层 SP数据 删除，改为写入各级详情.SP
源数据来自 skill_table.json（每级 spData 不同，顶层只取了 1 级）

用法：python refine_skill_sp.py
影响表：operators.技能列表、tokens.技能列表
"""

import json, sqlite3, os, time

# ─── 路径 ──────────────────────────────

DATA_ROOT = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    '..', '..', 'data-source', 'ArknightsGameData-master', 'zh_CN', 'gamedata'
)
OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
DB_PATH = os.path.join(OUTPUT_DIR, 'arknights.db')
ERROR_LOG = os.path.join(OUTPUT_DIR, 'refine_skill_sp_errors.log')

# ─── 源数据 ────────────────────────────

print('====== 读取源数据 ======')
with open(os.path.join(DATA_ROOT, 'excel/skill_table.json'), 'r', encoding='utf-8') as f:
    skill_table = json.load(f)
print(f'  技能条目: {len(skill_table)}')

# ─── 字段映射 ──────────────────────────

SP_KEY_MAP = {
    'spCost': '技力消耗',
    'initSp': '初始技力',
    'maxChargeTime': '可充能次数',
    'increment': '技力回复速度',
}

# ─── 核心函数 ──────────────────────────

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
    """修正技能列表 JSON。返回 (新JSON, changed: bool, suppressed_errors: list)"""
    if not skills_json:
        return None, False, []

    skills = json.loads(skills_json)
    changed = False
    errors = []

    for sk in skills:
        skill_id = sk.get('技能ID', '')
        skill_name = sk.get('技能名', '?')
        details = sk.get('各级详情', [])

        if 'SP数据' not in sk:
            continue

        per_level_sp = build_per_level_sp(skill_id, len(details))
        if per_level_sp is None:
            errors.append(f'技能ID不在源数据: {skill_id} ({skill_name})')
            continue

        for i, lv in enumerate(details):
            if i < len(per_level_sp):
                lv['SP'] = per_level_sp[i]

        del sk['SP数据']
        changed = True

    if changed:
        return json.dumps(skills, ensure_ascii=False), True, errors
    return skills_json, False, errors


# ═══════════════════════════════════════════════
# 执行
# ═══════════════════════════════════════════════

def main():
    conn = sqlite3.connect(DB_PATH)
    conn.execute("PRAGMA journal_mode=OFF")
    conn.execute("PRAGMA synchronous=OFF")

    all_errors = []
    total_checked = 0
    total_updated = 0
    start = time.time()

    # ── operators ──
    print('\n====== operators ======')
    rows = conn.execute(
        "SELECT rowid, 名称, 技能列表 FROM operators WHERE 技能列表 IS NOT NULL ORDER BY 名称"
    ).fetchall()

    for i, (rowid, name, skills_json) in enumerate(rows):
        if i % 100 == 0:
            elapsed = time.time() - start
            print(f'  [{i}/{len(rows)}] {elapsed:.0f}s | {name}')

        total_checked += 1
        new_json, changed, errs = fix_skills(skills_json)
        all_errors.extend([f'{name}\t{e}' for e in errs])

        if changed:
            conn.execute(
                "UPDATE operators SET 技能列表 = ? WHERE rowid = ?",
                (new_json, rowid)
            )
            total_updated += 1

    conn.commit()

    ops_checked = len(rows)
    ops_updated = total_updated

    # ── tokens ──
    print('\n====== tokens ======')
    rows = conn.execute(
        "SELECT rowid, 名称, 技能列表 FROM tokens WHERE 技能列表 IS NOT NULL ORDER BY 名称"
    ).fetchall()

    for i, (rowid, name, skills_json) in enumerate(rows):
        total_checked += 1
        new_json, changed, errs = fix_skills(skills_json)
        all_errors.extend([f'(token) {name}\t{e}' for e in errs])

        if changed:
            conn.execute(
                "UPDATE tokens SET 技能列表 = ? WHERE rowid = ?",
                (new_json, rowid)
            )
            total_updated += 1

    conn.commit()

    tok_checked = len(rows)
    tok_updated = total_updated - ops_updated

    # ── traps ──
    print('\n====== traps ======')
    rows = conn.execute(
        "SELECT rowid, 名称, 技能列表 FROM traps WHERE 技能列表 IS NOT NULL ORDER BY 名称"
    ).fetchall()

    for i, (rowid, name, skills_json) in enumerate(rows):
        if i % 200 == 0:
            elapsed = time.time() - start
            print(f'  [{i}/{len(rows)}] {elapsed:.0f}s | {name}')

        total_checked += 1
        new_json, changed, errs = fix_skills(skills_json)
        all_errors.extend([f'(trap) {name}\t{e}' for e in errs])

        if changed:
            conn.execute(
                "UPDATE traps SET 技能列表 = ? WHERE rowid = ?",
                (new_json, rowid)
            )
            total_updated += 1

    conn.commit()

    trap_checked = len(rows)
    trap_updated = total_updated - ops_updated - tok_updated

    # ── 统计 ──
    elapsed = time.time() - start
    print(f'\n====== 完成 ({elapsed:.1f}s) ======')
    print(f'  operators: 检查 {ops_checked}，更新 {ops_updated}')
    print(f'  tokens:    检查 {tok_checked}，更新 {tok_updated}')
    print(f'  traps:     检查 {trap_checked}，更新 {trap_updated}')
    print(f'  合计更新: {total_updated}')

    # 验证：检查是否还有顶层 SP数据 残留
    residual_ops = conn.execute(
        "SELECT COUNT(*) FROM operators WHERE 技能列表 LIKE '%SP数据%'"
    ).fetchone()[0]
    residual_tok = conn.execute(
        "SELECT COUNT(*) FROM tokens WHERE 技能列表 LIKE '%SP数据%'"
    ).fetchone()[0]
    residual_trap = conn.execute(
        "SELECT COUNT(*) FROM traps WHERE 技能列表 LIKE '%SP数据%'"
    ).fetchone()[0]
    if residual_ops > 0 or residual_tok > 0 or residual_trap > 0:
        print(f'\n  ⚠ 仍有顶层 SP数据 残留: operators {residual_ops}, tokens {residual_tok}, traps {residual_trap}')

    # ── 错误日志 ──
    if all_errors:
        with open(ERROR_LOG, 'w', encoding='utf-8') as f:
            f.write(f'# refine_skill_sp 错误日志\n')
            f.write(f'# 时间: {time.strftime("%Y-%m-%d %H:%M:%S")}\n')
            f.write(f'# 条目: {len(all_errors)}\n\n')
            for e in all_errors:
                f.write(f'{e}\n')
        print(f'  错误日志: {ERROR_LOG} ({len(all_errors)} 条)')

    conn.close()


if __name__ == '__main__':
    main()
