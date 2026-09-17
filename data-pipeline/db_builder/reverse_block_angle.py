"""逆向推理验证：已存在被阻挡敌人 E（距 O<=0.707）能否让新敌人旋转偏移角度超过45°？
新敌人基础位置 BASE = (-0.707, 0)。扫描 E 的全部可行坐标，求新敌人稳定位置的最大角度。
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


def angle_deg(p):
    """相对 -x 方向的偏移角度"""
    return math.degrees(math.atan2(p[1], -p[0]))


def new_enemy_angle(E):
    """新敌人（BASE）在已存在敌人 E 影响下的稳定位置角度"""
    d = dist(BASE, E)
    if d >= 0.4:
        return 0.0  # 不贡献，新敌人停留在 BASE
    if d < 0.1:
        # 从 O 指向 E 的单位向量，顺时针旋转90°
        v = norm((E[0] - O[0], E[1] - O[1]))
        c = (v[1], -v[0])
    else:
        # 远离 E 的方向（从 E 指向 BASE），幅度 (0.4-d)*50
        away = norm((BASE[0] - E[0], BASE[1] - E[1]))
        amp = (0.4 - d) * 50
        c = (away[0] * amp, away[1] * amp)
    dn = norm(c)
    temp = (BASE[0] + dn[0] * 0.2, BASE[1] + dn[1] * 0.2)
    s = R / dist(temp, O)  # 拉回半径 0.707
    final = (temp[0] * s, temp[1] * s)
    return abs(angle_deg(final))


# 扫描：E 在半径 0.707 圆内（外阻挡圈内），细网格 0.001
best = (0.0, None)
count = 0
for ix in range(-1415, 1416):
    for iy in range(-1415, 1416):
        E = (ix / 1000.0, iy / 1000.0)
        if dist(E, O) > R + 1e-9:
            continue
        count += 1
        a = new_enemy_angle(E)
        if a > best[0]:
            best = (a, E)

print(f"扫描点数（阻挡圈内）: {count}")
print(f"最大角度: {best[0]:.4f}°  @ E={best[1]}")
print(f"理论上限 asin(0.2/0.707): {math.degrees(math.asin(0.2 / R)):.4f}°")
print(f"45° 阈值能否达到: {'能' if best[0] > 45 else '不能，伪命题成立'}")
