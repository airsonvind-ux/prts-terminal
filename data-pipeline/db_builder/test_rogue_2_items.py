"""
测试脚本：水月肉鸽（rogue_2）数据入库
向 arknights_test.db 写入 rogue_2_entries 表
字段：id / 名称 / 效果 / 类型 / 稀有度 / buff / 备注 / 特性
数据源：JSON（基础数据）+ PRTS 双页面（备注、特性、额外信息）
"""
import json
import re
import sqlite3
import urllib.request

# ── 常量 ──────────────────────────────────────────
JSON_PATH  = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata\excel\roguelike_topic_table.json"
DB_PATH    = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-pipeline\db_builder\arknights_test.db"
RELIC_URL  = "https://prts.wiki/w/%E6%B0%B4%E6%9C%88%E4%B8%8E%E6%B7%B1%E8%93%9D%E4%B9%8B%E6%A0%91/%E7%94%9F%E7%89%A9%E5%88%B6%E5%93%81%E9%99%88%E8%AE%BE"
RECORD_URL = "https://prts.wiki/w/%E6%B0%B4%E6%9C%88%E4%B8%8E%E6%B7%B1%E8%93%9D%E4%B9%8B%E6%A0%91/%E6%B7%B1%E8%93%9D%E8%AE%B0%E5%BD%95%E4%BB%AA"
DEEP_URL   = "https://prts.wiki/w/%E6%B0%B4%E6%9C%88%E4%B8%8E%E6%B7%B1%E8%93%9D%E4%B9%8B%E6%A0%91/%E6%B7%B1%E5%85%A5%E6%8E%A2%E7%B4%A2"


# ── 工具函数 ──────────────────────────────────────
def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": "PRTS-DB/1.0"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return resp.read().decode("utf-8")

def strip(text):
    if not text:
        return ""
    return re.sub(r"\s+", " ", re.sub(r"</?[a-zA-Z][^>]*?>", "", text)).strip()


# ── 建表 ──────────────────────────────────────────
def create_table(conn):
    conn.execute("DROP TABLE IF EXISTS rogue_2_entries")
    conn.execute("""
        CREATE TABLE rogue_2_entries (
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
        r2 = json.load(f)["details"]["rogue_2"]

    items  = r2["items"]
    relics = r2["relics"]
    rows = []

    # ── 藏品：items[type=RELIC] ──
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

    # ── 启示：squadBuffData
    #    PRTS 深蓝记录仪使用 innerName（如"乌萨斯的怒号"），而非 outerName（"怒号"）
    for sid, sv in r2["squadBuffData"].items():
        b = sv.get("buffs")
        rows.append((
            sv["id"],
            sv["innerName"],
            sv["functionDesc"],
            "启示",
            None,
            json.dumps(b, ensure_ascii=False) if b else None,
            None, None,
        ))

    # ── 排异反应：charBuffData ──
    for cid, cv in r2["charBuffData"].items():
        b = cv.get("buffs")
        rows.append((
            cv["id"],
            cv["outerName"],
            cv["functionDesc"],
            "排异反应",
            None,
            json.dumps(b, ensure_ascii=False) if b else None,
            None, None,
        ))

    # ── 大群的呼唤：variationData
    #    注意：outerName 为纯数字"1"，不可读，名称用 innerName
    for vid, vv in r2["variationData"].items():
        rows.append((
            vv["id"],
            vv["innerName"],  # outerName 不可读
            vv["functionDesc"],
            "大群的呼唤",
            None, None, None, None,
        ))

    conn.executemany(
        "INSERT INTO rogue_2_entries (id, 名称, 效果, 类型, 稀有度, buff, 备注, 特性) "
        "VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        rows,
    )
    conn.commit()
    return len(rows)


# ── PRTS 藏品页解析 ───────────────────────────────
def parse_relic_page(html):
    """返回 (markers_dict, notes_dict)"""
    marker_defs = {
        "不可于风雨际会交换": "mdi-close-circle",
        "可计数叠加层数":     "mdi-transfer-up",
        "一次性使用":         "mdi-trophy-broken",
        "遭诅古物":           "mdi-ghost",
    }

    # 初始化：每个藏品名 → 标记列表
    relic_tags = {}   # name → [标记1, 标记2, ...]
    notes = {}

    for block in re.split(r'<table[^>]*class="wikitable logo"[^>]*>', html):
        end = block.find("</table>")
        if end == -1:
            continue
        body = block[:end]

        name_m = re.search(
            r"<th[^>]*>\s*No\.\d+\s*</th>\s*<th[^>]*>([^<]+)", body
        )
        if not name_m:
            continue
        name = name_m.group(1).strip()

        # 收集标记
        tags = []
        for label, icon in marker_defs.items():
            if icon in body:
                tags.append(label)
        if tags:
            relic_tags[name] = tags

        # 备注行
        note_m = re.search(
            r"<th[^>]*>\s*备注\s*</th>\s*<td[^>]*>(.*?)</td>", body, re.DOTALL
        )
        if note_m:
            notes[name] = strip(note_m.group(1))

    return relic_tags, notes


# ── PRTS 深蓝记录仪解析 ───────────────────────────
def parse_record_entry(tr_html):
    """
    从 <tr> 提取 (name, effect, extra, flavor)
    结构：<big>name</big><hr/><p><b>effect</b><br/>[extra]<br/><i>flavor</i></p>
    """
    big_m = re.search(r"<big>(.*?)</big>", tr_html, re.DOTALL)
    if not big_m:
        return None
    name = strip(big_m.group(1))

    p_m = re.search(r"<p>(.*?)</p>", tr_html, re.DOTALL)
    if not p_m:
        return name, "", "", ""

    p = p_m.group(1)

    b_m = re.search(r"<b>(.*?)</b>", p, re.DOTALL)
    effect = strip(b_m.group(1)) if b_m else ""

    i_m = re.search(r"<i>(.*?)</i>", p, re.DOTALL)
    flavor = strip(i_m.group(1)) if i_m else ""

    extra = ""
    if b_m:
        b_end = p.find("</b>") + 4
        i_start = p.find("<i>") if "<i>" in p else len(p)
        if b_end < i_start:
            extra = strip(p[b_end:i_start])

    return name, effect, extra, flavor


def parse_record_page(html):
    """解析深蓝记录仪全页，返回 {name: extra_text}"""
    result = {}
    for tr in re.findall(r"<tr>(.*?)</tr>", html, re.DOTALL):
        entry = parse_record_entry(tr)
        if entry is None:
            continue
        name, effect, extra, flavor = entry
        if extra:
            result[name] = extra
    return result


def parse_deep_section(html, anchor):
    """
    解析深入探索页面（启示+/排异反应+），返回 {name: note}
    结构：<big>名称+</big><hr/><p><b>效果</b><br/>※备注<br/><i>风味</i></p>
    启示+ 无备注（只有<b>效果</b><br/><i>风味</i>）
    """
    start = html.find(f'id="{anchor}"')
    if start == -1:
        return {}
    tag_start = html.rfind("<", max(0, start - 200), start)
    end = html.find("<h2", start + 100)
    sec = html[tag_start:end] if end != -1 else html[tag_start:start + 8000]

    result = {}
    for tr in re.findall(r"<tr>(.*?)</tr>", sec, re.DOTALL):
        big_m = re.search(r"<big>(.*?)</big>", tr, re.DOTALL)
        if not big_m:
            continue
        name = strip(big_m.group(1))

        p_m = re.search(r"<p>(.*?)</p>", tr, re.DOTALL)
        if not p_m:
            continue
        p = p_m.group(1)

        # 提取 </b> 到 <i> 之间所有含 ※ 的行
        b_end = p.find("</b>")
        if b_end == -1:
            continue
        after_b = p[b_end + 4:]

        i_start = after_b.find("<i>")
        if i_start != -1:
            after_b = after_b[:i_start]

        # 按 <br/> 或 <br> 分割，收集 ※ 开头的行
        lines = re.split(r"<br\s*/?>", after_b)
        notes = [strip(l) for l in lines if "※" in l]
        if notes:
            result[name] = "\n".join(notes)

    return result


# ── PRTS 更新 ─────────────────────────────────────
def update_from_prts(conn):
    try:
        relic_html  = fetch(RELIC_URL)
        record_html = fetch(RECORD_URL)
        deep_html   = fetch(DEEP_URL)
    except Exception as e:
        print(f"  ⚠ 网络错误: {e}")
        return {}

    cur = conn.cursor()
    stats = {}

    # ── 藏品备注 + 特性 ──
    relic_tags, relic_notes = parse_relic_page(relic_html)

    # 备注
    updated = 0
    for name, note in relic_notes.items():
        cur.execute(
            "UPDATE rogue_2_entries SET 备注 = ? WHERE 名称 = ? AND 类型 = '藏品'",
            (note, name),
        )
        updated += cur.rowcount
    stats["藏品备注"] = (len(relic_notes), updated)

    # 特性（JSON 数组）
    updated_flags = 0
    for name, tags in relic_tags.items():
        flag_json = json.dumps(tags, ensure_ascii=False)
        cur.execute(
            "UPDATE rogue_2_entries SET 特性 = ? WHERE 名称 = ? AND 类型 = '藏品'",
            (flag_json, name),
        )
        updated_flags += cur.rowcount
    stats["藏品特性"] = (len(relic_tags), updated_flags)

    # ── 深蓝记录仪：额外信息 → 备注 ──
    # 数据库名称与 PRTS 深蓝记录仪名称一致（启示/大群用 innerName，排异 outerName==innerName）
    all_extra = parse_record_page(record_html)

    type_counts = {"启示": 0, "排异反应": 0, "大群的呼唤": 0}
    for name, extra in all_extra.items():
        cur.execute(
            "UPDATE rogue_2_entries SET 备注 = ? WHERE 名称 = ? AND 类型 != '藏品' AND 名称 NOT LIKE '%+%'",
            (extra, name),
        )
        if cur.rowcount > 0:
            cur.execute("SELECT 类型 FROM rogue_2_entries WHERE 名称 = ?", (name,))
            row = cur.fetchone()
            if row:
                type_counts[row[0]] = type_counts.get(row[0], 0) + 1
    stats["深蓝记录仪"] = type_counts

    # ── 深入探索：启示+ / 排异反应+ → 备注（追加）──
    deep_counts = {"启示": 0, "排异反应": 0}
    for anchor, db_type in [("启示+", "启示"), ("排异反应+", "排异反应")]:
        deep_notes = parse_deep_section(deep_html, anchor)
        for name, note in deep_notes.items():
            # 数据库中已有备注 → 追加；否则直接设置
            cur.execute(
                "SELECT 备注 FROM rogue_2_entries WHERE 名称 = ? AND 类型 = ?",
                (name, db_type),
            )
            row = cur.fetchone()
            if row:
                existing = row[0] or ""
                merged = f"{existing}\n{note}" if existing else note
                cur.execute(
                    "UPDATE rogue_2_entries SET 备注 = ? WHERE 名称 = ? AND 类型 = ?",
                    (merged, name, db_type),
                )
                deep_counts[db_type] += 1
    stats["深入探索"] = deep_counts

    # ── 手动修正：PRTS 页面未记录的实际效果 ──
    manual_fixes = [
        ("衍生者终端-超级改", "实际效果为战斗开始时生效"),
        ("\u201c荣耀套餐\u201d",   "实际效果为对周围1.25半径内的敌人生效"),
    ]
    fixed = 0
    for name, note in manual_fixes:
        cur.execute(
            "UPDATE rogue_2_entries SET 备注 = ? WHERE 名称 = ? AND 类型 = '藏品'",
            (note, name),
        )
        fixed += cur.rowcount
    stats["手动修正"] = fixed

    conn.commit()

    return stats


# ── 统计输出 ──────────────────────────────────────
def print_stats(conn, json_count):
    cur = conn.cursor()

    print(f"\n{'=' * 60}")
    print("rogue_2_entries 写入统计")
    print(f"{'=' * 60}")
    print(f"  总行数: {json_count}")

    for t in ["藏品", "启示", "排异反应", "大群的呼唤"]:
        cur.execute("SELECT COUNT(*) FROM rogue_2_entries WHERE 类型 = ?", (t,))
        total = cur.fetchone()[0]
        cur.execute(
            "SELECT COUNT(*) FROM rogue_2_entries WHERE 类型 = ? AND 备注 IS NOT NULL",
            (t,),
        )
        noted = cur.fetchone()[0]
        cur.execute(
            "SELECT COUNT(*) FROM rogue_2_entries WHERE 类型 = ? AND 特性 IS NOT NULL",
            (t,),
        )
        tagged = cur.fetchone()[0]
        print(f"  {t}: {total} 条（备注 {noted}，特性 {tagged}）")

    # 样本抽查
    print(f"\n  ── 藏品样本（前3条）──")
    for row in cur.execute(
        "SELECT id, 名称, 效果, 稀有度, 特性, 备注 FROM rogue_2_entries WHERE 类型 = '藏品' LIMIT 3"
    ):
        print(f"    {row[0]}")
        print(f"      名称: {row[1]}  稀有度: {row[3]}")
        print(f"      效果: {row[2][:80]}{'...' if row[2] and len(row[2]) > 80 else ''}")
        if row[4]:
            print(f"      特性: {row[4]}")
        if row[5]:
            print(f"      备注: {row[5][:80]}")

    print(f"\n  ── 启示/排异/大群 样本（各1条）──")
    for t in ["启示", "排异反应", "大群的呼唤"]:
        cur.execute(
            "SELECT id, 名称, 效果, 备注 FROM rogue_2_entries WHERE 类型 = ? LIMIT 1",
            (t,),
        )
        row = cur.fetchone()
        if row:
            print(f"    [{t}] {row[0]}")
            print(f"      名称: {row[1]}")
            print(f"      效果: {row[2][:80]}")
            if row[3]:
                print(f"      备注: {row[3][:80]}")


# ── 主流程 ────────────────────────────────────────
def main():
    conn = sqlite3.connect(DB_PATH)

    print("创建表 rogue_2_entries ...")
    create_table(conn)

    print("写入 JSON 基础数据 ...")
    json_count = insert_from_json(conn)

    print("爬取 PRTS 备注和特性 ...")
    prts_stats = update_from_prts(conn)

    print_stats(conn, json_count)

    if prts_stats:
        print(f"\n  ── PRTS 爬取详情 ──")
        for label, val in prts_stats.items():
            if isinstance(val, dict):
                for k, v in val.items():
                    print(f"    {label}[{k}]: {v} 条")
            elif isinstance(val, tuple):
                found, updated = val
                print(f"    {label}: 爬取到 {found} 条，成功匹配 {updated} 条")
            else:
                print(f"    {label}: {val}")

    conn.close()


if __name__ == "__main__":
    main()
