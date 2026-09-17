"""多敌人旋转偏移角度趋势：随着阻挡敌人数量增加，新增敌人的偏移角度是否持续增大？
关键判断：自然阻挡时，角度 < 45° 则始终留在原格（跨格需 y>0.5，即 sinθ > 0.5/0.707 → θ>45°）
"""
import math

O = (0.0, 0.0)
R = 0.7071067811865476
BASE = (-R, 0.0)


def dist(a, b):
    return math.hypot(a[0] - b[0], a[1] - b[1])


def norm(v):
    l = math.hypot(v[0], v[1])
    if l == 0:
        return (0.0, 0.0)
    return (v[0] / l, v[1] / l)


def tile(p):
    return (round(p[0]), round(p[1]))


def angle_deg(p):
    return math.degrees(math.atan2(p[1], -p[0]))


def stable_position(stables):
    total = [0.0, 0.0]
    for sp in stables:
        d = dist(BASE, sp)
        if d < 0.1:
            v = norm((sp[0] - O[0], sp[1] - O[1]))
            c = (v[1], -v[0])
            total[0] += c[0]
            total[1] += c[1]
        elif d < 0.4:
            away = norm((BASE[0] - sp[0], BASE[1] - sp[1]))
            amp = (0.4 - d) * 50
            total[0] += away[0] * amp
            total[1] += away[1] * amp
    if total == [0.0, 0.0]:
        return BASE
    d = norm(total)
    temp = (BASE[0] + d[0] * 0.2, BASE[1] + d[1] * 0.2)
    s = R / dist(temp, O)
    return (O[0] + (temp[0] - O[0]) * s, O[1] + (temp[1] - O[1]) * s)


N_MAX = 15
stables = []
print(f"{'敌人#':<5}{'稳定位置':<26}{'角度':>9}{'地块':>10}")
max_angle = 0.0
for i in range(N_MAX):
    p = stable_position(stables)
    stables.append(p)
    a = abs(angle_deg(p))
    max_angle = max(max_angle, a)
    print(f"{i+1:<5}({p[0]:+.4f}, {p[1]:+.4f})   {angle_deg(p):+.2f}°   {tile(p)}")

print(f"\n最大绝对角度: {max_angle:.2f}°  (跨格阈值 45°)")
print(f"最大角度是否超过45°: {'是' if max_angle > 45 else '否'}")
tiles = set(tile(p) for p in stables)
print(f"全部地块集合: {tiles} → {'地块变化!' if len(tiles) > 1 else '地块始终不变'}")
