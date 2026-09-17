"""
测试脚本：萨米肉鸽（rogue_3）数据入库
向 arknights_test.db 写入 rogue_3_entries 表
字段：id / 名称 / 效果 / 类型 / 稀有度 / buff / 备注 / 特性

类型枚举：
  藏品 | 密文板（布局/本因：族群/自然/灵魂/视相/世相） | 修辞 | 坍缩范式

数据源：JSON（基础 + modules） + PRTS 三页面（备注、特性）
"""
import json
import re
import sqlite3
import urllib.request

# ── 常量 ──────────────────────────────────────────
JSON_PATH  = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata\excel\roguelike_topic_table.json"
DB_PATH    = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-pipeline\db_builder\arknights_test.db"
RELIC_URL  = "https://prts.wiki/w/%E6%8E%A2%E7%B4%A2%E8%80%85%E7%9A%84%E9%93%B6%E5%87%87%E6%AD%A2%E5%A2%83/%E4%BB%AA%E5%BC%8F%E7%94%A8%E5%93%81%E7%B4%A2%E5%BC%95?action=raw"
TOTEM_URL  = "https://prts.wiki/w/%E6%8E%A2%E7%B4%A2%E8%80%85%E7%9A%84%E9%93%B6%E5%87%87%E6%AD%A2%E5%A2%83/%E5%AF%86%E6%96%87%E6%9D%BF%E7%A0%94%E7%A9%B6?action=raw"
CHAOS_URL  = "https://prts.wiki/w/%E6%8E%A2%E7%B4%A2%E8%80%85%E7%9A%84%E9%93%B6%E5%87%87%E6%AD%A2%E5%A2%83/%E5%86%AC%E5%A4%9C%E5%B1%95%E8%A7%88%E9%A6%86?action=raw"

# 颜色/子类型 → 中文分类名
COLOR_MAP    = {"RED": "族群", "GREEN": "自然", "BLUE": "灵魂", "ALL": "视相", "NONE": "世相"}
SUBTYPE_MAP  = {"TOTEM_UPPER": "布局", "TOTEM_LOWER": "本因"}
FLAG_DEFS    = {
    "notrade": "不可于【失与得】交换",
    "stack":   "可计数叠加层数",
    "oneoff":  "一次性使用",
    "noshop":  "不在诡意行商中出售",
}


# ── 工具函数 ──────────────────────────────────────
def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": "PRTS-DB/1.0"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return resp.read().decode("utf-8")

def strip(text):
    """清洗 wikitext/HTML 标记，保留模板中的文本内容"""
    if not text:
        return ""
    # 颜色模板：{{color|red|文字}} → 文字
    text = re.sub(r"\{\{color\|[^}|]+\|([^}]+)\}\}", r"\1", text)
    # 特殊机制：{{特殊机制|自动路径|color=...}} → 自动路径
    text = re.sub(r"\{\{特殊机制\|([^}|]+).*?\}\}", r"\1", text)
    # 技能类型：{{技能类型|手动触发}} → 手动触发
    text = re.sub(r"\{\{技能类型\|([^}]+)\}\}", r"\1", text)
    # 异常效果：{{异常效果|孤立|...}} → 孤立
    text = re.sub(r"\{\{异常效果\|([^}|]+).*?\}\}", r"\1", text)
    # popup（带内容参数）：→ (内容)
    text = re.sub(r"(?is)\{\{popup[^}|]*\|内容\s*=\s*([^}|]+).*?\}\}", r"(\1)", text)
    # popup/range：纯图标，移除
    text = re.sub(r"(?i)\{\{popup/range[^}]*\}\}", "", text)
    # 其他 popup：取第一个参数
    text = re.sub(r"(?is)\{\{popup\|([^}|]+).*?\}\}", r"(\1)", text)
    # 模糊/污染：提取内容
    text = re.sub(r"\{\{模糊\|(.+?)(\}\})?", r"\1", text, flags=re.DOTALL)
    text = re.sub(r"\{\{污染\|(.+?)(\}\})?", r"\1", text, flags=re.DOTALL)
    # 修正：提取修正后文本
    text = re.sub(r"\{\{修正\|([^}|]+).*?\}\}", r"\1", text)
    # 变动数值lite：移除（纯渲染辅助）
    text = re.sub(r"\{\{变动数值lite\|[^}]+\}\}", "", text)
    # HTML 注释
    text = re.sub(r"<!--.*?-->", "", text, flags=re.DOTALL)
    # br → 换行
    text = re.sub(r"<br\s*/?>", "\n", text)
    # 移除 HTML 标签（保留 <作战> 等中文标记），移除 wiki 加粗标记
    text = re.sub(r"</?[a-zA-Z][^>]*?>", "", text)
    text = re.sub(r"''+", "", text)
    # wiki 链接：[[link]] → link, [[link|display]] → display
    text = re.sub(r"\[\[([^\]|]+)\]\]", r"\1", text)
    text = re.sub(r"\[\[[^\]|]+\|([^\]]+)\]\]", r"\1", text)
    # 残留模板标记
    text = text.replace("{{", "").replace("}}", "")
    # 按行压缩空白
    lines = []
    for line in text.split("\n"):
        line = re.sub(r"\s+", " ", line).strip()
        if line:
            lines.append(line)
    return "\n".join(lines)

def parse_wiki_params(body):
    """解析 wikitext 多行模板参数 → {key: value}"""
    params = {}
    current_key = None
    current_val = []
    for line in body.split("\n"):
        m = re.match(r"\|(\w+)\s*=\s*(.*)", line)
        if m:
            if current_key:
                params[current_key] = "\n".join(current_val).strip()
            current_key = m.group(1)
            current_val = [m.group(2)]
        elif current_key:
            current_val.append(line)
    if current_key:
        params[current_key] = "\n".join(current_val).strip()
    return params

def parse_flags(mark_text):
    """从角标参数解析特性列表"""
    flags = []
    for key, label in FLAG_DEFS.items():
        if key in mark_text:
            flags.append(label)
    return flags


# ── 建表 ──────────────────────────────────────────
def create_table(conn):
    conn.execute("DROP TABLE IF EXISTS rogue_3_entries")
    conn.execute("""
        CREATE TABLE rogue_3_entries (
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
        data = json.load(f)
    r3 = data["details"]["rogue_3"]
    m3 = data["modules"]["rogue_3"]
    items  = r3["items"]
    relics = r3["relics"]

    rows = []

    # ── 藏品：items[type=RELIC] ──
    for iid, it in items.items():
        if it.get("type") != "RELIC":
            continue
        rb = relics.get(iid, {}).get("buffs")
        rows.append((
            it["id"],
            it["name"],
            it.get("usage", ""),
            "藏品",
            it.get("rarity"),
            json.dumps(rb, ensure_ascii=False) if rb else None,
            None,  # 备注 → PRTS
            None,  # 特性 → PRTS
        ))

    # ── 密文板：items[type=TOTEM] ──
    tbd = m3["totemBuff"]["totemBuffDatas"]
    for iid, it in items.items():
        if it.get("type") != "TOTEM":
            continue
        tb = tbd.get(iid, {})
        color_cn = COLOR_MAP.get(tb.get("color", ""), tb.get("color", ""))
        sub_cn = SUBTYPE_MAP.get(it.get("subType", ""), it.get("subType", ""))
        cat_type = f"密文板（{sub_cn}：{color_cn}）"
        rows.append((
            it["id"],
            it["name"],
            tb.get("archiveDesc", it.get("usage", "")),
            cat_type,
            None, None, None, None,
        ))

    # ── 修辞：modules.rogue_3.totemBuff.subBuffs ──
    for sid, sv in m3["totemBuff"]["subBuffs"].items():
        rows.append((
            sv["subBuffId"],
            sv["name"],
            sv.get("desc", ""),
            "修辞",
            None, None, None, None,
        ))

    # ── 坍缩范式：modules.rogue_3.chaos.chaosDatas ──
    for cid, cv in m3["chaos"]["chaosDatas"].items():
        rows.append((
            cv["chaosId"],
            cv["name"],
            cv.get("functionDesc", ""),
            "坍缩范式",
            None, None, None, None,
        ))

    conn.executemany(
        "INSERT INTO rogue_3_entries (id, 名称, 效果, 类型, 稀有度, buff, 备注, 特性) "
        "VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        rows,
    )
    conn.commit()
    return len(rows)


# ── PRTS 藏品页解析 ───────────────────────────────
def parse_relic_page(raw):
    """返回 (iconId → flags, iconId → note)"""
    flags_map = {}
    notes_map = {}

    blocks = re.split(r"\{\{收藏品\s*\n", raw)
    for b in blocks[1:]:
        end = b.find("\n}}")
        if end == -1:
            end = b.find("}}")
        if end == -1:
            continue
        params = parse_wiki_params(b[:end])

        icon_id = params.get("iconId", "")
        if not icon_id:
            continue

        mark = params.get("角标", "")
        note = params.get("备注", "")

        fl = parse_flags(mark)
        if fl:
            flags_map[icon_id] = fl

        if note:
            notes_map[icon_id] = strip(note)

    return flags_map, notes_map


# ── PRTS 密文板页解析（含修辞）────────────────────
def parse_totem_page(raw):
    """返回 (名称 → flags, 名称 → note)"""
    flags_map = {}
    notes_map = {}

    blocks = re.split(r"\{\{:探索者的银凇止境/密文板\s*\n", raw)
    for b in blocks[1:]:
        end = b.find("\n}}")
        if end == -1:
            end = b.find("}}")
        if end == -1:
            continue
        params = parse_wiki_params(b[:end])

        name = params.get("名称", "")
        if not name:
            continue

        mark = params.get("角标", "")
        note = params.get("备注", "")

        fl = parse_flags(mark)
        if fl:
            flags_map[name] = fl

        if note:
            notes_map[name] = strip(note)

    return flags_map, notes_map


# ── PRTS 坍缩范式页解析 ───────────────────────────
def parse_chaos_section(raw):
    """返回 (名称 → note) 的备注映射"""
    notes_map = {}

    # 定位「坍缩范式辑录」节
    sec_start = raw.find("坍缩范式辑录")
    if sec_start == -1:
        print("  ⚠ 未找到坍缩范式辑录章节")
        return notes_map

    sec_end = raw.find("\n==", sec_start + 10)
    section = raw[sec_start:sec_end] if sec_end != -1 else raw[sec_start:sec_start + 15000]

    # 按 <big> 分割条目
    entries = re.split(r"<big>", section)
    for e in entries:
        name_end = e.find("</big>")
        if name_end == -1:
            continue
        name = e[:name_end].strip()
        # 去除名称中的模板包裹，如 {{污染|目空一些}}
        name = re.sub(r"\{\{[^}]+\|([^}]+)\}\}", r"\1", name)
        # 跳过非条目（如「检测到新的坍缩范式！」）
        if "检测到" in name or "坍缩范式" in name:
            continue

        # 找粗体结束位置
        bold_m = re.search(r"'''(.*?)'''", e)
        if not bold_m:
            continue
        after = e[bold_m.end():]

        # 截断到下一个 |- 或 rowspan 或 |}
        sep = re.search(r'\|-|rowspan|class="mw|\|\}', after)
        if sep:
            after = after[:sep.start()]

        # 提取 ※ 开头行
        extras = []
        for part in re.split(r"<br\s*/?>", after):
            part = strip(part)
            if part and part.startswith("※"):
                extras.append(part)

        if extras:
            notes_map[name] = "\n".join(extras)

    return notes_map


# ── PRTS 更新 ─────────────────────────────────────
def update_from_prts(conn):
    cur = conn.cursor()
    stats = {}

    try:
        relic_raw  = fetch(RELIC_URL)
        totem_raw  = fetch(TOTEM_URL)
        chaos_raw  = fetch(CHAOS_URL)
    except Exception as e:
        print(f"  ⚠ 网络错误: {e}")
        return stats

    # ── 藏品：特性 + 备注 ──
    relic_flags, relic_notes = parse_relic_page(relic_raw)

    updated_flags = 0
    for icon_id, flags in relic_flags.items():
        flag_json = json.dumps(flags, ensure_ascii=False)
        cur.execute(
            "UPDATE rogue_3_entries SET 特性 = ? WHERE id = ? AND 类型 = '藏品'",
            (flag_json, icon_id),
        )
        updated_flags += cur.rowcount
    stats["藏品特性"] = (len(relic_flags), updated_flags)

    updated_notes = 0
    for icon_id, note in relic_notes.items():
        cur.execute(
            "UPDATE rogue_3_entries SET 备注 = ? WHERE id = ? AND 类型 = '藏品'",
            (note, icon_id),
        )
        updated_notes += cur.rowcount
    stats["藏品备注"] = (len(relic_notes), updated_notes)

    # 多元奇物子条目：从基础版复制（验证基础版存在）
    cur.execute("SELECT id FROM rogue_3_entries WHERE 类型 = '藏品'")
    all_relic_ids = [r[0] for r in cur.fetchall()]
    all_relic_ids_set = set(all_relic_ids)
    copied = 0
    for rid in all_relic_ids:
        m = re.match(r"(.+)_[abc]$", rid)
        if not m:
            continue
        base_id = m.group(1)
        if base_id not in all_relic_ids_set:
            continue
        # 从基础版取备注和特性
        cur.execute(
            "SELECT 备注, 特性 FROM rogue_3_entries WHERE id = ? AND 类型 = '藏品'",
            (base_id,),
        )
        row = cur.fetchone()
        if row and (row[0] or row[1]):
            cur.execute(
                "UPDATE rogue_3_entries SET 备注 = ?, 特性 = ? WHERE id = ?",
                (row[0], row[1], rid),
            )
            copied += cur.rowcount
    if copied:
        stats["多元奇物复制"] = copied
    conn.commit()

    # ── 密文板 + 修辞：共用一次 PRTS 解析 ──
    totem_flags, totem_notes = parse_totem_page(totem_raw)

    # 密文板特性
    updated_flags = 0
    for name, flags in totem_flags.items():
        flag_json = json.dumps(flags, ensure_ascii=False)
        cur.execute(
            "UPDATE rogue_3_entries SET 特性 = ? WHERE 名称 = ? AND 类型 LIKE '密文板%'",
            (flag_json, name),
        )
        updated_flags += cur.rowcount
    stats["密文板特性"] = (len(totem_flags), updated_flags)

    # 密文板备注
    updated_notes = 0
    for name, note in totem_notes.items():
        cur.execute(
            "UPDATE rogue_3_entries SET 备注 = ? WHERE 名称 = ? AND 类型 LIKE '密文板%'",
            (note, name),
        )
        updated_notes += cur.rowcount
    stats["密文板备注"] = (len(totem_notes), updated_notes)

    # 修辞特性（同一次解析，按类型 = '修辞' 过滤）
    updated_flags = 0
    for name, flags in totem_flags.items():
        flag_json = json.dumps(flags, ensure_ascii=False)
        cur.execute(
            "UPDATE rogue_3_entries SET 特性 = ? WHERE 名称 = ? AND 类型 = '修辞'",
            (flag_json, name),
        )
        updated_flags += cur.rowcount
    stats["修辞特性"] = (len(totem_flags), updated_flags)

    # 修辞备注
    updated_notes = 0
    for name, note in totem_notes.items():
        cur.execute(
            "UPDATE rogue_3_entries SET 备注 = ? WHERE 名称 = ? AND 类型 = '修辞'",
            (note, name),
        )
        updated_notes += cur.rowcount
    stats["修辞备注"] = (len(totem_notes), updated_notes)

    # ── 坍缩范式：备注 ──
    chaos_notes = parse_chaos_section(chaos_raw)

    updated_notes = 0
    for name, note in chaos_notes.items():
        cur.execute(
            "UPDATE rogue_3_entries SET 备注 = ? WHERE 名称 = ? AND 类型 = '坍缩范式'",
            (note, name),
        )
        updated_notes += cur.rowcount
    stats["坍缩范式备注"] = (len(chaos_notes), updated_notes)

    conn.commit()
    return stats


# ── 统计输出 ──────────────────────────────────────
def print_stats(conn, json_count, prts_stats):
    cur = conn.cursor()

    print(f"\n{'=' * 60}")
    print("rogue_3_entries 写入统计")
    print(f"{'=' * 60}")
    print(f"  总行数: {json_count}")

    for t in ["藏品", "密文板（布局：族群）", "密文板（本因：灵魂）", "修辞", "坍缩范式"]:
        cur.execute("SELECT COUNT(*) FROM rogue_3_entries WHERE 类型 = ?", (t,))
        total = cur.fetchone()[0]
        if total == 0:
            continue
        cur.execute(
            "SELECT COUNT(*) FROM rogue_3_entries WHERE 类型 = ? AND 备注 IS NOT NULL", (t,)
        )
        noted = cur.fetchone()[0]
        cur.execute(
            "SELECT COUNT(*) FROM rogue_3_entries WHERE 类型 = ? AND 特性 IS NOT NULL", (t,)
        )
        tagged = cur.fetchone()[0]
        print(f"  {t}: {total} 条（备注 {noted}，特性 {tagged}）")

    # 按大类汇总
    for broad in ["藏品", "密文板", "修辞", "坍缩范式"]:
        cur.execute(
            f"SELECT COUNT(*) FROM rogue_3_entries WHERE 类型 LIKE '{broad}%'"
        )
        total = cur.fetchone()[0]
        cur.execute(
            f"SELECT COUNT(*) FROM rogue_3_entries WHERE 类型 LIKE '{broad}%' AND 备注 IS NOT NULL"
        )
        noted = cur.fetchone()[0]
        cur.execute(
            f"SELECT COUNT(*) FROM rogue_3_entries WHERE 类型 LIKE '{broad}%' AND 特性 IS NOT NULL"
        )
        tagged = cur.fetchone()[0]
        print(f"  [{broad} 合计]: {total} 条（备注 {noted}，特性 {tagged}）")

    # 样本抽查
    print(f"\n  ── 藏品样本 ──")
    for row in cur.execute(
        "SELECT id, 名称, 效果, 稀有度, 特性, 备注 FROM rogue_3_entries WHERE 类型 = '藏品' LIMIT 3"
    ):
        print(f"    {row[0]}  {row[1]}  稀有度={row[3]}")
        print(f"      效果: {(row[2] or '')[:100]}")
        if row[4]:
            print(f"      特性: {row[4]}")
        if row[5]:
            print(f"      备注: {row[5][:100]}")

    print(f"\n  ── 密文板/修辞/坍缩 各样本 ──")
    for broad in ["密文板（布局", "密文板（本因", "修辞", "坍缩范式"]:
        cur.execute(
            f"SELECT id, 名称, 效果, 类型, 特性, 备注 FROM rogue_3_entries WHERE 类型 LIKE '{broad}%' LIMIT 1"
        )
        row = cur.fetchone()
        if row:
            print(f"    [{row[3]}] {row[0]}")
            print(f"      名称: {row[1]}")
            print(f"      效果: {(row[2] or '')[:120]}")
            if row[4]:
                print(f"      特性: {row[4]}")
            if row[5]:
                print(f"      备注: {row[5][:120]}")

    # 多元奇物子条目复制验证（修正SQL LIKE中_通配符问题）
    print(f"\n  ── 多元奇物复制验证 ──")
    for row in cur.execute(
        "SELECT id, 名称, 备注, 特性 FROM rogue_3_entries WHERE 类型 = '藏品' AND id LIKE '%\\_16\\_%' ESCAPE '\\'"
    ):
        print(f"    {row[0]} ({row[1]}): 备注={row[2] is not None and len(row[2] or '')>0}, 特性={row[3]}")


# ── 主流程 ────────────────────────────────────────
def main():
    conn = sqlite3.connect(DB_PATH)

    print("创建表 rogue_3_entries ...")
    create_table(conn)

    print("写入 JSON 基础数据 ...")
    json_count = insert_from_json(conn)

    print("爬取 PRTS 备注和特性 ...")
    prts_stats = update_from_prts(conn)

    print_stats(conn, json_count, prts_stats)

    if prts_stats:
        print(f"\n  ── PRTS 爬取详情 ──")
        for label, val in prts_stats.items():
            if isinstance(val, tuple):
                found, updated = val
                print(f"    {label}: 爬取到 {found} 条，成功匹配 {updated} 条")
            else:
                print(f"    {label}: {val}")

    conn.close()
    print("\n完成。")


if __name__ == "__main__":
    main()
