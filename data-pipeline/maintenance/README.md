# PRTS 终端 · 数据更新系统

把游戏数据与 PRTS Wiki 更新成应用运行时读到的数据：库表更新进 `db/arknights.db`（唯一权威库），
最后由 `tools/push.py` 推送到 `src-tauri/resources/`；五张 JSON 表由各自脚本直接写进资源目录。

## 一、两类脚本

数据维护统一在游戏更新后进行。每张表按更新方式选一种脚本：

| 类型 | 文件名 | 做什么 |
|---|---|---|
| **update** | `update.py` | 增量：本地核对 → 产出待办 → 只爬新增条目，已有内容不动 |
| **rebuild** | `rebuild.py` | 整表重算：从数据源重新生成全表，与现有内容逐项比对，一致就不写 |

update 分两步，可以分开跑，也可以不带参数一次跑完：

1. **base**：只读本地数据源，核对库内已有行、发现新增行，把新增行登记成待办（`pending.json`）。
   这一步不联网。表没有变化时就到此为止，不写库、不写记录。
2. **crawl**：只处理待办里的条目，抓对应页面、补齐内容、写库并落记录。**已经处理过的条目不会重复爬。**

因为 crawl 只认待办，页面内容日后发生变化不会被回填，于是有了第三种运行方式：

- **recheck**：同一张表全量重爬，只比对爬取列，用于"页面更新了、库该跟上"。不产生待办。

## 二、数据源

数据源由每个脚本自己声明，写在文件头的「数据源」一节。常见四类：

| 类别 | 例子 |
|---|---|
| ArknightsGameData 的 excel 表 | `character_table`、`stage_table`、`range_table`、`enemy_handbook_table` |
| ArknightsGameData 的关卡文件 | `levels/obt/**`、`levels/activities/**` |
| PRTS 页面 | 敌人页、装置页、术语页、肉鸽图鉴页；`slang` 走全站重定向 |
| 人工源 | `sources/` 下的页面映射与模组特性（只读，脚本不改） |

同一张表常常是多源合并：例如 `stages` 以 `stage_table` 为主源，还要读集成战略、生息演算、
危机合约、悖论模拟、训练场等特殊表；`enemies` 的图鉴字段在本地，天赋与技能在页面。

## 三、脚本之间的依赖

部分表的数据互相引用，顺序错了不会报错，但内容会对不上：

```
enemies  ──▶ stages          关卡的敌人列表会覆写敌人属性基准
enemies  ──▶ enemy_name_map  友方形态判定需要敌人名称集合
operators ──▶ tokens         召唤物持有者从干员的「关联token」反查
```

因此一次更新的推荐顺序：

```
enemies → stages → enemy_name_map（按需）→ operators → tokens → traps
```

`levels`、`ranges`、`tile_info`、`term`、`abnormal_effects`、`slang` 与其他表没有依赖，
先后顺序不限；`rogue_6` 自成一套，单独处理。

## 四、一次完整更新的流程

### 1. 同步数据源

数据源是浅克隆，位于 `data-pipeline/data-source/ArknightsGameData`：

```powershell
cd data-pipeline\data-source\ArknightsGameData
git fetch --depth 1 origin master
git reset --hard FETCH_HEAD
git log -1 --format="%h %cs"
```

> **git 不读 Windows 系统代理。** 浏览器能打开 GitHub，git 仍可能卡二十秒后报
> `Failed to connect to github.com port 443`。给 git 单独配一份：
>
> ```powershell
> git config --global http.proxy http://127.0.0.1:7890
> git config --global https.proxy http://127.0.0.1:7890
> ```
>
> 端口按代理软件实际值填。也可以用 TUN / 虚拟网卡模式。验证：
> `git -c http.proxy=http://127.0.0.1:7890 ls-remote <仓库地址> HEAD`

### 2. 预分析

在各表目录下执行 `python update.py --dry-run`：只做本地比对，不联网、不写库、不写记录，
用来看这次有没有新增行、有没有和本地源对不上的列。

### 3. 按顺序跑 update

进入对应表目录，执行 `python update.py`（不带参数 = base + crawl）。参数与输出说明在各脚本
文件头。每张表跑完会打印新增行数、待办补齐情况、写回列数，并把结果落到更新记录。

### 4. 重建类表

`levels`、`enemy_name_map`、`ranges`、`tile_info`、`term`、`abnormal_effects` 用
`python rebuild.py`。它们整表重算，与现有内容逐项比对，一致时不写库、不写记录。

### 5. 推送

```powershell
cd data-pipeline\maintenance\tools
python push.py --dry-run   # 先看两边行数
python push.py
```

`push.py` 只推数据库种子。五张 JSON 表由各自脚本直接写进 `src-tauri/resources/`，不需要推送。

## 五、运行后会留下什么

- **待办**：`tables/<表>/pending.json`。只有三种状态——`未同步`（新增待爬）、`缺失`（页面不存在）、
  `部分`（页面在但内容不全）。补齐即删除。页面滞后于版本是常态，待办会留着，等页面出来再跑一次
  `python update.py --crawl` 即可。
- **记录**：`updates/<表>/<时间戳>.json`，**有变化或失败才写**。内含源版本（提交 / 游戏版本 /
  数据时间）、校对差异明细、新增明细、爬取统计、待办变化，以及「待人工核对」条目。
- **备份**：写库前自动备份到 `backups/`，同一文件名只保留 3 份。

## 六、注意事项

1. **备份只留 3 份**，连续跑多张表会把最早那份挤掉。要保住某个状态，先手动另存。
2. **预测试用副本**：`python update.py --db <副本路径>` 时，待办、记录、备份都跟着副本走，
   不碰仓库状态。直接跑默认库就是真更新。
3. **人工源缺失时脚本不运行**（如 `sources/模组特性信息.md`），不降级写半套数据。
4. **结构哨兵**：脚本开跑前先抓一个已知页面确认模板还在，改版会立刻中止，避免把空内容写进库。
5. **不要手改库**：人工修正改库后跑 `push.py`。更新脚本只填空、不覆盖人工内容，但 `--recheck`
   会按各表策略改写爬取列。
6. **抓取会变慢**：单页超过 5 秒会打一行 `! 慢 <页名>：12.4s`，整页失败会带 raw / api 两路原因。
   站点偶尔限速，看提示就知道卡在哪一页。

## 七、维护

- 执行器契约见 `lib/table_base.py` 顶部；新表照现有目录写 `update.py` 或 `rebuild.py`，
  给出 `SPEC`，先 `--dry-run` 与旧结果对齐。
- 通用能力只在第二张表确实需要时才往 `lib/` 提。
- `data-pipeline/db_builder/` 是早期建表与修复用的一次性脚本，已从当前仓库移除；需要参考时查 Git 历史，不作为维护入口。
- 改含中文的源码用编辑器或 `edit` 工具；PowerShell 的 `Get-Content`/`Set-Content` 与重定向
  默认走本地代码页，会把 UTF-8 中文写坏。
