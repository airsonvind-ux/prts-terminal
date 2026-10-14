"""
PRTS 终端 — 一次性修复：`生息演算·重启锚点` 的 `敌人列表`（按 PRTS 页面重建）

为什么要有这个脚本
------------------
这 29 行的 `敌人列表` 是 `build_sandbox2_prts.py` 从 PRTS 页面抓的，但那版解析有 bug：
用 `敌人{i}(.+)` 去匹配 `{{敌方情报}}` 的参数，**序号没锁边界** —— i=1 时 `敌人10数量`
也被吃掉，于是把 `0数量` 当成属性名、把第 10 只敌人的名字当成属性 `0`。
结果这 29 行凭空多出一整列假覆写属性（28 行本该是空的）。

本次按 PRTS 页面如实重建：只写 `stages.敌人列表` 一列，不动其它列。

口径
----
- 页面名 = `{关卡编号}_{关卡名}`（如 `RA-4_相助`），取 raw wikitext；
- 只读 `{{敌方情报}}` 模板：
  `敌人N` 名字、`敌人N级别`、`敌人N数量(下限)` 是固有参数（不写进覆写属性），
  其余 `敌人N<属性>` 才是覆写（实测 29 个页面里只有 `RA-A-2_较量` 的敌人 3 有）；
- 属性名 → `enemies` 表列名（`生命值`→`最大生命值`、`地位级别`→`地位`）；不认识的参数不写，
  但会列出来提醒（页面改版时能看见）；
- `敌人ID` 按名字回 `enemies` 表查；同名多 ID 时统一取**不带 `_2` 后缀**的那个；
- 页面取不到、或没有 `{{敌方情报}}` → 该行跳过并记入记录，不写库。

用法
----
    python rebuild_sandbox2_enemies.py --dry-run    # 只报告，不写库
    python rebuild_sandbox2_enemies.py              # 正式执行
"""

import argparse
import json
import os
import re
import shutil
import socket
import sqlite3
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import paths  # noqa: E402

socket.setdefaulttimeout(15)

TABLE = "stages"
KIND = "生息演算·重启锚点"
SCRIPT_VERSION = 1
SLEEP = 0.4
BACKUP_KEEP = 3

BACKUP_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "maintenance", "backups")
RECORD_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "maintenance", "updates")

# PRTS 参数名 → enemies 表列名。左边是页面上的写法，右边必须是表里真实存在的列名。
PRTS_ATTR_MAP = {
    "最大生命值": "最大生命值", "生命值": "最大生命值",
    "攻击力": "攻击力", "防御力": "防御力",
    "法术抗性": "法术抗性", "移动速度": "移动速度",
    "攻击速度": "攻击速度", "攻击间隔": "攻击间隔",
    "生命恢复速度": "生命自回速度",
    "重量": "重量", "地位级别": "地位", "地位": "地位",
    "攻击半径": "攻击半径", "目标价值": "目标价值",
    "眩晕抗性": "晕眩抗性", "沉默抗性": "沉默抗性", "沉睡抗性": "沉睡抗性",
    "冻结抗性": "冻结抗性", "浮空抗性": "浮空抗性",
    "战栗抗性": "战栗抗性", "恐惧抗性": "恐惧抗性", "麻痹抗性": "麻痹抗性",
    "诱导抗性": "诱导抗性", "传送抗性": "传送抗性", "缚地抗性": "缚地抗性",
    "元素抗性": "元素抗性", "损伤抗性": "损伤抗性",
}
# 固有参数：不是覆写
INHERENT = ("数量", "数量下限", "级别")


def fetch_raw(key):
    url = "https://prts.wiki/w/%s?action=raw" % urllib.parse.quote(key)
    req = urllib.request.Request(url, headers={"User-Agent": "PRTS-Terminal/1.0"})
    with urllib.request.urlopen(req, timeout=10) as resp:
        return resp.read().decode("utf-8")


def fetch_page(code, name):
    """先试 `编号_名称`，再试 `编号`；返回 (命中的页面名, 文本)。"""
    keys = ["%s_%s" % (code, name)] if name else []
    keys.append(code)
    for key in keys:
        try:
            text = fetch_raw(key)
        except urllib.error.HTTPError:
            continue
        if text and "敌方情报" in text:
            return key, text
        time.sleep(0.3)
    return None, None


def parse_template(text, tpl_name):
    """按 {{}} 配对切出模板参数（不处理透视，够用）。"""
    out = []
    tag = "{{" + tpl_name
    pos = 0
    while True:
        idx = text.find(tag, pos)
        if idx == -1:
            break
        depth, i, guard, end = 0, idx, 50000, None
        while i < len(text) and guard > 0:
            guard -= 1
            if text[i:i + 2] == "{{":
                depth += 1
                i += 2
                continue
            if text[i:i + 2] == "}}":
                depth -= 1
                if depth == 0:
                    end = i + 2
                    break
                i += 2
                continue
            i += 1
        if end is None:
            break
        content = text[idx + len(tag):end - 2]
        params = {}
        parts = re.split(r"\n\|", content)
        first = parts[0].strip().lstrip("|")
        if "=" in first:
            k, v = first.split("=", 1)
            params[k.strip()] = v.strip()
        for part in parts[1:]:
            part = part.strip()
            if "=" in part:
                k, v = part.split("=", 1)
                params[k.strip()] = v.strip()
        out.append(params)
        pos = end
    return out


def to_num(text):
    """能转数字就转，否则原样返回字符串。"""
    s = (text or "").strip()
    try:
        return int(s)
    except ValueError:
        pass
    try:
        return float(s)
    except ValueError:
        return s


def parse_enemy_intel(text, unknown):
    """`{{敌方情报}}` → [{名称, 级别, 覆写属性?}]；unknown 收集不认识的参数名。"""
    tpls = parse_template(text, "敌方情报")
    if not tpls:
        return None
    params = tpls[0]
    out = []
    i = 1
    while True:
        name = (params.get("敌人%d" % i) or "").strip()
        if not name:
            break
        item = {"名称": name, "级别": to_num(params.get("敌人%d级别" % i, "0"))}
        overrides = {}
        for key, value in params.items():
            m = re.match(r"敌人(\d+)(\D.*)$", key)
            # 序号必须与当前敌人相等，且属性名不以数字开头 —— 这两条一起挡住两位序号：
            # `敌人10` 不会被拆成“序号 1 + 属性 0”，`敌人10数量` 也不会落到 i=1 头上
            if not m or int(m.group(1)) != i:
                continue
            attr = m.group(2).strip()
            if attr in INHERENT:
                continue
            col = PRTS_ATTR_MAP.get(attr)
            if col is None:
                unknown.add(attr)
                continue
            if str(value).strip() == "":
                continue
            overrides[col] = to_num(value)
        if overrides:
            item["覆写属性"] = overrides
        out.append(item)
        i += 1
    return out


def resolve_ids(conn):
    """名称 → [敌人ID...]（同名会有一串）。"""
    by_name = {}
    for eid, nm in conn.execute("SELECT 敌人ID, 名称 FROM enemies"):
        by_name.setdefault(nm, [])
        if eid not in by_name[nm]:
            by_name[nm].append(eid)
    return by_name


def pick_id(ids):
    """同名多 ID：统一取不带 `_2` 后缀的。"""
    plain = [x for x in ids if not x.endswith("_2")]
    if len(plain) == 1:
        return plain[0], "取不带_2"
    return sorted(plain or ids)[0], "仍有歧义"


def write_record(record):
    os.makedirs(RECORD_DIR, exist_ok=True)
    path = os.path.join(RECORD_DIR, "%s-%s.json" % (
        datetime.now().strftime("%Y-%m-%d-%H%M%S"), TABLE))
    with open(path, "w", encoding="utf-8") as f:
        json.dump(record, f, ensure_ascii=False, indent=2)
    return os.path.abspath(path)


def backup_db(db_path):
    os.makedirs(BACKUP_DIR, exist_ok=True)
    name = "%s.%s.bak" % (os.path.basename(db_path), datetime.now().strftime("%Y%m%d-%H%M%S"))
    dst = os.path.join(BACKUP_DIR, name)
    shutil.copy2(db_path, dst)
    old = sorted(f for f in os.listdir(BACKUP_DIR) if f.startswith(os.path.basename(db_path) + "."))
    for f in old[:-BACKUP_KEEP]:
        os.remove(os.path.join(BACKUP_DIR, f))
    return os.path.abspath(dst)


def main():
    ap = argparse.ArgumentParser(description="按 PRTS 页面重建 生息演算·重启锚点 的敌人列表（一次性）")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写库、不写记录")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前的自动备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库（默认权威库）")
    args = ap.parse_args()

    if not os.path.exists(args.db):
        raise SystemExit("数据库不存在：%s" % args.db)

    started = datetime.now()
    print("====== 重建 生息演算·重启锚点 敌人列表（PRTS 页面）======")
    print("数据库 : %s" % args.db)

    conn = sqlite3.connect(args.db)
    conn.isolation_level = None
    by_name = resolve_ids(conn)

    record = {
        "表": TABLE, "范围": KIND,
        "运行时间": started.strftime("%Y-%m-%d %H:%M:%S"),
        "脚本": {"文件": "rebuild_sandbox2_enemies.py", "版本": SCRIPT_VERSION},
        "数据库": os.path.basename(args.db),
        "目标行": 0, "写回行": 0, "取不到页面": [], "未知参数": [], "同名多ID": [],
        "明细": [], "结果": "未执行",
    }

    targets = conn.execute(
        "SELECT 关卡ID, 关卡编号, 关卡名, 敌人列表 FROM %s WHERE 关卡类型 = ? ORDER BY 关卡ID" % TABLE,
        (KIND,),
    ).fetchall()
    record["目标行"] = len(targets)

    unknown = set()
    changes = []
    print("\n------ 逐行抓取 ------")
    for sid, code, name, old_raw in targets:
        page, text = fetch_page((code or "").strip(), (name or "").strip())
        if not text:
            record["取不到页面"].append(sid)
            print("  %-14s 取不到页面，跳过" % sid)
            continue
        items = parse_enemy_intel(text, unknown)
        if items is None:
            record["取不到页面"].append(sid)
            print("  %-14s 页面 %s 没有 {{敌方情报}}，跳过" % (sid, page))
            continue
        new = []
        for it in items:
            ids = by_name.get(it["名称"])
            if not ids:
                it["敌人ID"] = ""
                record["同名多ID"].append({"名": it["名称"], "情况": "表里没有"})
            elif len(ids) == 1:
                it["敌人ID"] = ids[0]
            else:
                it["敌人ID"], how = pick_id(ids)
                record["同名多ID"].append({"名": it["名称"], "候选": sorted(ids),
                                           "取": it["敌人ID"], "依据": how})
            new.append(it)
        old = json.loads(old_raw) if old_raw else []
        mark = "同" if old == new else "改"
        with_ov = sum(1 for x in new if x.get("覆写属性"))
        print("  %-14s 页面 %-20s 条目 %2d → %2d  覆写 %d  [%s]"
              % (sid, page, len(old), len(new), with_ov, mark))
        if old != new:
            changes.append((sid, old, new))
        time.sleep(SLEEP)

    record["写回行"] = len(changes)
    record["未知参数"] = sorted(unknown)
    record["明细"] = [{"键": sid, "旧条目数": len(o), "新条目数": len(n),
                       "旧前三条": o[:3], "新前三条": n[:3]} for sid, o, n in changes]

    print("\n------ 汇总 ------")
    print("  目标行 %d | 需写回 %d | 取不到页面 %d" % (len(targets), len(changes), len(record["取不到页面"])))
    if unknown:
        print("  !! 不认识的页面参数（没写进库）: %s" % sorted(unknown))
    if record["同名多ID"]:
        print("  同名多 ID（已按“取不带 _2”处理）:")
        for x in record["同名多ID"]:
            print("     %s" % x)

    if args.dry_run:
        print("\n(干跑：未写库、未写记录)")
        record["结果"] = "干跑"
        return 0

    record["结果"] = "成功"
    if not args.no_backup:
        print("\n  备份: %s" % backup_db(args.db))
    conn.execute("BEGIN IMMEDIATE")
    try:
        for sid, _old, new in changes:
            conn.execute("UPDATE %s SET 敌人列表 = ? WHERE 关卡ID = ?" % TABLE,
                         (json.dumps(new, ensure_ascii=False), sid))
        conn.execute("COMMIT")
    except Exception as e:
        conn.execute("ROLLBACK")
        record["结果"] = "中止（%s）" % e
        print("\n!! 中止：%s（已回滚）" % e)
        print("   记录：%s" % write_record(record))
        return 1
    conn.close()
    print("\n  写回 %d 行" % len(changes))
    print("  记录: %s" % write_record(record))
    return 0


if __name__ == "__main__":
    sys.exit(main())
