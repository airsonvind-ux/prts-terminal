"""
正式入库脚本：傀影肉鸽（rogue_1）
向 arknights.db 写入 rogue_1_entries 表
字段：id / 名称 / 效果 / 类型 / 稀有度 / buff / 备注 / 特性
数据源：JSON（基础数据）+ PRTS 双页面（备注、特性）
"""
import json
import re
import sqlite3
import urllib.request

# ── 常量 ──────────────────────────────────────────
JSON_PATH  = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata\excel\roguelike_topic_table.json"
DB_PATH    = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-pipeline\db_builder\arknights.db"
RELIC_URL  = "https://prts.wiki/w/%E5%82%80%E5%BD%B1%E4%B8%8E%E7%8C%A9%E7%BA%A2%E5%AD%A4%E9%92%BB/%E9%95%BF%E7%94%9F%E8%80%85%E5%AE%9D%E7%9B%92"
CAPSULE_URL = "https://prts.wiki/w/%E5%82%80%E5%BD%B1%E4%B8%8E%E7%8C%A9%E7%BA%A2%E5%AD%A4%E9%92%BB/%E7%8C%A9%E7%BA%A2%E7%8F%8D%E8%97%8F"


# ── 工具函数 ──────────────────────────────────────
def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": "PRTS-DB/1.0"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return resp.read().decode("utf-8")

def strip(text):
    if not text:
        return ""
    return re.sub(r"\s+", " ", re.sub(r"</?[a-zA-Z][^>]*?>", "", text)).strip()

def section(html, anchor_id, end_tag="h2", end_anchor=None):
    """截取 id=anchor_id 到下一个 <end_tag> 或 end_anchor 的 HTML 片段"""
    start = html.find(f'id="{anchor_id}"')
    if start == -1:
        return ""
    tag_start = html.rfind("<", max(0, start - 200), start)
    end = None
    if end_anchor:
        end = html.find(f'id="{end_anchor}"', start + 100)
    if end is None or end == -1:
        end = html.find(f"<{end_tag}", start + 100)
    return html[tag_start:end] if end != -1 else html[tag_start:]


# ── 建表 ──────────────────────────────────────────
def create_table(conn):
    conn.execute("DROP TABLE IF EXISTS rogue_1_entries")
    conn.execute("""
        CREATE TABLE rogue_1_entries (
            id    TEXT PRIMARY KEY,
            名称   TEXT NOT NULL,
            效果   TEXT,
            类型   TEXT NOT NULL,
            稀有度 TEXT,
            buff  TEXT,
            备注   TEXT,
            特性   TEXT
        )
    """)
    conn.commit()


# ── JSON 写入 ─────────────────────────────────────
def insert_from_json(conn):
    with open(JSON_PATH, "r", encoding="utf-8") as f:
        r1 = json.load(f)["details"]["rogue_1"]

    items  = r1["items"]
    relics = r1["relics"]

    rows = []

    # 藏品：items[type=RELIC]
    for iid, it in items.items():
        if it.get("type") != "RELIC":
            continue
        rb = relics.get(iid, {}).get("buffs")
        rows.append((
            iid,
            it["name"],
            it.get("usage", ""),
            "藏品",
            it.get("rarity"),
            json.dumps(rb, ensure_ascii=False) if rb else None,
            None,  # 备注 → PRTS
            None,  # 特性 → PRTS
        ))

    # 剧目：items[type=CAPSULE]
    for iid, it in items.items():
        if it.get("type") != "CAPSULE":
            continue
        rows.append((
            iid,
            it["name"],
            it.get("usage", ""),
            "剧目",
            None, None, None, None,
        ))

    # 幻觉（基础）：variationData
    # PRTS 页面上显示的是 innerName（如"迷茫"），outerName 多了"的"后缀
    for vid, v in r1["variationData"].items():
        rows.append((
            v["id"],
            v["innerName"],
            v["functionDesc"],
            "幻觉",
            None, None, None, None,
        ))

    # 幻觉（融合）：fusionData
    for vid, v in r1["fusionData"].items():
        rows.append((
            v["id"],
            v["name"],
            v["functionDesc"],
            "融合幻觉",
            None, None, None, None,
        ))

    conn.executemany(
        "INSERT INTO rogue_1_entries (id, 名称, 效果, 类型, 稀有度, buff, 备注, 特性) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        rows,
    )
    conn.commit()
    return len(rows)


# ── PRTS 解析 ─────────────────────────────────────
def parse_relic_page(html):
    """返回 (not_in_shop, relic_notes)"""
    not_in_shop = set()
    for m in re.finditer(
        r"<th[^>]*>([^<]+)<div[^>]*>.*?mdi-currency-usd-off.*?</div>", html
    ):
        not_in_shop.add(m.group(1).strip())

    notes = {}
    for table_html in re.split(r'<table[^>]*class="wikitable logo"[^>]*>', html):
        end = table_html.find("</table>")
        if end == -1:
            continue
        block = table_html[:end]

        name_m = re.search(
            r"<th[^>]*>\s*No\.\d+\s*</th>\s*<th[^>]*>([^<]+)", block
        )
        if not name_m:
            continue

        note_m = re.search(
            r"<th[^>]*>\s*备注\s*</th>\s*<td[^>]*>(.*?)</td>", block, re.DOTALL
        )
        if note_m:
            notes[name_m.group(1).strip()] = strip(note_m.group(1))

    return not_in_shop, notes


def parse_capsule_section(html, anchor, end_anchor=None):
    """
    解析剧目节（普通剧目/猩红剧目），返回 {剧名: 备注}
    备注来源：<td> 中 ※ 之后的文本
    猩红剧目节的名称含"猩红"前缀（如"猩红《一百零一日》"）
    """
    sec = section(html, anchor, end_anchor=end_anchor)
    result = {}
    for tr in re.findall(r"<tr>(.*?)</tr>", sec, re.DOTALL):
        tds = re.findall(r"<td[^>]*>(.*?)</td>", tr, re.DOTALL)
        if len(tds) < 2:
            continue
        text = strip(tds[1])
        if not text:
            continue

        # 提取剧目名：猩红剧目有"猩红"前缀
        if anchor == "猩红剧目":
            name_m = re.search(r"猩红(《[^》]+》)", text)
        else:
            name_m = re.search(r"(《[^》]+》)", text)
        display_name = name_m.group(0) if name_m else None
        if not display_name:
            continue

        # 提取 ※ 之后的备注
        idx = text.find("※")
        if idx == -1:
            continue
        note = text[idx + 1:].strip()
        if note:
            result[display_name] = note
    return result


def parse_variation_section(html, anchor):
    """幻觉节（基础/融合）：返回 {幻觉名: 备注}"""
    sec = section(html, anchor).replace("\n", " ")
    result = {}
    for tr in re.findall(r"<tr>(.*?)</tr>", sec, re.DOTALL):
        ths = re.findall(r"<th[^>]*>(.*?)</th>", tr, re.DOTALL)

        if anchor == "基础幻觉":
            if not ths:
                continue
            name = strip(ths[0])
        else:  # 融合幻觉
            if len(ths) < 2:
                continue
            name = strip(ths[1])
            if name == "融合症状":
                continue

        td_m = re.search(r"<td[^>]*>(.*?)</td>", tr, re.DOTALL)
        if not td_m:
            continue
        ul_m = re.search(r"<ul>(.*?)</ul>", td_m.group(1), re.DOTALL)
        if ul_m:
            note = strip(ul_m.group(1))
            if note:
                result[name] = note
    return result


# ── PRTS 更新 ─────────────────────────────────────
def update_from_prts(conn):
    try:
        relic_html  = fetch(RELIC_URL)
        capsule_html = fetch(CAPSULE_URL)
    except Exception as e:
        print(f"  ⚠ 网络错误: {e}")
        return {}

    cur = conn.cursor()
    stats = {}

    # ── 藏品备注 ──
    not_in_shop, relic_notes = parse_relic_page(relic_html)

    # 备注
    updated = 0
    for name, note in relic_notes.items():
        cur.execute(
            "UPDATE rogue_1_entries SET 备注 = ? WHERE 名称 = ? AND 类型 = '藏品'",
            (note, name),
        )
        updated += cur.rowcount
    stats["藏品备注"] = (len(relic_notes), updated)

    # 特性（不在诡意行商中出售）
    updated_flags = 0
    for name in not_in_shop:
        cur.execute(
            "UPDATE rogue_1_entries SET 特性 = '[\"不在诡意行商中出售\"]' WHERE 名称 = ? AND 类型 = '藏品'",
            (name,),
        )
        updated_flags += cur.rowcount
    stats["不在诡意行商中出售"] = (len(not_in_shop), updated_flags)

    # ── 剧目备注（普通 + 猩红，记录来源）──
    capsule_notes = {}  # name → set of notes
    capsule_sources = {}  # name → [来源节]
    for anchor in [("普通剧目", "猩红剧目"), ("猩红剧目", None)]:
        sec_anchor, end_anchor = anchor
        section_notes = parse_capsule_section(capsule_html, sec_anchor, end_anchor)
        for name, note in section_notes.items():
            if name not in capsule_notes:
                capsule_notes[name] = set()
                capsule_sources[name] = []
            capsule_notes[name].add(note)
            capsule_sources[name].append(sec_anchor)

    updated = 0
    for name, notes_set in capsule_notes.items():
        merged = "\n".join(notes_set)
        cur.execute(
            "UPDATE rogue_1_entries SET 备注 = ? WHERE 名称 = ? AND 类型 = '剧目'",
            (merged, name),
        )
        updated += cur.rowcount
    stats["剧目备注"] = (len(capsule_notes), updated, capsule_sources)

    # ── 基础幻觉备注 ──
    base_var_notes = parse_variation_section(capsule_html, "基础幻觉")
    updated = 0
    for name, note in base_var_notes.items():
        cur.execute(
            "UPDATE rogue_1_entries SET 备注 = ? WHERE 名称 = ? AND 类型 = '幻觉'",
            (note, name),
        )
        updated += cur.rowcount
    stats["基础幻觉备注"] = (len(base_var_notes), updated)

    # ── 融合幻觉备注 ──
    fusion_var_notes = parse_variation_section(capsule_html, "幻觉组合")
    updated = 0
    for name, note in fusion_var_notes.items():
        cur.execute(
            "UPDATE rogue_1_entries SET 备注 = ? WHERE 名称 = ? AND 类型 = '融合幻觉'",
            (note, name),
        )
        updated += cur.rowcount
    stats["融合幻觉备注"] = (len(fusion_var_notes), updated)

    conn.commit()
    return stats


# ── 统计输出 ──────────────────────────────────────
def print_stats(conn, json_count, prts_stats):
    cur = conn.cursor()

    print(f"\n{'=' * 60}")
    print("rogue_1_entries 写入统计")
    print(f"{'=' * 60}")
    print(f"  总行数: {json_count}")

    for t in ["藏品", "剧目", "幻觉", "融合幻觉"]:
        cur.execute("SELECT COUNT(*) FROM rogue_1_entries WHERE 类型 = ?", (t,))
        total = cur.fetchone()[0]
        cur.execute(
            "SELECT COUNT(*) FROM rogue_1_entries WHERE 类型 = ? AND 备注 IS NOT NULL", (t,)
        )
        noted = cur.fetchone()[0]
        cur.execute(
            "SELECT COUNT(*) FROM rogue_1_entries WHERE 类型 = ? AND 特性 IS NOT NULL", (t,)
        )
        tagged = cur.fetchone()[0]
        print(f"  {t}: {total} 条（备注 {noted}，特性 {tagged}）")

    # 样本：有备注的剧目（标注来源）
    print(f"\n  ── 有备注的剧目 ──")
    for row in cur.execute(
        "SELECT id, 名称, 备注 FROM rogue_1_entries WHERE 类型 = '剧目' AND 备注 IS NOT NULL"
    ):
        sources = prts_stats.get("剧目备注", (0, 0, {}))[2].get(row[1], [])
        print(f"    {row[0]} \"{row[1]}\" 来源: {sources}")
        print(f"      备注: {row[2][:150]}")


# ── 主流程 ────────────────────────────────────────
def main():
    conn = sqlite3.connect(DB_PATH)

    print("创建表 rogue_1_entries ...")
    create_table(conn)

    print("写入 JSON 基础数据 ...")
    json_count = insert_from_json(conn)

    print("爬取 PRTS 备注和特性 ...")
    prts_stats = update_from_prts(conn)

    print_stats(conn, json_count, prts_stats)

    if prts_stats:
        print(f"\n  ── PRTS 爬取详情 ──")
        for label, val in prts_stats.items():
            if isinstance(val, tuple) and len(val) == 3:
                found, updated, _ = val
                print(f"    {label}: 爬取到 {found} 条，成功匹配 {updated} 条")
            elif isinstance(val, tuple):
                found, updated = val
                print(f"    {label}: 爬取到 {found} 条，成功匹配 {updated} 条")
            else:
                print(f"    {label}: {val}")

    conn.close()


if __name__ == "__main__":
    main()
