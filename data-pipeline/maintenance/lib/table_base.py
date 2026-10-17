# -*- coding: utf-8 -*-
"""PRTS 终端 — 表更新执行器

表脚本只提供数据与钩子，主流程全在这里：读源、校对、新增、爬取、写库、记录、退出码。

``TableSpec`` 的钩子约定
    ``columns`` 列声明：all / source / crawl / manual / subkeys / json / int / float / compare
    ``key_cols`` 键：单键传一列；enemies 这种复合键传 ["敌人ID", "级别"]（类型由 int 列推断）
    ``load_rows()``   数据源 → {键元组: {列名: 值}}
    ``crawl(keys, rows)``  待爬键 + 全量行 → {键元组: Outcome}；自己控循环与进度
    ``row_exempt(key)``    行级豁免：永不校对、永不爬取、不进 pending
    ``on_diff(key, col, want, have)``  写库前的改写（如把库内备注接回 JSON 列）
    ``extra_args(parser)`` 表特有参数

三种模式
    --base      数据源 → 校对 + 新增插入 + 新行登记 pending；不联网
    --crawl     处理 pending；补齐的移出，仍缺的留档
    --recheck   全表（除豁免行）重爬，只比对爬取列并写差异；不碰 pending
    无参数      = --base + --crawl

约定
    --dry-run   本地预分析：不联网、不写库、不写记录、不备份
    记录        有变化或失败才落盘（updates/<表>/<时间戳>.json）
    失败        网络/解析/哨兵/写库 → 中止并回滚；页面缺失/空壳 → 记 pending，继续
"""

import argparse
import json
import os
import sqlite3
import sys
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path

import update_common as uc
import paths

MAX_DETAIL = 200
JOURNAL_MODE = "MEMORY"


# ─── 声明与状态 ───────────────────────────────────────────────

@dataclass
class Columns:
    all: list                                     # INSERT 列顺序（含所有列）
    source: list                                  # 数据源列 → 参与校对
    crawl: list = field(default_factory=list)     # 页面列 → 已有行只在 --recheck 时比对
    manual: list = field(default_factory=list)    # 人工列 → 永不比对、永不写入
    subkeys: list = field(default_factory=list)   # JSON 列里的子键，如 [("技能列表", "备注")]
    json: list = field(default_factory=list)
    int: list = field(default_factory=list)
    float: list = field(default_factory=list)
    compare: dict = field(default_factory=dict)   # 特殊比对：{"名称": "exact"}


@dataclass
class Outcome:
    """单行爬取结果。state: synced / missing / partial。"""
    state: str
    values: dict = field(default_factory=dict)
    note: str = ""
    page: str = ""


class Pending:
    """pending.json：新增行的首次爬取清单。{} = 没有待办。"""

    def __init__(self, path):
        self.path = Path(path)
        self.data = {}
        if self.path.exists():
            try:
                self.data = json.loads(self.path.read_text(encoding="utf-8"))
            except (ValueError, OSError):
                self.data = {}

    @staticmethod
    def kstr(key):
        return "|".join(str(k) for k in key)

    def keys(self, key_cols, int_cols):
        """把文件里的键还原成带类型的元组（级别这类列要转回 int）。"""
        out = []
        for k in self.data:
            parts = k.split("|")
            if len(parts) != len(key_cols):
                continue
            out.append(tuple(int(p) if c in int_cols else p
                             for p, c in zip(parts, key_cols)))
        return out

    def add(self, key, name):
        k = self.kstr(key)
        if k in self.data:
            return False
        now = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
        self.data[k] = {"名称": name, "状态": "未同步", "首次": now,
                        "最后检查": now, "尝试": 0, "页面": ""}
        return True

    def set_state(self, key, state, page="", note=""):
        k = self.kstr(key)
        item = self.data.setdefault(k, {"名称": "", "首次": ""})
        item.update({"状态": state,
                     "最后检查": datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
                     "尝试": item.get("尝试", 0) + 1})
        if page:
            item["页面"] = page
        if note:
            item["备注"] = note

    def drop(self, key):
        return self.data.pop(self.kstr(key), None) is not None

    def save(self):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.path.write_text(
            json.dumps(self.data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


@dataclass
class TableSpec:
    table: str
    columns: Columns
    key_cols: list
    load_rows: object                     # (db_path) -> {key: row}
    pending_path: Path                    # <表目录>/pending.json
    crawl: object = None                  # (keys, rows) -> {key: Outcome}
    row_exempt: object = None             # (key) -> bool
    skip_column: object = None            # (key, col) -> bool
    on_diff: object = None                # (key, col, want, have) -> 写入值
    extra_args: object = None             # (parser) -> None
    recheck_enabled: bool = True          # 本表要不要 --recheck


# ─── 工具 ─────────────────────────────────────────────────────

def _k(key):
    """键归一成元组：单键表（如 stages 的 关卡ID）常直接返回字符串键。"""
    return key if isinstance(key, tuple) else (key,)


def _is_empty(value):
    """空串与 NULL 都算空（与 uc.same 的判等口径一致）。"""
    return value is None or (isinstance(value, str) and not value.strip())


def _same(cols, col, a, b):
    if cols.compare.get(col) == "exact":
        return a == b
    return uc.same(col, a, b, set(cols.json), set(cols.float), set(cols.int))


def _strip_subkeys(cols, col, value):
    """JSON 列比对前剔掉子键（备注这类爬取子键不该参与数据源比对）。"""
    keys = [s for c, s in cols.subkeys if c == col]
    if not keys or value is None:
        return value
    try:
        obj = json.loads(value)
    except (TypeError, ValueError):
        return value
    if not isinstance(obj, list):
        return value
    for item in obj:
        if isinstance(item, dict):
            for s in keys:
                item.pop(s, None)
    return json.dumps(obj, ensure_ascii=False)


def _read_db(db, spec):
    cols = spec.columns
    conn = sqlite3.connect(db)
    db_cols = [d[1] for d in conn.execute("PRAGMA table_info(%s)" % spec.table)]
    out = {}
    for r in conn.execute("SELECT %s FROM %s" % (", ".join(db_cols), spec.table)):
        d = dict(zip(db_cols, r))
        out[tuple(d[c] for c in spec.key_cols)] = d
    conn.close()
    return out


def _write_db(db, spec, diffs, new_rows, backup):
    cols = spec.columns
    where = " AND ".join("%s=?" % c for c in spec.key_cols)
    if backup:
        print("  备份: %s" % (uc.backup_file(db) or "（未生成）"))
    conn = sqlite3.connect(db)
    conn.isolation_level = None
    try:
        conn.execute("PRAGMA journal_mode=%s" % JOURNAL_MODE)
        conn.execute("BEGIN IMMEDIATE")
        for key, col, _old, new in diffs:
            conn.execute("UPDATE %s SET %s=? WHERE %s" % (spec.table, col, where),
                         (new,) + tuple(key))
        for key, row in new_rows:
            conn.execute(
                "INSERT OR IGNORE INTO %s (%s) VALUES (%s)"
                % (spec.table, ", ".join(cols.all), ", ".join("?" * len(cols.all))),
                tuple(row.get(c) for c in cols.all))
        conn.execute("COMMIT")
    except Exception:
        conn.execute("ROLLBACK")
        raise
    finally:
        conn.close()


def _record(spec, mode, src_ver, db):
    return {
        "表": spec.table,
        "模式": mode,
        "运行时间": datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
        "数据源": {"提交": src_ver["提交"], "提交标题": src_ver["提交标题"],
                   "游戏版本": src_ver["游戏版本"], "数据时间": src_ver["数据时间"]},
        "脚本": {"文件": os.path.basename(sys.argv[0]) if sys.argv else "", "版本": 1},
        "数据库": os.path.basename(db),
        "校对": {"检查行": 0, "豁免行": 0, "差异行": 0, "差异列数": 0, "计数": {}, "明细": []},
        "新增": {"行": 0, "明细": []},
        "爬取": {"目标": 0, "补上": 0, "缺失": 0, "部分": 0},
        "pending": {"入口": 0, "补齐": 0, "仍缺": 0, "总数": 0},
        "结果": "未执行",
    }


# ─── 主流程 ───────────────────────────────────────────────────

def run(spec, argv=None):
    ap = argparse.ArgumentParser(description="%s 表更新" % spec.table)
    ap.add_argument("--dry-run", action="store_true",
                    help="本地预分析：不联网、不写库、不写记录、不备份")
    ap.add_argument("--base", action="store_true", help="数据源 → 校对 + 新增 + 登记待办")
    ap.add_argument("--crawl", action="store_true", help="只处理 pending 里的待爬项")
    ap.add_argument("--recheck", action="store_true", help="全表重爬，只比对爬取列")
    ap.add_argument("--limit", type=int, default=0,
                    help="--recheck 单次最多处理多少行（0 = 不限；便于分段跑与调试）")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前的自动备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库（默认 paths.DB_PATH）")
    if spec.extra_args:
        spec.extra_args(ap)
    args = ap.parse_args(argv)

    explicit = args.base or args.crawl or args.recheck
    do_base = args.base or not explicit
    do_crawl = (args.crawl or not explicit) and not args.recheck
    do_recheck = args.recheck
    if do_recheck and not spec.recheck_enabled:
        print("本表不提供 --recheck（爬取耗时且内容不易变化）；重爬列请用 --crawl。")
        return 1
    if do_recheck and args.dry_run:
        print("注意：--recheck 需要联网，--dry-run 下只报告目标行数。")

    if not os.path.exists(args.db):
        raise SystemExit("数据库不存在：%s" % args.db)
    paths.ensure_gamedata()

    cols = spec.columns
    int_cols = set(cols.int)
    src_ver = uc.read_source_version(paths.DATA_SOURCE)
    mode = "recheck" if do_recheck else (
        "base+crawl" if do_base and do_crawl else ("base" if do_base else "crawl"))

    print("====== %s 表更新（%s）======" % (spec.table, mode))
    print("数据库  : %s" % args.db)
    print("数据源  : %s" % paths.DATA_SOURCE)
    print("源版本  : 提交 %s | 游戏版本 %s | 数据时间 %s"
          % (src_ver["提交"], src_ver["游戏版本"], src_ver["数据时间"]))

    record = _record(spec, mode, src_ver, args.db)
    pending = Pending(spec.pending_path)
    existing = _read_db(args.db, spec)

    def exempt(key):
        return bool(spec.row_exempt and spec.row_exempt(key))

    print("\n------ 重算目标行 ------")
    rows = {_k(k): v for k, v in spec.load_rows(args.db).items()}
    print("  目标行数: %d" % len(rows))

    diffs, new_keys = [], []

    # ── 1. 校对 + 新增（--base）──
    if do_base:
        print("\n------ 1. 校对 ------")
        per_col, exempt_rows = {}, 0
        for key, want in rows.items():
            have = existing.get(key)
            if have is None:
                continue
            if exempt(key):
                exempt_rows += 1
                continue
            for col in cols.source:
                if spec.skip_column and spec.skip_column(key, col):
                    continue
                a = have[col]
                b = want.get(col)
                if any(c == col for c, _ in cols.subkeys):
                    a, b = _strip_subkeys(cols, col, a), _strip_subkeys(cols, col, b)
                if not _same(cols, col, a, b):
                    diffs.append((key, col, a, b))
                    per_col[col] = per_col.get(col, 0) + 1
        record["校对"] = {
            "检查行": len(existing), "豁免行": exempt_rows,
            "差异行": len({d[0] for d in diffs}), "差异列数": len(diffs), "计数": per_col,
            "明细": [dict({"键": Pending.kstr(k), "列": c},
                          **uc.diff_brief(a, b, as_json=c in cols.json))
                     for k, c, a, b in diffs[:MAX_DETAIL]],
        }
        print("  库内 %d 行 | 豁免 %d | 差异行 %d | 差异列 %d"
              % (len(existing), exempt_rows, record["校对"]["差异行"], len(diffs)))
        for col, n in sorted(per_col.items(), key=lambda x: -x[1]):
            print("    %s: %d 处" % (col, n))

        print("\n------ 2. 新增 ------")
        new_keys = [k for k in rows if k not in existing]
        record["新增"] = {"行": len(new_keys),
                          "明细": [{"键": Pending.kstr(k), "名称": rows[k].get("名称")}
                                   for k in new_keys[:MAX_DETAIL]]}
        print("  新增行: %d" % len(new_keys))
        for k in new_keys[:20]:
            print("    %s  %s" % (Pending.kstr(k), rows[k].get("名称") or ""))
        if not args.dry_run:
            added = 0
            for k in new_keys:
                if not exempt(k):
                    added += 1 if pending.add(k, rows[k].get("名称") or "") else 0
            record["pending"]["入口"] = added
            if added:
                print("  登记待办: %d（豁免 %d）" % (added, len(new_keys) - added))

    # ── 2. 爬取（--crawl）──
    outcomes = {}
    targets = pending.keys(spec.key_cols, int_cols) if do_crawl else []
    if targets and not args.dry_run:
        print("\n------ 3. 爬取（pending %d 项）------" % len(targets))
        try:
            outcomes = spec.crawl(targets, rows) or {}
        except Exception as e:
            record["结果"] = "中止（爬取失败：%s）" % e
            uc.write_record(spec.table, record)
            print("\n!! 中止：%s" % e)
            print("   未对数据库做任何修改")
            return 1
        ok = 0
        for key in targets:
            out = outcomes.get(tuple(key))
            if out is None:
                continue
            if out.state == "synced":
                pending.drop(key)
                ok += 1
                # 行已入库（--base 已在上一轮插入过）→ 把爬到的值写回爬取列。
                # 只填空：库内已有内容（多半是人工写的）一律不动。
                if key in existing:
                    for col in cols.crawl:
                        if col not in out.values:
                            continue
                        a = existing[key][col]
                        if _is_empty(a) and not _is_empty(out.values[col]):
                            diffs.append((key, col, a, out.values[col]))
            else:
                pending.set_state(key, out.state, page=out.page, note=out.note)
        record["爬取"] = {"目标": len(targets), "补上": ok,
                          "缺失": sum(1 for o in outcomes.values() if o.state == "missing"),
                          "部分": sum(1 for o in outcomes.values() if o.state == "partial")}
        record["pending"].update({"补齐": ok, "仍缺": len(targets) - ok})
        record["校对"].update({"差异行": len({d[0] for d in diffs}),
                              "差异列数": len(diffs)})
        print("  补上 %d | 仍缺 %d | 写回 %d 列" % (ok, len(targets) - ok, len(diffs)))

    # ── 3. 全量校对（--recheck）──
    if do_recheck:
        scope = [k for k in existing if k in rows and not exempt(k)]
        if args.limit:
            scope = scope[:args.limit]
        print("\n------ 全量重爬（%d 行）------" % len(scope))
        if args.dry_run:
            print("  (干跑：未联网)")
            return 0
        try:
            outs = spec.crawl(scope, rows) or {}
        except Exception as e:
            record["结果"] = "中止（爬取失败：%s）" % e
            uc.write_record(spec.table, record)
            print("\n!! 中止：%s" % e)
            print("   未对数据库做任何修改")
            return 1
        per_col = {}
        for key, out in outs.items():
            # 只写有内容的；partial / missing 不覆写库内已有的值
            # （页面暂时为空不代表内容被删，保守起见不动）
            if out.state != "synced":
                continue
            for col in cols.crawl:
                if col not in out.values:
                    continue
                a = existing[key][col]
                if not _same(cols, col, a, out.values[col]):
                    diffs.append((key, col, a, out.values[col]))
                    per_col[col] = per_col.get(col, 0) + 1
        record["校对"] = {"检查行": len(scope), "豁免行": len(existing) - len(scope),
                          "差异行": len({d[0] for d in diffs}), "差异列数": len(diffs),
                          "计数": per_col,
                          "明细": [dict({"键": Pending.kstr(k), "列": c},
                                       **uc.diff_brief(a, b, as_json=c in cols.json))
                                   for k, c, a, b in diffs[:MAX_DETAIL]]}
        record["爬取"] = {"目标": len(scope),
                          "补上": sum(1 for o in outs.values() if o.state == "synced"),
                          "缺失": sum(1 for o in outs.values() if o.state == "missing"),
                          "部分": sum(1 for o in outs.values() if o.state == "partial")}
        print("  差异行 %d | 差异列 %d" % (record["校对"]["差异行"], len(diffs)))

    # ── 4. 写库 ──
    if args.dry_run:
        print("\n(干跑：未联网、未写库、未写记录、未备份)")
        if pending.data:
            print("  待爬条目: %d（未执行）" % len(pending.data))
        return 0

    if not diffs and not new_keys:
        print("\n------ 没有变化：未写库、未写记录 ------")
        pending.save()
        return 0

    print("\n------ 4. 写库（单事务）------")
    if args.no_backup:
        print("  (已跳过备份)")
    new_rows = []
    for key in new_keys:
        want = dict(rows[key])
        out = outcomes.get(tuple(key))
        if out and out.values:
            want.update(out.values)
        new_rows.append((key, want))
    try:
        if spec.on_diff:
            diffs = [(k, c, o, spec.on_diff(k, c, n, existing[k][c])) for k, c, o, n in diffs]
        _write_db(args.db, spec, diffs, new_rows, backup=not args.no_backup)
    except Exception as e:
        record["结果"] = "中止（写库失败：%s）" % e
        uc.write_record(spec.table, record)
        print("\n!! 写库失败并已回滚：%s" % e)
        return 1
    pending.save()
    record["pending"]["总数"] = len(pending.data)

    record["结果"] = "成功"
    path = uc.write_record(spec.table, record)
    print("\n====== 完成 ======")
    print("  校对写回 %d 列 | 新增 %d 行 | 待办剩余 %d | 记录 %s"
          % (len(diffs), len(new_keys), len(pending.data), os.path.basename(path)))
    return 0


# ─── 重建型（JSON 表）───────────────────────────────────────

@dataclass
class RebuildSpec:
    """重建表：每次从源全量生成，与现有文件对比后整体替换。

    ``build(old)`` 拿到旧数据（首次运行是空 dict），返回完整的新数据。
    人工层（如 ranges 的 alias）由 build 自己从旧数据带回，执行器不关心。
    """
    name: str                     # 记录名 / 表名
    output: Path                  # 输出文件（如 resources/ranges.json）
    build: object                 # (old: dict) -> dict


def run_rebuild(spec, argv=None):
    ap = argparse.ArgumentParser(description="%s 表重建" % spec.name)
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写文件")
    ap.add_argument("--no-backup", action="store_true", help="跳过写前备份")
    args = ap.parse_args(argv)

    old, old_text = {}, None
    if spec.output.exists():
        old_text = spec.output.read_text(encoding="utf-8")
        try:
            old = json.loads(old_text)
        except ValueError:
            raise SystemExit("现有文件不是合法 JSON：%s" % spec.output)

    src_ver = uc.read_source_version(paths.DATA_SOURCE)
    print("====== %s 表重建 ======" % spec.name)
    print("输出    : %s" % spec.output)
    print("源版本  : 提交 %s | 游戏版本 %s | 数据时间 %s"
          % (src_ver["提交"], src_ver["游戏版本"], src_ver["数据时间"]))

    new = spec.build(old)
    added = [k for k in new if k not in old]
    gone = [k for k in old if k not in new]
    changed = [k for k in new if k in old and old[k] != new[k]]
    text = json.dumps(new, ensure_ascii=False, indent=2)
    identical = old_text == text

    print("\n------ 对比 ------")
    print("  原 %d 项 | 新 %d 项 | 新增 %d | 消失 %d | 变化 %d"
          % (len(old), len(new), len(added), len(gone), len(changed)))
    for label, keys in (("新增", added), ("消失", gone), ("变化", changed)):
        if keys:
            print("    %s: %s" % (label, " ".join(str(k) for k in keys[:8])))
    print("  与现有文件逐字节一致: %s" % identical)

    if args.dry_run:
        print("\n(干跑：未写文件)")
        return 0

    if identical:
        print("\n------ 没有变化：未写文件、未写记录 ------")
        return 0

    if args.no_backup:
        print("\n  (已跳过备份)")
    else:
        print("\n  备份: %s" % (uc.backup_file(str(spec.output)) or "（未生成）"))
    spec.output.parent.mkdir(parents=True, exist_ok=True)
    tmp = str(spec.output) + ".tmp"
    with open(tmp, "w", encoding="utf-8", newline="") as f:
        f.write(text)
    os.replace(tmp, str(spec.output))

    record = {
        "表": spec.name,
        "模式": "重建",
        "运行时间": datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
        "数据源": {"提交": src_ver["提交"], "提交标题": src_ver["提交标题"],
                   "游戏版本": src_ver["游戏版本"], "数据时间": src_ver["数据时间"]},
        "脚本": {"文件": os.path.basename(sys.argv[0]) if sys.argv else "", "版本": 1},
        "计数": {"原": len(old), "新": len(new)},
        "新增": added[:MAX_DETAIL], "消失": gone[:MAX_DETAIL], "变化": changed[:MAX_DETAIL],
        "结果": "成功",
    }
    path = uc.write_record(spec.name, record)
    print("\n====== 完成：%s ======" % spec.output)
    print("  记录: %s" % os.path.basename(path))
    return 0
