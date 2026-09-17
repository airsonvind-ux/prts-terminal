"""
blackboard.py 测试脚本
覆盖：5 种格式、四舍六入五成双、标签剥离、多占位符、未使用 blackboard 检测
"""
import json, os, sys

# 确保能 import blackboard
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from blackboard import replace, format_value

pass_count = 0
fail_count = 0

def check(case_name, desc, blackboard, expected, expected_unused=None):
    global pass_count, fail_count
    result, unused = replace(desc, blackboard)
    ok = (result == expected)
    if expected_unused is not None:
        ok = ok and (sorted(unused) == sorted(expected_unused))
    if ok:
        pass_count += 1
        print(f'  ✅ {case_name}')
    else:
        fail_count += 1
        print(f'  ❌ {case_name}')
        print(f'     输入:    {desc}')
        print(f'     bb:      {blackboard}')
        print(f'     期望:    {expected}')
        print(f'     实际:    {result}')
        if expected_unused is not None:
            print(f'     期望未用: {expected_unused}')
            print(f'     实际未用: {unused}')

# ========================================================
# 一、格式单元测试
# ========================================================
print('====== 一、格式单元测试 ======')

# ❶ :0%
check('0% 整数',         '{atk:0%}',     {'atk': 0.1},     '10%')
check('0% 百分数',       '{atk:0%}',     {'atk': 0.15},    '15%')
check('0% 四舍六入五成双', '{atk:0%}',   {'atk': 0.155},   '16%')   # 5 后面有 5，进→16
check('0% 五成双 偶',    '{atk:0%}',     {'atk': 0.025},   '2%')    # 2 是偶数，舍
check('0% 五成双 奇',    '{atk:0%}',     {'atk': 0.035},   '4%')    # 3是奇数，4是偶数，进

# ❻ 负号前缀（仅剥离前缀用于 key 匹配，数值原样格式化）
check('-前缀 :0%',       '{-atk:0%}',    {'atk': -0.15},    '-15%')
check('-前缀 无格式整数', '{-cnt}',      {'cnt': -3.0},     '-3')
check('-前缀 无格式小数', '{-cnt}',      {'cnt': -2.5},     '-2.5')
check('-前缀 :0.0',      '{-val:0.0}',   {'val': -1.5},     '-1.5')

# ❷ :0.0%
check('0.0% 一位小数',   '{hp:0.0%}',    {'hp': 0.025},    '2.5%')
check('0.0% 整数边界',   '{hp:0.0%}',    {'hp': 0.15},     '15.0%')

# ❸ :0
check(':0 整数',         '{t:0}',        {'t': 6.0},       '6')
check(':0 有小数',       '{t:0}',        {'t': 3.7},       '4')
check(':0 五成双',       '{t:0}',        {'t': 2.5},       '2')     # 2 是偶数

# ❹ :0.0
check(':0.0 整数',       '{v:0.0}',      {'v': 1.5},       '1.5')
check(':0.0 五成双',     '{v:0.0}',      {'v': 1.55},      '1.6')   # Python round(1.55,1) ... 这个有浮点精度问题

# ❺ 无格式
check('无格式 整数',     '{cost}',       {'cost': 6.0},     '6')
check('无格式 小数',     '{cost}',       {'cost': 3.5},     '3.5')
check('无格式 真整数',   '{n}',          {'n': 5},          '5')
check('无格式 100整数',  '{atk}',        {'atk': 100.0},    '100')

# ========================================================
# 二、标签 + 占位符组合
# ========================================================
print('\n====== 二、标签剥离 + 占位符组合 ======')

check('vup标签+占位符',
    '攻击力<@ba.vup>+{atk:0%}</>',
    {'atk': 0.1},
    '攻击力+10%')

check('多个占位符+标签',
    '立即获得<@ba.vup>{cost}</>点费用，攻击力<@ba.vup>+{atk:0%}</>',
    {'cost': 6.0, 'atk': 0.2},
    '立即获得6点费用，攻击力+20%')

check('rem标签',
    '<@ba.rem>技能自动开启，持续{cnt:0}秒</>',
    {'cnt': 25.0},
    '技能自动开启，持续25秒')

check('$ba标签（术语链接）',
    '天赋<$ba.sluggish>停顿</>的持续时间提升至{scale:0.0}倍',
    {'scale': 1.5},
    '天赋停顿的持续时间提升至1.5倍')

check('占位符key不存在',
    '造成{atk:0%}的伤害，{missing_key}',
    {'atk': 0.5},
    '造成50%的伤害，{missing_key}')

check('负号前缀+标签',
    '攻击力和防御力<@ba.vup>-{-atk:0%}</>',
    {'atk': -0.2},
    '攻击力和防御力--20%')

# ========================================================
# 三、真实数据样例（来自 skill_table.json）
# ========================================================
print('\n====== 三、真实数据样例 ======')

with open(os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    '..', '..', 'data-source', 'ArknightsGameData-master',
    'zh_CN', 'gamedata', 'excel', 'skill_table.json'
), 'r', encoding='utf-8') as f:
    skt = json.load(f)

# 战车 燃烧榴弹 Lv1
sk = skt['skchr_tachak_1']
lv0 = sk['levels'][0]
real_bb = {b['key']: b['value'] for b in lv0['blackboard']}
result, unused = replace(lv0['description'], real_bb)
print(f'  战车·燃烧榴弹:')
print(f'    原文: {lv0["description"][:120]}...')
print(f'    结果: {result[:120]}...')
print(f'    未用blackboard: {unused}')

# Greyy 静电释放 Lv1
sk = skt['skchr_greyy_2']
lv0 = sk['levels'][0]
real_bb = {b['key']: b['value'] for b in lv0['blackboard']}
result, unused = replace(lv0['description'], real_bb)
print(f'  Greyy·静电释放:')
print(f'    原文: {lv0["description"][:120]}...')
print(f'    结果: {result[:120]}...')
print(f'    未用blackboard: {unused}')

# 铁钳号 Lv1
sk = skt['sktok_ironmn_pile3']
lv0 = sk['levels'][0]
real_bb = {b['key']: b['value'] for b in lv0['blackboard']}
result, unused = replace(lv0['description'], real_bb)
print(f'  铁钳号·原型机:')
print(f'    原文: {lv0["description"][:120]}...')
print(f'    结果: {result[:120]}...')
print(f'    未用blackboard: {unused}')

# ========================================================
# 四、未使用 blackboard 检测
# ========================================================
print('\n====== 四、未使用 blackboard 检测 ======')

check('部分未使用',
    '攻击力+{atk:0%}',
    {'atk': 0.1, 'extra_param': 5.0},
    '攻击力+10%',
    expected_unused=['extra_param'])

check('全部未使用',
    '被动技能，无占位符',
    {'atk': 0.1, 'def': 0.2},
    '被动技能，无占位符',
    expected_unused=['atk', 'def'])

check('全部使用',
    '攻击+{atk}，防御+{def:0}',
    {'atk': 50.0, 'def': 30.0},
    '攻击+50，防御+30',
    expected_unused=[])

# ========================================================
print(f'\n====== 结果: {pass_count} 通过, {fail_count} 失败 ======')
