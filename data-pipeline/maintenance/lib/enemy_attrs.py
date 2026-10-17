# -*- coding: utf-8 -*-
"""敌人属性枚举映射：enemies 与 stages 共用。

IMMUNE_MAP 是「免疫字段 → 中文状态」的**唯一事实来源**：游戏新增异常状态时只改这里。
顺序 = 游戏 attributes 里的字段顺序（追加新状态就追加在末尾，已有条目顺序不变，
避免校对把历史行整列判成差异）。

db_builder/build_enemies.py 里那份是构建期快照，已归档、不再随游戏更新。
"""

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
