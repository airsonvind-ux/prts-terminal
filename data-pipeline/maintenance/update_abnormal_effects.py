# -*- coding: utf-8 -*-
"""PRTS 终端 — 异常效果表更新（维护用）

数据源：PRTS「异常效果」页（名称 / 效果 / 分类）+ PRTS「可抵抗状态」页（可抵抗）
输出：src-tauri/resources/abnormal_effects.json

用法：
    python update_abnormal_effects.py --dry-run
    python update_abnormal_effects.py

口径（均有页面实证）：
  - 条目 = 异常效果页「异常效果（AbnormalFlag）」表格里的行（去注释、去 × 占位）；
  - `效果` = 该表格「效果」列的原文（页面全文，`<br>` 作段落分隔）；
  - `分类` = 「异常效果的统一检查」小节的成员关系，本库用其中 6 类：
    无法选择 / 阻止攻击 / 阻止能力 / 阻止移动 / 对友方隐藏 / 对非友方隐藏
    （页面另有「无法被友方选择」「无法被治疗选择」两节，页面注明这两类站内通常
     直接标注异常效果名，故不作为分类。）
  - `可抵抗` = 「可抵抗状态」页「已知的可抵抗状态」中「常见同名状态」一列
    （晕眩 / 寒冷 / 冻结 / 恐惧 / 诱导 / 麻痹）；其余条目为该页列出的
    具体 Buff，不落到异常效果上。
"""

import argparse
import json
import os
import shutil
import sys
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
from paths import RESOURCES  # noqa: E402

import prts_wiki as pw  # noqa: E402

PAGE_AB = "异常效果"
PAGE_RESIST = "可抵抗状态"
DST = os.path.join(str(RESOURCES), "abnormal_effects.json")
UPDATES = os.path.join(HERE, "updates")
BACKUPS = os.path.join(HERE, "backups")

CATEGORIES = ["无法选择", "阻止攻击", "阻止能力", "阻止移动", "对友方隐藏", "对非友方隐藏"]


def parse_abnormal_flags():
    """异常效果表格 → {名称: 效果原文}（保持页面顺序）。"""
    text = pw.strip_comments(pw.fetch_page(PAGE_AB))
    out = {}
    for frag in pw.templates(text, "异常效果表格"):
        ps = pw.split_params(frag)
        if len(ps) < 5:
            continue
        if any(p.strip().startswith("组合=") for p in ps):   # 异常组合表，跳过
            continue
        name = pw.plain(ps[2])
        if not name or name == "×":
            continue
        out[name] = ps[4]
    return out


def parse_categories(names):
    """统一检查各小节的成员 → {名称: [分类]}。只保留在 6 类名单里的小节。"""
    text = pw.strip_comments(pw.fetch_page(PAGE_AB))
    result = {n: [] for n in names}
    for level, sec, body in pw.sections(text):
        if sec not in CATEGORIES:
            continue
        for cell in pw.block_cells(body):
            for line in cell.split("\n"):
                cand = pw.plain(line).strip()
                if cand in names:
                    if sec not in result[cand]:
                        result[cand].append(sec)
    for sec in CATEGORIES:      # 分类列按固定顺序
        pass
    return {n: [c for c in CATEGORIES if c in v] for n, v in result.items()}


def parse_resistable(names):
    """「常见同名状态」列出的可抵抗状态 → 名称集合。"""
    text = pw.fetch_page(PAGE_RESIST)
    out = set()
    hit = False
    for level, sec, body in pw.sections(text):
        if sec == "已知的可抵抗状态":
            hit = True
            chunk = body
            break
    if not hit:
        raise RuntimeError("可抵抗状态页未找到「已知的可抵抗状态」小节")
    # 取「常见同名状态」这一条下的 :: 项
    for line in chunk.splitlines():
        s = line.strip()
        if not s.startswith("::"):
            continue
        item = s[2:].strip()
        m = re_search(r"''(.*?)''", item)
        if m:
            name = pw.plain(m).split()[0].split("→")[0].strip()
            if name in names:
                out.add(name)
    return out


def re_search(pattern, text):
    import re
    m = re.search(pattern, text, re.S)
    return m.group(1) if m else ""


def build():
    flags = parse_abnormal_flags()
    names = list(flags)
    cats = parse_categories(names)
    resist = parse_resistable(names)
    out = {}
    for n in names:
        out[n] = {
            "效果": pw.clean_value(flags[n]),
            "分类": cats.get(n, []),
            "可抵抗": n in resist,
        }
    return out


def main():
    ap = argparse.ArgumentParser(description="更新异常效果表 abnormal_effects.json")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写文件")
    args = ap.parse_args()

    new = build()
    old = {}
    if os.path.exists(DST):
        with open(DST, encoding="utf-8") as f:
            old = json.load(f)

    changed = []
    for k, v in new.items():
        if k not in old:
            changed.append((k, None, v))
        elif old[k] != v:
            changed.append((k, old[k], v))
    removed = [k for k in old if k not in new]

    print("====== 异常效果表更新 ======")
    print("页面条目 : %d" % len(new))
    print("库内     : %d" % len(old))
    print("变化     : %d" % len(changed))
    for k, o, n in changed[:8]:
        if o is None:
            print("  [%s] 新增：%s" % (k, json.dumps(n, ensure_ascii=False)[:110]))
        else:
            for field in ("效果", "分类", "可抵抗"):
                if o.get(field) != n.get(field):
                    print("  [%s].%s\n    旧 %s\n    新 %s" % (
                        k, field, json.dumps(o.get(field), ensure_ascii=False)[:90],
                        json.dumps(n.get(field), ensure_ascii=False)[:90]))
    if len(changed) > 8:
        print("  …（其余 %d 项见记录文件）" % (len(changed) - 8))
    print("页面无、库内有（将保留）: %s" % removed)
    yes = [k for k, v in new.items() if v["可抵抗"]]
    print("可抵抗标记为 true : %d %s" % (len(yes), yes))

    if args.dry_run:
        print("\n(干跑：未写文件)")
        return 0

    out = dict(new)
    for k in removed:                    # 页面没有的，原样保留
        out[k] = old[k]

    os.makedirs(BACKUPS, exist_ok=True)
    os.makedirs(UPDATES, exist_ok=True)
    ts = datetime.now().strftime("%Y-%m-%d-%H%M%S")
    if os.path.exists(DST):
        shutil.copy2(DST, os.path.join(BACKUPS, "abnormal_effects.json.%s.bak"
                                       % ts.replace("-", "")[0:8]))
    with open(DST, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, indent=2)

    record = {
        "脚本": {"文件": "maintenance/update_abnormal_effects.py", "版本": 1},
        "运行时间": datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
        "数据源": {"页面": [PAGE_AB, PAGE_RESIST], "段": ["异常效果（AbnormalFlag）",
                                                        "异常效果的统一检查",
                                                        "已知的可抵抗状态"]},
        "表": "abnormal_effects.json",
        "计数": {"页面": len(new), "库内": len(old), "输出": len(out)},
        "变化": [{"键": k, "旧": o, "新": n} for k, o, n in changed],
        "变化数": len(changed),
        "页面无库内有": removed,
        "结果": "成功",
        "写入": {"更新": len([c for c in changed if c[1] is not None]),
                 "新增": len([c for c in changed if c[1] is None])},
    }
    path = os.path.join(UPDATES, "%s-abnormal_effects.json" % ts)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(record, f, ensure_ascii=False, indent=2)
    print("\n====== 完成：%s ======" % DST)
    print("记录：%s" % os.path.basename(path))
    return 0


if __name__ == "__main__":
    sys.exit(main())
