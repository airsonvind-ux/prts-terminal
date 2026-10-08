---
name: MAA 任务
description: 通过 MAA 在本机《明日方舟》中执行游戏操作时的任务类型、参数与用法。需要刷关、公招、基建、领奖、肉鸽、抄作业、识别等实际执行时激活；只在对话里讨论游戏内容、不要求执行时不激活。
source: builtin
---

# MAA 任务

用 `run_maa_task` 把任务交给本机 MAA。各类型下只列常用参数；完整参数、含义与示例见文末官方文档。

- `tasks` 是数组，按顺序依次执行——单个任务写一项，"清日常"这类连续操作写多项。
- 每项 `{ "type": 任务类型, "params": 参数对象 }`；`params` 省略即用 MAA 默认值。
- `client_type`（客户端服）由工具按设置补上。
- `wait` 是最多等多少秒：省略时按任务类型取默认——OperBox 180、Depot 60、StartUp 90、CloseDown 与 SwitchTheme 60、Award 60；其余类型为 `0`。
- 长任务（Fight / Infrast / Roguelike / Copilot 等）默认 `0`：派发式——发出去就返回，任务在后台跑，不阻塞当前对话；要进度用 `status`，跑完后 `status` 的“最近一次”就是它的结果。
- **队列的默认是各项的合成：任一项默认 `0`，整条就是 `0`；否则为各项之和。**
- 返回给这次做了什么、耗时多少；回执里的次数是实际发生的量。识别结果另存一份 JSONL（`%APPDATA%\com.tauri-app.prts-terminal\maa\results\`）：带 `done` 的只留终帧，其余一帧一行（一行一个 JSON 对象）。

## 任务类型

### 启动与关闭

- **StartUp** —— 开始唤醒：把游戏唤到主界面。
  参数：`start_game_enabled`（是否自动启动客户端，默认否）。

- **CloseDown** —— 关闭游戏。

### 日常

- **Fight** —— 理智作战（刷关）。
  `stage` 关卡名，省略则打上次/当前那一关（如 `1-7`、`CE-6`）；剿灭写 `Annihilation`（当期剿灭）；复刻活动写 `SSReopen-<关卡前缀>`（如 `SSReopen-IC`，一次刷完该活动的 XX-1 ~ XX-9 关）。
  刷到理智耗尽：不填 `times`（不限次数）+ `series: 0`（按当前理智自动取最大代理倍率）。`times` 是战斗次数，连战倍率算在其内（`series: 6, times: 6` 即一次六连战）；`series` 默认 1，`0` 取最大、`-1` 不切换、`1~10` 指定。

- **Recruit** —— 公开招募。每次执行会先领取已完成的招募，返回这次的招募次数与高星情况。`select`、`confirm` 必填，都填星级数组，判断的是最优组合的星级：星级在 `select` 里才选择标签，在 `confirm` 里才点确认（真正招募）；如 `select: [5, 4]`、`confirm: [4, 3]`，即 3★ 不点标签直接招、4★ 点亮标签再招、5★/6★ 不招。
  `refresh` 设 `true` 时，只出 3★ 的槽会自动刷新，直到出现高星组合或刷新次数用完；`times` 是招募次数，默认 0。
  例：`{ "type": "Recruit", "params": { "select": [5, 4], "confirm": [4, 3], "times": 4, "refresh": true } }`

- **Infrast** —— 基建换班。`facility` 必填，填要换班的设施名：`Mfg` / `Trade` / `Power` / `Control` / `Reception` / `Office` / `Dorm` / `Processing` / `Training` / `AssistantChange`；`drones` 可指定无人机用途（默认不用）。换班用 MAA 的默认方式；要自定义排班，见文末的基建排班文档。
  例：`{ "type": "Infrast", "params": { "facility": ["Mfg", "Trade", "Power", "Control", "Reception", "Office", "Dorm", "Processing"], "drones": "Money" } }`

- **Mall** —— 领取信用及商店购物。买法分三遍：先按 `buy_first` 买一遍，再从列表左到右跳过 `blacklist` 买一遍，信用溢出时再无视黑名单买一遍直到不溢出。`buy_first`、`blacklist` 填商品名（如 `"招聘许可"`、`"家具零件"`）；`shopping` 设 `false` 只领信用不买。返回给完成与耗时。

- **Award** —— 领取奖励。默认只领每日/每周任务奖励；另有额外领取途径（邮件、限定池每日免费单抽、幸运墙、限时开采等）。返回给执行了哪几项。

- **SwitchTheme** —— 更换游戏主界面主题。`themes` 必填，填与游戏内一致的主题名，多个则每次随机选一个。

### 集成战略（肉鸽）

- **Roguelike** —— 集成战略（肉鸽）：从开局选分队/干员开始一层层打，按 `mode` 决定刷什么，到停止条件收工。`theme` 选主题（`Phantom` 傀影 / `Mizuki` 水月 / `Sami` 萨米 / `Sarkaz` 萨卡兹 / `JieGarden` 界园 / `BlackFlow` 黑流树海）。
  最常用的是存钱：`mode: 1` 刷源石锭，第一层投资完就退出；另一常用 `mode: 0` 刷分与奖励点数。`squad` 分队、`roles` 职业组、`core_char_list` 开局干员（中文名，最多三个）、`difficulty` 难度、`starts_count` 局数、`stop_at_max_level` 等级刷满即停。
  例：`{ "type": "Roguelike", "params": { "theme": "Sami", "mode": 1, "starts_count": 10 } }`

### 抄作业

- **抄作业（Copilot / SSSCopilot / ParadoxCopilot）** —— 照一份作业 JSON 自动打一关：`Copilot` 普通关卡、`SSSCopilot` 保全派驻、`ParadoxCopilot` 悖论模拟。三者都要给作业文件的绝对路径：`SSSCopilot` 用 `filename`；`Copilot` 用 `filename` 或 `copilot_list`；`ParadoxCopilot` 用 `filename` 或 `list`。
  `Copilot` 另有 `loop_times`（同一份打几遍，默认 1，仅单份作业时有效）和 `formation`（默认关：关着用玩家当前编队，开着才由 MAA 按作业自动编队）；`SSSCopilot` 的 `loop_times` 同理。
  例：`{ "type": "Copilot", "params": { "filename": "C:/copilot/1-7.json", "loop_times": 2 } }`

- **VideoRecognition** —— 视频识别：从一段作战视频生成作业 JSON。`filename` 必填，填视频的绝对路径。

### 识别

- **Depot** —— 识别仓库：返回物品的种数。

- **OperBox** —— 识别持有的干员：截图识别干员列表，返回账号持有的干员数。

### 其他

- **Reclamation** —— 生息演算：`Tales` 沙洲遗闻下，`mode: 0` 无存档靠进出关卡刷点、`mode: 1` 有存档靠组装支援道具刷点；`RelaunchAnchor` 重启锚点下，`mode` 换成关卡号——`16` 打 RA-1、`32` 打 RA-15、`48` 打 RA-4。
  例：`{ "type": "Reclamation", "params": { "theme": "Tales", "mode": 0 } }`

- **Custom** —— 自定义任务：按名字执行 MAA 内置的任务或小功能，常见于活动里 MAA 专门提供的小功能（如像素画 `MiniGame@PixelPaint@Begin`、秘密前线、活动商店兑换 `SS@Store@Begin`）。`task_names` 必填，只执行数组里第一个匹配上的名字。
  例：`{ "type": "Custom", "params": { "task_names": ["MiniGame@PixelPaint@Begin"] } }`

- **SingleStep** —— 单步作战：把一场战斗拆成一个个动作逐个下发，`type` 固定 `"copilot"`，`subtype` 取 `stage`（设关卡）/ `start`（开始）/ `action`（执行单个动作）。

## 运行注意

- 本机需已装 MAA、模拟器与《明日方舟》，且模拟器分辨率为 16:9（如 1600×900）；非 16:9 时 MAA 会拒绝连接。
- 服务器（官服 / B 服）由博士在设置里选择，任务按此执行；与账号不符时任务会异常。
- 连接的是**运行中**的模拟器：多台同时在跑（或同一家开了多个实例）时，需在设置页中进行选择。
- 目前仅支持模拟器。PC 端要把鼠标、键盘事件注入游戏窗口——需要管理员权限，受窗口缩放与**真实鼠标位置**影响，功能不稳定。
- 截图增强：MuMu 12 与雷电提供更快的通道，博士在设置页中启用。
- 任务运行期间博士手动操作模拟器会影响识别，可能影响任务结果。
- 如果博士的MAA应用正在执行任务，PRTS的MAA任务会与其相互干扰。
- 执行类任务会长时间接管模拟器；派发之后随时可以查询任务状态或中止任务（清空整条队列），PRTS 退出时桥也自动断开。PRTS 启动后首次调用要先建桥连模拟器。

## 深入

- 任务类型一览：`https://docs.maa.plus/zh-cn/protocol/integration.html`
- 作业 JSON 格式：`https://docs.maa.plus/zh-cn/protocol/copilot-schema.html`
- 保全作业：`https://docs.maa.plus/zh-cn/protocol/sss-schema.html`
- 基建排班：`https://docs.maa.plus/zh-cn/protocol/base-scheduling-schema.html`
- 肉鸽辅助：`https://docs.maa.plus/zh-cn/protocol/integrated-strategy-schema.html`
- 回调（识别结果）：`https://docs.maa.plus/zh-cn/protocol/callback-schema.html`
