# scripts — 专用测试入口与手工验证

这些脚本大多不是 CI 自动化测试；`data-contract.ps1` 是本地数据契约套件的专用入口，`grounding-eval.ps1` 是录制会话接地评测的入口。其余脚本用于驱动 Rust 查询工具的 `show_output`、做真机验证或做轨迹模拟。

## 数据推送与契约

| 文件 | 用途 |
|---|---|
| `push-data.ps1` | 日常数据推送：自动读取数据源提交和当天 `dataVersion`，依次推送数据库、跑数据契约、回写清单 |
| `data-contract.ps1` | 检查 `src-tauri/resources/arknights.db`，打印大小与 SHA-256，再以 `data-contract` feature 运行真实数据契约测试 |
| `grounding-eval.ps1` | 对一份 PRTS 会话 JSONL 运行离线接地评测；详见 `evals/grounding/README.md` |

默认 `cargo test --lib` 会跳过这些用例；缺少数据库时本脚本会明确报错。

## 发布暂存与验签

| 文件 | 用途 |
|---|---|
| `prepare-release.mjs` | 从已构建安装包生成 `latest.json`、`SHA256SUMS.txt` 和发布暂存目录；先验签，不上传、不签名 |
| `verify-update-signature.mjs` | 用 `tauri.conf.json` 的 `plugins.updater.pubkey` 验证安装包的 minisign `.sig`，并检查版本/文件名 |
| `prepare-data-release.mjs` | 校验数据库与 `DATA_MANIFEST.json` 一致，生成数据 Release 目录和 `SHA256SUMS.txt`；不上传 |
| `fetch-data-release.mjs` | 从 `data-<version>` GitHub Release 下载数据库和 manifest，校验 SHA-256、版本和契约后写入资源目录 |
| `publish-release.mjs` | 把发布暂存目录上传到 GitHub Release 和 Gitee Release；支持 `--dry-run` |

示例：

```powershell
npm run verify:signature -- --installer <安装包> --signature <安装包>.sig --config src-tauri/tauri.conf.json --version 0.1.0
npm run release:manifest -- --installer <安装包> --gitee-repo <owner/repo> --github-repo <owner/repo>
node scripts/prepare-data-release.mjs --db src-tauri/resources/arknights.db --manifest src-tauri/resources/DATA_MANIFEST.json --data-version 2026.10.25
node scripts/publish-data-release.mjs --repo <owner/repo> --data-version 2026.10.25 --dir release-staging/data-2026.10.25 --dry-run
node scripts/fetch-data-release.mjs --repo <owner/repo> --data-version 2026.10.25 --out-dir src-tauri/resources
node scripts/publish-release.mjs --staging-dir release-staging/0.1.0 --github-repo <owner/repo> --gitee-repo <owner/repo> --dry-run
```

输出目录默认是 `release-staging/<version>/`，已在 `.gitignore` 中排除。发布仓 URL 必须显式传入，脚本不硬编码账号。

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
