"""
正式入库脚本：萨卡兹肉鸽（rogue_4）
向 arknights.db 写入 rogue_4_entries 表
字段：id / 名称 / 效果 / 类型 / 稀有度 / buff / 特性 / 备注

类型枚举：
  藏品 | 思绪（遗愿）| 思绪（灵感）| 思绪（构想）| 年代追忆（成型期/扩张期/鼎盛期）

数据源：JSON（items + modules） + PRTS 三页面
"""
import json
import re
import sqlite3
import urllib.request

# ── 常量 ──────────────────────────────────────────
JSON_PATH    = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata\excel\roguelike_topic_table.json"
DB_PATH      = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-pipeline\db_builder\arknights.db"
RELIC_URL    = "https://prts.wiki/w/%E8%90%A8%E5%8D%A1%E5%85%B9%E7%9A%84%E6%97%A0%E7%BB%88%E5%A5%87%E8%AF%AD/%E6%83%B3%E8%B1%A1%E5%AE%9E%E4%BD%93%E5%9B%BE%E9%89%B4?action=raw"
FRAG_URL     = "https://prts.wiki/w/%E8%90%A8%E5%8D%A1%E5%85%B9%E7%9A%84%E6%97%A0%E7%BB%88%E5%A5%87%E8%AF%AD/%E4%BD%9A%E6%95%A3%E6%80%9D%E7%BB%B4%E8%BE%91%E5%BD%95?action=raw"
DISASTER_URL = "https://prts.wiki/w/%E8%90%A8%E5%8D%A1%E5%85%B9%E7%9A%84%E6%97%A0%E7%BB%88%E5%A5%87%E8%AF%AD/%E5%B7%AB%E4%BB%AA%E6%A1%A3%E6%A1%88%E5%BA%93?action=raw"

# 角标变量 → 中文
FLAG_DEFS = {
    "notrade":              "不可于【失与得】交换",
    "stack":                "可计数叠加层数",
    "oneoff":               "一次性使用",
    "suit_king":            "属性：国王收藏品",
    "suit_demonking":       "属性：魔王收藏品",
    "type_curse":           "属性：诡谲断章收藏品",
    "merchant_refresh":     "仅在特定印象重建后有概率于诡意行商出售",
    "truth_common":         "有概率于去伪存真“寻常奖励”中出现",
    "truth_advanced":       "有概率于去伪存真“高级奖励”中出现",
    "truth_rare":           "有概率于去伪存真“稀有奖励”中出现",
    "truth_flash":          "有概率于去伪存真“灵光乍现”中出现",
    "final_chorus_wealth":  "有概率于“终曲合声”掉落",
    "noshop":               "不在诡意行商中出售",
}
# 需要排除的角标（dlc 标记）
FLAG_EXCLUDE = {"dlc1", "dlc2"}

# 思绪子类型映射
FRAG_TYPE_MAP = {"WISH": "遗愿", "INSPIRATION": "灵感", "IDEA": "构想"}

# 年代追忆级别名
DISASTER_LEVEL_MAP = {"a": "成型期", "b": "扩张期", "c": "鼎盛期"}

# 三个特殊藏品
SPECIAL_THREE = ["希望时代的涂鸦", "死仇时代的恨意", "美愿时代的留恋"]


# ── 工具函数 ──────────────────────────────────────
def fetch(url):
    req = urllib.request.Request(url, headers={"User-Agent": "PRTS-DB/1.0"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return resp.read().decode("utf-8")

def strip(text):
    """清洗 wikitext/HTML 标记，保留模板中的文本内容"""
    if not text:
        return ""
    text = re.sub(r"\{\{color\|[^}|]+\|([^}]+)\}\}", r"\1", text)
    text = re.sub(r"\{\{特殊机制\|([^}|]+).*?\}\}", r"\1", text)
    text = re.sub(r"\{\{技能类型\|([^}]+)\}\}", r"\1", text)
    text = re.sub(r"\{\{异常效果\|([^}|]+).*?\}\}", r"\1", text)
    text = re.sub(r"(?is)\{\{popup[^}|]*\|内容\s*=\s*([^}|]+).*?\}\}", r"(\1)", text)
    text = re.sub(r"(?i)\{\{popup/range[^}]*\}\}", "", text)
    text = re.sub(r"(?is)\{\{popup\|([^}|]+).*?\}\}", r"(\1)", text)
    text = re.sub(r"\{\{模糊\|(.+?)(\}\})?", r"\1", text, flags=re.DOTALL)
    text = re.sub(r"\{\{污染\|(.+?)(\}\})?", r"\1", text, flags=re.DOTALL)
    text = re.sub(r"\{\{修正\|([^}|]+).*?\}\}", r"\1", text)
    text = re.sub(r"\{\{变动数值lite\|[^}]+\}\}", "", text)
    text = re.sub(r"<!--.*?-->", "", text, flags=re.DOTALL)
    text = re.sub(r"<br\s*/?>", "\n", text)
    text = re.sub(r"</?[a-zA-Z][^>]*?>", "", text)
    text = re.sub(r"''+", "", text)
    text = re.sub(r"\[\[([^\]|]+)\]\]", r"\1", text)
    text = re.sub(r"\[\[[^\]|]+\|([^\]]+)\]\]", r"\1", text)
    text = text.replace("{{", "").replace("}}", "")
    lines = []
    for line in text.split("\n"):
        line = re.sub(r"\s+", " ", line).strip()
        if line:
            lines.append(line)
    return "\n".join(lines)

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

def parse_flags(mark_text):
    """从角标参数解析特性列表，排除 dlc 标记"""
    flags = []
    for key, label in FLAG_DEFS.items():
        if key in mark_text:
            flags.append(label)
    return flags


# ── 建表 ──────────────────────────────────────────
def create_table(conn):
    conn.execute("DROP TABLE IF EXISTS rogue_4_entries")
    conn.execute("""
        CREATE TABLE rogue_4_entries (
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
    r4 = data["details"]["rogue_4"]
    m4 = data["modules"]["rogue_4"]
    items  = r4["items"]
    relics = r4["relics"]

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

    # ── 思绪：items[type=FRAGMENT] + modules.fragment.fragmentData ──
    frag_data = m4["fragment"]["fragmentData"]
    for iid, it in items.items():
        if it.get("type") != "FRAGMENT":
            continue
        fv = frag_data.get(iid, {})
        ftype = fv.get("type", "?")
        type_cn = FRAG_TYPE_MAP.get(ftype, ftype)
        weight = fv.get("weight")
        trait = f"负荷：{weight}" if weight is not None else None
        rows.append((
            it["id"],
            it["name"],
            it.get("usage", ""),
            f"思绪（{type_cn}）",
            fv.get("value"),          # 稀有度 ← value
            None,                     # buff 无
            None,                     # 备注 → PRTS
            trait,                    # 特性 ← 负荷：weight
        ))

    # ── 年代追忆：modules.disaster.disasterData ──
    dis_data = m4["disaster"]["disasterData"]
    for did, dv in dis_data.items():
        suffix = did.rsplit("_", 1)[1]  # a/b/c
        level_cn = DISASTER_LEVEL_MAP.get(suffix, suffix)
        rows.append((
            dv["id"],
            dv["name"],
            dv.get("functionDesc", ""),
            f"年代追忆（{level_cn}）",
            None,   # 稀有度 空
            None,   # buff 空
            None,   # 备注 → PRTS
            None,   # 特性 空
        ))

    conn.executemany(
        "INSERT INTO rogue_4_entries (id, 名称, 效果, 类型, 稀有度, buff, 备注, 特性) "
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
            # 剥离 parse_wiki_params 泄漏进来的模板闭合 }} 
            note = re.sub(r"\n?\}\}\s*$", "", note)
            notes_map[icon_id] = strip(note)

    return flags_map, notes_map


def parse_special_three_note(raw):
    """提取「希望时代的涂鸦 / 死仇时代的恨意 / 美愿时代的留恋 备注」表格的共享备注文本"""
    marker = "希望时代的涂鸦 / 死仇时代的恨意 / 美愿时代的留恋 备注"
    idx = raw.find(marker)
    if idx == -1:
        return ""

    # 截取到表格结束 |}
    end = raw.find("\n|}", idx)
    if end == -1:
        end = idx + 5000
    table = raw[idx:end]

    # 表格结构：
    # | {{mdi|arrow-up-left-bold}}
    # | 备注文本...
    # | {{mdi|arrow-down-right-bold}}

    # 提取三个方向箭头之间的文本（箭头上、文本、箭头下）
    # 找第一个 arrow-up-left-bold 之后、arrow-down-right-bold 之前的文本
    up_idx = table.find("arrow-up-left-bold")
    down_idx = table.find("arrow-down-right-bold")
    if up_idx == -1 or down_idx == -1:
        return ""

    # 从 up 后的 | 开始到 down 前的 | 结束
    mid = table[up_idx:down_idx]
    # 找到第二个 | 分隔符
    pipes = [m.start() for m in re.finditer(r"(?<!\\)\|", mid)]
    if len(pipes) < 2:
        return ""

    content = mid[pipes[0]+1:pipes[1]]
    return strip(content)


# ── PRTS 思绪页解析 ───────────────────────────────
def _parse_thought_blocks(section_body, include_obtain=False):
    """
    解析一节中的 {{:萨卡兹的无终奇语/思绪}} 块
    include_obtain: 是否提取 |获取= 字段（仅灵感节为 True）
    返回 (名称→note, 名称→trait_text)
    """
    notes_map = {}
    traits_map = {}

    blocks = re.split(r"\{\{:萨卡兹的无终奇语/思绪\s*\n", section_body)
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

        # 售价
        price = params.get("售价", "")
        # 备注
        note = params.get("备注", "")

        # 构建特性文本
        trait_parts = []
        if price:
            trait_parts.append(f"售价：{strip(price)}源石锭")
        if include_obtain:
            obtain = params.get("获取", "")
            clean_obtain = strip(obtain)
            if clean_obtain:
                trait_parts.append(clean_obtain)

        if trait_parts:
            traits_map[name] = "\n".join(trait_parts)

        if note:
            notes_map[name] = strip(note)

    return notes_map, traits_map

def parse_fragment_page(raw):
    """
    按章节拆分解析思绪页。
    仅灵感节提取 |获取= 字段（合成配方），遗愿/构想只取售价和备注。
    返回 (名称→note, 名称→trait_text)
    """
    all_notes = {}
    all_traits = {}

    # 按 ==章节名== 拆分
    sections = {}
    section_names = ["遗愿", "灵感", "构想"]
    for i, name in enumerate(section_names):
        start = raw.find(f"== {name} ==")
        if start == -1:
            start = raw.find(f"=={name}==")
        if start == -1:
            continue
        # 下一个章节开始位置
        end = len(raw)
        for j in range(i + 1, len(section_names)):
            next_start = raw.find(f"== {section_names[j]} ==", start + 10)
            if next_start == -1:
                next_start = raw.find(f"=={section_names[j]}==", start + 10)
            if next_start != -1 and next_start < end:
                end = next_start
        sections[name] = raw[start:end]

    # 遗愿：不包括获取方式
    n, t = _parse_thought_blocks(sections.get("遗愿", ""), include_obtain=False)
    all_notes.update(n)
    all_traits.update(t)

    # 灵感：包括获取方式
    n, t = _parse_thought_blocks(sections.get("灵感", ""), include_obtain=True)
    all_notes.update(n)
    all_traits.update(t)

    # 构想：不包括获取方式
    n, t = _parse_thought_blocks(sections.get("构想", ""), include_obtain=False)
    all_notes.update(n)
    all_traits.update(t)

    return all_notes, all_traits


# ── PRTS 年代追忆页解析 ───────────────────────────
def parse_disaster_page(raw):
    """返回 (名称 → note) 的备注映射"""
    notes_map = {}

    sec_start = raw.find("==年代追忆==")
    if sec_start == -1:
        sec_start = raw.find("== 年代追忆 ==")
    if sec_start == -1:
        print("  ⚠ 未找到年代追忆章节")
        return notes_map

    sec_end = raw.find("\n==", sec_start + 10)
    section = raw[sec_start:sec_end] if sec_end != -1 else raw[sec_start:]

    # 按年代名称分割
    name_positions = []
    for m in re.finditer(r"<big>'''(.*?)'''</big>", section):
        name_positions.append((m.start(), m.group(1)))

    for i, (pos, name) in enumerate(name_positions):
        start = pos
        end = name_positions[i + 1][0] if i + 1 < len(name_positions) else len(section)
        entry = section[start:end]

        # 找标题（</big>）之后、成型期之前的 ※ 行
        bold_end = entry.find("</big>")
        after_title = entry[bold_end + 6:] if bold_end >= 0 else entry

        form_pos = after_title.find("成型期")
        if form_pos == -1:
            continue
        after_title = after_title[:form_pos]

        notes = re.findall(r"※(.+?)(?=<br|$|\n)", after_title)
        cleaned = []
        for n in notes:
            c = strip(n)
            if c:
                cleaned.append(f"※{c}")

        if cleaned:
            notes_map[name] = "\n".join(cleaned)

    return notes_map


# ── PRTS 更新 ─────────────────────────────────────
def update_from_prts(conn):
    cur = conn.cursor()
    stats = {}

    try:
        relic_raw  = fetch(RELIC_URL)
        frag_raw   = fetch(FRAG_URL)
        dis_raw    = fetch(DISASTER_URL)
    except Exception as e:
        print(f"  ⚠ 网络错误: {e}")
        return stats

    # ── 藏品：特性 + 备注 ──
    relic_flags, relic_notes = parse_relic_page(relic_raw)

    # 特性（角标 → JSON 数组）
    updated_flags = 0
    for icon_id, flags in relic_flags.items():
        flag_json = json.dumps(flags, ensure_ascii=False)
        cur.execute(
            "UPDATE rogue_4_entries SET 特性 = ? WHERE id = ? AND 类型 = '藏品'",
            (flag_json, icon_id),
        )
        updated_flags += cur.rowcount
    stats["藏品特性"] = (len(relic_flags), updated_flags)

    # 备注
    updated_notes = 0
    for icon_id, note in relic_notes.items():
        cur.execute(
            "UPDATE rogue_4_entries SET 备注 = ? WHERE id = ? AND 类型 = '藏品'",
            (note, icon_id),
        )
        updated_notes += cur.rowcount
    stats["藏品备注"] = (len(relic_notes), updated_notes)

    # 三个特殊藏品的共享备注
    shared_note = parse_special_three_note(relic_raw)
    if shared_note:
        updated_special = 0
        for name in SPECIAL_THREE:
            cur.execute(
                "UPDATE rogue_4_entries SET 备注 = ? WHERE 名称 = ? AND 类型 = '藏品'",
                (shared_note, name),
            )
            updated_special += cur.rowcount
        stats["特殊藏品共享备注"] = updated_special
    else:
        print("  ⚠ 未提取到共享备注")

    # 多元奇物子条目：从基础版复制（验证基础版存在）
    cur.execute("SELECT id FROM rogue_4_entries WHERE 类型 = '藏品'")
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
            "SELECT 特性, 备注 FROM rogue_4_entries WHERE id = ? AND 类型 = '藏品'",
            (base_id,),
        )
        row = cur.fetchone()
        if row and (row[0] or row[1]):
            cur.execute(
                "UPDATE rogue_4_entries SET 特性 = ?, 备注 = ? WHERE id = ?",
                (row[0], row[1], rid),
            )
            copied += cur.rowcount
    if copied:
        stats["多元奇物复制"] = copied
    conn.commit()

    # ── 思绪：售价+获取 → 特性（与 JSON weight 合并）；备注 ──
    frag_notes, frag_traits = parse_fragment_page(frag_raw)

    # 特性（保留 JSON 负荷，追加 PRTS 售价/获取）
    updated_traits = 0
    for name, trait_text in frag_traits.items():
        cur.execute(
            "SELECT 特性 FROM rogue_4_entries WHERE 名称 = ? AND 类型 LIKE '思绪%'",
            (name,),
        )
        row = cur.fetchone()
        existing = row[0] if row else None
        merged = f"{existing}\n{trait_text}" if existing else trait_text
        cur.execute(
            "UPDATE rogue_4_entries SET 特性 = ? WHERE 名称 = ? AND 类型 LIKE '思绪%'",
            (merged, name),
        )
        updated_traits += cur.rowcount
    stats["思绪特性"] = (len(frag_traits), updated_traits)

    # 备注
    updated_notes = 0
    for name, note in frag_notes.items():
        cur.execute(
            "UPDATE rogue_4_entries SET 备注 = ? WHERE 名称 = ? AND 类型 LIKE '思绪%'",
            (note, name),
        )
        updated_notes += cur.rowcount
    stats["思绪备注"] = (len(frag_notes), updated_notes)

    # ── 年代追忆：※ 备注 ──
    dis_notes = parse_disaster_page(dis_raw)

    updated_notes = 0
    for name, note in dis_notes.items():
        cur.execute(
            "UPDATE rogue_4_entries SET 备注 = ? WHERE 名称 = ? AND 类型 LIKE '年代追忆%'",
            (note, name),
        )
        updated_notes += cur.rowcount
    stats["年代追忆备注"] = (len(dis_notes), updated_notes)

    # ── 手动修正 ──
    manual_fixes = [
        ("魔王年代", "实际效果为处于年代印痕中的我方单位无法主动撤退", "年代追忆%"),
        (
            "繁荣年代",
            '（由于该年代不会自然出现，受年代长度结算机制影响，该年代的实际持续为\u201c退出当前节点后的下5步\u201d。）',
            "年代追忆%",
        ),
    ]
    fixed = 0
    for name, extra, type_like in manual_fixes:
        # 取一条现有备注作为基准（同一名称的三级别备注相同）
        cur.execute(
            "SELECT 备注 FROM rogue_4_entries WHERE 名称 = ? AND 类型 LIKE ? LIMIT 1",
            (name, type_like),
        )
        row = cur.fetchone()
        existing = row[0] if row else None
        merged = f"{existing}\n{extra}" if existing else extra
        cur.execute(
            "UPDATE rogue_4_entries SET 备注 = ? WHERE 名称 = ? AND 类型 LIKE ?",
            (merged, name, type_like),
        )
        fixed += cur.rowcount
    stats["手动修正"] = fixed

    conn.commit()
    return stats


# ── 统计输出 ──────────────────────────────────────
def print_stats(conn, json_count, prts_stats):
    cur = conn.cursor()

    print(f"\n{'=' * 60}")
    print("rogue_4_entries 写入统计")
    print(f"{'=' * 60}")
    print(f"  总行数: {json_count}")

    # 按大类统计
    broad_types = [
        ("藏品", "藏品"),
        ("思绪", "思绪%"),
        ("年代追忆", "年代追忆%"),
    ]
    for label, like in broad_types:
        cur.execute(f"SELECT COUNT(*) FROM rogue_4_entries WHERE 类型 LIKE ?", (like,))
        total = cur.fetchone()[0]
        cur.execute(f"SELECT COUNT(*) FROM rogue_4_entries WHERE 类型 LIKE ? AND 备注 IS NOT NULL", (like,))
        noted = cur.fetchone()[0]
        cur.execute(f"SELECT COUNT(*) FROM rogue_4_entries WHERE 类型 LIKE ? AND 特性 IS NOT NULL", (like,))
        tagged = cur.fetchone()[0]
        print(f"  {label}: {total} 条（备注 {noted}，特性 {tagged}）")

    # 子类统计
    for t in ["藏品", "思绪（遗愿）", "思绪（灵感）", "思绪（构想）",
              "年代追忆（成型期）", "年代追忆（扩张期）", "年代追忆（鼎盛期）"]:
        cur.execute("SELECT COUNT(*) FROM rogue_4_entries WHERE 类型 = ?", (t,))
        total = cur.fetchone()[0]
        if total > 0:
            cur.execute("SELECT COUNT(*) FROM rogue_4_entries WHERE 类型 = ? AND 备注 IS NOT NULL", (t,))
            noted = cur.fetchone()[0]
            cur.execute("SELECT COUNT(*) FROM rogue_4_entries WHERE 类型 = ? AND 特性 IS NOT NULL", (t,))
            tagged = cur.fetchone()[0]
            print(f"    {t}: {total} 条（备注 {noted}，特性 {tagged}）")

    # ── 样本抽查 ──
    print(f"\n  ── 藏品样本 ──")
    for row in cur.execute(
        "SELECT id, 名称, 效果, 稀有度, 特性, 备注 FROM rogue_4_entries WHERE 类型 = '藏品' LIMIT 3"
    ):
        print(f"    {row[0]}  {row[1]}  稀有度={row[3]}")
        print(f"      效果: {(row[2] or '')[:100]}")
        if row[4]:
            print(f"      特性: {row[4][:150]}")
        if row[5]:
            print(f"      备注: {row[5][:120]}")

    print(f"\n  ── 特殊藏品（共享备注）──")
    for name in SPECIAL_THREE:
        cur.execute(
            "SELECT 备注 FROM rogue_4_entries WHERE 名称 = ? AND 类型 = '藏品'",
            (name,),
        )
        row = cur.fetchone()
        if row:
            print(f"    {name}: 备注长度={len(row[0] or '')}")

    print(f"\n  ── 思绪样本 ──")
    for t in ["思绪（遗愿）", "思绪（灵感）", "思绪（构想）"]:
        cur.execute(
            "SELECT id, 名称, 效果, 特性, 备注 FROM rogue_4_entries WHERE 类型 = ? LIMIT 1",
            (t,),
        )
        row = cur.fetchone()
        if row:
            print(f"    [{t}] {row[0]}")
            print(f"      名称: {row[1]}")
            print(f"      效果: {(row[2] or '')[:100]}")
            if row[3]:
                print(f"      特性: {row[3][:150]}")
            if row[4]:
                print(f"      备注: {row[4][:120]}")

    print(f"\n  ── 年代追忆样本 ──")
    for t in ["年代追忆（成型期）", "年代追忆（扩张期）", "年代追忆（鼎盛期）"]:
        cur.execute(
            "SELECT id, 名称, 效果, 备注 FROM rogue_4_entries WHERE 类型 = ? LIMIT 1",
            (t,),
        )
        row = cur.fetchone()
        if row:
            print(f"    [{t}] {row[0]}  {row[1]}")
            print(f"      效果: {(row[2] or '')[:120]}")
            if row[3]:
                print(f"      备注: {row[3][:150]}")

    # 多元奇物复制验证
    print(f"\n  ── 多元奇物复制验证 ──")
    cur.execute(
        "SELECT id, 名称, 特性, 备注 FROM rogue_4_entries "
        "WHERE 类型 = '藏品' AND id LIKE '%\\_a' ESCAPE '\\' LIMIT 5"
    )
    for row in cur.fetchall():
        print(f"    {row[0]} ({row[1]}): 特性={'有' if row[2] else '无'}, 备注={'有' if row[3] else '无'}")

    # 手动修正验证
    print(f"\n  ── 手动修正验证 ──")
    for name in ["魔王年代", "繁荣年代"]:
        cur.execute(
            "SELECT 名称, 备注 FROM rogue_4_entries WHERE 名称 = ? AND 类型 LIKE '年代追忆%' LIMIT 1",
            (name,),
        )
        row = cur.fetchone()
        if row:
            print(f"    {row[0]}: 备注={(row[1] or '')[:200]}")


# ── 主流程 ────────────────────────────────────────
def main():
    conn = sqlite3.connect(DB_PATH)

    print("创建表 rogue_4_entries ...")
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
