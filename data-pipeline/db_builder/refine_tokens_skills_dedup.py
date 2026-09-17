"""
PRTS 终端 — token 技能列表去重
原始数据中 token 的 skills 数组按精英化阶段占位（同一 skillId 重复出现多次，
如 龙腾.F 的 sktok_mgllan_drone1 出现 3 次），导致 tokens 表技能列表出现
多个完全相同的技能。按 skillId 去重，保留首次出现。

用法：python refine_tokens_skills_dedup.py
影响表：tokens.技能列表
"""

import json, sqlite3, os, time

OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
DB_PATH = os.path.join(OUTPUT_DIR, 'arknights.db')
DETAIL_LOG = os.path.join(OUTPUT_DIR, 'refine_tokens_skills_dedup.log')


def dedup_skills(skills_json):
    """按 skillId 去重，保留首次出现。返回 (新JSON, changed)"""
    if not skills_json:
        return skills_json, False
    try:
        skills = json.loads(skills_json)
    except json.JSONDecodeError:
        return skills_json, False

    seen = set()
    result = []
    changed = False
    for sk in skills:
        sid = sk.get('技能ID', '')
        if sid in seen:
            changed = True
            continue
        seen.add(sid)
        result.append(sk)

    if changed:
        return json.dumps(result, ensure_ascii=False), True
    return skills_json, False


def main():
    conn = sqlite3.connect(DB_PATH)
    conn.execute("PRAGMA journal_mode=OFF")
    conn.execute("PRAGMA synchronous=OFF")

    rows = conn.execute(
        "SELECT rowid, 名称, 技能列表 FROM tokens WHERE 技能列表 IS NOT NULL ORDER BY 名称"
    ).fetchall()

    total_checked = 0
    total_updated = 0
    total_removed = 0
    detail_log = []

    start = time.time()
    for i, (rowid, name, skills_json) in enumerate(rows):
        total_checked += 1
        new_json, changed = dedup_skills(skills_json)

        if changed:
            before = len(json.loads(skills_json))
            after = len(json.loads(new_json))
            total_removed += before - after
            conn.execute(
                "UPDATE tokens SET 技能列表 = ? WHERE rowid = ?",
                (new_json, rowid)
            )
            total_updated += 1
            detail_log.append(f'{name}: {before} → {after} 个技能')
            if i % 20 == 0:
                elapsed = time.time() - start
                print(f'  [{i}/{len(rows)}] {elapsed:.0f}s | {name}')

    conn.commit()
    conn.close()

    print(f'\n检查: {total_checked} 行')
    print(f'更新: {total_updated} 行（存在重复）')
    print(f'移除: {total_removed} 个重复技能')
    if detail_log:
        with open(DETAIL_LOG, 'w', encoding='utf-8') as f:
            f.write('\n'.join(detail_log) + '\n')
        print(f'详情: {DETAIL_LOG}')


if __name__ == '__main__':
    main()
