# -*- coding: utf-8 -*-
"""abnormal_effects · 重建（异常效果表）

数据源（PRTS 两页）
    ① 异常效果页「异常效果（AbnormalFlag）」表格 → 条目名 + 效果（表格「效果」列原文）
    ② 同页「异常效果的统一检查」小节 → 分类（本表用 6 类）
    ③ 可抵抗状态页「已知的可抵抗状态」的「常见同名状态」→ 可抵抗

输出
    src-tauri/resources/abnormal_effects.json · {名: {效果, 分类, 可抵抗}}

口径
    表格行取注释清除后的 {{异常效果表格|…}}，跳过带「组合=」的异常组合表与 × 占位。
    分类只取 6 类：无法选择 / 阻止攻击 / 阻止能力 / 阻止移动 / 对友方隐藏 / 对非友方隐藏
    （页面另有「无法被友方选择」「无法被治疗选择」两节，页面注明站内通常直接标注异常效果名）。
    可抵抗只取「常见同名状态」那一小节，该小节之外列的是具体 Buff，不落到异常效果上。
    与页面对齐，不做保留合并。

本表专有模板规则
    EXTRA_RULES 暂时为空——页面表格的「效果」列基础层已够用。
    将来报残留时在这里加 (名称, 编译正则, 替换函数)，与 term 同形。

用法
    python rebuild.py --dry-run
    python rebuild.py
"""

import json
import re
import sys
from collections import OrderedDict
from pathlib import Path

HERE = Path(__file__).resolve().parent
MAINT = next(p for p in HERE.parents if (p / "lib").is_dir())   # maintenance/
for _p in (str(HERE), str(MAINT / "lib"), str(MAINT.parent)):   # 本表目录 / 通用模块 / data-pipeline
    if _p not in sys.path:
        sys.path.insert(0, _p)

import paths  # noqa: E402
import table_base as tb  # noqa: E402
import update_common as pw  # noqa: E402

PAGE_AB = "异常效果"
PAGE_RESIST = "可抵抗状态"
OUT = paths.RESOURCES / "abnormal_effects.json"

CATEGORIES = ["无法选择", "阻止攻击", "阻止能力", "阻止移动", "对友方隐藏", "对非友方隐藏"]

EXTRA_RULES = []


def clean_value(raw):
    return pw.clean_value(raw, EXTRA_RULES)


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
    chunk = ""
    for level, sec, body in pw.sections(pw.fetch_page(PAGE_RESIST)):
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


def build(old=None):
    flags = parse_flags()
    cats = parse_categories(list(flags))
    resist = parse_resistable(list(flags))
    out = OrderedDict()
    for name in flags:
        out[name] = {
            "效果": clean_value(flags[name]),
            "分类": cats.get(name, []),
            "可抵抗": name in resist,
        }
    yes = [k for k, v in out.items() if v["可抵抗"]]
    print("  页面条目 %d | 可抵抗 %d %s" % (len(out), len(yes), yes))
    return out


SPEC = tb.RebuildSpec(name="abnormal_effects", output=OUT, build=build)

if __name__ == "__main__":
    sys.exit(tb.run_rebuild(SPEC))
