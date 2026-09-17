"""
PRTS 终端 — enemies 技能/天赋 入库
从 PRTS wiki 爬取敌人技能和天赋文本，写入 技能文本 和 天赋文本 列

用法：python build_enemies_prts.py
失败记录：prts_errors.log（敌人名称列表）
"""

import re, json, sqlite3, os, time, urllib.request, urllib.parse

OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB  = os.path.join(OUTPUT_DIR, "arknights.db")
ERROR_LOG  = os.path.join(OUTPUT_DIR, "prts_errors.log")

# ─── wikitext 获取 ────────────────────────

def fetch_wikitext(name):
    """获取 PRTS 页面原始 wikitext，自动跟随重定向"""
    encoded = urllib.parse.quote(name)
    url = f"https://prts.wiki/w/{encoded}?action=raw"
    req = urllib.request.Request(url, headers={'User-Agent': 'PRTS-Terminal/1.0'})
    with urllib.request.urlopen(req) as resp:
        text = resp.read().decode("utf-8")
    # 跟随重定向: #REDIRECT [[target]] 或 #重定向 [[target]]
    m = re.match(r'#(?:REDIRECT|重定向)\s*\[\[([^\]]+)\]\]', text)
    if m:
        target = m.group(1)
        return fetch_wikitext(target)
    return text

# ─── 模板解析 ────────────────────────────

def parse_template(text, tpl_name):
    """从 wikitext 中提取指定模板（处理嵌套 {{}}）"""
    results = []
    start_tag = '{{' + tpl_name
    pos = 0
    while True:
        idx = text.find(start_tag, pos)
        if idx == -1: break
        depth, i = 0, idx
        while i < len(text):
            if text[i:i+2] == '{{': depth += 1; i += 2; continue
            if text[i:i+2] == '}}':
                depth -= 1
                if depth == 0: end = i + 2; break
                i += 2; continue
            i += 1
        content = text[idx+len(start_tag):end-2]
        params = {}
        parts = re.split(r'\n\|', content)
        first = parts[0].strip()
        if first.startswith('|'): first = first[1:]
        if '=' in first:
            k, v = first.split('=', 1)
            params[k.strip()] = v.strip()
        for part in parts[1:]:
            part = part.strip()
            if '=' not in part: continue
            k, v = part.split('=', 1)
            params[k.strip()] = v.strip()
        results.append(params)
        pos = end
    return results

# ─── 文本清洗 ────────────────────────────

def clean_wiki(text):
    """
    清洗 PRTS wikitext 标记，白名单模式。
    只处理已知模板/语法，未知标记保留原文。

    规则：
    ① [[链接|显示]] → 显示，[[链接]] → 链接
    ② {{color|#xxx|text}} → text
    ③ {{异常效果|name|flag}} → name+flag（如 沉睡|免疫 → 沉睡免疫）
    ④ {{术语|key|display}} → display
    ⑤ {{变动数值|value|arrow}} → value↓/value↑（-→↓，+→↑）
    ⑥ {{特殊机制|name}} → name
    ⑦ {{popup/range|code}} → [攻击范围: code]
    ⑧ <br> → 换行
    ⑨ '''粗体''' → 去标记
    ⑩ 多条空行压缩
    """
    if not text: return text
    # ①
    text = re.sub(r'\[\[([^\]|]+?)(?:\|([^\]]+?))?\]\]',
                  lambda m: m.group(2) or m.group(1), text)
    # ②
    text = re.sub(r'\{\{color\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)
    # ③ 异常效果：优先匹配带 显示名称= 的多参数版，再兜底标准格式
    text = re.sub(r'\{\{异常效果\|[^|]*\|[^|]*\|显示名称=([^}]+?)\}\}', r'\1', text)
    text = re.sub(r'\{\{异常效果\|([^|}]*)(?:\|([^|}]+))?\}\}',
                  lambda m: (m.group(1) or '') + (m.group(2) or ''), text)
    # ④
    text = re.sub(r'\{\{术语\|[^|}]+?\|([^}]+?)\}\}', r'\1', text)
    # ⑤
    def _arrow(m):
        val, arr = m.group(1), m.group(2)
        if arr == '-': return val + '↓'
        if arr == '+': return val + '↑'
        return val
    text = re.sub(r'\{\{变动数值\|([^|}]+?)\|([^}]+?)\}\}', _arrow, text)
    # ⑤-2 变动数值：兜底匹配单参数版本（无 arrow）
    text = re.sub(r'\{\{变动数值\|([^}]+?)\}\}', r'\1', text)
    # ⑥
    text = re.sub(r'\{\{特殊机制\|([^}]+?)\}\}', r'\1', text)
    # ⑦
    text = re.sub(r'\{\{popup/range\|([^}]+?)\}\}', r'[攻击范围: \1]', text)
    # ⑧
    text = re.sub(r'<br\s*/?>', '\n', text)
    # ⑨
    text = re.sub(r"'''([^']*)'''", r'\1', text)
    # ⑩
    text = re.sub(r'\n{3,}', '\n\n', text)
    return text.strip()

# ─── 提取技能列表 ────────────────────────

def extract_skills(params):
    """从模板参数中提取技能列表"""
    skills = []
    for key in sorted(params.keys()):
        m = re.match(r'^技能(\d+)$', key)
        if not m: continue
        si = m.group(1)
        skills.append({
            "名称":     clean_wiki(params.get(key, '')),
            "初始冷却": params.get(f'技能{si}初始', ''),
            "周期冷却": params.get(f'技能{si}消耗', ''),
            "技力消耗": params.get(f'技能{si}技力', '/'),
            "效果":     clean_wiki(params.get(f'技能{si}效果', '')),
        })
    return skills

# ─── 主流程 ──────────────────────────────

def main():
    conn = sqlite3.connect(OUTPUT_DB)
    conn.execute("PRAGMA journal_mode=OFF")
    conn.execute("PRAGMA synchronous=OFF")

    # 获取所有 level 0 的敌人（以名称去重）
    enemies = conn.execute(
        "SELECT 敌人ID, 名称 FROM enemies WHERE 级别=0 ORDER BY 名称"
    ).fetchall()

    errors = []
    no_pages = []
    no_data_list = []
    updated = 0
    total = len(enemies)
    start_time = time.time()

    print(f"====== 开始爬取 ({total} 种敌人) ======")

    for i, (eid, name) in enumerate(enemies):
        # 进度（每 10 个汇报一次）
        if i % 10 == 0:
            elapsed = time.time() - start_time
            rate = (i / elapsed) if elapsed > 0 else 0
            eta = (total - i) / rate if rate > 0 else 0
            print(f"  [{i}/{total}] {elapsed:.0f}s | ETA {eta:.0f}s | {name}")

        try:
            wikitext = fetch_wikitext(name)
        except urllib.error.HTTPError as e:
            if e.code == 404:
                no_pages.append(name)
                continue
            errors.append(f"{name}\tHTTP {e.code}")
            continue
        except Exception as e:
            errors.append(f"{name}\t网络: {e}")
            continue

        levels = parse_template(wikitext, "敌人信息/levelcontent")
        if not levels:
            no_data_list.append(name)
            continue

        lv0 = levels[0]

        # 构建 per-level 数据（level>0 以 lv0 为基准合并）
        for lv in levels:
            lv_idx = lv.get('index', '0')
            merged = dict(lv0)
            merged.update(lv)

            skills_raw = extract_skills(merged)
            skills_json = json.dumps(skills_raw, ensure_ascii=False) if skills_raw else None
            talent_text = clean_wiki(merged.get('天赋', ''))

            # 更新同一 enemy_id 下对应 level 的行
            conn.execute(
                "UPDATE enemies SET 技能文本=?, 天赋文本=? WHERE 敌人ID=? AND 级别=?",
                (skills_json, talent_text or None, eid, int(lv_idx))
            )
        updated += 1

        time.sleep(0.1)

    conn.commit()

    # 写入错误日志
    with open(ERROR_LOG, "w", encoding="utf-8") as f:
        f.write(f"# PRTS 爬取错误日志\n")
        f.write(f"# 更新时间: {time.strftime('%Y-%m-%d %H:%M:%S')}\n\n")
        if no_pages:
            f.write(f"=== 404 无页面 ({len(no_pages)} 个) ===\n")
            for name in no_pages:
                f.write(f"{name}\n")
            f.write("\n")
        if no_data_list:
            f.write(f"=== 无 levelcontent ({len(no_data_list)} 个) ===\n")
            for name in no_data_list:
                f.write(f"{name}\n")
            f.write("\n")
        if errors:
            f.write(f"=== 其他错误 ({len(errors)} 个) ===\n")
            for e in errors:
                f.write(f"{e}\n")
            f.write("\n")

    # 验证
    has_skill = conn.execute(
        "SELECT COUNT(*) FROM enemies WHERE 技能文本 IS NOT NULL"
    ).fetchone()[0]
    has_talent = conn.execute(
        "SELECT COUNT(*) FROM enemies WHERE 天赋文本 IS NOT NULL"
    ).fetchone()[0]

    print(f"====== 完成 ======")
    print(f"  更新: {updated} 种敌人")
    print(f"  404: {len(no_pages)}")
    print(f"  无数据: {len(no_data_list)}")
    print(f"  错误: {len(errors)}")
    print(f"  有技能: {has_skill} 行")
    print(f"  有天赋: {has_talent} 行")
    print(f"  日志: {ERROR_LOG}")

    conn.close()

if __name__ == '__main__':
    main()
