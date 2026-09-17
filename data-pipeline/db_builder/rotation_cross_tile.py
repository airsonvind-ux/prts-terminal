"""旋转偏移跨格验证：基础位置带 y 偏移时，旋转偏移能否让敌人离开原格
场景：新敌人 B 基础位置在 r=0.707 圆上且 y∈(0,0.5)（自然阻挡、有竖直偏移）
已阻挡敌人 A 在 B 下方（贡献"远离 A"方向，向上推）
验证 B 修正后取整是否跨格
"""
import math

O = (0.0, 0.0)
R = 0.7071067811865476


def dist(a, b):
    return math.hypot(a[0] - b[0], a[1] - b[1])


def norm(v):
    l = math.hypot(v[0], v[1])
    if l == 0:
        return (0.0, 0.0)
    return (v[0] / l, v[1] / l)


def corrected_position(base, stables):
    """旋转偏移修正：遍历所有已阻挡敌人，按距离分档贡献"""
    total = [0.0, 0.0]
    for sp in stables:
        d = dist(base, sp)
        if d < 0.1:
            v = norm((sp[0] - O[0], sp[1] - O[1]))
            c = (v[1], -v[0])
            total[0] += c[0]
            total[1] += c[1]
        elif d < 0.4:
            away = norm((base[0] - sp[0], base[1] - sp[1]))
            amp = (0.4 - d) * 50
            total[0] += away[0] * amp
            total[1] += away[1] * amp
        # >=0.4 被遍历但不贡献
    if total == [0.0, 0.0]:
        return base
    d = norm(total)
    temp = (base[0] + d[0] * 0.2, base[1] + d[1] * 0.2)
    s = R / dist(temp, O)
    return (temp[0] * s, temp[1] * s)


def tile(p):
    return (round(p[0]), round(p[1]))


print("===== 具体场景验证：B 基础位置 (-0.55, 0.45)，A 在其下方 =====")
B = (-0.55, 0.45)
A = (-0.6, 0.1)
print(f"B 初始: ({B[0]}, {B[1]})  距阻挡位 {dist(B, O):.4f}  地块 {tile(B)}")
print(f"A 稳定: ({A[0]}, {A[1]})  距 B: {dist(B, A):.4f}")
p = corrected_position(B, [A])
print(f"B 修正后: ({p[0]:+.4f}, {p[1]:+.4f})  地块 {tile(p)}  →  {'跨格!' if tile(p) != tile(B) else '不跨格'}")
print()

print("===== 扫描：B 在 r=0.707 圆上第二象限，y∈(0,0.5)，A 固定在下方 (-0.707, 0.05) =====")
cross_count = 0
total = 0
examples = []
for iy in range(1, 500):
    y = iy / 1000.0  # 0.001 ~ 0.499
    x = -math.sqrt(R * R - y * y)
    B = (x, y)
    if dist(B, O) > R + 1e-6:
        continue
    total += 1
    A = (-0.7071067811865476, 0.05)
    p = corrected_position(B, [A])
    if tile(p) != tile(B):
        cross_count += 1
        if len(examples) < 5:
            examples.append((B, p, tile(B), tile(p)))

print(f"扫描 {total} 个基础位置，跨格 {cross_count} 个")
print(f"跨格比例: {cross_count / total * 100:.1f}%")
for B, p, t0, t1 in examples:
    print(f"  B=({B[0]:+.3f}, {B[1]:+.3f}) 地块 {t0} → 修正后 ({p[0]:+.3f}, {p[1]:+.3f}) 地块 {t1}")
