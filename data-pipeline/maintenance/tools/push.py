# -*- coding: utf-8 -*-
"""PRTS 终端 — 权威库推送到发布资源目录（维护用）

更新链路：数据更新脚本只动 `maintenance/arknights.db`（唯一权威），
本脚本把它推到 `src-tauri/resources/arknights.db`（随包发布、运行时读取的种子），
并在发布副本里写入 `meta` 表、生成 `DATA_MANIFEST.json`。

用法（示例）：
    python push.py --dry-run --data-version 2026.10.18 --source-commit <commit>
    python push.py --data-version 2026.10.18 --source-commit <commit>
    python push.py --manifest-only --data-contract-status passed \
        --data-contract-passed 1209 --data-contract-ignored 21

说明：
  - resources 下的五张 JSON 表由各自的 rebuild/update 脚本直接写入，不走这里。
  - 目标文件在 .gitignore 中；备份与记录仍按更新系统的统一约定落盘。
  - 发布副本才写 meta；权威库本身不被本脚本改写。
"""

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import sqlite3
import sys
import time

from pathlib import Path

HERE = Path(__file__).resolve().parent
MAINT = next(p for p in HERE.parents if (p / "lib").is_dir())   # maintenance/
for _p in (str(HERE), str(MAINT / "lib"), str(MAINT.parent)):   # 本目录 / 通用模块 / data-pipeline
    if _p not in sys.path:
        sys.path.insert(0, _p)
from paths import DB_PATH, PROJECT_ROOT, RESOURCES  # noqa: E402

import update_common as uc  # noqa: E402

SRC = str(DB_PATH)
DST = os.path.join(str(RESOURCES), "arknights.db")
MANIFEST = os.path.join(str(RESOURCES), "DATA_MANIFEST.json")
DATA_VERSION_RE = re.compile(r"^\d{4}\.\d{2}\.\d{2}(?:\.\d+)?$")


def utc_now():
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat().replace(
        "+00:00", "Z"
    )


def app_version():
    # 记录数据生成时的应用版本快照，不是应用发布版本；
    # 数据生成早于版本号提升时，允许与当前 package.json 不同。
    try:
        with open(PROJECT_ROOT / "package.json", encoding="utf-8") as f:
            return str(json.load(f).get("version") or "").strip()
    except Exception:
        return ""


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest().upper()


def table_counts(path, include_meta=True):
    """{表名: 行数}；库不存在或读不开返回 None。"""
    if not os.path.exists(path):
        return None
    try:
        con = sqlite3.connect(path)
        names = [r[0] for r in con.execute(
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")]
        out = {}
        for t in names:
            if not include_meta and t == "meta":
                continue
            out[t] = con.execute("SELECT COUNT(*) FROM %s" % t).fetchone()[0]
        con.close()
        return out
    except sqlite3.Error as e:
        return {"（读库失败）": str(e)}


def read_meta(path):
    """读发布库的 meta；缺表/缺行返回 None。"""
    if not os.path.exists(path):
        return None
    try:
        con = sqlite3.connect(path)
        has = con.execute(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='meta'"
        ).fetchone()[0]
        if not has:
            con.close()
            return None
        row = con.execute(
            "SELECT dataVersion, generatedAt, sourceCommit, schemaVersion FROM meta LIMIT 1"
        ).fetchone()
        con.close()
        if not row:
            return None
        return {
            "dataVersion": row[0],
            "generatedAt": row[1],
            "sourceCommit": row[2],
            "schemaVersion": int(row[3]),
        }
    except (sqlite3.Error, TypeError, ValueError):
        return None


def write_meta(conn, meta):
    """在发布副本里重建 meta 表。只对临时副本调用，不碰权威库。"""
    conn.execute("DROP TABLE IF EXISTS meta")
    conn.execute(
        "CREATE TABLE meta ("
        "dataVersion TEXT NOT NULL, "
        "generatedAt TEXT NOT NULL, "
        "sourceCommit TEXT NOT NULL, "
        "schemaVersion INTEGER NOT NULL"
        ")"
    )
    conn.execute(
        "INSERT INTO meta (dataVersion, generatedAt, sourceCommit, schemaVersion) "
        "VALUES (?, ?, ?, ?)",
        (meta["dataVersion"], meta["generatedAt"], meta["sourceCommit"], meta["schemaVersion"]),
    )
    conn.commit()


def stage_copy(src, dst, meta):
    """复制源库到临时文件，并在临时文件里写 meta；返回临时路径。"""
    tmp = dst + ".tmp"
    if os.path.exists(tmp):
        os.remove(tmp)
    with sqlite3.connect(src) as s, sqlite3.connect(tmp) as d:
        s.backup(d)
        write_meta(d, meta)
    return tmp


def manifest_payload(meta, data_contract):
    return {
        "appVersion": app_version(),
        "dataVersion": meta["dataVersion"],
        "generatedAt": meta["generatedAt"],
        "sourceCommit": meta["sourceCommit"],
        "sourceNote": meta.get("sourceNote") or None,
        "schemaVersion": meta["schemaVersion"],
        "database": {
            "file": "arknights.db",
            "bytes": os.path.getsize(DST),
            "sha256": sha256_file(DST),
        },
        "dataContract": data_contract,
    }


def write_manifest(payload):
    tmp = MANIFEST + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(payload, f, ensure_ascii=False, indent=2)
        f.write("\n")
    os.replace(tmp, MANIFEST)


def contract_payload(args):
    status = args.data_contract_status
    checked = utc_now() if status != "pending" else None
    return {
        "status": status,
        "passed": args.data_contract_passed,
        "ignored": args.data_contract_ignored,
        "checkedAt": checked,
        "note": args.data_contract_note or None,
    }


def size_mb(path):
    return round(os.path.getsize(path) / 1024 / 1024, 2) if os.path.exists(path) else None


def main():
    ap = argparse.ArgumentParser(description="把权威库推送到 src-tauri/resources/")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写文件")
    ap.add_argument("--manifest-only", action="store_true",
                    help="只按现有资源库重写 DATA_MANIFEST.json")
    ap.add_argument("--data-version", help="数据快照版本，格式 YYYY.MM.DD[.N]")
    ap.add_argument("--source-commit", help="数据源提交或维护快照标识")
    ap.add_argument("--source-note", help="sourceCommit 的补充说明（可选）")
    ap.add_argument("--generated-at", help="数据生成时间；缺省为当前 UTC 时间")
    ap.add_argument("--schema-version", type=int, default=1, help="数据库结构版本，默认 1")
    ap.add_argument("--data-contract-status", default="pending",
                    choices=("pending", "passed", "failed"), help="data-contract 结果")
    ap.add_argument("--data-contract-passed", type=int, default=0)
    ap.add_argument("--data-contract-ignored", type=int, default=0)
    ap.add_argument("--data-contract-note")
    args = ap.parse_args()

    if args.manifest_only:
        if not os.path.exists(DST):
            print("资源库不存在：%s" % DST)
            return 1
        meta = read_meta(DST)
        if meta is None:
            print("资源库缺少 meta 表，无法只重写清单；请先正常推送一次。")
            return 1
        old_manifest = {}
        try:
            with open(MANIFEST, encoding="utf-8") as f:
                old_manifest = json.load(f)
        except Exception:
            pass
        meta["sourceNote"] = args.source_note or old_manifest.get("sourceNote")
        payload = manifest_payload(meta, contract_payload(args))
        if args.dry_run:
            print(json.dumps(payload, ensure_ascii=False, indent=2))
            return 0
        write_manifest(payload)
        print("已重写清单：%s" % MANIFEST)
        return 0

    if not os.path.exists(SRC):
        print("权威库不存在：%s" % SRC)
        return 1
    if not args.data_version:
        print("缺少 --data-version（格式 YYYY.MM.DD[.N]）")
        return 1
    if not DATA_VERSION_RE.match(args.data_version):
        print("dataVersion 格式无效：%s（应为 YYYY.MM.DD[.N]）" % args.data_version)
        return 1
    if not args.source_commit or not args.source_commit.strip():
        print("缺少 --source-commit（数据源提交或维护快照标识）")
        return 1
    if args.schema_version < 1:
        print("schemaVersion 必须 >= 1")
        return 1

    meta = {
        "dataVersion": args.data_version,
        "generatedAt": args.generated_at or utc_now(),
        "sourceCommit": args.source_commit.strip(),
        "sourceNote": args.source_note,
        "schemaVersion": args.schema_version,
    }
    src_counts, dst_counts = table_counts(SRC, include_meta=False), table_counts(DST, include_meta=False)

    print("====== 推送到 resources ======")
    print("源   %s  %.2f MB" % (SRC, size_mb(SRC)))
    print("目标 %s  %s" % (DST, "%.2f MB" % size_mb(DST) if dst_counts else "（不存在）"))
    print("数据版本 %s / schema %s / 应用 %s" % (
        meta["dataVersion"], meta["schemaVersion"], app_version() or "（未知）"))

    if dst_counts is None:
        print("\n目标不存在，将新建。")
    else:
        print("\n%-24s %8s %8s %s" % ("表", "源", "目标", "差异"))
        for t in sorted(set(src_counts) | set(dst_counts)):
            a, b = src_counts.get(t), dst_counts.get(t)
            mark = "" if a == b else "  ←"
            print("%-24s %8s %8s%s" % (t, a, b, mark))

    if args.dry_run:
        print("\n(干跑：未写文件)")
        print(json.dumps(meta, ensure_ascii=False, indent=2))
        return 0

    tmp = None
    try:
        tmp = stage_copy(SRC, DST, meta)
        uc.backup_file(DST)                  # 写前备份目标（按文件名各留 3 份）
        os.replace(tmp, DST)                 # 原子替换，避免拷到一半被读到
        tmp = None
    finally:
        if tmp and os.path.exists(tmp):
            os.remove(tmp)

    manifest = manifest_payload(meta, contract_payload(args))
    write_manifest(manifest)
    print("\n====== 已推送：%s ======" % DST)
    print("清单：%s" % MANIFEST)
    print("DB SHA-256：%s" % manifest["database"]["sha256"])

    record = {
        "脚本": {"文件": "maintenance/tools/push.py", "版本": 2},
        "运行时间": time.strftime("%Y-%m-%d %H:%M:%S"),
        "模式": "推送",
        "应用版本": manifest["appVersion"],
        "数据版本": meta["dataVersion"],
        "数据生成时间": meta["generatedAt"],
        "数据源提交": meta["sourceCommit"],
        "数据源说明": meta["sourceNote"],
        "结构版本": meta["schemaVersion"],
        "源": SRC,
        "目标": DST,
        "大小MB": size_mb(DST),
        "SHA-256": manifest["database"]["sha256"],
        "行数": table_counts(DST),
        "源行数": src_counts,
        "行数不一致": [t for t in set(src_counts or {}) | set(dst_counts or {})
                       if (src_counts or {}).get(t) != (dst_counts or {}).get(t)],
        "data-contract": manifest["dataContract"],
        "结果": "成功",
    }
    print("记录：%s" % os.path.basename(uc.write_record("arknights.db", record)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
