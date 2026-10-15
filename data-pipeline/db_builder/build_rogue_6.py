# -*- coding: utf-8 -*-
"""建表并全量入库：沉沦者的黑流树海（rogue_6）

表：rogue_6_entries，8 列与前五个主题一致
    id / 名称 / 效果 / 类型 / 稀有度 / buff / 备注 / 特性

类型与来源（行数为当前数据源实测，哨兵按此设下限）：
    藏品                     239 + 多元奇物变体   页面「拟造物质编目」+ JSON items/relics
    战术道具                   6                   同页面（No.219-224）+ JSON items/traps
    零件（加工品/自然物/概念体）   12 / 12 / 6         页面「引擎配件目录」+ JSON items
    实托邦·理念（早期/中期/晚期） 30（10 条 × 3 级）   页面「黑流数据库」§乌托邦幸福论 + JSON weather
    实托邦·方针                4                   同上（subWeatherData）
    乌托邦                    9                   同上（details.variationData）

列的口径（Air 定）：
    名称 / 效果 / 稀有度 / buff   取 JSON（稀有度用数据源枚举 NORMAL/RARE/SUPER_RARE）
    备注                         取页面；理念/方针/乌托邦 = 标题下那行 + 独立 ※ 行 + 效果格内 ※ 段，全取
    特性                         藏品 = 4 种角标中文数组；零件 = {"标识": […], "售价"/"购买价"+"基础估价"}
    风味描述（description）、解锁条件、获取方式 不入库（与前五张表一致）

手填类型（分队 / 节点事件类型）不在本脚本范围内——由 Air 自行补全。
"""

import argparse
import json
import os
import re
import sqlite3
import sys
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))                     # data-pipeline/
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "maintenance"))

import paths  # noqa: E402
import update_common as uc  # noqa: E402
from cleaning_wiki import clean_wiki, looks_dirty  # noqa: E402

SCRIPT_VERSION = 1
TABLE = "rogue_6_entries"
COLS = ["id", "名称", "效果", "类型", "稀有度", "buff", "备注", "特性"]

TOPIC = "rogue_6"
PAGE_RELIC = "沉沦者的黑流树海/拟造物质编目"
PAGE_SCRAP = "沉沦者的黑流树海/引擎配件目录"
PAGE_DB = "沉沦者的黑流树海/黑流数据库"
TPL_SCRAP = ":沉沦者的黑流树海/零件"

T_RELIC = "藏品"
T_TOOL = "战术道具"
T_SCRAP = "零件（%s）"
T_MAIN = "实托邦·理念（%s）"
T_SUB = "实托邦·方针"
T_UTOPIA = "乌托邦"

# 藏品四种角标（Air 给的文本）
RELIC_BADGE = {
    "notrade": "不可于【失与得】交换",
    "stack": "可计数叠加层数",
    "oneoff": "一次性使用",
    "type_doghunt": "与“追猎”作战有关",
}
VARIANT_SUFFIX = [("a", "α"), ("b", "β"), ("c", "γ")]
# 哨兵：类型 → 允许的最少条数
SENTINELS = {
    "拟造物质编目 · 收藏品块": 200,
    "引擎配件目录 · 零件块": 25,
    "乌托邦幸福论 · 理念": 24,
    "乌托邦幸福论 · 方针": 3,
    "乌托邦幸福论 · 乌托邦": 7,
}

RE_VAR = re.compile(r"\{\{#var(?:define)?:[^}]*\}\}")
RE_RANGE = re.compile(r"\{\{popup/range\|([^|}]+)[^}]*\}\}")
RE_BADGE_RELIC = re.compile(r"\{\{收藏品/角标\|([^}|]+)\}\}")
RE_BADGE_VAR = re.compile(r"\{\{#var:([^}|]+)\}\}")
RE_VARDEF = re.compile(r"\{\{#vardefine:\s*([^|]+?)\s*\|([\s\S]*?)\}\}\s*(?=\{\{#vardefine|\}\})")
RE_TOOLTIP = re.compile(r"data-trigger[^>]*>([^<]+)</span>")
RE_SECTION = re.compile(r"^==\s*([^=]+?)\s*==\s*$", re.M)
RE_SUBSECTION = re.compile(r"^===\s*([^=]+?)\s*===\s*$", re.M)
RE_ANCHOR = re.compile(r"\{\{锚点\|([^}]+)\}\}")
RE_TITLE = re.compile(r"class=\"title\"\|<big>'''(.+?)'''</big><br>''(.+?)''")
RE_LEVEL = re.compile(r"^(早期|中期|晚期)\b")
# 表格单元：属性段（以 rowspan/colspan/style/class/… 开头）才算分隔，
# 否则内容里自带的 | （如 {{popup/range|x-4}}）会被误当分隔符
RE_CELL_ATTR = re.compile(
    r"^\|(?:rowspan|colspan|style|class|width|height|background|align|valign|id|title"
    r"|scope|data-[\w-]+)\s*=[^|]*\|(.*)$")
RE_CELL_PLAIN = re.compile(r"^\|(.*)$")
RE_WIKILINK = re.compile(r"\[\[|\]\]")

# 手工修正钩子：{id: {列: 值}}。写在这里的内容在入库前覆盖，记录里注明行数。
MANUAL_FIXES = {}


# ─── 清洗 ─────────────────────────────────────────────────────

def clean(text, scrap=False):
    """页面文本 → 纯文本。零件上的 {{popup/range}} 按 Air 口径叫「范围」。"""
    if text is None:
        return None
    text = RE_VAR.sub("", text)
    if scrap:
        text = RE_RANGE.sub(lambda m: "[范围: %s]" % m.group(1).strip(), text)
    return clean_wiki(text)


def unlink(text):
    return re.sub(r"\[\[([^\]|]+?)(?:\|([^\]]+?))?\]\]",
                  lambda m: m.group(2) or m.group(1), text or "")


def parse_block_params(body):
    """块参数 → {键: 值}；续行算进上一个键。"""
    out, key, buf = {}, None, []
    for line in body.split("\n"):
        m = re.match(r"^\|([^=]+?)\s*=\s?(.*)$", line)
        if m:
            if key:
                out[key] = "\n".join(buf).strip()
            key, buf = m.group(1).strip(), [m.group(2)]
        elif key is not None:
            buf.append(line)
    if key:
        out[key] = "\n".join(buf).strip()
    return out


# ─── 页面解析 ─────────────────────────────────────────────────

def parse_relic_page(text):
    """拟造物质编目 → [ {序号, ID, 名称, iconId, 备注, 角标[]} ]（页面顺序）"""
    out = []
    for i, p in enumerate(uc.parse_template(text, "收藏品"), 1):
        icon = (p.get("iconId") or "").strip()
        if not icon:
            continue
        badges = [RELIC_BADGE[k] for k in RE_BADGE_RELIC.findall(p.get("角标") or "")
                  if k in RELIC_BADGE]
        out.append({
            "序号": i,
            "ID": (p.get("ID") or "").strip(),
            "名称": clean(unlink(p.get("名称"))),
            "iconId": icon,
            "备注": clean(p.get("备注")) if p.get("备注") else None,
            "角标": badges,
        })
    return out


def parse_scrap_page(text):
    """引擎配件目录 → ({标识变量: 中文}, [ {ID, 名称, 类型, 备注, 角标[], 价格{}} ])"""
    defs = {}
    for m in RE_VARDEF.finditer(text):
        tip = RE_TOOLTIP.search(m.group(2))
        if tip:
            defs[m.group(1).strip()] = clean(tip.group(1))
    out = []
    for p in uc.parse_template(text, TPL_SCRAP):
        sid = (p.get("ID") or "").strip()
        if not sid:
            continue
        badges = [defs[k] for k in RE_BADGE_VAR.findall(p.get("角标") or "") if k in defs]
        out.append({
            "ID": sid,
            "名称": clean(unlink(p.get("名称"))),
            "类型": (p.get("类型") or "").strip(),
            "备注": clean(p.get("备注"), scrap=True) if p.get("备注") else None,
            "角标": badges,
            "价格": {"售价": p.get("售价"), "购买价": p.get("购买价"),
                     "基础估价": p.get("出售价")},
        })
    return defs, out


def _rows_of_table(seg):
    """wikitext 表格 → [[单元格…], …]（去掉属性段）"""
    rows, cur = [], []
    for line in seg.split("\n"):
        line = line.strip()
        if line == "|-":
            if cur:
                rows.append(cur)
            cur = []
        elif line.startswith("|") and not line.startswith("|}"):
            m = RE_CELL_ATTR.match(line) or RE_CELL_PLAIN.match(line)
            cur.append(m.group(1).strip() if m else line[1:].strip())
    if cur:
        rows.append(cur)
    return rows


def parse_utopia_page(text):
    """黑流数据库 §乌托邦幸福论 → { 节: [ {名称, 描述, 备注[], 效果{级别: 文本}} ] }"""
    m = re.search(r"^==\s*乌托邦幸福论\s*==\s*$", text, re.M)
    if not m:
        return {}
    tail = text[m.end():]
    nxt = RE_SECTION.search(tail)
    seg = tail[:nxt.start()] if nxt else tail

    out = {}
    marks = [(mm.start(), mm.group(1).strip()) for mm in RE_SUBSECTION.finditer(seg)]
    for idx, (pos, name) in enumerate(marks):
        end = marks[idx + 1][0] if idx + 1 < len(marks) else len(seg)
        body = seg[pos:end]
        chunks, entries = RE_ANCHOR.split(body), []
        for j in range(1, len(chunks), 2):
            chunk = chunks[j + 1]
            tbl = chunk[:chunk.find("|}")] if "|}" in chunk else chunk
            t = RE_TITLE.search(tbl)
            entry = {"名称": clean(chunks[j]), "描述": None, "备注": [], "效果": {}}
            if t:
                entry["名称"] = clean(unlink(t.group(1)))
                entry["描述"] = clean(t.group(2))
            for cells in _rows_of_table(tbl):
                joined = "\n".join(cells)
                if not cells or "<big>" in joined or 'class="title"' in joined:
                    continue
                lv = RE_LEVEL.match(cells[0]) if len(cells) > 1 else None
                if lv:
                    entry["效果"][lv.group(1)] = cells[1]
                    continue
                c = cells[-1]
                if c.startswith("※"):
                    entry["备注"].append(clean(c))
                else:
                    entry["效果"].setdefault("_single", clean(c))
            entries.append(entry)
        out[name] = entries
    return out


# ─── 组装 ─────────────────────────────────────────────────────

def jdumps(obj):
    return json.dumps(obj, ensure_ascii=False) if obj else None


def load_json():
    """主题表 → (topics[主题], details[主题], modules[主题])"""
    p = os.path.join(str(paths.EXCEL), "roguelike_topic_table.json")
    with open(p, encoding="utf-8") as f:
        data = json.load(f)
    return data["topics"][TOPIC], data["details"][TOPIC], data["modules"][TOPIC]


def build_rows(relic_page, items, relics, tool_ids, scrap_page, weather, variations, utopia):
    rows, tips, seen = [], [], set()

    def add(row, iid):
        if iid in seen:                      # 页面上同一块可能出现两次（襁褓收藏品那类被引用的段）
            tips.append("重复条目（已跳过）：%s" % iid)
            return
        seen.add(iid)
        rows.append(dict((c, row.get(c)) for c in COLS))

    # 1) 藏品（含多元奇物变体：页面 ID 001-013 的基础条目 + α/β/γ）
    for b in relic_page:
        iid = b["iconId"]
        if iid in tool_ids:                  # 战术道具在同一个页面里，归入下一段
            continue
        if iid not in items:
            tips.append("页面有、JSON 无：%s（%s）" % (iid, b["名称"]))
            continue
        it = items[iid]
        base = {"id": iid, "名称": it.get("name") or b["名称"], "效果": it.get("usage"),
                "类型": T_RELIC, "稀有度": it.get("rarity"),
                "buff": jdumps((relics.get(iid) or {}).get("buffs")),
                "备注": b["备注"], "特性": jdumps(b["角标"])}
        add(base, iid)
        if b["ID"].isdigit() and int(b["ID"]) <= 13:
            for suf, cn in VARIANT_SUFFIX:
                vid = "%s_%s" % (iid, suf)
                if vid not in items:
                    continue
                vit = items[vid]
                add({"id": vid,
                     "名称": vit.get("name") or "%s-%s" % (base["名称"], cn),
                     "效果": vit.get("usage"), "类型": T_RELIC,
                     "稀有度": vit.get("rarity"),
                     "buff": jdumps((relics.get(vid) or {}).get("buffs")),
                     "备注": b["备注"], "特性": base["特性"]}, vid)

    # 2) 战术道具
    by_icon = dict((b["iconId"], b) for b in relic_page)
    for tid in tool_ids:
        it = items.get(tid) or {}
        pb = by_icon.get(tid)
        add({"id": tid, "名称": it.get("name"), "效果": it.get("usage"), "类型": T_TOOL,
             "稀有度": it.get("rarity"), "buff": None,
             "备注": pb["备注"] if pb else None, "特性": None}, tid)
        if not pb:
            tips.append("战术道具 %s 在收藏品页上没有对应块" % tid)

    # 3) 零件
    for b in scrap_page:
        iid = b["ID"]
        if iid not in items:
            tips.append("页面有、JSON 无：%s（%s）" % (iid, b["名称"]))
            continue
        it = items[iid]
        feat = {}
        if b["角标"]:
            feat["标识"] = b["角标"]
        for k in ("售价", "购买价", "基础估价"):
            v = b["价格"].get(k)
            if v not in (None, ""):
                feat[k] = int(v) if str(v).strip().isdigit() else str(v).strip()
        add({"id": iid, "名称": it.get("name") or b["名称"], "效果": it.get("usage"),
             "类型": T_SCRAP % b["类型"], "稀有度": it.get("rarity"), "buff": None,
             "备注": b["备注"], "特性": jdumps(feat)}, iid)

    # 4) 乌托邦幸福论
    def page_entry(section, name):
        for e in utopia.get(section, []):
            if e["名称"] == name or e["名称"].strip("“”") == (name or "").strip("“”"):
                return e
        return None

    def notes_of(e, desc):
        ns = []
        if e and e["描述"]:
            ns.append(e["描述"])
        elif desc:
            ns.append(clean(desc))
        if e:
            ns += e["备注"]
        return "\n".join(x for x in ns if x) or None

    for wid, w in (weather.get("mainWeatherData") or {}).items():
        e = page_entry("实托邦·理念", w.get("name"))
        add({"id": wid, "名称": w.get("name"), "效果": w.get("functionDesc"),
             "类型": T_MAIN % (w.get("levelName") or ""), "稀有度": None, "buff": None,
             "备注": notes_of(e, w.get("desc")), "特性": None}, wid)

    for sid, s in (weather.get("subWeatherData") or {}).items():
        e = page_entry("实托邦·方针", s.get("name"))
        add({"id": sid, "名称": s.get("name"), "效果": s.get("functionDesc"),
             "类型": T_SUB, "稀有度": None, "buff": None,
             "备注": notes_of(e, s.get("desc")), "特性": None}, sid)

    for vid, v in variations.items():
        e = page_entry("乌托邦", v.get("outerName") or v.get("innerName"))
        eff, extra = v.get("functionDesc"), []
        if e and e["效果"].get("_single"):
            cell = e["效果"]["_single"]
            if "\n※" in cell:
                eff, tail = cell.split("\n※", 1)
                extra.append("※" + tail)
            else:
                eff = cell
        notes = []
        if e and e["描述"]:
            notes.append(e["描述"])
        elif v.get("desc"):
            notes.append(clean(v.get("desc")))
        notes += extra
        if e:
            notes += e["备注"]
        add({"id": vid, "名称": v.get("outerName") or v.get("innerName"),
             "效果": clean(eff), "类型": T_UTOPIA, "稀有度": None, "buff": None,
             "备注": "\n".join(x for x in notes if x) or None, "特性": None}, vid)

    # 效果列统一清洗（JSON 侧也可能带标记）；零件按其口径
    for r in rows:
        if r["效果"]:
            r["效果"] = clean(r["效果"], scrap=r["类型"].startswith("零件"))
    return rows, tips


# ─── 主流程 ───────────────────────────────────────────────────

def main():
    uc.reconfigure_stdout()
    ap = argparse.ArgumentParser(description="建表并全量入库：沉沦者的黑流树海（rogue_6）")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写库、不备份、不写记录")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库")
    args = ap.parse_args()

    started = datetime.now()
    print("====== %s 建表入库 ======" % TOPIC)
    version = uc.read_source_version(str(paths.DATA_SOURCE))
    print("数据源 : %s" % version)
    print("数据库 : %s" % args.db)

    print("\n------ 抓取 PRTS ------")
    pages = {}
    for key, name in (("relic", PAGE_RELIC), ("scrap", PAGE_SCRAP), ("db", PAGE_DB)):
        pages[key] = uc.fetch_wikitext(name)
        print("  %-6s %7d 字符  %s" % (key, len(pages[key]), name))
        uc.time.sleep(uc.SLEEP_BETWEEN_FETCH)

    print("\n------ 解析页面 ------")
    relic_page = parse_relic_page(pages["relic"])
    scrap_defs, scrap_page = parse_scrap_page(pages["scrap"])
    utopia = parse_utopia_page(pages["db"])

    topic, details, modules = load_json()
    items = details.get("items") or {}
    relics = details.get("relics") or {}
    weather = modules.get("weather") or {}
    variations = details.get("variationData") or {}
    tool_ids = list(details.get("traps") or {})

    counts = [
        ("拟造物质编目 · 收藏品块", len(relic_page)),
        ("引擎配件目录 · 零件块", len(scrap_page)),
        ("乌托邦幸福论 · 理念", len(weather.get("mainWeatherData") or {})),
        ("乌托邦幸福论 · 方针", len(weather.get("subWeatherData") or {})),
        ("乌托邦幸福论 · 乌托邦", len(variations)),
    ]
    print("\n------ 结构哨兵 ------")
    bad = False
    for label, n in counts:
        ok = n >= SENTINELS[label]
        bad = bad or not ok
        print("  %-24s %4d  (下限 %d) %s" % (label, n, SENTINELS[label], "OK" if ok else "!! 不足"))
    if bad:
        raise SystemExit("结构哨兵未通过，中止（页面或数据源结构可能变了）")

    rows, tips = build_rows(relic_page, items, relics, tool_ids, scrap_page, weather, variations,
                           utopia)
    n_fix = 0
    for r in rows:
        fix = MANUAL_FIXES.get(r["id"])
        if fix:
            r.update(fix)
            n_fix += 1

    by_type = {}
    for r in rows:
        by_type[r["类型"]] = by_type.get(r["类型"], 0) + 1
    print("\n------ 组装结果 ------")
    for k, v in sorted(by_type.items()):
        print("  %-26s %d" % (k, v))
    print("  合计 %d 行；手工修正 %d 行" % (len(rows), n_fix))
    ids = [r["id"] for r in rows]
    if len(ids) != len(set(ids)):
        raise SystemExit("!! 主键重复，中止")
    dirty = [r["id"] for r in rows if looks_dirty(r.get("效果")) or looks_dirty(r.get("备注"))]
    if dirty:
        print("  !! 未洗净 %d 行：%s" % (len(dirty), dirty[:8]))
    for t in tips[:10]:
        print("  · 提示：%s" % t)

    record = {
        "脚本": {"文件": "db_builder/build_rogue_6.py", "版本": SCRIPT_VERSION},
        "运行时间": started.strftime("%Y-%m-%d %H:%M:%S"),
        "数据源": version, "数据库": os.path.basename(args.db), "表": TABLE,
        "计数": by_type, "合计": len(rows), "手工修正": n_fix,
        "构建提示": tips, "结果": "干跑" if args.dry_run else "成功",
    }

    if args.dry_run:
        print("\n(干跑：未写库、未备份、未写记录)")
        return 0

    print("\n------ 写库（单事务）------")
    if args.no_backup:
        print("  (已跳过备份)")
    else:
        print("  备份: %s" % (uc.backup_db(args.db) or "（未生成）"))
    conn = sqlite3.connect(args.db)
    conn.isolation_level = None
    try:
        conn.execute("PRAGMA journal_mode=MEMORY")
        conn.execute("BEGIN IMMEDIATE")
        try:
            conn.execute("DROP TABLE IF EXISTS %s" % TABLE)
            conn.execute("CREATE TABLE %s (%s)" % (TABLE, ", ".join(
                ["id TEXT PRIMARY KEY", "名称 TEXT NOT NULL"] +
                ["%s TEXT" % c for c in COLS[2:]])))
            conn.executemany(
                "INSERT INTO %s (%s) VALUES (%s)" % (TABLE, ", ".join(COLS),
                                                     ", ".join("?" * len(COLS))),
                [tuple(r.get(c) for c in COLS) for r in rows])
            conn.execute("COMMIT")
        except Exception as e:
            conn.execute("ROLLBACK")
            record["结果"] = "中止（写库失败：%s）" % e
            uc.write_record(TOPIC, record)
            print("\n!! 写库失败并已回滚：%s" % e)
            return 1
    finally:
        conn.close()

    print("\n====== 完成 ======")
    print("  入库 %d 行 | 记录: %s" % (len(rows), uc.write_record(TOPIC, record)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
