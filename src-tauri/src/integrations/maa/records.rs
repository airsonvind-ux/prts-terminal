//! 一次运行的记录件：识别帧归并 → 落盘 → 清理，另留一份「最近一次」的留痕。
//!
//! 落盘挂在**帧到达**这一侧（桥客户端的读线程），不挂在「有人在等」上。
//! 派发即返回（`wait=0`）的任务同样要留下结果文件——否则那条路的结果永远不会出现。
//!
//! 归并只看**结构**，不看**种类**：
//! - `details` 里带 `done` 的是**快照型**（`DepotInfo` / `OperBoxInfo`），逐帧递增，
//!   只留最新一条，它就是终帧；
//! - 不带 `done` 的是**事件型**（公招 / 基建 / 刷图掉落 …），每条都可能有信息，全部保留。
//!
//! 于是 MAA 冒出没见过的种类时，它自己会落进合适的桶，这里不需要补表。
//! （`OperBoxInfo` 的 `all_opers[].own` 是过程量，扫到才翻——只留终帧这件事，
//! 正是为了避免拿过程量当结论。）

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// 一次队列跑完的回调码（`AllTasksCompleted`），实测为 3。
pub const CODE_ALL_TASKS_COMPLETED: i64 = 3;

/// 每个类型保留的历史份数（另有 `latest-<类型>` 指针，永不清理）。
pub const KEEP_PER_KIND: usize = 10;

/// 结果目录总量上限，超了按时间从旧删。
pub const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;

/// 结果目录：`<data_dir>/maa/results`。
pub fn results_dir() -> PathBuf {
    crate::config::data_dir().join("maa").join("results")
}

/// 「最近一次」指针路径：`latest-<类型>.jsonl`（该类型每次运行都覆盖它）。
pub fn pointer_path(kind: &str) -> PathBuf {
    results_dir().join(format!("latest-{}.jsonl", sanitize(kind)))
}

/// 在给定目录里找写入时刻最新的 `latest-<类型>.jsonl`。
/// 返回 `(识别种类, 路径, 写入时刻, 行数, 任务类型)`——任务类型取自文件里第一帧的
/// `taskchain`（老文件可能没有）；只看文件本身，**看不出“那次算不算跑完”**。
pub fn latest_pointer_in(
    dir: &Path,
) -> Option<(String, PathBuf, SystemTime, usize, Option<String>)> {
    let mut best: Option<(String, PathBuf, SystemTime)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(kind) = name
            .strip_prefix("latest-")
            .and_then(|rest| rest.strip_suffix(".jsonl"))
        else {
            continue;
        };
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let at = meta.modified().unwrap_or(UNIX_EPOCH);
        if best.as_ref().is_none_or(|(_, _, cur)| at > *cur) {
            best = Some((kind.to_string(), entry.path(), at));
        }
    }
    let (kind, path, at) = best?;
    let body = std::fs::read_to_string(&path).unwrap_or_default();
    let lines = body.lines().count();
    // 第一帧的 `taskchain` 就是那次跑的任务类型；给不出就 None（调用方退回识别种类）。
    let chain = body
        .lines()
        .next()
        .and_then(|first| serde_json::from_str::<Value>(first).ok())
        .and_then(|v| {
            v.get("taskchain")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    Some((kind, path, at, lines, chain))
}

/// 磁盘上最新的「最近一次」指针：桥没笔记时用它兜底。
pub fn latest_pointer_on_disk() -> Option<(String, PathBuf, SystemTime, usize, Option<String>)> {
    latest_pointer_in(&results_dir())
}

/// 把时刻换算成毫秒（JSON 里能存的形状）。
pub fn ms_of(at: SystemTime) -> u128 {
    at.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 一个类型攒下的帧。
#[derive(Default)]
struct KindBuf {
    /// 出现过 `done` → 快照型。
    snapshot: bool,
    /// 见过的帧数（快照型远多于留下的）。
    seen: usize,
    /// 这一份属于哪条任务链（帧里的 `taskchain`）。
    chain: Option<String>,
    frames: Vec<Value>,
}

impl KindBuf {
    fn observe(&mut self, frame: &Value) {
        self.seen += 1;
        if self.chain.is_none() {
            if let Some(chain) = frame.get("taskchain").and_then(Value::as_str) {
                self.chain = Some(chain.to_string());
            }
        }
        let has_done = frame
            .get("details")
            .and_then(|d| d.get("done"))
            .and_then(Value::as_bool)
            .is_some();
        if has_done {
            self.snapshot = true;
        }
        if self.snapshot {
            // 快照型：后来的覆盖先前的，终帧就是完整结果。
            self.frames.clear();
        }
        self.frames.push(frame.clone());
    }
}

/// 收尾时写下的一个文件。
#[derive(Clone, Debug)]
pub struct Kept {
    pub kind: String,
    /// 这一份属于哪条任务链（= 任务类型，如 `Infrast`）。
    /// 取自帧里的 `taskchain`；磁盘上的老记录可能没有。
    pub chain: Option<String>,
    /// 历史文件（带时间戳）；同内容的指针是 `latest-<kind>.jsonl`。
    pub path: PathBuf,
    /// 写进文件的帧数（快照型通常是 1）。
    pub frames: usize,
    /// 见过的帧数。
    pub seen: usize,
}

/// 一次运行攒下的全部识别帧。
#[derive(Default)]
pub struct RunRecord {
    started_at_ms: Option<u128>,
    ended_at_ms: Option<u128>,
    task_ids: Vec<i64>,
    kinds: BTreeMap<String, KindBuf>,
    kept: Vec<Kept>,
}

impl RunRecord {
    /// 记下一次运行的开始。
    pub fn begin(&mut self, task_ids: Vec<i64>) {
        self.started_at_ms = Some(now_ms());
        self.ended_at_ms = None;
        self.task_ids = task_ids;
        self.kinds.clear();
        self.kept.clear();
    }

    pub fn task_ids(&self) -> &[i64] {
        &self.task_ids
    }

    /// 补齐任务 id（`task_add` 之后才知道）。
    pub fn set_task_ids(&mut self, task_ids: Vec<i64>) {
        self.task_ids = task_ids;
    }

    pub fn started_at_ms(&self) -> Option<u128> {
        self.started_at_ms
    }

    /// 本次运行是否还没收尾（用来防「完成信号没收到」）。
    pub fn in_flight(&self) -> bool {
        self.started_at_ms.is_some() && self.ended_at_ms.is_none()
    }

    pub fn ended_at_ms(&self) -> Option<u128> {
        self.ended_at_ms
    }

    /// 上次收尾写下的文件。
    pub fn kept(&self) -> &[Kept] {
        &self.kept
    }

    /// 某个类型留下的帧数（测试用）。
    pub fn kind_frames(&self, kind: &str) -> usize {
        self.kinds.get(kind).map(|b| b.frames.len()).unwrap_or(0)
    }

    /// 吃进一帧识别结果。
    pub fn observe(&mut self, frame: &Value) {
        let kind = frame
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("result")
            .to_string();
        self.kinds.entry(kind).or_default().observe(frame);
    }

    /// 收尾：每个类型写 `<时间>-<类型>.jsonl`（一行一帧）与 `latest-<类型>.jsonl`，
    /// 然后按上限清理。没有帧时只记结束时刻。
    pub fn finish(&mut self, dir: &Path) -> Result<(), String> {
        self.ended_at_ms = Some(now_ms());
        if self.kinds.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("建结果目录失败：{e}"))?;
        let stamp = now_ms();
        let mut kept = Vec::new();
        for (kind, buf) in &self.kinds {
            let safe = sanitize(kind);
            let path = dir.join(format!("{stamp}-{safe}.jsonl"));
            let pointer = dir.join(format!("latest-{safe}.jsonl"));
            let body = buf
                .frames
                .iter()
                .filter_map(|f| serde_json::to_string(f).ok())
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(&path, &body).map_err(|e| format!("写识别结果失败：{e}"))?;
            std::fs::write(&pointer, &body).map_err(|e| format!("写结果指针失败：{e}"))?;
            kept.push(Kept {
                kind: kind.clone(),
                chain: buf.chain.clone(),
                path,
                frames: buf.frames.len(),
                seen: buf.seen,
            });
        }
        cleanup(dir);
        self.kept = kept;
        Ok(())
    }
}

/// 「最近一次」的留痕：给状态查询用——PRTS 不靠记，问一下就有。
#[derive(Clone, Debug, Default)]
pub struct LastRun {
    pub started_at_ms: Option<u128>,
    pub ended_at_ms: Option<u128>,
    pub task_ids: Vec<i64>,
    pub files: Vec<Kept>,
    /// 见过过 `AllTasksCompleted`。
    pub completed: bool,
    /// 落盘失败等原因，如实留痕。
    pub error: Option<String>,
    /// 这份留痕是从磁盘上的指针文件推出来的（桥的笔记丢了），
    /// 所以“完成与否”未知。
    pub from_disk: bool,
}

/// 按上限清理：每类型只留最近 [`KEEP_PER_KIND`] 份，总量不超 [`MAX_TOTAL_BYTES`]。
/// `latest-` 指针永不参与。
fn cleanup(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(PathBuf, SystemTime, u64, String)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("latest-") || !name.ends_with(".jsonl") {
            continue;
        }
        let Some((_, rest)) = name.split_once('-') else {
            continue;
        };
        let kind = rest.trim_end_matches(".jsonl").to_string();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let at = meta.modified().unwrap_or(UNIX_EPOCH);
        files.push((entry.path(), at, meta.len(), kind));
    }

    // 新 → 旧；每类超过 KEEP_PER_KIND 的直接删。
    files.sort_by(|a, b| b.1.cmp(&a.1));
    let mut per_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut kept: Vec<(PathBuf, u64)> = Vec::new();
    let mut total = 0u64;
    for (path, _, len, kind) in files {
        let count = per_kind.entry(kind).or_insert(0);
        *count += 1;
        if *count > KEEP_PER_KIND {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        total += len;
        kept.push((path, len));
    }

    // 总量上限：`kept` 是新→旧，从尾部（最旧）删起。
    if total > MAX_TOTAL_BYTES {
        for (path, len) in kept.iter().rev() {
            if total <= MAX_TOTAL_BYTES {
                break;
            }
            if std::fs::remove_file(path).is_ok() {
                total = total.saturating_sub(*len);
            }
        }
    }
}

/// 文件名安全化：只留字母数字、`_`、`-`。
fn sanitize(kind: &str) -> String {
    let cleaned: String = kind
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "result".to_string()
    } else {
        cleaned
    }
}

pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snapshot_kinds_keep_only_the_last_frame() {
        let mut rec = RunRecord::default();
        rec.begin(vec![2]);
        for own in [14usize, 24, 429] {
            rec.observe(&json!({
                "event": "recognition",
                "kind": "OperBoxInfo",
                "details": { "done": own == 429, "own_opers": vec![0; own] }
            }));
        }
        let buf = &rec.kinds["OperBoxInfo"];
        assert_eq!(buf.frames.len(), 1, "快照型只留终帧");
        assert_eq!(buf.frames[0]["details"]["done"], json!(true));
        assert_eq!(buf.seen, 3, "见过的帧数照记");
    }

    #[test]
    fn event_kinds_keep_every_frame() {
        let mut rec = RunRecord::default();
        rec.begin(vec![1]);
        for facility in ["Mfg", "Trade", "Power"] {
            rec.observe(&json!({
                "event": "recognition",
                "kind": "EnterFacility",
                "details": { "facility": facility, "index": 0 }
            }));
        }
        assert_eq!(rec.kinds["EnterFacility"].frames.len(), 3, "事件型全留");
    }

    #[test]
    fn finish_writes_one_line_per_frame_plus_pointer() {
        let dir = std::env::temp_dir().join(format!("prts-records-{}", now_ms()));
        let mut rec = RunRecord::default();
        rec.begin(vec![1]);
        for _ in 0..2 {
            rec.observe(&json!({
                "event": "recognition",
                "kind": "StageDrops",
                "details": { "stars": 3 }
            }));
        }
        rec.finish(&dir).unwrap();

        let pointer = dir.join("latest-StageDrops.jsonl");
        let body = std::fs::read_to_string(&pointer).unwrap();
        assert_eq!(body.lines().count(), 2, "事件型一行一帧");
        assert_eq!(rec.kept().len(), 1);
        assert_eq!(rec.kept()[0].kind, "StageDrops");
        assert!(rec.kept()[0].path.exists(), "历史文件也写了");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_keeps_pointer_and_caps_history() {
        let dir = std::env::temp_dir().join(format!("prts-cleanup-{}", now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..KEEP_PER_KIND + 5 {
            std::fs::write(
                dir.join(format!("{}-OperBoxInfo.jsonl", 1_000_000 + i)),
                "x",
            )
            .unwrap();
        }
        std::fs::write(dir.join("latest-OperBoxInfo.jsonl"), "pointer").unwrap();

        cleanup(&dir);

        let history = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| !e.file_name().to_string_lossy().starts_with("latest-"))
            .count();
        assert_eq!(history, KEEP_PER_KIND, "每类型只留最近 N 份");
        assert!(
            dir.join("latest-OperBoxInfo.jsonl").exists(),
            "指针不参与清理"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn latest_pointer_picks_the_newest_file() {
        let dir = std::env::temp_dir().join(format!("prts-pointer-{}", now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("latest-StageDrops.jsonl"), "a\nb").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("latest-DepotInfo.jsonl"), "x").unwrap();
        // 历史文件与其它文件不参与。
        std::fs::write(dir.join("1791278903185-StageDrops.jsonl"), "a").unwrap();

        let (kind, path, _, lines, chain) = latest_pointer_in(&dir).expect("应当找到指针");
        assert_eq!(kind, "DepotInfo", "取最新写入的那个");
        assert_eq!(lines, 1);
        assert!(path.ends_with("latest-DepotInfo.jsonl"));
        assert_eq!(chain, None, "没有 taskchain 的老文件给 None");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `Kept.chain` 就是那条任务链的名字，落盘后仍能从文件里读回来
    /// （「最近一次」要报的是任务类型，不是 MAA 的节点名）。
    #[test]
    fn kept_carries_the_task_chain_on_disk_too() {
        let dir = std::env::temp_dir().join(format!("prts-chain-{}", now_ms()));
        let mut rec = RunRecord::default();
        rec.begin(vec![2]);
        rec.observe(&json!({
            "event": "recognition",
            "kind": "EnterFacility",
            "taskchain": "Infrast",
            "details": { "facility": "Mfg", "index": 0 }
        }));
        rec.finish(&dir).unwrap();
        assert_eq!(rec.kept()[0].chain.as_deref(), Some("Infrast"));

        let (_, _, _, _, chain) = latest_pointer_in(&dir).unwrap();
        assert_eq!(chain.as_deref(), Some("Infrast"), "磁盘上也读得出来");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_are_sanitized() {
        assert_eq!(sanitize("OperBoxInfo"), "OperBoxInfo");
        assert_eq!(sanitize("a/b:c"), "a_b_c");
        assert_eq!(sanitize(""), "result");
    }
}
