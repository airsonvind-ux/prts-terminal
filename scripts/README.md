# scripts — 机制模拟与查询工具测试脚本

## 查询工具测试脚本

`test_*.py` 为查询工具交互测试脚本：通过环境变量传入查询参数，驱动 Rust 侧 `cargo test query_xxx::tests::show_output` 输出真实查询结果，用于验收各查询工具的输出格式与内容。

| 文件 | 对应工具 | 用法 |
|---|---|---|
| `test_level.py` | query_level | `python test_level.py <levelId> [fields]` |
| `test_op.py` | query_operator | `python test_op.py`（编辑脚本内 cases 列表） |
| `test_rogue.py` | query_rogue | `python test_rogue.py <theme> <name>` |
| `test_token.py` | query_token | 编辑脚本内 cases 列表 |
| `test_trap.py` | query_trap | 编辑脚本内 cases 列表 |

用例列表在脚本内以注释形式维护，按需取消注释。脚本内 `proj` 常量指向 `src-tauri` 目录（绝对路径），移动脚本不破坏运行。

---

# level_rogue5_t-10（为崖作伥）轨迹模拟脚本

核心运行脚本，供 PRTS 终端后续工具开发参考。验证过程结论已体现在代码中。

## 文件说明

| 文件 | 用途 |
|---|---|
| `sim_all.py` | 四车（R1/R2/R3/R8）逐帧轨迹模拟器：寻路、避障、停驻滑行、阻挡注入 |
| `plot_sim.py` | 轨迹交互图：悬停显示相对帧 / 周期帧位 / 坐标；支持保存静态图 |
| `level_map.py` | 关卡地图加载与模型构建（sim_all / plot_sim 依赖） |
| `path_finder.py` | 寻路地图生成（SPFA + 平整化，sim_all 依赖） |
| `test_*.py` | 查询工具交互测试脚本（见上方小节） |

## 运行环境

- 数据库：`src-tauri/resources/arknights.db`（`sim_all.py` 内 DB 常量指定路径）
- 依赖：numpy、matplotlib（仅 plot_sim 需要）

## sim_all.py

### 命令行

```bash
python sim_all.py
```

输出四车段长、触发帧、回程 x4 帧与坑余数（x 距 4.0 的偏差，≤ ~0.0001 判定可坑入坑）。

### 作为模块调用

```python
import sim_all as s

# 无阻挡模拟
frames, cp_done = s.simulate_car(3)

# 阻挡注入：reroute = {frame, pos, freeze_until}
#   frame:      偏移生效帧（全局帧）——位置修正到 pos，该帧 nav 照常计数
#   freeze_until: 冻结段结束帧（frame < f < freeze_until 位置不变、不计数）
# 例：R3 自然阻挡（进入圈帧 3758 正常移动，冻结段 3759~4470）
frames, cp_done = s.simulate_car(3, reroute={
    "frame": 3759, "pos": (2.305408, 0.988175), "freeze_until": 4471})
```

返回：`frames` 为逐帧记录列表（`frame` 全局帧、`entity` 坐标、`given` 给定方向、`vel` 速度、`avoid` 避障力、`cp` 检查点索引）；`cp_done` 为各检查点完成帧。

### 机制要点（已确认结论）

- 避障三帧周期按**寻路帧计数**（nav）判定：周期第 0 帧 = 出生后第 K 个寻路帧；K 为每车参数——**R1/R2 K=2、R3/R8 K=1**（`K_MECH`）
- 出生帧的全局位置只决定路径全局偏移，不影响周期相位
- 冻结段不计数：阻挡期间周期完全冻结，解除后从冻结值继续——**不产生任何相位偏移**，只有坐标改变（阻挡偏移）才改变后续路径
- 停驻（WAIT）= MOVE 状态、给定方向 0、惯性滑行，周期照常
- 阻挡偏移帧：位置修正、nav 照常计数（避障帧 = 该帧无阻挡时的相位）

## plot_sim.py

```bash
python plot_sim.py                # 交互图（悬停查看帧位信息）
python plot_sim.py --save x.png   # 保存静态图
python plot_sim.py --routes 1,3   # 只画指定路线
```

悬停显示：相对帧（出生后第几帧）、周期帧位（0 = 重算帧）、坐标。

## 帧号约定

- 全局帧：模拟循环帧号（出生前的帧不入轨迹）
- 相对帧 = 全局帧 − 出生帧
- 周期帧位 = (nav − K) % 3，0 = 重算帧（避障力计算帧）

## r1、r3的偏移帧
r1: 1930 (4,1)
r3: 4381 (9,5)