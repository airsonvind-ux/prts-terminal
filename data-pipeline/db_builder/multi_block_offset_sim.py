"""多敌人旋转偏移（自然阻挡 0.707）：稳定阻挡位置 + 地块归属判断
场景：多个敌人从 -x 方向水平靠近阻挡位 O=(0,0)，y 处于格子中心
只可能是自然阻挡：基础位置均为 (-0.707, 0)，在半径为 0.707 的圆（外阻挡圈）上运动
"""
import math

O = (0.0, 0.0)
R = 0.7071067811865476  # 外阻挡圈半径 = 格子对角线一半
BASE = (-R, 0.0)


def dist(a, b):
    return math.hypot(a[0] - b[0], a[1] - b[1])


def norm(v):
    l = math.hypot(v[0], v[1])
    if l == 0:
        return (0.0, 0.0)
    return (v[0] / l, v[1] / l)


def tile(p):
    """四舍六入五成双取整"""
    return (round(p[0]), round(p[1]))


def angle_deg(p):
    """相对 -x 方向的偏移角度（0=正左，+为向上偏，-为向下偏）"""
    return math.degrees(math.atan2(p[1], -p[0]))


def stable_position(stables):
    """计算新来敌人（基础位置 BASE）在多敌人修正后的稳定位置"""
    # 1. 对每个其他被阻挡敌人贡献修正向量
    total = [0.0, 0.0]
    for sp in stables:
        d = dist(BASE, sp)
        if d < 0.1:
            # 从阻挡位指向该敌人，顺时针旋转90°后的单位方向
            v = norm((sp[0] - O[0], sp[1] - O[1]))
            c = (v[1], -v[0])
            total[0] += c[0]
            total[1] += c[1]
        elif d < 0.4:
            # 远离该敌人的方向，幅度 (0.4-d)*50
            away = norm((BASE[0] - sp[0], BASE[1] - sp[1]))
            amp = (0.4 - d) * 50
            total[0] += away[0] * amp
            total[1] += away[1] * amp
        # >= 0.4 不贡献
    # 2. 无修正则保持基础位置
    if total == [0.0, 0.0]:
        return BASE
    # 3. 修正方向 = 总向量方向，长度固定 0.2
    d = norm(total)
    temp = (BASE[0] + d[0] * 0.2, BASE[1] + d[1] * 0.2)
    # 4. 拉回与基础位置相同的距阻挡位距离（保持距离，仅改变方向）
    s = R / dist(temp, O)
    return (O[0] + (temp[0] - O[0]) * s, O[1] + (temp[1] - O[1]) * s)


for n in (2, 3, 4):
    print(f"========== {n} 个敌人 ==========")
    stables = []
    for i in range(n):
        p = stable_position(stables)
        stables.append(p)
        t = tile(p)
        print(
            f"  敌人{i+1}: 稳定位置 ({p[0]:+.4f}, {p[1]:+.4f})  "
            f"角度 {angle_deg(p):+.2f}°  距阻挡位 {dist(p, O):.4f}  地块 {t}"
        )
    tiles = set(tile(p) for p in stables)
    print(f"  地块集合 {tiles} → {'地块变化!' if len(tiles) > 1 else '地块不变'}")
    print()
