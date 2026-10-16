# -*- coding: utf-8 -*-
"""PRTS 终端 — 权威库推送到发布资源目录（维护用）

更新链路：数据更新脚本只动 `maintenance/arknights.db`（唯一权威），
本脚本把它推到 `src-tauri/resources/arknights.db`（随包发布、运行时读取的种子）。

用法：
    python push_to_resources.py --dry-run   # 只报告：两边行数、大小差异
    python push_to_resources.py             # 推送（写前备份目标 + 写记录）

说明：
  - resources 下的五张 JSON 表由各自的 rebuild/update 脚本直接写入，不走这里。
  - 目标文件在 .gitignore 中；备份与记录仍按更新系统的统一约定落盘。
"""

import argparse
import json
import os
import shutil
import sqlite3
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
sys.path.insert(0, HERE)
from paths import DB_PATH, RESOURCES  # noqa: E402

import update_common as uc  # noqa: E402

SRC = str(DB_PATH)
DST = os.path.join(str(RESOURCES), "arknights.db")


def table_counts(path):
    """{表名: 行数}；库不存在或读不开返回 None。"""
    if not os.path.exists(path):
        return None
    try:
        con = sqlite3.connect(path)
        names = [r[0] for r in con.execute(
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")]
        out = {}
        for t in names:
            out[t] = con.execute("SELECT COUNT(*) FROM %s" % t).fetchone()[0]
        con.close()
        return out
    except sqlite3.Error as e:
        return {"（读库失败）": str(e)}


def size_mb(path):
    return round(os.path.getsize(path) / 1024 / 1024, 2) if os.path.exists(path) else None


def main():
    ap = argparse.ArgumentParser(description="把权威库推送到 src-tauri/resources/")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写文件")
    args = ap.parse_args()

    if not os.path.exists(SRC):
        print("权威库不存在：%s" % SRC)
        return 1

    src_counts, dst_counts = table_counts(SRC), table_counts(DST)

    print("====== 推送到 resources ======")
    print("源   %s  %.2f MB" % (SRC, size_mb(SRC)))
    print("目标 %s  %s" % (DST, "%.2f MB" % size_mb(DST) if dst_counts else "（不存在）"))

    if dst_counts is None:
        print("\n目标不存在，将新建。")
    else:
        print("\n%-24s %8s %8s %s" % ("表", "源", "目标", "差异"))
        for t in sorted(set(src_counts) | set(dst_counts)):
            a, b = src_counts.get(t), dst_counts.get(t)
            mark = "" if a == b else "  ←"
            print("%-24s %8s %8s%s" % (t, a, b, mark))
        diff = [t for t in set(src_counts) | set(dst_counts)
                if src_counts.get(t) != dst_counts.get(t)]
        print("\n行数不一致的表：%d %s" % (len(diff), sorted(diff)[:6]))

    if args.dry_run:
        print("\n(干跑：未写文件)")
        return 0

    uc.backup_file(DST)                      # 写前备份目标（按文件名各留 3 份）
    tmp = DST + ".tmp"
    try:
        with open(SRC, "rb") as fsrc, open(tmp, "wb") as fdst:
            shutil.copyfileobj(fsrc, fdst, 1024 * 1024)
        os.replace(tmp, DST)                 # 原子替换，避免拷到一半被读到
    except Exception:
        if os.path.exists(tmp):
            os.remove(tmp)
        raise RuntimeError("推送失败（目标可能被占用，请先关掉 PRTS 终端）：%s" % DST)

    record = {
        "脚本": {"文件": "maintenance/push_to_resources.py", "版本": 1},
        "运行时间": time.strftime("%Y-%m-%d %H:%M:%S"),
        "模式": "推送",
        "源": SRC,
        "目标": DST,
        "大小MB": size_mb(DST),
        "行数": table_counts(DST),
        "源行数": src_counts,
        "行数不一致": [t for t in set(src_counts or {}) | set(dst_counts or {})
                       if (src_counts or {}).get(t) != (dst_counts or {}).get(t)],
        "结果": "成功",
    }
    print("\n====== 已推送：%s ======" % DST)
    print("记录：%s" % os.path.basename(uc.write_record("arknights.db", record)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
