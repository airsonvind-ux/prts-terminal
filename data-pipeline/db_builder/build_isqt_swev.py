"""
PRTS 终端 — IS-QT + SW-EV 战斗数据补全

处理两类数据源缺失的活动关卡：
  IS-QT（叙拉古人记忆映射+特殊战斗）：消歧义页面使用括号后缀
  SW-EV（战地秘闻）：标准 URL

从 PRTS wiki 提取：部署上限、初始费用、费用上限、目标点耐久、敌人列表

用法：python build_isqt_swev.py
"""

import re, json, sqlite3, os, time, urllib.request, urllib.error, socket, sys

socket.setdefaulttimeout(15)

OUTPUT_DIR = os.path.dirname(os.path.abspath(__file__))
OUTPUT_DB  = os.path.join(OUTPUT_DIR, "arknights.db")
ERROR_LOG  = os.path.join(OUTPUT_DIR, "isqt_swev_errors.log")

# 复用已有函数
sys.path.insert(0, OUTPUT_DIR)
from build_sandbox2_prts import parse_template, parse_enemy_intel

# ═══════════════════════════════════════════
# IS-QT 消歧义 → PRTS 页面名
# ═══════════════════════════════════════════

ISQT_DISAMBIG = {
    "act21side_01_t":  "IS-QT 新城区大街(德克萨斯)",
    "act21side_06_t":  "IS-QT 新城区大街(丹布朗)",
    "act21side_02_t":  "IS-QT 萨卢佐家(拉普兰德2)",
    "act21side_04_m1": "IS-QT 萨卢佐家(拉普兰德1)",
    "act21side_03_m2": "IS-QT 后巷(拉普兰德1)",
    "act21side_05_m1": "IS-QT 后巷(乔万娜)",
    "act21side_05_t":  "IS-QT 后巷(拉普兰德2)",
}


# ═══════════════════════════════════════════
# wikitext 获取
# ═══════════════════════════════════════════

def fetch_wikitext(name):
    url = f"https://prts.wiki/w/{urllib.parse.quote(name)}?action=raw"
    req = urllib.request.Request(url, headers={'User-Agent': 'PRTS-Terminal/1.0'})
    with urllib.request.urlopen(req, timeout=10) as resp:
        return resp.read().decode("utf-8")


# ═══════════════════════════════════════════
# 主流程
# ═══════════════════════════════════════════

def main():
    conn = sqlite3.connect(OUTPUT_DB)
    conn.execute("PRAGMA journal_mode=OFF")
    conn.execute("PRAGMA synchronous=OFF")

    # 待处理的 IS-QT 关卡（含 _t，部分可能有部分数据，但全部重写）
    isqt_stages = conn.execute("""
        SELECT 关卡ID, 关卡编号, 关卡名
        FROM stages
        WHERE 关卡类型 = '活动'
          AND 关卡编号 = 'IS-QT'
        ORDER BY 关卡ID
    """).fetchall()

    # SW-EV 关卡
    swev_stages = conn.execute("""
        SELECT 关卡ID, 关卡编号, 关卡名
        FROM stages
        WHERE 关卡类型 = '活动'
          AND 关卡编号 LIKE 'SW-EV-%'
        ORDER BY 关卡ID
    """).fetchall()

    all_stages = list(isqt_stages) + list(swev_stages)

    print("=" * 60)
    print("  IS-QT + SW-EV 战斗数据补全")
    print("=" * 60)
    print(f"  IS-QT: {len(isqt_stages)} 关,  SW-EV: {len(swev_stages)} 关")
    print(f"  合计: {len(all_stages)} 关")
    print()

    updated = 0
    errors = []
    start_time = time.time()

    for i, (sid, code, name) in enumerate(all_stages):
        # 构造 URL
        if sid in ISQT_DISAMBIG:
            page_name = ISQT_DISAMBIG[sid]
        else:
            page_name = f"{code}_{name}"

        # 进度
        if i % 5 == 0 or i == len(all_stages) - 1:
            elapsed = time.time() - start_time
            pct = (i + 1) / len(all_stages) * 100
            bar_len = 24
            filled = int(bar_len * (i + 1) / len(all_stages))
            bar = "▓" * filled + "░" * (bar_len - filled)
            eta = (len(all_stages) - i - 1) * (elapsed / (i + 1)) if i > 0 else 0
            print(f"  [{bar}] {i+1:>3}/{len(all_stages)}  {pct:5.1f}%  ETA {eta:.0f}s  |  {page_name[:40]}")

        # 获取 wikitext
        try:
            text = fetch_wikitext(page_name)
        except urllib.error.HTTPError as e:
            errors.append((sid, code, name, f"HTTP {e.code}"))
            continue
        except Exception as e:
            errors.append((sid, code, name, f"网络: {e}"))
            continue

        # 消歧义页
        if '{{消歧义页}}' in text[:200]:
            errors.append((sid, code, name, "消歧义页（URL 未匹配）"))
            continue

        # 提取战斗参数
        tpls = parse_template(text, "普通关卡信息")
        if not tpls:
            errors.append((sid, code, name, "无普通关卡信息模板"))
            continue

        params = tpls[0]

        deploy  = params.get("部署上限")
        init    = params.get("初始COST")
        maxcost = params.get("COST上限")
        maxlife = params.get("目标点耐久")

        # 提取敌人列表
        enemy_json = parse_enemy_intel(text)

        # 写入
        updates = []
        vals = []
        if deploy:
            updates.append("部署上限 = ?")
            vals.append(int(deploy))
        if init:
            updates.append("初始费用 = ?")
            vals.append(int(init))
        if maxcost:
            updates.append("费用上限 = ?")
            vals.append(int(maxcost))
        if maxlife:
            updates.append("目标点耐久 = ?")
            vals.append(int(maxlife))
        if enemy_json and enemy_json != "[]":
            updates.append("敌人列表 = ?")
            vals.append(enemy_json)

        if updates:
            vals.append(sid)
            conn.execute(
                f"UPDATE stages SET {', '.join(updates)} WHERE 关卡ID = ?",
                vals
            )
            updated += 1
        else:
            errors.append((sid, code, name, "无任何可写入数据"))

        time.sleep(0.1)

    conn.commit()

    # ─── 错误日志 ───
    with open(ERROR_LOG, "w", encoding="utf-8") as f:
        f.write(f"# IS-QT / SW-EV 战斗数据补全 错误日志\n")
        f.write(f"# 更新时间: {time.strftime('%Y-%m-%d %H:%M:%S')}\n\n")
        if errors:
            f.write(f"=== 错误 ({len(errors)} 个) ===\n")
            f.write(f"# 关卡ID\t关卡编号\t关卡名\t原因\n")
            for sid, code, name, reason in errors:
                f.write(f"{sid}\t{code}\t{name}\t{reason}\n")

    # ─── 汇总 ───
    elapsed = time.time() - start_time
    print(f"\n  {'─' * 40}")
    print(f"  更新: {updated}/{len(all_stages)} 关")
    print(f"  错误: {len(errors)}")
    print(f"  耗时: {elapsed:.0f}s")
    print(f"  日志: {ERROR_LOG}")

    # 验证
    for sid, _, _, _ in all_stages:
        r = conn.execute(
            "SELECT 部署上限, 初始费用, 费用上限, 目标点耐久, 敌人列表 FROM stages WHERE 关卡ID = ?",
            (sid,)
        ).fetchone()
        if r:
            has = sum(1 for v in r if v is not None and v != '' and v != '[]')
            if has == 0:
                print(f"  ⚠ 仍然全空: {sid}")

    conn.close()


if __name__ == '__main__':
    main()
