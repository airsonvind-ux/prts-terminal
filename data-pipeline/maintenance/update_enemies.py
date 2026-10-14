# -*- coding: utf-8 -*-
"""PRTS 终端 — enemies 表更新脚本（维护用）

三段式，顺序固定：
  1. 校对：以 (敌人ID, 级别) 为键，把库内行与「由本地数据源重新算出的值」逐列比对，
          差异写回库并记入更新记录。
          校对列 = 除 `天赋文本` / `技能文本` / `备注` 之外的全部列
          （前两者来源于 PRTS 爬取，逐次重爬太慢；`备注` 是人工补充列）。
  2. 新增：库内不存在的键 INSERT（同一套继承与回填规则，含 `关联敌人`）；
          这些敌人的名称去重后爬一次 PRTS wiki，取回天赋/技能文本，过 wiki 清洗后写入。
          已有敌人不重爬。
  3. 记录：单事务，任何异常都不落库；结束写 updates/<时间戳>-enemies.json。

数据源（只读，来自 paths.GAMEDATA）：
  levels/enemydata/enemy_database.json   数值与属性
  excel/enemy_handbook_table.json        图鉴字段（能力列表 / 种类 / 伤害类型 / 关联敌人）
游戏版本号取自数据源仓库最新提交信息（形如 [CN UPDATE] Client:2.7.81 Data:26-10-08-...）。

失败语义（Air 定，2026-10-13）：
  - 网络异常 / PRTS 站点结构变化 → 中止本次更新（不改库），记录里写明原因；
  - PRTS 404（页面不存在，多为新敌人尚无页面）→ 跳过并记录，不计失败。

用法：
    python update_enemies.py --dry-run   # 只报告校对差异与新增，不写库、不爬取、不写记录
    python update_enemies.py             # 正式执行
"""

import argparse
import json
import os
import re
import sqlite3
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import paths  # noqa: E402
from cleaning_game import clean_desc  # noqa: E402
from cleaning_wiki import clean_wiki  # noqa: E402

TABLE = "enemies"
SCRIPT_VERSION = 3
MAX_DETAIL = 200          # 记录里明细最多留多少条（超出只计数）
SLEEP_BETWEEN_FETCH = 0.1  # 爬取间隔，礼貌抓取
KEEP_BACKUPS = 3           # 自动备份保留份数
NO_DETAIL_RECHECK_EVERY = 5   # 每累计跳过这么多个，复核一次哨兵（站点结构还健康吗）
NO_DETAIL_RATIO_LIMIT = 0.5   # 无结构页面占比超过这个值（且已试够 NO_DETAIL_MIN_TOTAL 个）→ 中止
NO_DETAIL_MIN_TOTAL = 20      # 比例兜底的生效下限
FETCH_RETRIES = 2             # 单页网络异常重试次数

# ID 段豁免：这些段不参与「校对」与「PRTS 爬取」，只做新增。
# 190 段是奇象生物（活动捕捉后可在特定玩法召唤的敌人 + 捕捉关卡中出现的同名非常规敌人）：
#   ① PRTS 上只有装置页/图鉴页，没有 {{敌人信息/levelcontent}} 结构，本来就没有技能与天赋；
#   ② 段内 18 个名字与正常敌人重名，按名称爬会重定向到正常敌人页、抓到别人的数据；
#   ③ 描述 / 能力列表 / 天赋文本 / 技能文本 由人工维护，校对会用数据源重算覆盖它们。
# 用 ID 前缀判定是唯一可靠的信号（页面行为分辨不了同名实体）。
EXCLUDE_FROM_CHECK_AND_CRAWL = ("enemy_190",)

# 输出实时可见：关掉缓冲、统一 UTF-8（PowerShell 下否则进度憋到最后才吐、中文可能乱码）
try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace", line_buffering=True)
except Exception:
    pass

ENEMY_DB = paths.GAMEDATA / "levels" / "enemydata" / "enemy_database.json"
HANDBOOK = paths.GAMEDATA / "excel" / "enemy_handbook_table.json"

# ─── 枚举映射 ────────────────────────────────────────────────
# IMMUNE_MAP 是「免疫字段 → 中文状态」的唯一事实来源：游戏新增异常状态时只改这里。
# 顺序 = 游戏 attributes 里的字段顺序（追加新状态就追加在末尾，已有条目顺序不变，
# 避免校对把历史行整列判成差异）。
# db_builder/build_enemies.py 里那份是构建期快照，已归档、不再随游戏更新。

STATUS_MAP = {"NORMAL": "普通", "ELITE": "精英", "BOSS": "领袖"}
ATK_MAP = {"MELEE": "近战", "RANGED": "远程", "NONE": "不攻击"}
MOTION_MAP = {"WALK": "地面", "FLY": "飞行"}
DAMAGE_MAP = {"PHYSIC": "物理", "MAGIC": "法术", "HEAL": "治疗", "NO_DAMAGE": "无"}

IMMUNE_MAP = {
    "stunImmune": "晕眩", "silenceImmune": "沉默",
    "sleepImmune": "沉睡", "frozenImmune": "冻结",
    "levitateImmune": "浮空", "disarmedCombatImmune": "战栗",
    "fearedImmune": "恐惧", "palsyImmune": "麻痹",
    "attractImmune": "诱导",
    "teleportImmune": "传送", "groundBoundImmune": "缚地",
}

# ─── 列分类（比较方式不同）──────────────────────────────────────

# 不参与校对：PRTS 爬取列 + 人工补充列
SKIP_COLS = {"天赋文本", "技能文本", "备注"}

JSON_COLS = {"能力列表", "种类", "关联敌人", "伤害类型", "异常抗性"}
FLOAT_COLS = {"攻击半径", "法术抗性", "移动速度", "攻击速度", "攻击间隔",
              "生命自回速度", "损伤抗性", "元素抗性", "viewRadius"}
INT_COLS = {"级别", "目标价值", "不计入敌人总数", "numOfExtraDrops", "最大生命值",
            "攻击力", "防御力", "重量", "基础嘲讽等级"}

# INSERT 列顺序（与实际库 schema 一致，含 `备注`；不含已废弃的 `模式标记`）
ALL_COLS = [
    "敌人ID", "名称", "级别", "描述", "地位", "能力列表", "种类", "关联敌人",
    "攻击方式", "伤害类型", "行动方式", "攻击半径", "目标价值", "不计入敌人总数",
    "numOfExtraDrops", "viewRadius", "最大生命值", "攻击力", "防御力", "法术抗性",
    "移动速度", "攻击速度", "攻击间隔", "生命自回速度", "重量", "基础嘲讽等级",
    "损伤抗性", "元素抗性", "异常抗性", "天赋文本", "技能文本", "备注",
]


# ─── 数据源读取 ────────────────────────────────────────────────

def load_source():
    with open(ENEMY_DB, encoding="utf-8") as f:
        database = json.load(f)
    with open(HANDBOOK, encoding="utf-8") as f:
        handbook = json.load(f)
    return database, handbook


def read_source_version():
    """从数据源仓库的最新提交信息里取游戏版本与数据时间。取不到不算失败。"""
    info = {"提交": None, "游戏版本": None, "数据时间": None, "提交标题": None}
    try:
        out = subprocess.run(
            ["git", "-C", str(paths.DATA_SOURCE), "log", "-1", "--format=%H%n%s"],
            capture_output=True, text=True, timeout=30,
        )
        if out.returncode == 0:
            lines = out.stdout.strip().splitlines()
            if lines:
                info["提交"] = lines[0][:7]
            if len(lines) > 1:
                info["提交标题"] = lines[1]
                m_client = re.search(r'Client:([0-9.]+)', lines[1])
                m_data = re.search(r'Data:([0-9A-Za-z\-]+)', lines[1])
                if m_client:
                    info["游戏版本"] = m_client.group(1)
                if m_data:
                    info["数据时间"] = m_data.group(1)
    except Exception:
        pass
    return info


# ─── 取值辅助（与 db_builder/build_enemies.py 同一套语义）─────────

def mv(obj, default=None):
    """解包 m_defined/m_value，始终返回 m_value"""
    if obj is None:
        return default
    if isinstance(obj, dict) and "m_defined" in obj:
        return obj["m_value"]
    return obj


def is_defined(obj):
    if obj is None:
        return False
    if isinstance(obj, dict) and "m_defined" in obj:
        return obj["m_defined"]
    return True


def val(ed, lv0_ed, key, default=None):
    """取 enemyData 直接字段，level>0 时 m_defined=false 从 lv0 继承"""
    obj = ed.get(key)
    if not is_defined(obj) and lv0_ed:
        return mv(lv0_ed.get(key), default)
    return mv(obj, default)


def aval(ed, lv0_ed, key, default=None):
    """取 attributes 子字段，继承规则同 val"""
    attrs = ed.get("attributes", {})
    obj = attrs.get(key)
    if not is_defined(obj) and lv0_ed:
        return mv(lv0_ed.get("attributes", {}).get(key), default)
    return mv(obj, default)


def pick_hb(hb_data, enemy_id, field, default=None):
    return hb_data.get(enemy_id.lower(), {}).get(field, default)


# ─── 重算全部目标行 ────────────────────────────────────────────

def compute_rows():
    """按与构建脚本一致的规则，从本地数据源算出全部目标行。

    返回 {(敌人ID, 级别): {列名: 值}}。
    """
    database, handbook = load_source()
    race_map = {k: v.get("raceName") for k, v in handbook.get("raceData", {}).items()}
    hb_data = handbook.get("enemyData", {})

    # 名称映射（level 0 的清洗后名称），供 `关联敌人` 转中文名
    name_map = {}
    for entry in database["enemies"]:
        for v in entry["Value"]:
            if v["level"] == 0:
                nm = (clean_desc(mv(v["enemyData"].get("name"))) or "").strip()
                name_map[entry["Key"].lower()] = nm or None
                break

    rows = {}
    for entry in database["enemies"]:
        eid = entry["Key"]
        lv0_ed = None
        for v in entry["Value"]:
            if v["level"] == 0:
                lv0_ed = v["enemyData"]
                break

        # 关联敌人：同一敌人所有 level 行相同（与 build_enemies_link.py 的 UPDATE 效果一致）
        link_ids = pick_hb(hb_data, eid, "linkEnemies", [])
        links = json.dumps(
            [name_map.get(lid.lower()) or lid for lid in link_ids], ensure_ascii=False
        ) if link_ids else None

        for variant in entry["Value"]:
            level = variant["level"]
            ed = variant["enemyData"]

            name = (clean_desc(val(ed, lv0_ed, "name")) or "").strip() or None
            desc = clean_desc(val(ed, lv0_ed, "description"))

            # 地位：优先游戏数据 levelType（未定义时 val 已回退 level 0），再回退图鉴
            raw_level = val(ed, lv0_ed, "levelType") or pick_hb(hb_data, eid, "enemyLevel")
            status = STATUS_MAP.get(raw_level, raw_level)

            raw_abilities = pick_hb(hb_data, eid, "abilityList", [])
            abilities = json.dumps(
                [clean_desc(a.get("text")) for a in raw_abilities], ensure_ascii=False
            ) if raw_abilities else None
            dmg_types = pick_hb(hb_data, eid, "damageType", [])
            dmg_cn = json.dumps([DAMAGE_MAP.get(t, t) for t in dmg_types], ensure_ascii=False)

            tags = val(ed, lv0_ed, "enemyTags", [])
            tags_cn = json.dumps([race_map.get(t, t) for t in tags], ensure_ascii=False) if tags else None

            atk_raw = val(ed, lv0_ed, "applyWay")
            mot_raw = val(ed, lv0_ed, "motion")

            immunes = []
            for key, cn_name in IMMUNE_MAP.items():
                if aval(ed, lv0_ed, key) is True:
                    immunes.append(cn_name)
            immune_json = json.dumps(immunes, ensure_ascii=False) if immunes else None

            rows[(eid, level)] = {
                "敌人ID": eid,
                "名称": name,
                "级别": level,
                "描述": desc,
                "地位": status,
                "能力列表": abilities,
                "种类": tags_cn,
                "关联敌人": links,
                "攻击方式": ATK_MAP.get(atk_raw, atk_raw),
                "伤害类型": dmg_cn,
                "行动方式": MOTION_MAP.get(mot_raw, mot_raw),
                "攻击半径": val(ed, lv0_ed, "rangeRadius"),
                "目标价值": val(ed, lv0_ed, "lifePointReduce"),
                "不计入敌人总数": 1 if val(ed, lv0_ed, "notCountInTotal") else 0,
                "numOfExtraDrops": val(ed, lv0_ed, "numOfExtraDrops"),
                "viewRadius": val(ed, lv0_ed, "viewRadius"),
                "最大生命值": aval(ed, lv0_ed, "maxHp"),
                "攻击力": aval(ed, lv0_ed, "atk"),
                "防御力": aval(ed, lv0_ed, "def"),
                "法术抗性": aval(ed, lv0_ed, "magicResistance"),
                "移动速度": aval(ed, lv0_ed, "moveSpeed"),
                "攻击速度": aval(ed, lv0_ed, "attackSpeed"),
                "攻击间隔": aval(ed, lv0_ed, "baseAttackTime"),
                "生命自回速度": aval(ed, lv0_ed, "hpRecoveryPerSec"),
                "重量": aval(ed, lv0_ed, "massLevel"),
                "基础嘲讽等级": aval(ed, lv0_ed, "tauntLevel"),
                "损伤抗性": aval(ed, lv0_ed, "epResistance"),
                "元素抗性": aval(ed, lv0_ed, "epDamageResistance"),
                "异常抗性": immune_json,
                "天赋文本": None,   # 由爬取写入
                "技能文本": None,   # 由爬取写入
                "备注": None,       # 人工列，脚本不写
            }
    return rows


# ─── 比较 ────────────────────────────────────────────────────

def _as_json(value):
    if value is None:
        return None
    try:
        return json.loads(value)
    except (TypeError, ValueError):
        return value


def same(col, a, b):
    """按列类型比较库内值与重算值。"""
    if col in JSON_COLS:
        return _as_json(a) == _as_json(b)
    if a is None or b is None:
        return a is None and b is None
    if col in FLOAT_COLS:
        try:
            return abs(float(a) - float(b)) < 1e-9
        except (TypeError, ValueError):
            return str(a) == str(b)
    if col in INT_COLS:
        try:
            return int(a) == int(b)
        except (TypeError, ValueError):
            return str(a) == str(b)
    return str(a).strip() == str(b).strip()


# ─── PRTS 爬取（源自 build_enemies_prts.py）─────────────────────

def fetch_wikitext(name, redirects=None):
    """获取 PRTS 页面原始 wikitext，自动跟随重定向。

    redirects 传入列表时，每次重定向追加 (原名, 目标名)，供调用方报告。
    """
    encoded = urllib.parse.quote(name)
    url = "https://prts.wiki/w/%s?action=raw" % encoded
    req = urllib.request.Request(url, headers={"User-Agent": "PRTS-Terminal/1.0"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        text = resp.read().decode("utf-8")
    m = re.match(r'#(?:REDIRECT|重定向)\s*\[\[([^\]]+)\]\]', text)
    if m:
        target = m.group(1)
        if redirects is not None:
            redirects.append((name, target))
        return fetch_wikitext(target, redirects)
    return text


def parse_template(text, tpl_name):
    """从 wikitext 中提取指定模板的参数（处理嵌套 {{}}）。"""
    results = []
    start_tag = "{{" + tpl_name
    pos = 0
    while True:
        idx = text.find(start_tag, pos)
        if idx == -1:
            break
        depth, i, end = 0, idx, None
        while i < len(text):
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
        content = text[idx + len(start_tag):end - 2]
        params = {}
        parts = re.split(r'\n\|', content)
        first = parts[0].strip()
        if first.startswith("|"):
            first = first[1:]
        if "=" in first:
            k, v = first.split("=", 1)
            params[k.strip()] = v.strip()
        for part in parts[1:]:
            part = part.strip()
            if "=" not in part:
                continue
            k, v = part.split("=", 1)
            params[k.strip()] = v.strip()
        results.append(params)
        pos = end
    return results


def extract_skills(params):
    """从模板参数中提取技能列表。"""
    skills = []
    for key in sorted(params.keys()):
        m = re.match(r'^技能(\d+)$', key)
        if not m:
            continue
        si = m.group(1)
        skills.append({
            "名称": clean_wiki(params.get(key, '')),
            "初始冷却": params.get("技能%s初始" % si, ''),
            "周期冷却": params.get("技能%s消耗" % si, ''),
            "技力消耗": params.get("技能%s技力" % si, '/'),
            "效果": clean_wiki(params.get("技能%s效果" % si, '')),
        })
    return skills


def sentinel_ok(sample_name):
    """结构哨兵：抓一个已知页面，确认关键模板仍在。返回 (ok, 说明)。"""
    try:
        text = fetch_wikitext(sample_name)
    except Exception as e:
        return False, "哨兵页面抓取失败：%s" % e
    if "敌人信息/levelcontent" not in text:
        return False, "哨兵页面 %r 中未见 {{敌人信息/levelcontent}}，疑似 PRTS 结构变化" % sample_name
    return True, "ok"


def crawl_new(names, sample_name):
    """爬取若干敌人名。

    返回 (结果字典, 404 无页面列表, 有页面但无结构列表)。
    真·事故向上抛异常（触发整体中止）：哨兵失败 / 单页重试后仍网络异常 /
    跳过数达阈值时哨兵复核失败 / 无结构占比超过比例上限。
    注意：不能用「连续多少个无结构」当信号——爬取名单按 ID 排序，同类对象（如
    19xxx 段的奇象生物）天然相邻，连续失败只说明“这一批是同一类”。
    个别页面「有页面但无 {{敌人信息/levelcontent}}」属可预见情况——奇象生物等特殊敌人
    在 PRTS 只建了装置页、本无技能与天赋——跳过并记录，不计失败。
    """
    if not names:
        return {}, [], [], []

    ok, why = sentinel_ok(sample_name)
    if not ok:
        raise RuntimeError(why)

    results = {}
    no_page = []
    no_detail = []
    redirects = []
    skipped = 0
    total = len(names)
    start = time.time()
    for i, name in enumerate(names):
        if i % 10 == 0:
            elapsed = time.time() - start
            rate = (i / elapsed) if elapsed > 0 else 0
            eta = (total - i) / rate if rate > 0 else 0
            print("  [%d/%d] %.0fs | ETA %.0fs | %s" % (i, total, elapsed, eta, name))

        wikitext = None
        last_err = None
        for _attempt in range(FETCH_RETRIES + 1):
            try:
                wikitext = fetch_wikitext(name, redirects)
                last_err = None
                break
            except urllib.error.HTTPError as e:
                if e.code == 404:
                    last_err = None
                    break
                last_err = "HTTP %d" % e.code
            except Exception as e:
                last_err = str(e)
            time.sleep(1.0)
        if wikitext is None and last_err is not None:
            raise RuntimeError("PRTS 抓取失败（%s，重试 %d 次）：%s"
                               % (name, FETCH_RETRIES, last_err))

        levels = parse_template(wikitext, "敌人信息/levelcontent") if wikitext else []
        if not levels:
            if wikitext is None:
                no_page.append(name)
            else:
                no_detail.append(name)
            skipped += 1
            print("      · 跳过（无详情结构）: %s" % name)
            # 每累计跳过若干，复核站点结构：哨兵通过 ⇒ 站点健康，是本批页面自身的情况
            if skipped % NO_DETAIL_RECHECK_EVERY == 0:
                ok2, why2 = sentinel_ok(sample_name)
                if not ok2:
                    raise RuntimeError(
                        "站点结构复核失败（已跳过 %d 个）: %s" % (skipped, why2)
                    )
                print("      · 站点结构复核通过（已跳过 %d 个）" % skipped)
            # 比例兜底
            if skipped >= NO_DETAIL_MIN_TOTAL and skipped > (i + 1) * NO_DETAIL_RATIO_LIMIT:
                raise RuntimeError(
                    "无结构页面占比过高（%d/%d），疑似 PRTS 结构变化" % (skipped, i + 1)
                )
            time.sleep(SLEEP_BETWEEN_FETCH)
            continue
        results[name] = levels
        time.sleep(SLEEP_BETWEEN_FETCH)
    return results, no_page, no_detail, redirects


# ─── 更新记录 ────────────────────────────────────────────────

def backup_db(db_path, keep=KEEP_BACKUPS):
    """写库前把权威库备份到 maintenance/backups/，只保留最近 keep 份。"""
    if not os.path.exists(db_path):
        return None
    bak_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "backups")
    os.makedirs(bak_dir, exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    dst = os.path.join(bak_dir, "%s.%s.bak" % (os.path.basename(db_path), stamp))
    with open(db_path, "rb") as src, open(dst, "wb") as out:
        while True:
            chunk = src.read(1024 * 1024)
            if not chunk:
                break
            out.write(chunk)
    backups = sorted(f for f in os.listdir(bak_dir) if f.endswith(".bak"))
    for stale in (backups[:-keep] if len(backups) > keep else []):
        try:
            os.remove(os.path.join(bak_dir, stale))
        except OSError:
            pass
    return dst


def write_record(record):
    out_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "updates")
    os.makedirs(out_dir, exist_ok=True)
    fn = time.strftime("%Y-%m-%d-%H%M%S") + "-%s.json" % TABLE
    path = os.path.join(out_dir, fn)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(record, f, ensure_ascii=False, indent=2)
    return path


# ─── 主流程 ──────────────────────────────────────────────────

def main():
    ap = argparse.ArgumentParser(description="enemies 表更新（校对 + 新增 + 记录）")
    ap.add_argument("--dry-run", action="store_true", help="只报告，不写库、不爬取、不写记录")
    ap.add_argument("--no-backup", action="store_true", help="跳过写库前的自动备份")
    ap.add_argument("--db", default=str(paths.DB_PATH), help="目标数据库（默认 paths.DB_PATH）")
    args = ap.parse_args()

    paths.ensure_gamedata()
    if not os.path.exists(args.db):
        raise SystemExit("数据库不存在：%s" % args.db)

    started = datetime.now()
    print("====== enemies 表更新 ======")
    print("数据库  : %s" % args.db)
    print("数据源  : %s" % paths.DATA_SOURCE)
    src_ver = read_source_version()
    print("源版本  : 提交 %s | 游戏版本 %s | 数据时间 %s"
          % (src_ver["提交"], src_ver["游戏版本"], src_ver["数据时间"]))

    record = {
        "表": TABLE,
        "运行时间": started.strftime("%Y-%m-%d %H:%M:%S"),
        "数据源": {
            "仓库": "Kengxxiao/ArknightsGameData",
            "提交": src_ver["提交"],
            "提交标题": src_ver["提交标题"],
            "游戏版本": src_ver["游戏版本"],
            "数据时间": src_ver["数据时间"],
        },
        "脚本": {"文件": "update_enemies.py", "版本": SCRIPT_VERSION},
        "数据库": os.path.basename(args.db),
        "校对": {"检查行": 0, "豁免行": 0, "差异行": 0, "差异列数": 0, "明细": [], "计数": {}},
        "新增": {"敌人": 0, "行": 0, "豁免爬取": 0, "明细": []},
        "爬取": {"目标敌人": 0, "成功": 0, "无页面": 0, "无详情结构": 0,
                 "重定向": [], "无页面明细": [], "无详情明细": []},
        "结果": "未执行",
    }

    print("\n------ 重算目标行 ------")
    rows = compute_rows()
    print("  目标行数: %d" % len(rows))

    conn = sqlite3.connect(args.db)
    conn.isolation_level = None  # 手动管理事务
    try:
        db_cols = [d[1] for d in conn.execute("PRAGMA table_info(%s)" % TABLE)]
        cur = conn.execute("SELECT %s FROM %s" % (", ".join(db_cols), TABLE))
        existing = {}
        for r in cur.fetchall():
            d = dict(zip(db_cols, r))
            existing[(d["敌人ID"], d["级别"])] = d

        # ── 1. 校对 ──
        diffs = []
        per_col = {}
        excluded_rows = 0
        for n, (key, want) in enumerate(rows.items(), 1):
            if n % 500 == 0 or n == len(rows):
                print("  比对 %d/%d" % (n, len(rows)))
            have = existing.get(key)
            if have is None:
                continue
            if key[0].startswith(EXCLUDE_FROM_CHECK_AND_CRAWL):
                excluded_rows += 1
                continue
            for col in db_cols:
                if col in SKIP_COLS:
                    continue
                if not same(col, have[col], want[col]):
                    diffs.append((key, col, have[col], want[col]))
                    per_col[col] = per_col.get(col, 0) + 1
        record["校对"]["检查行"] = len(existing)
        record["校对"]["豁免行"] = excluded_rows
        record["校对"]["差异行"] = len({(k[0], k[1]) for k, _, _, _ in diffs})
        record["校对"]["差异列数"] = len(diffs)
        record["校对"]["计数"] = per_col
        record["校对"]["明细"] = [
            {"键": "%s|%s" % (k[0], k[1]), "列": col, "旧": _brief(old), "新": _brief(new)}
            for k, col, old, new in diffs[:MAX_DETAIL]
        ]

        print("\n------ 1. 校对结果 ------")
        print("  库内行数: %d | 豁免: %d | 差异行: %d | 差异列: %d"
              % (len(existing), excluded_rows, record["校对"]["差异行"], len(diffs)))
        for col, n in sorted(per_col.items(), key=lambda x: -x[1]):
            print("    %s: %d 处" % (col, n))

        # ── 2. 新增 ──
        new_keys = sorted(k for k in rows if k not in existing)
        new_ids = sorted({k[0] for k in new_keys})
        record["新增"]["行"] = len(new_keys)
        record["新增"]["敌人"] = len(new_ids)
        record["新增"]["明细"] = [
            {"键": "%s|%s" % (k[0], k[1]), "名称": rows[k]["名称"]} for k in new_keys[:MAX_DETAIL]
        ]
        print("\n------ 2. 新增 ------")
        print("  新增敌人: %d | 新增行: %d" % (len(new_ids), len(new_keys)))
        for k in new_keys[:20]:
            print("    %s|%s  %s" % (k[0], k[1], rows[k]["名称"] or ""))

        if args.dry_run:
            print("\n(干跑：未写库、未爬取、未写记录)")
            record["结果"] = "干跑"
            return 0

        # ── 3. 爬取（先爬后写；失败则整体中止）──
        crawl_results, no_page, no_detail, redirects = {}, [], [], []
        if new_ids:
            # 每个新敌人取 level 0 名称，按名称去重
            names = []
            seen = set()
            skipped_excluded = 0
            for eid in new_ids:
                if eid.startswith(EXCLUDE_FROM_CHECK_AND_CRAWL):
                    skipped_excluded += 1
                    continue
                nm = rows[(eid, 0)]["名称"] if (eid, 0) in rows else None
                if not nm:
                    for k in new_keys:
                        if k[0] == eid and rows[k]["名称"]:
                            nm = rows[k]["名称"]
                            break
                if nm and nm not in seen:
                    seen.add(nm)
                    names.append(nm)
            record["新增"]["豁免爬取"] = skipped_excluded
            record["爬取"]["目标敌人"] = len(names)
            if skipped_excluded:
                print("  豁免爬取（%s 段）: %d 个敌人"
                      % (", ".join(EXCLUDE_FROM_CHECK_AND_CRAWL), skipped_excluded))
            print("\n------ 3. 爬取 PRTS（%d 个新敌人名）------" % len(names))
            sample = rows[(new_ids[0], 0)]["名称"] if (new_ids[0], 0) in rows else names[0]
            try:
                crawl_results, no_page, no_detail, redirects = crawl_new(names, sample)
            except Exception as e:
                record["结果"] = "中止（%s）" % e
                path = write_record(record)
                print("\n!! 中止：%s" % e)
                print("   未对数据库做任何修改；记录：%s" % path)
                return 1
            record["爬取"]["成功"] = len(crawl_results)
            record["爬取"]["无页面"] = len(no_page)
            record["爬取"]["无页面明细"] = no_page[:MAX_DETAIL]
            record["爬取"]["无详情结构"] = len(no_detail)
            record["爬取"]["无详情明细"] = no_detail[:MAX_DETAIL]
            record["爬取"]["重定向"] = [
                "%s → %s" % (a, b) for a, b in redirects[:MAX_DETAIL]
            ]
            if redirects:
                print("  重定向 %d 次（已记入更新记录）：" % len(redirects))
                for a, b in redirects[:5]:
                    print("      %s → %s" % (a, b))

        # ── 写库（单事务）──
        print("\n------ 4. 写库（单事务）------")
        if args.no_backup:
            print("  (已跳过备份)")
        else:
            bak = backup_db(args.db)
            print("  备份: %s" % (bak or "（未生成）"))
        print("  写回校对差异: %d 列 | 插入新增: %d 行" % (len(diffs), len(new_keys)))
        conn.execute("BEGIN IMMEDIATE")
        try:
            for (eid, level), col, old, new in diffs:
                conn.execute(
                    "UPDATE %s SET %s=? WHERE 敌人ID=? AND 级别=?" % (TABLE, col),
                    (new, eid, level),
                )

            for (eid, level) in new_keys:
                want = dict(rows[(eid, level)])
                name = want["名称"]
                levels = crawl_results.get(name)
                if levels:
                    # 该 level 的爬取文本（level 索引超出时用 level 0 合并结果）
                    picked = None
                    for lv in levels:
                        if int(lv.get("index", "0")) == level:
                            picked = lv
                            break
                    if picked is None and levels:
                        merged = dict(levels[0])
                        merged.update(picked or {})
                        picked = merged
                    if picked is not None:
                        skills = extract_skills(picked)
                        want["技能文本"] = json.dumps(skills, ensure_ascii=False) if skills else None
                        want["天赋文本"] = clean_wiki(picked.get("天赋", "")) or None

                conn.execute(
                    "INSERT OR IGNORE INTO %s (%s) VALUES (%s)"
                    % (TABLE, ", ".join(ALL_COLS), ", ".join("?" * len(ALL_COLS))),
                    tuple(want[c] for c in ALL_COLS),
                )
            conn.execute("COMMIT")
        except Exception as e:
            conn.execute("ROLLBACK")
            record["结果"] = "中止（写库失败：%s）" % e
            path = write_record(record)
            print("\n!! 写库失败并已回滚：%s" % e)
            print("   记录：%s" % path)
            return 1
    finally:
        conn.close()

    record["结果"] = "成功"
    path = write_record(record)

    elapsed = (datetime.now() - started).total_seconds()
    print("\n====== 完成（%.1fs）======" % elapsed)
    print("  校对写回: %d 列 | 新增: %d 行（%d 个敌人）| 爬取成功: %d | 无页面: %d | 无详情结构: %d"
          % (record["校对"]["差异列数"], record["新增"]["行"], record["新增"]["敌人"],
             record["爬取"]["成功"], record["爬取"]["无页面"], record["爬取"]["无详情结构"]))
    print("  更新记录: %s" % path)
    return 0


def _brief(value, limit=120):
    if value is None:
        return None
    s = str(value)
    return s[:limit] + ("…" if len(s) > limit else "")


if __name__ == "__main__":
    sys.exit(main())
