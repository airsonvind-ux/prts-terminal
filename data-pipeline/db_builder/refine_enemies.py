"""
PRTS 终端 — enemies 表后处理
对已入库的 enemies 表进行文本清洗、模式标记、备注列添加

用法：python refine_enemies.py

当前实现：文本清洗（天赋文本 + 技能文本）
后续：模式标记解析 + 备注列
"""

import re, sqlite3, os

# ─── 路径配置 ──────────────────────────────

OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB  = os.path.join(OUTPUT_DIR, "arknights.db")

# ─── 正则规则（按顺序应用）─────────────────
#
#   规则 ① 必须在 ③ 前执行，否则 【{{污染|Y}}】 中的 {{污染|Y}}
#   会被 ③ 先提取为 Y，留下多余的 【Y】

RULES = [
    # ① 【{{模板名|显示文本}}】 → 显示文本（先于③）
    ("① 方括号模板",  re.compile(r'【\{\{(.+?)\|(.+?)\}\}】'),
     lambda m: m.group(2)),

    # ② <!--[...JSON块...]--> → 删除
    ("② JSON注释块",  re.compile(r'<!--\s*\[[\s\S]*?\]\s*-->'),
     lambda m: ''),

    # ③ {{污染|显示文本}} → 显示文本（无【】版本）
    ("③ {{污染}}",    re.compile(r'\{\{污染\|([^}]+)\}\}'),
     lambda m: m.group(1)),

    # ④ {{技能类型|显示文本}} → 显示文本
    ("④ {{技能类型}}", re.compile(r'\{\{技能类型\|([^}]+)\}\}'),
     lambda m: m.group(1)),

    # ⑤ {{术语/custom|key|显示}} → 显示（最后一个 | 后）
    ("⑤ {{术语/custom}}", re.compile(r'\{\{术语/custom\|[^|]+\|([^}]+)\}\}'),
     lambda m: m.group(1)),

    # ⑥ {{font|文字|color=...|...}} → 文字（第一个 | 后）
    ("⑥ {{font}}",    re.compile(r'\{\{font\|([^|]+)\|[^}]+\}\}'),
     lambda m: m.group(1)),

    # ⑦ {{敌人头像|名称|px=...}} → 名称（第一个 | 后）
    ("⑦ {{敌人头像}}", re.compile(r'\{\{敌人头像\|([^|]+)\|[^}]+\}\}'),
     lambda m: m.group(1)),

    # ⑧ {{mdi|图标名}} → 删除（纯装饰）
    ("⑧ {{mdi}}",     re.compile(r'\{\{mdi\|[^}]+\}\}'),
     lambda m: ''),
]

# ─── 模板名列表（用于 SQL 查询定位脏数据）─────

DIRTY_TEMPLATES = [
    '<!--[',          # JSON 注释块
    '【{{',           # 方括号模板
    '{{污染|',
    '{{技能类型|',
    '{{术语/custom|',
    '{{font|',
    '{{敌人头像|',
    '{{mdi|',
]

def clean(text):
    """对单段文本按顺序应用全部清洗规则"""
    if not text:
        return text
    for _label, pattern, replacer in RULES:
        text = pattern.sub(replacer, text)
    text = re.sub(r'\n{3,}', '\n\n', text)
    return text.strip()

def build_dirty_query():
    """构建查找所有脏数据的 WHERE 子句"""
    clauses = []
    for col in ("天赋文本", "技能文本"):
        for tpl in DIRTY_TEMPLATES:
            clauses.append(f"{col} LIKE '%{tpl}%'")
    return " OR ".join(clauses)

# ─── 主流程 ─────────────────────────────────

def main():
    conn = sqlite3.connect(OUTPUT_DB)
    conn.execute("PRAGMA journal_mode=OFF")
    conn.execute("PRAGMA synchronous=OFF")

    # ── 确认列存在 ──────────────────────
    cols = {row[1] for row in conn.execute("PRAGMA table_info(enemies)").fetchall()}
    for col in ("天赋文本", "技能文本"):
        if col not in cols:
            print(f"错误: enemies 表缺少 {col} 列，请先运行 build_enemies_prts.py")
            conn.close()
            return

    # ── 定位脏数据 ───────────────────────
    where = build_dirty_query()
    dirty = conn.execute(f"""
        SELECT rowid, 敌人ID, 名称, 级别, 天赋文本, 技能文本
        FROM enemies
        WHERE {where}
    """).fetchall()

    print(f"====== 文本清洗 ======")
    print(f"  命中行数: {len(dirty)}")

    if not dirty:
        print("  无需清洗，退出")
        conn.close()
        return

    # ── 各规则命中统计（干跑，不写库）────────
    print(f"\n------ 规则命中统计 ------")
    for label, pattern, _ in RULES:
        hits = 0
        for _, _, _, _, talent, skill in dirty:
            if pattern.search(talent or '') or pattern.search(skill or ''):
                hits += 1
        if hits:
            print(f"  {label}: {hits} 行")

    # ── 清洗前样本 ──────────────────────
    print(f"\n------ 清洗前样本（3 条）------")
    for rowid, eid, name, lv, talent, skill in dirty[:3]:
        print(f"\n[{name} Lv{lv}]")
        if talent:
            print(f"  天赋: {talent[:200]}...")
        if skill:
            print(f"  技能: {skill[:200]}...")

    # ── 逐行更新 ────────────────────────
    updated = 0
    for rowid, eid, name, lv, talent, skill in dirty:
        new_talent = clean(talent)
        new_skill  = clean(skill)
        if new_talent != talent or new_skill != skill:
            conn.execute(
                "UPDATE enemies SET 天赋文本=?, 技能文本=? WHERE rowid=?",
                (new_talent or None, new_skill or None, rowid)
            )
            updated += 1

    conn.commit()

    # ── 清洗后样本（重新取同一批 rowid）─────
    print(f"\n------ 清洗后样本（3 条）------")
    for rowid, eid, name, lv, talent, skill in dirty[:3]:
        row = conn.execute(
            "SELECT 天赋文本, 技能文本 FROM enemies WHERE rowid=?",
            (rowid,)
        ).fetchone()
        nt, ns = row
        print(f"\n[{name} Lv{lv}]")
        if nt:
            print(f"  天赋: {nt[:200]}...")
        if ns:
            print(f"  技能: {ns[:200]}...")

    # ── 残留检查 ────────────────────────
    remain = conn.execute(f"""
        SELECT COUNT(*) FROM enemies WHERE {build_dirty_query()}
    """).fetchone()[0]

    total_talent = conn.execute(
        "SELECT COUNT(*) FROM enemies WHERE 天赋文本 IS NOT NULL"
    ).fetchone()[0]
    total_skill = conn.execute(
        "SELECT COUNT(*) FROM enemies WHERE 技能文本 IS NOT NULL"
    ).fetchone()[0]

    print(f"\n====== 完成 ======")
    print(f"  更新行数: {updated}")
    print(f"  残留异常: {remain}（应为 0）")
    print(f"  有天赋文本: {total_talent} 行")
    print(f"  有技能文本: {total_skill} 行")

    conn.close()

if __name__ == '__main__':
    main()
