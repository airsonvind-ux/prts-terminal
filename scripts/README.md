# scripts — 手工验证与开发辅助

这些脚本不是自动化测试，不进入 CI。它们用于驱动 Rust 查询工具的 `show_output`、做真机验证或做轨迹模拟。

## manual — 查询工具人工验证

`manual/test_*.py` 通过环境变量传入参数，驱动对应的 `cargo test ...::tests::show_output`，用于查看真实输出。

| 文件 | 对应工具 | 用法 |
|---|---|---|
| `manual/test_level.py` | query_level | `python manual/test_level.py <levelId> [fields]` |
| `manual/test_op.py` | query_operator | 编辑脚本内 cases 列表 |
| `manual/test_rogue.py` | query_rogue | `python manual/test_rogue.py <theme> <name>` |
| `manual/test_token.py` | query_token | 编辑脚本内 cases 列表 |
| `manual/test_trap.py` | query_trap | 编辑脚本内 cases 列表 |
| `manual/test_enemy.py` | query_enemy | 编辑脚本内 cases 列表 |
| `manual/test_stage.py` | query_stage | 编辑脚本内 cases 列表 |
| `manual/test_sim.py` | sim_path | `python manual/test_sim.py <levelId> <routeIndex>` |

脚本根据自身位置解析仓库根，不依赖开发机绝对路径。

## sim — 轨迹模拟

| 文件 | 用途 |
|---|---|
| `sim/sim_all.py` | 四车（R1/R2/R3/R8）逐帧轨迹模拟：寻路、避障、停驻滑行、阻挡注入 |
| `sim/sim_tool.py` | 基于关卡与路线的通用寻路模拟 |
| `sim/level_map.py` | 关卡地图加载与模型构建 |
| `sim/path_finder.py` | 寻路地图生成（SPFA + 平整化） |
| `sim/plot_sim.py` | 轨迹交互图 |
| `sim/plot_log.py` | 从模拟 log 绘制轨迹 |

运行环境：

- 数据库：`src-tauri/resources/arknights.db`
- 依赖：numpy；`plot_sim.py` / `plot_log.py` 另需 matplotlib

## dev — 开发辅助

`dev/` 放一次性的数据探索或环境探测脚本，不作为产品入口。

## 约定

- 手工脚本从仓库相对路径解析数据，不写死开发机路径
- 需要真机、真实游戏窗口或真实更新源的验证，结果记录在对应任务文档，不由 CI 代替
- 可自动化的行为测试应放进 Rust/TypeScript 测试，而不是留在这里
