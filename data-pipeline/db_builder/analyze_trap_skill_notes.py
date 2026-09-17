"""
PRTS 终端 — traps 技能备注分析
分析所有装置技能在 PRTS 上的备注字段存在情况

用法：python analyze_trap_skill_notes.py
输出：控制台统计 + analyze_trap_skill_notes.log（详细结果）
"""

import re, json, sqlite3, os, time, urllib.request, urllib.parse

OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB  = os.path.join(OUTPUT_DIR, "arknights.db")
RESULT_LOG = os.path.join(OUTPUT_DIR, "analyze_trap_skill_notes.log")


def fetch_wikitext(name):
    """获取 PRTS 页面原始 wikitext，跟随重定向"""
    encoded = urllib.parse.quote(name)
    url = f"https://prts.wiki/w/{encoded}?action=raw"
    req = urllib.request.Request(url, headers={'User-Agent': 'PRTS-Terminal/1.0'})
    with urllib.request.urlopen(req) as resp:
        text = resp.read().decode("utf-8")
    m = re.match(r'#(?:REDIRECT|重定向)\s*\[\[([^\]]+)\]\]', text)
    if m:
        return fetch_wikitext(m.group(1))
    return text


def parse_template(text, tpl_name):
    """
    解析所有 {{模板名|...}} 实例，处理嵌套模板。
    返回 [{param_name: value}, ...]
    """
    results = []
    start_tag = '{{' + tpl_name
    pos = 0

    while True:
        idx = text.find(start_tag, pos)
        if idx == -1:
            break

        depth = 0
        i = idx
        while i < len(text):
            if text[i:i+2] == '{{':
                depth += 1
                i += 2
                continue
            if text[i:i+2] == '}}':
                depth -= 1
                if depth == 0:
                    # 提取模板内容（从模板名后冒号到闭合的 }}）
                    content = text[idx + len(start_tag):i]
                    params = {}
                    # 按 | 分割，首个 | 前的部分通常是匿名参数，跳过
                    # PRTS 模板中管道符 '|' 用于分隔参数
                    parts = content.split('\n|')
                    for part in parts:
                        part = part.strip()
                        # 处理管道符开头（第一个参数或以 | 开头的）
                        while part.startswith('|'):
                            part = part[1:].strip()
                        if '=' not in part:
                            continue
                        k, v = part.split('=', 1)
                        params[k.strip()] = v.strip()
                    results.append(params)
                    pos = i + 2
                    break
                i += 2
                continue
            i += 1
        else:
            # 未找到闭合的 }}，跳过这个匹配
            pos = idx + 2

    return results


def main():
    conn = sqlite3.connect(OUTPUT_DB)

    # 获取所有有技能的装置
    rows = conn.execute(
        "SELECT 装置ID, 名称, 技能列表 FROM traps WHERE 技能列表 IS NOT NULL ORDER BY 名称"
    ).fetchall()

    total = len(rows)
    traps_with_notes = []       # [(名称, 装置ID, [技能名列表])]
    skills_with_notes_count = 0
    errors = []                 # [(名称, 原因)]
    no_page = []                # [名称]
    no_skill_template = []      # [名称]

    start_time = time.time()

    print(f"====== 开始分析 ({total} 个有技能的装置) ======")

    for i, (tid, name, skill_json) in enumerate(rows):
        if i % 50 == 0:
            elapsed = time.time() - start_time
            rate = i / elapsed if elapsed > 0 else 0
            eta = (total - i) / rate if rate > 0 else 0
            notes_found = len(traps_with_notes)
            print(f"  [{i}/{total}] {elapsed:.0f}s | ETA {eta:.0f}s | {name} | 已发现备注: {notes_found}")

        try:
            wikitext = fetch_wikitext(name)
        except urllib.error.HTTPError as e:
            if e.code == 404:
                no_page.append(name)
            else:
                errors.append((name, f"HTTP {e.code}"))
            continue
        except Exception as e:
            errors.append((name, f"网络: {e}"))
            continue

        # 解析 {{装置技能}} 模板（注意：不是 {{技能2}} 也不是 {{技能\n}}）
        skill_tpls = parse_template(wikitext, '装置技能')

        if not skill_tpls:
            no_skill_template.append(name)
            # 继续，不跳过——有些页面可能用其他模板名
            # 也尝试 {{技能2}} 作为降级
            skill_tpls = parse_template(wikitext, '技能2')

        # 检查备注
        skills_with_notes = []
        for tpl in skill_tpls:
            note = tpl.get('备注', '')
            sname = tpl.get('技能名', '(未命名)')
            if note:
                skills_with_notes.append(sname)
                skills_with_notes_count += 1

        if skills_with_notes:
            traps_with_notes.append((name, tid, skills_with_notes))

        time.sleep(0.1)

    elapsed = time.time() - start_time
    conn.close()

    # ═══════════════════════════════════════════
    # 输出结果
    # ═══════════════════════════════════════════

    print(f"\n====== 完成 ======")
    print(f"  总装置数（有技能）: {total}")
    print(f"  耗时: {elapsed:.0f}s")

    print(f"\n--- 技能备注分析 ---")
    print(f"  含备注的装置数: {len(traps_with_notes)}/{total}")
    print(f"  含备注的技能数: {skills_with_notes_count}")
    print(f"  无 {{装置技能}} 模板: {len(no_skill_template)}")
    print(f"  404（页面不存在）: {len(no_page)}")
    print(f"  其他错误: {len(errors)}")

    if traps_with_notes:
        print(f"\n--- 含技能备注的装置 ---")
        for name, tid, s_names in traps_with_notes:
            print(f"  {name} ({tid})")
            for sn in s_names:
                print(f"    [{sn}]")

    # ═══════════════════════════════════════════
    # 写入日志
    # ═══════════════════════════════════════════

    with open(RESULT_LOG, 'w', encoding='utf-8') as f:
        f.write(f"# traps 技能备注分析日志\n")
        f.write(f"# 时间: {time.strftime('%Y-%m-%d %H:%M:%S')}\n")
        f.write(f"# 总装置数: {total}\n\n")

        if traps_with_notes:
            f.write(f"=== 含技能备注的装置 ({len(traps_with_notes)} 个) ===\n")
            for name, tid, s_names in traps_with_notes:
                f.write(f"\n## {name} ({tid})\n")
                for sn in s_names:
                    f.write(f"  [{sn}]\n")
        else:
            f.write("(无)\n")

        f.write(f"\n\n=== 无 {{装置技能}} 模板 ({len(no_skill_template)} 个) ===\n")
        if no_skill_template:
            for n in no_skill_template:
                f.write(f"  {n}\n")
        else:
            f.write("  (无)\n")

        f.write(f"\n=== 404 ({len(no_page)} 个) ===\n")
        if no_page:
            for n in no_page:
                f.write(f"  {n}\n")
        else:
            f.write("  (无)\n")

        f.write(f"\n=== 其他错误 ({len(errors)} 个) ===\n")
        if errors:
            for n, reason in errors:
                f.write(f"  {n}\t{reason}\n")
        else:
            f.write("  (无)\n")

    print(f"\n详细日志: {RESULT_LOG}")


if __name__ == '__main__':
    main()
