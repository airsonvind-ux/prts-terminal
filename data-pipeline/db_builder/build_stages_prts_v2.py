"""
PRTS 终端 — 关卡情报爬取 v2（重写）

变更（2026-07-20）：
  - 重写模板解析：由 PRTS 页面模板声明身份，不再依赖 stage ID 后缀反推难度
  - 新增 IS 紧急作战支持（子类型=紧急作战 → IS_EMERGENCY 标签）
  - clean_intel 保留语义标记（{{color}} / {{术语}} / '''粗体'''）
  - 默认增量模式（只爬情报为 NULL 的行），--force 全量重爬
  - 分类日志：区分"已有值跳过""请求失败""无匹配模板""消歧义页"

用法：
  python build_stages_prts_v2.py          # 增量（只补 NULL）
  python build_stages_prts_v2.py --force  # 全量重爬

日志说明：
  prts_v2_errors.log  — HTTP/网络/重定向/消歧义页等请求级错误
  prts_v2_skip.log    — 页面获取成功但未写入情报的情况，包括：
      * 页面模板确实没有情报字段（悖论模拟、保全派驻等设计上就很少带情报）
      * 模板匹配到了但情报值为 NULL（如普通关卡模板无情报，情报在突袭模板中，
        而当前 stage 的难度身份恰好匹配普通模板）
      日志中显示为"无匹配模板(N个模板)"，指页面上的 N 个模板均无法为当前关卡提供非空情报，
      属于预期行为而非映射失败。
"""

import re, json, sqlite3, os, sys, time, urllib.request, urllib.error, socket

socket.setdefaulttimeout(15)

OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB  = os.path.join(OUTPUT_DIR, "arknights.db")
ERROR_LOG  = os.path.join(OUTPUT_DIR, "prts_v2_errors.log")
SKIP_LOG   = os.path.join(OUTPUT_DIR, "prts_v2_skip.log")

FORCE_MODE = "--force" in sys.argv


# ═══════════════════════════════════════════
# 常量
# ═══════════════════════════════════════════

URL_RULES = {
    "生息演算·沙洲遗闻": "{code}_{name}(沙洲遗闻)",
    "悖论模拟":             "悖论模拟_{name}",
}
DEFAULT_URL = "{code}_{name}"
SKIP_TYPES  = {"全息作战矩阵"}

TPL_NAMES = [
    "普通关卡信息", "突袭关卡信息", "剿灭关卡信息", "保全派驻关卡信息",
    "集成战略关卡信息", "区块化关卡信息", "教学关卡信息",
    "活动关卡信息", "主线关卡信息", "支线关卡信息",
]


# ═══════════════════════════════════════════
# wikitext 获取
# ═══════════════════════════════════════════

def fetch_wikitext(name, depth=0):
    if depth > 5:
        raise RuntimeError(f"重定向过深: {name}")
    encoded = urllib.parse.quote(name)
    url = f"https://prts.wiki/w/{encoded}?action=raw"
    req = urllib.request.Request(url, headers={'User-Agent': 'PRTS-Terminal/1.0'})
    with urllib.request.urlopen(req, timeout=10) as resp:
        text = resp.read().decode("utf-8")
    m = re.match(r'#(?:redirect|重定向)\s*\[\[:?([^\]]+)\]\]', text, re.IGNORECASE)
    if m:
        target = m.group(1).replace(' ', '_')
        return fetch_wikitext(target, depth + 1)
    return text


# ═══════════════════════════════════════════
# 模板解析
# ═══════════════════════════════════════════

def parse_template(text, tpl_name):
    """从 wikitext 中提取指定模板，返回参数列表（每个模板一个 dict）"""
    results = []
    start_tag = '{{' + tpl_name
    pos = 0
    while True:
        idx = text.find(start_tag, pos)
        if idx == -1:
            break
        depth, i = 0, idx
        max_iter = 50000
        while i < len(text) and max_iter > 0:
            max_iter -= 1
            if text[i:i+2] == '{{':
                depth += 1
                i += 2
                continue
            if text[i:i+2] == '}}':
                depth -= 1
                if depth == 0:
                    end = i + 2
                    break
                i += 2
                continue
            i += 1
        content = text[idx+len(start_tag):end-2]
        params = {}
        parts = re.split(r'\n\|', content)
        first = parts[0].strip()
        if first.startswith('|'):
            first = first[1:]
        if '=' in first:
            k, v = first.split('=', 1)
            params[k.strip()] = v.strip()
        for part in parts[1:]:
            part = part.strip()
            if '=' not in part:
                continue
            k, v = part.split('=', 1)
            params[k.strip()] = v.strip()
        results.append(params)
        pos = end
    return results


def clean_intel(text):
    """清洗情报文本。保留语义标记（color/术语/粗体），仅删除纯格式噪声。"""
    if not text:
        return text
    # HTML 注释
    text = re.sub(r'<!--.*?-->', '', text, flags=re.DOTALL)
    # wiki 链接 [[目标|显示]] → 显示
    text = re.sub(r'\[\[([^\]|]+?)(?:\|([^\]]+?))?\]\]',
                  lambda m: m.group(2) or m.group(1), text)
    # <span> 标签 → 保留内容
    text = re.sub(r'<span[^>]*>([^<]*)</span>', r'\1', text)
    # <br/> → 换行
    text = re.sub(r'<br\s*/?>', '\n', text)
    # 压缩空行
    text = re.sub(r'\n{3,}', '\n\n', text)
    # {{=}} → =
    text = text.replace('{{=}}', '=')
    # 保留：{{color|...}} {{术语|...}} {{模糊|...}} '''...'''
    return text.strip()


def extract_all_intel(text):
    """
    从 PRTS wikitext 中提取所有模板的情报及身份标签。

    返回 list[dict]:
        {"label": str,       # NORMAL | FOUR_STAR | SIX_STAR | IS_EMERGENCY | ANNIHILATION
         "intel": str|None,  # 清洗后的情报文本
         "tpl_name": str}    # 来源模板名（调试用）
    """
    results = []

    # A. 普通关卡信息
    for tpl in parse_template(text, "普通关卡信息"):
        if tpl.get("全息矩阵化") == "true":
            continue
        if tpl.get("战斗关卡") == "false":
            continue

        mode = tpl.get("关卡难度", "")
        raw = tpl.get("情报")
        intel = clean_intel(raw) if raw else None

        if mode == "SIX_STAR":
            results.append({"label": "SIX_STAR", "intel": intel, "tpl_name": "普通关卡信息(SIX)"})
        elif mode == "FOUR_STAR":
            results.append({"label": "FOUR_STAR", "intel": intel, "tpl_name": "普通关卡信息(FOUR)"})
        else:
            results.append({"label": "NORMAL", "intel": intel, "tpl_name": "普通关卡信息"})

    # B. 突袭关卡信息
    for tpl in parse_template(text, "突袭关卡信息"):
        subtype = tpl.get("子类型", "")
        raw = tpl.get("情报")
        intel = clean_intel(raw) if raw else None

        if subtype == "紧急作战":
            results.append({"label": "IS_EMERGENCY", "intel": intel, "tpl_name": "突袭关卡信息(紧急)"})
        else:
            results.append({"label": "FOUR_STAR", "intel": intel, "tpl_name": "突袭关卡信息"})

    # C. 磨难险地关卡信息
    for tpl in parse_template(text, "磨难险地关卡信息"):
        raw = tpl.get("情报")
        results.append({"label": "SIX_STAR", "intel": clean_intel(raw) if raw else None,
                         "tpl_name": "磨难险地关卡信息"})

    # D. 剿灭关卡信息
    for tpl in parse_template(text, "剿灭关卡信息"):
        raw = tpl.get("情报")
        results.append({"label": "ANNIHILATION", "intel": clean_intel(raw) if raw else None,
                         "tpl_name": "剿灭关卡信息"})

    return results


def match_stage_to_template(stage_ids, template_results):
    """将 stage ID 列表匹配到模板结果。返回 {stage_id: intel}"""
    assignment = {}
    for tpl in template_results:
        label = tpl["label"]
        intel = tpl["intel"]

        if label == "NORMAL":
            candidates = [sid for sid in stage_ids
                          if "#f#" not in sid and "#s" not in sid
                          and "_e_" not in sid and not sid.startswith("tough_")]
        elif label == "FOUR_STAR":
            candidates = [sid for sid in stage_ids if "#f#" in sid]
        elif label == "SIX_STAR":
            candidates = [sid for sid in stage_ids if "#s" in sid or sid.startswith("tough_")]
        elif label == "IS_EMERGENCY":
            candidates = [sid for sid in stage_ids if "_e_" in sid]
        elif label == "ANNIHILATION":
            candidates = stage_ids[:]
        else:
            candidates = []

        for c in candidates:
            if c not in assignment:
                assignment[c] = intel
    return assignment


def extract_enemy_count(text):
    """从 wikitext 提取敌人数量"""
    for tpl_name in TPL_NAMES:
        results = parse_template(text, tpl_name)
        if not results:
            continue
        count = results[0].get("敌人数量")
        if count:
            return count
    return None


# ═══════════════════════════════════════════
# URL 构造
# ═══════════════════════════════════════════

def build_url_key(sid: str, code: str, name: str, stype: str) -> str:
    """构造 PRTS URL key"""
    eff = code.strip()
    if sid.startswith("tough_"):
        eff = "磨难" + eff
    elif "#s" in sid:
        eff = "险地" + eff

    url_tpl = URL_RULES.get(stype, DEFAULT_URL)
    return url_tpl.format(code=eff, name=name.strip())


# ═══════════════════════════════════════════
# Phase 1: 敌人名称解析（保持不变）
# ═══════════════════════════════════════════

def phase1_enemy_names(conn):
    print("\n" + "=" * 70)
    print("  Phase 1: 敌人名称解析")
    print("=" * 70)

    enemy_map = {}
    for eid, ename in conn.execute("SELECT 敌人ID, 名称 FROM enemies").fetchall():
        if eid:
            enemy_map[eid] = ename or eid

    stages = conn.execute("""
        SELECT 关卡ID, 敌人列表 FROM stages
        WHERE 敌人列表 IS NOT NULL
        ORDER BY 关卡ID
    """).fetchall()

    resolved = 0
    unresolved_ids = set()
    for sid, enemy_json in stages:
        if not enemy_json:
            continue
        try:
            enemies = json.loads(enemy_json)
        except json.JSONDecodeError:
            continue
        modified = False
        for e in enemies:
            name = e.get("名称", "")
            if name.startswith("enemy_"):
                cn_name = enemy_map.get(name)
                if cn_name:
                    e["名称"] = cn_name
                    resolved += 1
                    modified = True
                else:
                    unresolved_ids.add(name)
        if modified:
            conn.execute("UPDATE stages SET 敌人列表 = ? WHERE 关卡ID = ?",
                         (json.dumps(enemies, ensure_ascii=False), sid))
    conn.commit()
    print(f"  解析: {resolved} 个敌人名称")
    if unresolved_ids:
        print(f"  无法解析: {len(unresolved_ids)} 个唯一 ID")
    return unresolved_ids


# ═══════════════════════════════════════════
# Phase 2: PRTS 爬取
# ═══════════════════════════════════════════

def phase2_prts_crawl(conn):
    print("\n" + "=" * 70)
    print("  Phase 2: PRTS 爬取（敌人数 + 情报）")
    print("=" * 70)

    if FORCE_MODE:
        conn.execute("UPDATE stages SET 情报 = NULL, 待处理目标数量 = NULL")
        conn.commit()
        print("  --force: 已清空全部 情报 / 待处理目标数量")
    else:
        print("  增量模式: 仅处理情报为 NULL 的关卡（使用 --force 全量重爬）")

    # 读取需要处理的关卡
    if FORCE_MODE:
        stages = conn.execute("""
            SELECT 关卡ID, 关卡编号, 关卡名, 关卡类型
            FROM stages
            WHERE 关卡编号 IS NOT NULL
              AND 关卡名 IS NOT NULL
            ORDER BY 关卡ID
        """).fetchall()
    else:
        stages = conn.execute("""
            SELECT 关卡ID, 关卡编号, 关卡名, 关卡类型
            FROM stages
            WHERE 关卡编号 IS NOT NULL
              AND 关卡名 IS NOT NULL
              AND (情报 IS NULL OR 情报 = '')
            ORDER BY 关卡ID
        """).fetchall()

    # 按 URL 分组
    url_groups = {}
    for sid, code, name, stype in stages:
        url_key = build_url_key(sid, code, name, stype)
        if url_key not in url_groups:
            url_groups[url_key] = []
        url_groups[url_key].append((sid, code.strip(), name.strip(), stype))

    total_urls = len(url_groups)

    # 统计
    errors = []        # (sid, code, etype, reason)
    no_match = []      # (sid, code, "无匹配模板")
    skipped = []       # (sid, "已有值")
    updated_count = 0
    updated_intel = 0
    no_count = 0
    no_intel_page = 0
    start_time = time.time()

    print(f"\n  共 {len(stages)} 个关卡, {total_urls} 个 unique URL")
    print()

    for i, (url_key, group) in enumerate(url_groups.items()):
        # 进度显示
        if i % 50 == 0 or i == total_urls - 1:
            elapsed = time.time() - start_time
            done = i + 1
            rate = done / elapsed if elapsed > 0 else 0
            pct = done / total_urls * 100
            bar_len = 30
            filled = int(bar_len * done / total_urls)
            bar = "▓" * filled + "░" * (bar_len - filled)
            eta = (total_urls - done) / rate if rate > 0 else 0
            print(f"  [{bar}] {done:>4}/{total_urls}  {pct:5.1f}%  "
                  f"ETA {eta:.0f}s  |  {url_key[:55]}")

        eff_code = group[0][1]

        # ── 获取 wikitext ──
        try:
            text = fetch_wikitext(url_key)
        except urllib.error.HTTPError as e:
            for sid, _, _, _ in group:
                errors.append((sid, eff_code, "HTTP", str(e.code)))
            continue
        except RuntimeError as e:
            for sid, _, _, _ in group:
                errors.append((sid, eff_code, "重定向", str(e)))
            continue
        except Exception as e:
            for sid, _, _, _ in group:
                errors.append((sid, eff_code, "网络", str(e)))
            continue

        # 消歧义页
        if '{{消歧义页}}' in text or '#消歧义页' in text:
            for sid, _, _, _ in group:
                errors.append((sid, eff_code, "页面", "消歧义页"))
            continue

        # ── 敌人数 ──
        count_val = extract_enemy_count(text)
        if count_val is not None:
            for sid, _, _, _ in group:
                conn.execute("UPDATE stages SET 待处理目标数量 = ? WHERE 关卡ID = ?",
                             (count_val, sid))
            updated_count += len(group)
        else:
            no_count += len(group)

        # ── 情报 ──
        intel_results = extract_all_intel(text)
        if intel_results:
            assignment = match_stage_to_template([s[0] for s in group], intel_results)
            for sid, _, _, _ in group:
                intel = assignment.get(sid)
                if intel is not None:
                    conn.execute("UPDATE stages SET 情报 = ? WHERE 关卡ID = ?", (intel, sid))
                    updated_intel += 1
                else:
                    no_match.append((sid, eff_code, f"无匹配模板({len(intel_results)}个模板)"))
        else:
            # 页面上没有任何可提取情报的模板
            for sid, _, _, _ in group:
                no_intel_page += 1

        time.sleep(0.1)

    conn.commit()

    # ── 日志 ──

    # 错误日志
    with open(ERROR_LOG, "w", encoding="utf-8") as f:
        f.write(f"# build_stages_prts_v2 错误日志\n")
        f.write(f"# 模式: {'全量' if FORCE_MODE else '增量'}\n")
        f.write(f"# 时间: {time.strftime('%Y-%m-%d %H:%M:%S')}\n\n")
        if errors:
            err_types = {}
            for _, _, etype, _ in errors:
                err_types[etype] = err_types.get(etype, 0) + 1
            f.write(f"=== 请求错误 ({len(errors)} 个) ===\n")
            for et, cnt in sorted(err_types.items()):
                f.write(f"  {et}: {cnt}\n")
            f.write(f"\n=== 详细 ===\n")
            f.write(f"# 关卡ID\t关卡编号\t错误类型\t原因\n")
            for sid, code, etype, reason in errors:
                f.write(f"{sid}\t{code}\t{etype}\t{reason}\n")

    # 跳过/缺失日志
    with open(SKIP_LOG, "w", encoding="utf-8") as f:
        f.write(f"# build_stages_prts_v2 跳过/未匹配日志\n")
        f.write(f"# 模式: {'全量' if FORCE_MODE else '增量'}\n")
        f.write(f"# 时间: {time.strftime('%Y-%m-%d %H:%M:%S')}\n\n")
        if no_match:
            f.write(f"=== 无匹配模板 ({len(no_match)} 个) ===\n")
            f.write(f"# 关卡ID\t关卡编号\t原因\n")
            for sid, code, reason in no_match:
                f.write(f"{sid}\t{code}\t{reason}\n")

    # ── 汇总 ──
    elapsed = time.time() - start_time
    print(f"\n  {'─' * 55}")
    print(f"  敌人数   更新: {updated_count} 个关卡")
    print(f"  情报     更新: {updated_intel} 个关卡")
    print(f"  无敌人数 字段: {no_count}")
    print(f"  页面无情报模板: {no_intel_page}")
    print(f"  无匹配模板:     {len(no_match)}")
    print(f"  请求错误:       {len(errors)}")
    print(f"  耗时:           {elapsed:.0f}s ({elapsed/60:.1f}min)")
    print(f"  错误日志:       {ERROR_LOG}")
    print(f"  跳过日志:       {SKIP_LOG}")

    # ── 按类型统计 ──
    stats = conn.execute("""
        SELECT 关卡类型,
            COUNT(*) AS 总数,
            SUM(CASE WHEN 待处理目标数量 IS NOT NULL THEN 1 ELSE 0 END) AS 有敌人数,
            SUM(CASE WHEN 情报 IS NOT NULL AND 情报 != '' THEN 1 ELSE 0 END) AS 有情报
        FROM stages
        WHERE 关卡编号 IS NOT NULL
        GROUP BY 关卡类型
        ORDER BY 总数 DESC
    """).fetchall()

    print(f"\n  {'类型':<25s} {'总数':>5s} {'有敌人数':>7s} {'有情报':>6s}")
    print(f"  {'─'*48}")
    for r in stats:
        print(f"  {r[0]:<23s}  {r[1]:5d}  {r[2]:7d}  {r[3]:6d}")

    return errors, no_match


# ═══════════════════════════════════════════
# 主流程
# ═══════════════════════════════════════════

def main():
    mode_str = "全量重爬" if FORCE_MODE else "增量"
    print("=" * 70)
    print(f"  PRTS 关卡情报爬取 v2（{mode_str}）")
    print("=" * 70)

    conn = sqlite3.connect(OUTPUT_DB)
    conn.execute("PRAGMA journal_mode=OFF")
    conn.execute("PRAGMA synchronous=OFF")

    unresolved = phase1_enemy_names(conn)
    errors, no_match = phase2_prts_crawl(conn)

    print("\n" + "=" * 70)
    print("  完成")
    print("=" * 70)
    print(f"  未解析敌人 ID: {len(unresolved)}")
    print(f"  请求错误:       {len(errors)}")
    print(f"  无匹配模板:     {len(no_match)}")

    conn.close()


if __name__ == '__main__':
    main()
