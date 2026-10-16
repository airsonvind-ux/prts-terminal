# -*- coding: utf-8 -*-
"""PRTS 终端 — 异常效果表重建（维护用）

**重建制**：每次从 PRTS 页面全量重建 src-tauri/resources/abnormal_effects.json，
与页面对齐，不做保留合并。

数据源：
  ① 异常效果页「异常效果（AbnormalFlag）」表格 → 条目名 + `效果`（表格「效果」列原文）
  ② 同页「异常效果的统一检查」小节 → `分类`（本表用 6 类：无法选择 / 阻止攻击 /
     阻止能力 / 阻止移动 / 对友方隐藏 / 对非友方隐藏；页面另有「无法被友方选择」
     「无法被治疗选择」两节，页面注明这两类站内通常直接标注异常效果名，故不作为分类）
  ③ 可抵抗状态页「已知的可抵抗状态」的「常见同名状态」→ `可抵抗`
     （晕眩 / 寒冷 / 冻结 / 恐惧 / 诱导 / 麻痹；该小节之外列的是具体 Buff，不落到异常效果上）

口径：
  - 表格行取注释清除后的 `{{异常效果表格|…}}`，跳过带 `组合=` 的异常组合表与 × 占位；
  - 文本：`<br>` 作段落分隔、※ 与 `<code>` 标记去掉，其余交给 cleaning_wiki。

用法：
    python rebuild_abnormal_effects.py --dry-run
    python rebuild_abnormal_effects.py
"""

import argparse
import json
import os
import re
import sys
from collections import OrderedDict
from datetime import datetime

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)
from paths import RESOURCES  # noqa: E402

import prts_wiki as pw  # noqa: E402
import update_common as uc  # noqa: E402

PAGE_AB = "异常效果"
PAGE_RESIST = "可抵抗状态"
DST = os.path.join(str(RESOURCES), "abnormal_effects.json")

CATEGORIES = ["无法选择", "阻止攻击", "阻止能力", "阻止移动", "对友方隐藏", "对非友方隐藏"]


def parse_flags():
    """异常效果表格 → OrderedDict{名称: 效果原文}（保持页面顺序）。"""
    text = pw.strip_comments(pw.fetch_page(PAGE_AB))
    out = OrderedDict()
    for frag in pw.templates(text, "异常效果表格"):
        ps = pw.split_params(frag)
        if len(ps) < 5:
            continue
        if any(p.strip().startswith("组合=") for p in ps):      # 异常组合表，跳过
            continue
        name = pw.plain(ps[2])
        if not name or name == "×":
            continue
        out[name] = ps[4]
    return out


def parse_categories(names):
    """统一检查各小节的成员 → {名称: [分类]}。"""
    text = pw.strip_comments(pw.fetch_page(PAGE_AB))
    result = {n: [] for n in names}
    for level, sec, body in pw.sections(text):
        if sec not in CATEGORIES:
            continue
        for cell in pw.block_cells(body):
            for line in cell.split("\n"):
                cand = pw.plain(line).strip()
                if cand in names and sec not in result[cand]:
                    result[cand].append(sec)
    return {n: [c for c in CATEGORIES if c in v] for n, v in result.items()}


def parse_resistable(names):
    """可抵抗状态页「常见同名状态」列出的可抵抗状态。"""
    text = pw.fetch_page(PAGE_RESIST)
    chunk = ""
    for level, sec, body in pw.sections(text):
        if sec == "已知的可抵抗状态":
            chunk = body
            break
    if not chunk:
        raise RuntimeError("可抵抗状态页未找到「已知的可抵抗状态」小节")
    out = set()
    for line in chunk.splitlines():
        s = line.strip()
        if not s.startswith("::"):
            continue
        m = re.search(r"''(.*?)''", s, re.S)
        if not m:
            continue
        name = pw.plain(m.group(1)).split()[0].split("→")[0].strip()
        if name in names:
            out.add(name)
    return out


def build():
    flags = parse_flags()
    cats = parse_categories(list(flags))
    resist = parse_resistable(list(flags))
    out = OrderedDict()
    for name in flags:
        out[name] = {
            "效果": pw.clean_value(flags[name]),
            "分类": cats.get(name, []),
            "可抵抗": name in resist,
        }
    return out


def main():
    ap = argparse.ArgumentParser(description="重建异常效果表 abnormal_effects.json")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写文件")
    args = ap.parse_args()

    new = build()
    old = {}
    if os.path.exists(DST):
        with open(DST, encoding="utf-8") as f:
            old = json.load(f)

    added = [k for k in new if k not in old]
    removed = [k for k in old if k not in new]
    changed = [(k, old[k], new[k]) for k in new if k in old and old[k] != new[k]]

    print("====== 异常效果表重建 ======")
    print("页面条目   : %d" % len(new))
    print("原文件     : %d" % len(old))
    print("新增       : %d %s" % (len(added), added[:10]))
    print("页面已无（将删除）: %d %s" % (len(removed), removed[:10]))
    print("变化       : %d" % len(changed))
    for k, o, n in changed[:8]:
        for field in ("效果", "分类", "可抵抗"):
            if o.get(field) != n.get(field):
                print("  [%s].%s\n    旧 %s\n    新 %s" % (
                    k, field, json.dumps(o.get(field), ensure_ascii=False)[:90],
                    json.dumps(n.get(field), ensure_ascii=False)[:90]))
    if len(changed) > 8:
        print("  …（其余 %d 项%s）" % (len(changed) - 8,
                                    "见记录文件" if not args.dry_run else "干跑略过"))
    yes = [k for k, v in new.items() if v["可抵抗"]]
    print("可抵抗标记为 true : %d %s" % (len(yes), yes))

    if args.dry_run:
        print("\n(干跑：未写文件)")
        return 0

    uc.backup_file(DST)                      # 写前备份（保留 3 份）
    with open(DST, "w", encoding="utf-8") as f:
        json.dump(new, f, ensure_ascii=False, indent=2)

    record = {
        "脚本": {"文件": "maintenance/rebuild_abnormal_effects.py", "版本": 1},
        "运行时间": datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
        "模式": "重建",
        "数据源": {"页面": [PAGE_AB, PAGE_RESIST],
                   "段": ["异常效果（AbnormalFlag）", "异常效果的统一检查", "已知的可抵抗状态"]},
        "表": "abnormal_effects.json",
        "计数": {"页面": len(new), "原文件": len(old)},
        "新增": added, "新增数": len(added),
        "页面已无": removed, "页面已无数": len(removed),
        "变化": [{"键": k, "旧": o, "新": n} for k, o, n in changed],
        "变化数": len(changed),
        "结果": "成功",
    }
    print("\n====== 完成：%s ======" % DST)
    print("记录：%s" % os.path.basename(uc.write_record("abnormal_effects", record)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
