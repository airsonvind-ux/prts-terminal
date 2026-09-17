"""
测试脚本：界园肉鸽（rogue_5）数据入库
向 arknights_test.db 写入 rogue_5_entries 表
字段：id / 名称 / 效果 / 类型 / 稀有度 / buff / 特性 / 备注

类型枚举：
  藏品 | 通宝（随盒赠钱/砺武兵钱/富贵商钱/待铸子钱/天师奇钱）| 岁时（朦胧/真切/入髓/醒觉）

数据源：JSON（items + modules） + PRTS 三页面 + 手工修正（硬编码）
"""
import json
import re
import sqlite3
import urllib.request

# ── 常量 ──────────────────────────────────────────
JSON_PATH  = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata\excel\roguelike_topic_table.json"
DB_PATH    = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-pipeline\db_builder\arknights_test.db"
RELIC_URL  = "https://prts.wiki/w/%E5%B2%81%E7%9A%84%E7%95%8C%E5%9B%AD%E5%BF%97%E5%BC%82/%E7%8F%8D%E7%8E%A9%E9%9B%86%E5%86%8C?action=raw"
COPPER_URL = "https://prts.wiki/w/%E5%B2%81%E7%9A%84%E7%95%8C%E5%9B%AD%E5%BF%97%E5%BC%82/%E8%97%8F%E9%92%B1%E6%9C%A8%E7%9B%92?action=raw"
WRATH_URL  = "https://prts.wiki/w/%E5%B2%81%E7%9A%84%E7%95%8C%E5%9B%AD%E5%BF%97%E5%BC%82/%E8%A7%81%E5%AD%97%E5%9B%BE%E5%86%8C?action=raw"

# 通宝分类
COPPER_TYPE_CN = {
    "BLANK": "随盒赠钱", "FIGHT": "砺武兵钱",
    "RESOURCE": "富贵商钱", "UNSOUND": "待铸子钱", "TREASURE": "天师奇钱",
}

# 藏品角标 → 中文
RELIC_FLAG_DEFS = {
    "notrade":        "不可于【失与得】交换",
    "stack":          "可计数叠加层数",
    "oneoff":         "一次性使用",
    "suit_king":      "属性：国王收藏品",
    "suit_demonking": "属性：魔王收藏品",
    "noshop":         "不在诡意行商中出售",
    "noyiyu":         "不在易与中出售",
    "bossdrop":       "可能在\"险路恶敌\"…掉落",
    "seedreward":     "可能在\"种因得果\"事件中奖励",
}

# 通宝角标 → 中文
COPPER_FLAG_DEFS = {
    "nosell":         "不在诡意行商和易与中出售",
    "lucky_tile":     "与化境地块存在特殊关联",
    "noexchangeget":  "不可在【筹谋】节点交换得到",
    "notrade":        "不可在【筹谋】节点用于交换",
}

# 岁时等级映射
WRATH_LEVEL = {"a": "朦胧", "b": "真切", "c": "入髓", "d": "醒觉"}


# ── 工具函数 ──────────────────────────────────────
def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": "PRTS-DB/1.0"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return resp.read().decode("utf-8")


def strip(text):
    """清洗 wikitext/HTML 标记，保留模板中的文本内容"""
    # 颜色模板：{{color|red|文字}} → 文字
    text = re.sub(r"\{\{color\|[^}|]+\|([^}]+)\}\}", r"\1", text)
    # 特殊机制：{{特殊机制|自动路径|color=...}} → 自动路径
    text = re.sub(r"\{\{特殊机制\|([^}|]+).*?\}\}", r"\1", text)
    # 技能类型：{{技能类型|手动触发}} → 手动触发
    text = re.sub(r"\{\{技能类型\|([^}]+)\}\}", r"\1", text)
    # 异常效果（带 color）：{{异常效果|孤立|color=...}} → 孤立
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
    # 残留模板标记：移除剩余的 {{ 和 }}
    text = text.replace("{{", "").replace("}}", "")
    # 按行压缩空白
    lines = []
    for line in text.split("\n"):
        line = re.sub(r"\s+", " ", line).strip()
        if line:
            lines.append(line)
    return "\n".join(lines)


def parse_flags(mark_text, defs):
    flags = []
    for key, label in defs.items():
        if key in mark_text:
            flags.append(label)
    return flags


def parse_wiki_params(body):
    """解析 wikitext 模板参数 → {key: value}"""
    params = {}
    current_key = None
    current_val = []
    for line in body.split("\n"):
        m = re.match(r"\|(\S+)\s*=\s*(.*)", line)
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


def append_note(cur, table, name, note, type_cond):
    """
    追加备注到已存在的备注列，返回更新行数。
    内部工具函数：table 和 type_cond 由调用方硬编码传入，无 SQL 注入风险。
    """
    cur.execute(
        f"UPDATE {table} SET 备注 = CASE WHEN 备注 IS NULL OR 备注 = '' THEN ? ELSE 备注 || X'0A' || ? END WHERE (名称 = ? OR 名称 LIKE ?) AND {type_cond}",
        (note, note, name, f"{name}-%"),
    )
    return cur.rowcount


# ── 建表 ──────────────────────────────────────────
def create_table(conn):
    conn.execute("DROP TABLE IF EXISTS rogue_5_entries")
    conn.execute("""
        CREATE TABLE rogue_5_entries (
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
    r5 = data["details"]["rogue_5"]
    m5 = data["modules"]["rogue_5"]
    items = r5["items"]
    relics = r5["relics"]

    rows = []

    # ── 藏品：items[type=RELIC] ──
    for iid, it in items.items():
        if it.get("type") != "RELIC":
            continue
        rb = relics.get(iid, {}).get("buffs")
        rows.append((
            it["id"], it["name"], it.get("usage", ""), "藏品",
            it.get("rarity"),
            json.dumps(rb, ensure_ascii=False) if rb else None,
            None, None,
        ))

    # ── 通宝：items[type=COPPER_BUFF]，仅基础版（_a 后缀）──
    arch_coppers = r5["archiveComp"]["copper"]["coppers"]
    copper_type_map = {}
    for group_id, group in arch_coppers.items():
        ct = group.get("copperType")
        display = group.get("displayCopperId")
        if ct and ct != "NONE" and display:
            copper_type_map[display] = ct

    for iid, it in items.items():
        if it.get("type") != "COPPER_BUFF":
            continue
        if not iid.endswith("_a"):
            continue
        rb = relics.get(iid, {}).get("buffs")
        copper_id = iid.replace("_buff", "")
        ct_en = copper_type_map.get(copper_id, None)
        ct_cn = f"通宝（{COPPER_TYPE_CN[ct_en]}）" if ct_en else "通宝"
        rows.append((
            it["id"], it["name"], it.get("usage", ""), ct_cn,
            it.get("rarity"),
            json.dumps(rb, ensure_ascii=False) if rb else None,
            None, None,
        ))

    # ── 岁时：wrathData，仅标准条目 ──
    wd = m5["wrath"]["wrathData"]
    for wid, wv in wd.items():
        if not re.match(r"rogue_5_wrath_\d+_[a-d]$", wid):
            continue
        suffix = wid.rsplit("_", 1)[1]
        level_cn = WRATH_LEVEL.get(suffix, suffix)
        rows.append((
            wv["id"], wv["name"], wv.get("functionDesc", ""),
            f"岁时（{level_cn}）", None, None, None, None,
        ))

    conn.executemany(
        "INSERT INTO rogue_5_entries (id, 名称, 效果, 类型, 稀有度, buff, 备注, 特性) "
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

        fl = parse_flags(mark, RELIC_FLAG_DEFS)
        if fl:
            flags_map[icon_id] = fl

        if note:
            # 剥离 parse_wiki_params 泄漏进来的模板闭合 }}
            note = re.sub(r"\n?\}\}\s*$", "", note)
            notes_map[icon_id] = strip(note)

    return flags_map, notes_map


# ── PRTS 通宝页解析 ───────────────────────────────
def parse_copper_page(raw):
    """返回 (buff_id → flags_json, buff_id → note)"""
    flags_map = {}
    notes_map = {}

    blocks = re.split(r"\{\{:岁的界园志异/通宝\s*\n", raw)
    for b in blocks[1:]:
        end = b.find("\n}}")
        if end == -1:
            end = b.find("}}")
        if end == -1:
            continue
        body = b[:end]

        id_m = re.search(r"\|ID\s*=\s*(.+)", body)
        if not id_m:
            continue
        gid = id_m.group(1).strip()

        售价_m = re.search(r"\|售价\s*=\s*(.+)", body)
        售价 = int(售价_m.group(1).strip()) if 售价_m else None

        售价2_m = re.search(r"\|售价2\s*=\s*(.+)", body)
        售价2 = int(售价2_m.group(1).strip()) if 售价2_m else None

        traits = {}
        if 售价 is not None:
            traits["源石锭售价"] = 售价
        if 售价2 is not None:
            traits["生命值售价"] = 售价2

        mark_m = re.search(r"\|角标\s*=\s*(.+)", body)
        if mark_m:
            fl = parse_flags(mark_m.group(1), COPPER_FLAG_DEFS)
            if fl:
                traits["标识"] = fl

        # 备注（支持多行：从 |备注= 到下一个 |参数 或 }}）
        note_m = re.search(r"\|备注\s*=\s*", body)
        note = None
        if note_m:
            note_start = note_m.end()
            # 找下一个顶级参数或闭合标记结尾
            rest = body[note_start:]
            # 匹配到换行后跟 | 或闭合标记为止（允许跨越多行）
            note_end = re.search(r"\n(?=\||\}\})", rest)
            note_raw = rest[:note_end.start()] if note_end else rest
            note = strip(note_raw) if note_raw.strip() else None

        buff_id = gid.replace("rogue_5_copper_", "rogue_5_copper_buff_") + "_a"

        flags_map[buff_id] = json.dumps(traits, ensure_ascii=False) if traits else None
        if note:
            notes_map[buff_id] = note

    return flags_map, notes_map


# ── PRTS 岁时页解析 ───────────────────────────────
def parse_wrath_section(raw):
    """返回 name → extra_info_text，用于写入备注"""
    pos = raw.find("秉烛岁谱")
    if pos == -1:
        return {}
    section = raw[pos:]

    result = {}

    entries = list(re.finditer(
        r"<span class=\"serif widetitle\">'''(.+?)'''</span>", section
    ))
    for i, m in enumerate(entries):
        name = m.group(1)
        end_pos = entries[i + 1].start() if i + 1 < len(entries) else len(section)
        block = section[m.start():end_pos]

        # 额外信息
        extra_m = re.search(r"\|colspan=2[^|]*?\|(.*?)\n\|-", block, re.DOTALL)
        extra = strip(extra_m.group(1)) if extra_m else None

        if extra:
            result[name] = extra

    return result


# ── 手工修正（硬编码）─────────────────────────────

def apply_relic_manual_corrections(cur):
    """对藏品追加手工修正文本到备注列"""
    manual = {
        "霜牡的肩甲": "不满足生效条件后增益仍能持续30秒",
        "《炎国字汇》": "仅限干员的弹药类技能",
        "《雅集》": "仅限干员的弹药类技能",
        "铁卫-临时要塞": "效果结束时若自身阻挡数不高于0，则阻挡数+1，持续时间无限",
        "忘生珍珑": "效果仅适用于战斗开始999秒内",
        "奔兽战车": "效果仅适用于我方单位",
        "电弧的掌机": "效果仅适用于我方召唤物",
        "雪牝的护手": "不满足生效条件后增益仍能持续30秒",
        "荆棘环": "实际效果为使我方单位获得技力",
        "赏善郎": "实际效果为我方单位触发闪避或抵挡后",
        "长明灯引": "实际效果并非抵抗，而是可抵抗状态生效时间倍率-50%",
        "“萤灯映牍”": "实际效果并非我方干员和召唤物，而是我方单位",
        "玩具弹弓": "实际效果为我方单位释放技能时",
        "遥乡之引": "实际效果并非我方单位，而是我方干员和召唤物",
        "折戟-浴血": "实际效果并非攻击后，而是每对一个单位成功造成伤害就获得2点技力",
        "医者-涌泉": "实际效果并非我方单位，而是可部署单位",
        "铁卫-无锋": "实际效果并非受到攻击后，而是受到伤害后",
        "鸣镝吹筒": "实际效果并非损失30%的生命值时，而是损失30%、60%、90%的生命值时（也即最多可触发3次；被击倒时若仍有剩余可触发次数，立即触发1次）",
        "“掷厉”": "实际效果并非抵抗，而是可抵抗状态生效时间倍率-50%",
        "云与漆": "实际效果并非流失2%的最大生命值，而是受到最大生命值2%的真实伤害",
        "不赦": "实际效果为使\"宿傲\"获得召唤额外敌人的能力，部分\"身形\"获得持续召唤敌人的能力",
        "不息": "实际效果为【伺烛客】在作战时以及【伺烛客】的召唤物在非岁兽残识作战时攻击力、生命值-0％，从第三层开始，每进入新的一层（包括第三层），额外-10%",
        "四时丹青毫": "实际效果并非抵抗，而是可抵抗状态生效时间倍率-50%",
        "老磨盘": "实际效果并非干员，而是我方单位",
        "鸣脊兽": "实际效果并非我方干员，而是我方单位",
        "登天斧": "实际效果并非干员，而是我方单位",
        "虚实线团": "实际效果并非干员，而是我方单位",
        "绸锁": "实际效果并非干员，而是我方单位",
        "得胜琴": "实际效果并非我方干员，而是我方单位",
        "“万灵方”": "实际效果并非干员，而是我方单位",
        "烟花之手": "实际效果并非每次攻击命中时，而是每对一个单位造成伤害时",
    }
    updated = 0
    for name, note in manual.items():
        append_note(cur, "rogue_5_entries", name, note, "类型 = '藏品'")
        updated += cur.rowcount
    return (len(manual), updated)


def apply_copper_manual_corrections(cur):
    """对通宝追加手工修正文本到备注列"""
    manual = {
        "苦寒": "仅针对可部署化境地块",
        "匪风": "仅针对可部署化境地块",
        "霖雨": "仅针对可部署化境地块",
        "霹雳": "仅针对可部署化境地块",
        "旱热": "仅针对可部署化境地块",
        "虹霓": "仅针对可部署化境地块",
        "雾凇": "仅针对可部署化境地块",
        "霜雪": "仅针对可部署化境地块",
        "驰道长": "仅针对可部署化境地块",
        "寒窗志": "实际效果为投出时，所有我方单位的防御力+200，但我方干员部署后冻结5秒。每额外投出一次，所有我方单位的防御力额外+100",
        "贵有衡": "实际效果为投出时，我方单位攻击力+10％。钱盒里每有一枚其他衡钱，我方单位攻击力额外+10％",
        "天命难违": "实际效果为投出时，敌人重量-1，所有我方单位技能开启时较大力度将攻击范围内的目标拖拽至自己面前",
        "诛邪雷法": "仅针对 可部署化境地块",
        "溯正道": "实际效果并非+80，而是+90",
        "左秉烛": "实际效果并非我方干员，而是我方单位",
        "孤芳自赏": "实际效果并非干员，而是我方单位",
    }
    updated = 0
    for name, note in manual.items():
        # 通宝在 DB 中名称带前缀（花-/衡-/厉-），用 LIKE 后缀匹配
        cur.execute(
            "UPDATE rogue_5_entries SET 备注 = CASE WHEN 备注 IS NULL OR 备注 = '' THEN ? ELSE 备注 || X'0A' || ? END WHERE 名称 LIKE ? AND 类型 LIKE '通宝%'",
            (note, note, f"%{name}"),
        )
        updated += cur.rowcount
    return (len(manual), updated)


def apply_wrath_manual_corrections(cur):
    """对岁时追加手工修正文本到备注列"""
    manual = {
        "卯律": "实际效果并非敌方，而是单位",
    }
    updated = 0
    for name, note in manual.items():
        append_note(cur, "rogue_5_entries", name, note, "类型 LIKE '岁时%'")
        updated += cur.rowcount
    return (len(manual), updated)


# ── PRTS 更新 ─────────────────────────────────────
def update_from_prts(conn):
    cur = conn.cursor()
    stats = {}

    try:
        relic_raw  = fetch(RELIC_URL)
        copper_raw = fetch(COPPER_URL)
        wrath_raw  = fetch(WRATH_URL)
    except Exception as e:
        print(f"  ⚠ 网络错误: {e}")
        return stats

    # ════════════════════════════════════════════════
    # 藏品
    # ════════════════════════════════════════════════
    relic_flags, relic_notes = parse_relic_page(relic_raw)

    # 特性（角标 → JSON 数组，按 id 匹配）
    updated = 0
    for icon_id, flags in relic_flags.items():
        flag_json_val = json.dumps(flags, ensure_ascii=False)
        cur.execute(
            "UPDATE rogue_5_entries SET 特性 = ? WHERE id = ? AND 类型 = '藏品'",
            (flag_json_val, icon_id),
        )
        updated += cur.rowcount
    stats["藏品特性"] = (len(relic_flags), updated)

    # 备注
    updated = 0
    for icon_id, note in relic_notes.items():
        cur.execute(
            "UPDATE rogue_5_entries SET 备注 = ? WHERE id = ? AND 类型 = '藏品'",
            (note, icon_id),
        )
        updated += cur.rowcount
    stats["藏品备注"] = (len(relic_notes), updated)

    # 多元奇物子条目：从基础版复制 特性/备注
    cur.execute("SELECT id FROM rogue_5_entries WHERE 类型 = '藏品'")
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
        cur.execute(
            "SELECT 特性, 备注 FROM rogue_5_entries WHERE id = ? AND 类型 = '藏品'",
            (base_id,),
        )
        row = cur.fetchone()
        if row and (row[0] or row[1]):
            cur.execute(
                "UPDATE rogue_5_entries SET 特性 = ?, 备注 = ? WHERE id = ?",
                (row[0], row[1], rid),
            )
            copied += cur.rowcount
    if copied:
        stats["多元奇物复制"] = copied
    conn.commit()

    relic_manual = apply_relic_manual_corrections(cur)
    stats["藏品修正"] = relic_manual

    conn.commit()

    # ════════════════════════════════════════════════
    # 通宝
    # ════════════════════════════════════════════════
    copper_flags, copper_notes = parse_copper_page(copper_raw)

    updated = 0
    for buff_id, flag_json_val in copper_flags.items():
        if flag_json_val is None:
            continue
        cur.execute(
            "UPDATE rogue_5_entries SET 特性 = ? WHERE id = ?",
            (flag_json_val, buff_id),
        )
        updated += cur.rowcount
    stats["通宝特性"] = (len(copper_flags), updated)

    updated = 0
    for buff_id, note in copper_notes.items():
        cur.execute(
            "UPDATE rogue_5_entries SET 备注 = ? WHERE id = ?",
            (note, buff_id),
        )
        updated += cur.rowcount
    stats["通宝备注"] = (len(copper_notes), updated)

    copper_manual = apply_copper_manual_corrections(cur)
    stats["通宝修正"] = copper_manual

    conn.commit()

    # ════════════════════════════════════════════════
    # 岁时
    # ════════════════════════════════════════════════
    wrath_extras = parse_wrath_section(wrath_raw)

    updated = 0
    for name, extra in wrath_extras.items():
        append_note(cur, "rogue_5_entries", name, extra, "类型 LIKE '岁时%'")
        updated += cur.rowcount
    stats["岁时备注"] = (len(wrath_extras), updated)

    wrath_manual = apply_wrath_manual_corrections(cur)
    stats["岁时修正"] = wrath_manual

    conn.commit()
    return stats


# ── 统计输出 ──────────────────────────────────────
def print_stats(conn, json_count):
    cur = conn.cursor()

    print(f"\n{'=' * 60}")
    print("rogue_5_entries 写入统计")
    print(f"{'=' * 60}")
    print(f"  总行数: {json_count}")

    for t in [
        "藏品",
        "通宝（随盒赠钱）", "通宝（砺武兵钱）", "通宝（富贵商钱）",
        "通宝（待铸子钱）", "通宝（天师奇钱）",
        "岁时（朦胧）", "岁时（真切）", "岁时（入髓）", "岁时（醒觉）",
    ]:
        cur.execute("SELECT COUNT(*) FROM rogue_5_entries WHERE 类型 = ?", (t,))
        total = cur.fetchone()[0]
        if total == 0:
            continue
        cur.execute(
            "SELECT COUNT(*) FROM rogue_5_entries WHERE 类型 = ? AND 备注 IS NOT NULL", (t,)
        )
        noted = cur.fetchone()[0]
        cur.execute(
            "SELECT COUNT(*) FROM rogue_5_entries WHERE 类型 = ? AND 特性 IS NOT NULL", (t,)
        )
        tagged = cur.fetchone()[0]
        print(f"  {t}: {total} 条（备注 {noted}，特性 {tagged}）")

    # 样本
    print(f"\n  ── 藏品样本 ──")
    for row in cur.execute(
        "SELECT id, 名称, 效果, 稀有度, 特性, 备注 FROM rogue_5_entries WHERE 类型 = '藏品' LIMIT 3"
    ):
        print(f"    {row[0]}  {row[1]}  稀有度={row[3]}")
        print(f"      效果: {(row[2] or '')[:100]}")
        if row[4]: print(f"      特性: {row[4][:100]}")
        if row[5]: print(f"      备注: {row[5][:100]}")

    print(f"\n  ── 通宝样本 ──")
    for row in cur.execute(
        "SELECT id, 名称, 类型, 稀有度, 特性, 备注 FROM rogue_5_entries WHERE 类型 LIKE '通宝%' LIMIT 5"
    ):
        print(f"    {row[0]}  {row[1]}  ({row[2]})")
        if row[4]: print(f"      特性: {row[4][:120]}")
        if row[5]: print(f"      备注: {row[5][:120]}")

    print(f"\n  ── 岁时样本 ──")
    for row in cur.execute(
        "SELECT id, 名称, 类型, 效果, 备注 FROM rogue_5_entries WHERE 类型 LIKE '岁时%' LIMIT 5"
    ):
        print(f"    {row[0]}  {row[1]}  ({row[2]})")
        print(f"      效果: {(row[3] or '')[:100]}")
        if row[4]: print(f"      备注: {row[4][:100]}")


# ── 主流程 ────────────────────────────────────────
def main():
    conn = sqlite3.connect(DB_PATH)

    print("创建表 rogue_5_entries ...")
    create_table(conn)

    print("写入 JSON 基础数据 ...")
    json_count = insert_from_json(conn)

    print("爬取 PRTS 备注和特性 ...")
    prts_stats = update_from_prts(conn)

    print_stats(conn, json_count)

    if prts_stats:
        print(f"\n  ── PRTS 爬取详情 ──")
        for label, val in prts_stats.items():
            if isinstance(val, tuple):
                found, updated = val
                print(f"    {label}: {found} 条, 匹配 {updated} 行")
            else:
                print(f"    {label}: {val}")

    conn.close()
    print("\n完成。")


if __name__ == "__main__":
    main()
