"""
PRTS 终端 — traps PRTS 数据补丁
仅处理上一轮爬取中失败的装置（404 / 消歧义 / 无模板）

用法：python patch_traps_prts.py
"""

import re, json, sqlite3, os, time, urllib.request, urllib.parse

OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB  = os.path.join(OUTPUT_DIR, "arknights.db")

# ═══════════════════════════════════════════════════════
# 补丁目标：device_id → 正确 PRTS 搜索名
# 来源：traps_prts_errors_t.log 中手动核实的结果
# ═══════════════════════════════════════════════════════

PATCH_MAP = {
    # ── 404：游戏数据名称与 PRTS 页面名不一致 ──
    "trap_1043_acarm043": "坚定维式重锤",
    "trap_1048_acarm048": "口袋法阵",
    "trap_1066_acarm066": "幸运硬币",

    # ── 消歧义：需指定子页面 ──
    "trap_065_normbox":  "宝箱(水月)",
    "trap_108_smbox":    "宝箱(萨米)",
    "trap_757_skzbox":   "宝箱(萨卡兹)",
    "trap_066_rarebox":  "尖刺宝箱(水月)",
    "trap_109_smrbox":   "尖刺宝箱(萨米)",

    # ── 无模板：需特定后缀 ──
    "trap_1005_acarm005": "信标(卫戍协议)",
    "trap_1109_acarm109": "信标(装置)",

    # ── 降级列表中唯一不需要后缀的 ──
    "trap_1500_lhgras": "草丛",
}

# ═══════════════════════════════════════════════════════
# 降级策略：先试原名，不行再加 (装置) 后缀
# 覆盖：上一轮"无模板"+"无装置信息"+"干员页"
# ═══════════════════════════════════════════════════════

FALLBACK_NAMES = [
    # 上一轮失败，统一加 (装置) 后缀重试
    # ── 无模板 ──
    "R-11a突击动力装甲", "R-11突击动力装甲",
    "R-31a重型动力装甲", "R-31重型动力装甲",
    "压力归零帮手", "压力舒缓帮手",
    "天桩", "巨壳藤蔓", "弱化节点", "怪葫芦",
    "物资浮板", "箱形恐鱼", "观赏植物",
    # ── 无装置信息 ──
    "冰镇饮料机", "加热饮料机",
    "活性态萨卡兹术师结晶", "稳定态萨卡兹术师结晶",
    "\\\"寒灾回响\\\"", "\\\"风雪中的影子\\\"",
    # ── 干员页 ──
    "Mon3tr", "Stormeye", "Thermal-EX",
    "佩佩", "可露希尔", "图耶", "异客", "断罪者",
    "森蚺", "狮蝎", "红隼", "阿米娅",
    "大祭司", "特雷西斯",
]


def fetch_wikitext(name):
    encoded = urllib.parse.quote(name)
    url = f"https://prts.wiki/w/{encoded}?action=raw"
    req = urllib.request.Request(url, headers={'User-Agent': 'PRTS-Terminal/1.0'})
    with urllib.request.urlopen(req) as resp:
        text = resp.read().decode("utf-8")
    m = re.match(r'(?i)#(?:redirect|重定向)\s*\[\[([^\]]+)\]\]', text)
    if m:
        return fetch_wikitext(m.group(1))
    return text


def parse_template(text, tpl_name):
    results = []
    start_tag = '{{' + tpl_name
    pos = 0
    while True:
        idx = text.find(start_tag, pos)
        if idx == -1:
            break
        depth, i = 0, idx
        while i < len(text):
            if text[i:i+2] == '{{':
                depth += 1; i += 2; continue
            if text[i:i+2] == '}}':
                depth -= 1
                if depth == 0:
                    content = text[idx+len(start_tag):i]
                    params = {}
                    for part in content.split('\n|'):
                        part = part.strip()
                        if '=' not in part:
                            continue
                        k, v = part.split('=', 1)
                        params[k.lstrip('|').strip()] = v.strip()
                    results.append(params)
                    pos = i + 2
                    break
                i += 2; continue
            i += 1
        else:
            pos = idx + 2
    return results


def clean_wiki(text):
    if not text:
        return text
    text = re.sub(r'\[\[([^\]|]+?)(?:\|([^\]]+?))?\]\]',
                  lambda m: m.group(2) or m.group(1), text)
    text = re.sub(r'\{\{color\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)
    text = re.sub(r'\{\{术语\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)
    text = re.sub(r'\{\{异常效果\|([^|}]+)(?:\|([^|}]+))?\}\}',
                  lambda m: m.group(1) + (m.group(2) or ''), text)
    text = re.sub(r"'''([^']*)'''", r'\1', text)
    text = re.sub(r'<br\s*/?>', '\n', text)
    text = re.sub(r'<nowiki\s*/>', '', text)
    text = re.sub(r'<!--[\s\S]*?-->', '', text)
    text = re.sub(r'\{\{popup\|[^}]*?内容=([^}|]+?)[^}]*\}\}', r'\1', text)
    text = re.sub(r'</?nowiki>', '', text)
    return text.strip()


def is_operator_page(wikitext):
    # 干员页无法获取装置模板
    if '{{干员信息}}' in wikitext or '{{CharinfoV2' in wikitext:
        return True
    # 部分 NPC 角色也有特殊页面结构
    if '{{敌人信息' in wikitext:
        return True
    return False


def try_fetch_and_process(conn, rowid, tid, name, skill_json, search_name):
    """
    尝试用 search_name 获取页面并写入数据库。
    返回 "ok"（成功写入）、"retry"（页面类型不对，可换候选名）、"fail"（网络错误，放弃）
    """
    try:
        wikitext = fetch_wikitext(search_name)
        # 删除 HTML 注释，防止内部的 {{ }} 干扰模板深度解析
        wikitext = re.sub(r'<!--[\s\S]*?-->', '', wikitext)
    except urllib.error.HTTPError as e:
        if e.code == 404:
            print(f"  ↻ 404: {search_name}")
            return "retry"
        print(f"  ✗ HTTP {e.code}: {search_name}")
        return "fail"
    except Exception as e:
        print(f"  ✗ 网络: {e}")
        return "fail"

    if is_operator_page(wikitext):
        print(f"  ↻ 干员/敌人页，非装置模板: {name} → {search_name}")
        return "retry"

    updated = False

    # 装置信息
    info_tpls = parse_template(wikitext, '装置信息')
    if info_tpls:
        info = info_tpls[0]

        radius_raw = info.get('阻挡半径', '')
        if radius_raw:
            try:
                radius = float(radius_raw)
                cur = conn.execute(
                    "SELECT 阻挡半径 FROM traps WHERE rowid=?", (rowid,)
                ).fetchone()[0]
                if cur != radius:
                    conn.execute(
                        "UPDATE traps SET 阻挡半径=? WHERE rowid=?",
                        (radius, rowid)
                    )
                    print(f"  ✓ 阻挡半径: {name} → {radius}")
                    updated = True
            except ValueError:
                pass

        mech_raw = info.get('装置机制', '')
        mech = clean_wiki(mech_raw) if mech_raw else None
        if mech:
            cur = conn.execute(
                "SELECT 装置机制 FROM traps WHERE rowid=?", (rowid,)
            ).fetchone()[0]
            if cur != mech:
                conn.execute(
                    "UPDATE traps SET 装置机制=? WHERE rowid=?",                    (mech, rowid)
                )
                print(f"  ✓ 装置机制: {name}")
                updated = True

    # 技能备注
    if skill_json:
        skill_tpls = parse_template(wikitext, '装置技能')
        if skill_tpls:
            skills = json.loads(skill_json)
            seen = {}
            for s in skill_tpls:
                sname = s.get('技能名', '')
                if not sname or sname in seen:
                    continue
                seen[sname] = s

            changed = False
            for sk in skills:
                sname = sk.get('技能名', '')
                if sname in seen:
                    note_raw = seen[sname].get('备注', '')
                    note = clean_wiki(note_raw) if note_raw else None
                    if sk.get('备注') != note:
                        sk['备注'] = note
                        changed = True

            if changed:
                conn.execute(
                    "UPDATE traps SET 技能列表=? WHERE rowid=?",
                    (json.dumps(skills, ensure_ascii=False), rowid)
                )
                print(f"  ✓ 技能备注: {name}")
                updated = True

    if not updated and not info_tpls and not (skill_json and parse_template(wikitext, '装置技能')):
        # 页面存在但没有装置相关模板，不是装置页，换候选名
        print(f"  ↻ 无装置模板: {name} → {search_name}")
        return "retry"

    if not updated:
        print(f"  - 无新数据: {name}")
    return "ok"


# ═══════════════════════════════════════════════════════
# 主流程
# ═══════════════════════════════════════════════════════

def main():
    conn = sqlite3.connect(OUTPUT_DB)
    conn.execute("PRAGMA journal_mode=OFF")

    # ── 收集需要补丁的装置 ──
    rows = conn.execute(
        "SELECT rowid, 装置ID, 名称, 技能列表 FROM traps"
    ).fetchall()

    # PATCH_MAP：装置ID → 精确搜索名
    exact_targets = []
    # FALLBACK_NAMES：名称 → [原名, 原名(装置)]
    fallback_targets = []

    fallback_set = set(FALLBACK_NAMES)

    for rowid, tid, name, skill_json in rows:
        clean_name = name.strip().replace('\n', '').replace('\r', '')

        if tid in PATCH_MAP:
            exact_targets.append((rowid, tid, name, skill_json, PATCH_MAP[tid]))
        elif clean_name in fallback_set:
            fallback_targets.append((rowid, tid, name, skill_json, clean_name))

    total = len(exact_targets) + len(fallback_targets)
    print(f"====== 补丁 ({total} 个装置) ======")
    print(f"  精确映射: {len(exact_targets)}")
    print(f"  降级尝试: {len(fallback_targets)}")

    ok = 0
    fail = 0
    idx = 0

    # ── 精确映射：直接用指定名 ──
    for rowid, tid, name, skill_json, search_name in exact_targets:
        idx += 1
        print(f"\n[{idx}/{total}] {name} ({tid})")
        print(f"  搜索: {search_name}")

        result = try_fetch_and_process(conn, rowid, tid, name, skill_json, search_name)
        if result == "ok":
            ok += 1
        else:
            print(f"  ✗ 失败: {search_name}")
            fail += 1

        time.sleep(0.1)

    # ── 降级：直接搜 原名(装置) ──
    for rowid, tid, name, skill_json, clean_name in fallback_targets:
        idx += 1
        search_name = f"{clean_name}(装置)"

        print(f"\n[{idx}/{total}] {name} ({tid})")
        print(f"  搜索: {search_name}")

        result = try_fetch_and_process(conn, rowid, tid, name, skill_json, search_name)
        if result == "ok":
            ok += 1
        else:
            print(f"  ✗ 失败: {search_name}")
            fail += 1

        time.sleep(0.1)

    conn.commit()
    conn.close()

    print(f"\n====== 完成 ======")
    print(f"  成功: {ok}")
    print(f"  失败: {fail}")


if __name__ == '__main__':
    main()
