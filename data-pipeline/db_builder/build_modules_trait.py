"""
PRTS 终端 — 模组特性入库
从 PRTS 整理的特性文本（模组特性信息.md）覆盖 operators 表的模组特性字段

用法：python build_modules_trait.py
"""

import re, json, sqlite3, os

OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB  = os.path.join(OUTPUT_DIR, "arknights.db")
TRAIT_MD   = r'F:\Obsidian_notes\20.Projects\PRTS终端助手\模组特性信息.md'
MISS_LOG   = os.path.join(OUTPUT_DIR, 'build_modules_trait_miss.log')

# raw data typeName2 → md 文件中的符号
TYPE_REMAP = {'D': 'Δ'}


def load_trait_map():
    """解析模组特性信息.md，返回 {key: trait_text}"""
    with open(TRAIT_MD, 'r', encoding='utf-8') as f:
        lines = f.readlines()

    lookup = {}
    current_key = None
    current_trait = []

    for line in lines:
        line = line.rstrip()

        m = re.match(r'^### (.+)', line)
        if m:
            if current_key and current_trait:
                lookup[current_key] = '\n'.join(current_trait)
            current_key = m.group(1).strip()
            current_trait = []
            continue

        if not line or line.startswith('#'):
            continue

        if line.startswith('特性追加：') or line.startswith('特性更新：') or line.startswith('※'):
            current_trait.append(line)
            continue

        if current_key:
            current_trait.append(line)

    if current_key and current_trait:
        lookup[current_key] = '\n'.join(current_trait)

    return lookup


# ─── 主流程 ─────────────────────────────────

def main():
    print('====== 解析 PRTS 特性文本 ======')
    trait_map = load_trait_map()
    print(f'  条目数: {len(trait_map)}')

    print('\n====== 写入模组特性 ======')
    conn = sqlite3.connect(OUTPUT_DB)
    conn.execute("PRAGMA journal_mode=OFF")

    rows = conn.execute(
        "SELECT rowid, 干员ID, 名称, 模组列表 FROM operators WHERE 模组列表 IS NOT NULL"
    ).fetchall()

    updated = 0
    not_found = []

    for rowid, cid, name, mod_json in rows:
        mods = json.loads(mod_json)
        changed = False

        for m in mods:
            mod_name = m.get('名称', '')
            branch = m.get('分支名1', '')
            mtype  = m.get('分支名2', '')

            # 1) 按模组名称查找（特限证章）
            prts_text = trait_map.get(mod_name)

            # 2) 按 分支-类型 查找（常规模组）
            if not prts_text and branch and mtype:
                mtype = TYPE_REMAP.get(mtype, mtype)
                prts_text = trait_map.get(f'{branch}-{mtype}')

            if prts_text:
                phases = m.get('各级效果', [])
                if phases:
                    phases[0]['特性'] = prts_text
                for ph in phases[1:]:
                    ph['特性'] = None
                changed = True
            elif mod_name:  # 不在 lookup 中，但模组有名称
                not_found.append(f'{name} | {mod_name} | {branch}-{mtype}')

        if changed:
            conn.execute(
                "UPDATE operators SET 模组列表=? WHERE rowid=?",
                (json.dumps(mods, ensure_ascii=False), rowid)
            )
            updated += 1

    conn.commit()

    print(f'  更新: {updated} 名干员')

    if not_found:
        print(f'\n  未匹配: {len(not_found)} 个模组')
        with open(MISS_LOG, 'w', encoding='utf-8') as f:
            for entry in sorted(set(not_found)):
                f.write(f'{entry}\n')
        print(f'  日志: {MISS_LOG}')

    conn.close()
    print('\n====== 完成 ======')


if __name__ == '__main__':
    main()
