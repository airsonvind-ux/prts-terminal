"""
修正 stages 表中敌人列表的覆写属性，补充遗漏的 talentBlackboard / skills

背景：build_stages.py 的 extract_overrides 只递归 dict 类型，导致
      overwrittenData 中的 talentBlackboard / skills（list 类型）被静默丢弃。
      本脚本重读 level JSON 的 enemyDbRefs，补全遗漏字段。

用法：python refine_stage_overrides.py
"""

import json, sqlite3, os, glob

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
DB_PATH = os.path.join(SCRIPT_DIR, "arknights.db")
LEVELS_DIR = r"F:\Obsidian_notes\60.Dev\prts-terminal\data-source\ArknightsGameData-master\zh_CN\gamedata\levels"

ATTR_CN = {
    "name": "名称", "description": "描述", "prefabKey": "模型",
    "attributes": "属性", "applyWay": "攻击方式", "motion": "行动方式",
    "enemyTags": "种类标签", "lifePointReduce": "目标价值",
    "levelType": "地位", "rangeRadius": "攻击半径",
    "numOfExtraDrops": "额外掉落数", "viewRadius": "视野半径",
    "notCountInTotal": "不计入总数",
    "maxHp": "最大生命值", "atk": "攻击力", "def": "防御力",
    "magicResistance": "法术抗性", "moveSpeed": "移动速度",
    "attackSpeed": "攻击速度", "baseAttackTime": "攻击间隔",
    "hpRecoveryPerSec": "生命自回速度", "spRecoveryPerSec": "SP自回速度",
    "massLevel": "重量", "tauntLevel": "基础嘲讽等级",
    "epDamageResistance": "元素抗性", "epResistance": "损伤抗性",
    "stunImmune": "晕眩抗性", "silenceImmune": "沉默抗性",
    "sleepImmune": "沉睡抗性", "frozenImmune": "冻结抗性",
    "levitateImmune": "浮空抗性",
    "talentBlackboard": "天赋覆写", "skills": "技能覆写",
}


def mv(obj, default=None):
    if obj is None: return default
    if isinstance(obj, dict) and "m_defined" in obj:
        return obj["m_value"]
    return obj


def is_defined(obj):
    if obj is None: return False
    if isinstance(obj, dict) and "m_defined" in obj:
        return obj["m_defined"]
    return True


def extract_overrides(od):
    """递归提取 overwrittenData 中 m_defined=true 的叶节点（含 list 字段）"""
    result = {}
    if not isinstance(od, dict):
        return result
    if "m_defined" in od:
        if od["m_defined"]:
            return {"__leaf__": od.get("m_value")}
        return {}
    for key, val in od.items():
        if isinstance(val, dict):
            sub = extract_overrides(val)
            cn_key = ATTR_CN.get(key, key)
            if "__leaf__" in sub:
                result[cn_key] = sub["__leaf__"]
            else:
                for sk, sv in sub.items():
                    if sk != "__leaf__":
                        result[sk] = sv
        elif isinstance(val, list) and len(val) > 0:
            cn_key = ATTR_CN.get(key, key)
            result[cn_key] = val
    return result


def build_enemy_list(enemy_db_refs, name_map):
    if not enemy_db_refs: return []
    out = []
    for ref in enemy_db_refs:
        eid = ref["id"]
        item = {
            "名称": name_map.get(eid, eid),
            "级别": ref.get("level", 0),
        }
        overrides = extract_overrides(ref.get("overwrittenData"))
        if overrides:
            item["覆写属性"] = overrides
        out.append(item)
    return out


def main():
    print("=" * 70)
    print("  关卡敌人覆写属性修正")
    print("=" * 70)

    # 1. 加载敌人名称表
    conn = sqlite3.connect(DB_PATH)
    name_map = {}
    for eid, ename in conn.execute("SELECT 敌人ID, 名称 FROM enemies"):
        name_map[eid] = ename or eid

    # 2. 建立 levelId → stakes 的映射
    stage_map = {}
    for sid, lid in conn.execute("SELECT 关卡ID, levelId FROM stages WHERE levelId IS NOT NULL"):
        if lid not in stage_map:
            stage_map[lid] = []
        stage_map[lid].append(sid)

    # 3. 遍历 level JSON 文件
    level_files = glob.glob(os.path.join(LEVELS_DIR, "**", "level_*.json"), recursive=True)
    print(f"  找到 {len(level_files)} 个 level 文件")

    updated_stages = 0
    updated_enemies = 0
    samples = []

    for fpath in level_files:
        lid = fpath.rsplit("\\", 1)[-1].replace(".json", "")
        target_sids = stage_map.get(lid)
        if not target_sids:
            continue

        with open(fpath, "r", encoding="utf-8") as f:
            lv = json.load(f)

        refs = lv.get("enemyDbRefs", [])
        if not refs:
            continue

        # 提取完整的敌人列表（含 new extract_overrides）
        new_list = build_enemy_list(refs, name_map)
        if not new_list:
            continue

        # 对每个 stage ID，检查当前敌人列表是否有遗漏
        for sid in target_sids:
            curr_json = conn.execute("SELECT 敌人列表 FROM stages WHERE 关卡ID = ?", (sid,)).fetchone()
            if not curr_json or not curr_json[0]:
                continue
            try:
                curr_list = json.loads(curr_json[0])
            except json.JSONDecodeError:
                continue

            # 按名称+级别匹配，比较覆写属性
            curr_by_key = {}
            for e in curr_list:
                curr_by_key[(e.get("名称"), e.get("级别"))] = e

            modified = False
            for new_e in new_list:
                key = (new_e.get("名称"), new_e.get("级别"))
                curr_e = curr_by_key.get(key)
                if not curr_e:
                    continue
                new_ov = new_e.get("覆写属性")
                curr_ov = curr_e.get("覆写属性")
                if not new_ov:
                    continue
                if not curr_ov:
                    # 当前没有覆写属性但实际有 → 补充整个覆写块
                    curr_e["覆写属性"] = new_ov
                    modified = True
                    updated_enemies += 1
                else:
                    # 检查是否有遗漏字段
                    for k, v in new_ov.items():
                        if k not in curr_ov:
                            curr_ov[k] = v
                            modified = True
                            updated_enemies += 1

            if modified:
                conn.execute("UPDATE stages SET 敌人列表 = ? WHERE 关卡ID = ?",
                             (json.dumps(curr_list, ensure_ascii=False), sid))
                updated_stages += 1
                if len(samples) < 10:
                    samples.append((sid, lid))

    conn.commit()

    # 报告
    print(f"  修正关卡: {updated_stages}")
    print(f"  补充覆写: {updated_enemies} 条")
    if samples:
        print(f"\n  修正样本:")
        for sid, lid in samples:
            print(f"    {sid} ({lid})")

    conn.close()
    print(f"\n  完成")


if __name__ == "__main__":
    main()
