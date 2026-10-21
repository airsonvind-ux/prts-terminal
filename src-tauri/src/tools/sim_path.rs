//! tools::sim_path —— 寻路模拟工具
//!
//! 基于关卡 ID + 路线序号自动完成该路线的逐帧寻路模拟，支持自定义路线
//! （route DSL）、强制位移注入（push/block）、避障相位参数（k）、时间轴
//! 参数（timeAxis）。逻辑与 scripts/sim/sim_tool.py 对齐（float32 逐帧模拟，
//! 已通过为崖作伥四车逐帧对拍验证）。
//!
//! 输出：summary = 格级事件流（相对帧，f0 = 生成帧）；rows = 结构化摘要；
//! 帧级 JSONL log 默认写入工作区 sim_logs/ 目录（workspace_root/sim_logs/，
//! M1 批 2 已迁移；log 参数可覆盖，相对路径基于工作区解析）。

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::json;

use crate::db::{Filter, FilterOp};
use crate::tools::{Tool, ToolContext, ToolDisplay, ToolError, ToolOutput};
use crate::workspace::WorkspaceRoot;

// ═══════════════════════════════════════════════════════
// 常量与基础数据结构
// ═══════════════════════════════════════════════════════

const FPS: f32 = 30.0;
/// 帧时长字面量对齐 sim_all.py 的 F(0.0333333)：与 1f/30f 差 2ulp，
/// 但在 WAIT 20s（600 帧）处跨越完成边界（600×0.0333333=19.99998 残留正），
/// 必须与验证基准一致，否则 R8 停驻差 1 帧导致相位错开
const DELTA: f32 = 0.0333333;
const GOAL_R: f32 = 0.05;
const STEERING: f32 = 8.0;
const MAX_STEER: f32 = 10.0;
const HALF_BODY: f32 = 0.2;
const HAZARD_COST: i64 = 1_000_000;
const EPS: f32 = 1e-6;
/// 阻挡判定半径平方（对齐知识条目《阻挡底层机制》：地面阻挡半径平方 0.49999037，float32）
const BLOCK_R2: f32 = 0.49999037;
/// 内阻挡圈半径平方（0.5²，float32 精确）
const INNER_R2: f32 = 0.25;
/// 关卡全局移速倍率（默认 0.5，理论移速 = 属性移速 × 该值）
const MOVE_MULTIPLIER: f32 = 0.5;

#[derive(Deserialize)]
struct MapData {
    width: i64,
    height: i64,
    grid: Vec<Vec<usize>>,
    tiles: Vec<TileEntry>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct TileEntry(String, i64, i64, i64); // tileKey, heightType, buildableType, passableMask

/// 内部检查点模型（数据库路线与 DSL 统一归一）
#[derive(Clone, Debug, serde::Serialize)]
struct Checkpoint {
    kind: String,          // MOVE / WAIT / WAIT_GLOBAL / WAIT_FRAGMENT / WAIT_WAVE / DISAPPEAR / APPEAR / END / START
    time: f64,
    pos: Option<(i64, i64)>,
    offset: Option<(f32, f32)>,
}

impl Checkpoint {
    fn new(kind: &str) -> Self {
        Checkpoint { kind: kind.to_string(), time: 0.0, pos: None, offset: None }
    }
}

/// 注入事件（push / block）：设定帧仅为触发信号，不改变设定帧状态，下一帧起生效
#[derive(Clone, Debug, serde::Serialize)]
struct Injection {
    kind: String,          // push / block
    f: i64,
    dir: Option<(f32, f32)>, // push 方向（已归一化）
    dist: Option<f32>,      // push 距离
    dur: i64,               // push 失衡帧数 / block 部署帧到撤退帧的总帧数
    at: Option<(i64, i64)>, // block 阻挡位（自动模式）
    pos: Option<(f32, f32)>, // block 偏移后坐标（pos 模式，预计算目标）
}

/// 失衡状态（push 触发后登记，设定帧下一帧起生效）
#[derive(Clone)]
struct PushState {
    dir: (f32, f32),
    dist: f32,
    dur: i64,
    start_f: i64, // 失衡起始帧 = 设定帧 + 1
}

/// 登记的阻挡事件（部署帧到撤退帧的生命周期，期间每帧帧首判定）
struct BlockEvent {
    at: Option<(i64, i64)>,
    pos: Option<(f32, f32)>,
    deploy_f: i64,
    end_frame: i64, // 撤退帧 = 部署帧 + dur
}

/// 当前阻挡归属（偏移中 / 稳定）
#[derive(Clone)]
struct BlockState {
    ev_idx: usize,
    offset_rem: i64, // >0 偏移剩余帧；=0 稳定
    offset_from: (f32, f32),
    offset_to: (f32, f32),
}

// ═══════════════════════════════════════════════════════
// 数值工具（对齐 Python/numpy float32 语义）
// ═══════════════════════════════════════════════════════

/// 银行家舍入（Python round 语义，坐标归属地块：四舍六入五成双）
fn round_half_even(v: f64) -> f64 {
    let f = v.floor();
    let diff = v - f;
    if diff < 0.5 {
        f
    } else if diff > 0.5 {
        f + 1.0
    } else if f % 2.0 == 0.0 {
        f
    } else {
        f + 1.0
    }
}

fn cell32(x: f32, y: f32) -> (i64, i64) {
    (round_half_even(f64::from(x)) as i64, round_half_even(f64::from(y)) as i64)
}

/// clamp 到最大长度（纯 float32，对齐游戏语义 / sim_all.py：np.sqrt 保持 f32）。
fn clamp32(vx: f32, vy: f32, maxlen: f32) -> (f32, f32) {
    let l = (vx * vx + vy * vy).sqrt();
    if l <= maxlen || l < EPS {
        (vx, vy)
    } else {
        let k = maxlen / l;
        (vx * k, vy * k)
    }
}

/// 归一化（纯 float32）。
fn unit32(vx: f32, vy: f32) -> Option<(f32, f32)> {
    let l = (vx * vx + vy * vy).sqrt();
    if l < EPS {
        None
    } else {
        Some((vx / l, vy / l))
    }
}

/// 去除 given 方向分量（纯 float32）。
fn project32(a: (f32, f32), given: Option<(f32, f32)>) -> (f32, f32) {
    let g = match given {
        Some(g) => g,
        None => return a,
    };
    let gu = match unit32(g.0, g.1) {
        Some(u) => u,
        None => return a,
    };
    let proj = a.0 * gu.0 + a.1 * gu.1;
    (a.0 - proj * gu.0, a.1 - proj * gu.1)
}

// ═══════════════════════════════════════════════════════
// 解析：路线 DSL / 数据库路线 / 注入
// ═══════════════════════════════════════════════════════

/// 数据库路线数组 → (起点, 终点, 生成偏移, 检查点列表)
fn parse_db_route(route: &serde_json::Value) -> Result<(Checkpoint, (i64, i64), (f32, f32), Vec<Checkpoint>), String> {
    let motion = route[0].as_i64().unwrap_or(0);
    if motion == 1 {
        return Err("FLY 路线暂不支持（飞行移动未实现）".into());
    }
    if motion == 2 {
        return Err("E_NUM 路线无意义".into());
    }
    let start = (route[1][1].as_i64().unwrap_or(0), route[1][0].as_i64().unwrap_or(0));
    let end = (route[2][1].as_i64().unwrap_or(0), route[2][0].as_i64().unwrap_or(0));
    let spawn_offset = if route.get(4).is_some() {
        (route[4][0].as_f64().unwrap_or(0.0) as f32, route[4][1].as_f64().unwrap_or(0.0) as f32)
    } else {
        (0.0, 0.0)
    };
    let mut cps = Vec::new();
    if let Some(arr) = route.get(5).and_then(|v| v.as_array()) {
        for cp in arr {
            let t = cp[0].as_i64().unwrap_or(0);
            let kind = match t {
                0 => "MOVE",
                1 => "WAIT",
                2 => "WAIT_GLOBAL",
                3 => "WAIT_FRAGMENT",
                4 => "WAIT_WAVE",
                5 => "DISAPPEAR",
                6 => "APPEAR",
                7 => "ALERT",      // 纯演出，跳过
                8 => "MOVE",       // PATROL_MOVE 同 MOVE
                _ => return Err(format!("检查点类型 {} 暂不支持", t)),
            };
            if kind == "ALERT" {
                continue;
            }
            let time = cp[1].as_f64().unwrap_or(0.0);
            let pos = (cp[2][1].as_i64().unwrap_or(0), cp[2][0].as_i64().unwrap_or(0));
            let offset = (cp[3][0].as_f64().unwrap_or(0.0) as f32, cp[3][1].as_f64().unwrap_or(0.0) as f32);
            match kind {
                "MOVE" => cps.push(Checkpoint {
                    kind: kind.into(), time: 0.0, pos: Some(pos), offset: Some(offset) }),
                "WAIT" | "WAIT_GLOBAL" | "WAIT_FRAGMENT" | "WAIT_WAVE" => cps.push(Checkpoint {
                    kind: kind.into(), time, pos: None, offset: None }),
                "DISAPPEAR" => cps.push(Checkpoint::new("DISAPPEAR")),
                "APPEAR" => cps.push(Checkpoint {
                    kind: "APPEAR".into(), time: 0.0, pos: Some(pos), offset: None }),
                _ => {}
            }
        }
    }
    let start_cp = Checkpoint { kind: "START".into(), time: 0.0, pos: Some(start), offset: None };
    Ok((start_cp, end, spawn_offset, cps))
}

/// 自定义路线 DSL → 检查点列表（START/MOVE/WAIT/.../END，分号分隔）
fn parse_dsl(dsl: &str) -> Result<Vec<Checkpoint>, String> {
    let mut out = Vec::new();
    for token in dsl.split(';') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let (kind, args) = match token.find('(') {
            Some(open) if token.ends_with(')') => {
                (&token[..open], &token[open + 1..token.len() - 1])
            }
            _ => return Err(format!("DSL 语法错误：{token}")),
        };
        let parts: Vec<&str> = args.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        match kind {
            "START" | "MOVE" | "PATROL" | "APPEAR" | "END" => {
                let n = if kind == "MOVE" { if parts.len() == 4 { 4 } else { 2 } } else { 2 };
                if parts.len() != n {
                    return Err(format!("{kind} 需要 {} 个参数：{token}", n));
                }
                let pos = (parts[0].parse::<i64>().map_err(|_| format!("坐标解析失败：{token}"))?,
                          parts[1].parse::<i64>().map_err(|_| format!("坐标解析失败：{token}"))?);
                let offset = if parts.len() == 4 {
                    Some((parts[2].parse::<f32>().map_err(|_| format!("偏移解析失败：{token}"))?,
                          parts[3].parse::<f32>().map_err(|_| format!("偏移解析失败：{token}"))?))
                } else {
                    None
                };
                let k = match kind {
                    "PATROL" => "MOVE",
                    _ => kind,
                };
                out.push(Checkpoint { kind: k.to_string(), time: 0.0, pos: Some(pos), offset });
            }
            "WAIT" | "WAIT_GLOBAL" | "WAIT_FRAGMENT" | "WAIT_WAVE" => {
                if parts.len() != 1 {
                    return Err(format!("{kind} 需要 1 个参数：{token}"));
                }
                let time = parts[0].parse::<f64>().map_err(|_| format!("时间解析失败：{token}"))?;
                out.push(Checkpoint { kind: kind.to_string(), time, pos: None, offset: None });
            }
            "DISAPPEAR" => {
                if !parts.is_empty() {
                    return Err(format!("DISAPPEAR 无参数：{token}"));
                }
                out.push(Checkpoint::new("DISAPPEAR"));
            }
            _ => return Err(format!("未知检查点类型：{kind}")),
        }
    }
    Ok(out)
}

fn parse_kv(body: &str) -> HashMap<String, String> {
    let mut kv = HashMap::new();
    // 支持 dir=(a,b) / pos=(a,b) / at=(a,b) 与 dist=1.5 / dur=30 两种形态
    let mut i = 0;
    let chars: Vec<char> = body.chars().collect();
    while i < chars.len() {
        let start = i;
        while i < chars.len() && chars[i] != '=' {
            i += 1;
        }
        let key: String = chars[start..i].iter().collect();
        i += 1; // '='
        let vstart = i;
        if i < chars.len() && chars[i] == '(' {
            while i < chars.len() && chars[i] != ')' {
                i += 1;
            }
            i += 1; // ')'
        } else {
            while i < chars.len() && chars[i] != ',' {
                i += 1;
            }
        }
        let val: String = chars[vstart..i].iter().collect();
        let val = val.trim();
        // 去括号：dir=(a,b) / at=(a,b) / pos=(a,b) 形态
        let val = if val.starts_with('(') && val.ends_with(')') {
            &val[1..val.len() - 1]
        } else {
            val
        };
        kv.insert(key.trim().to_string(), val.to_string());
        if i < chars.len() {
            i += 1; // ','
        }
    }
    kv
}

fn parse_injections(spec: &str) -> Result<Vec<Injection>, String> {
    let mut events = Vec::new();
    for token in spec.split(';') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let at = match token.find('@') {
            Some(a) => a,
            None => return Err(format!("注入语法错误：{token}")),
        };
        let kind = &token[..at];
        let rest = &token[at + 1..];
        let (f_str, body) = match rest.find(':') {
            Some(c) => (&rest[..c], &rest[c + 1..]),
            None => return Err(format!("注入语法错误：{token}")),
        };
        let f = f_str.parse::<i64>().map_err(|_| format!("注入帧号解析失败：{token}"))?;
        let kv = parse_kv(body);
        let get_f32 = |k: &str| -> Result<f32, String> {
            kv.get(k).ok_or_else(|| format!("缺少参数 {k}：{token}"))?.parse::<f32>()
                .map_err(|_| format!("参数 {k} 解析失败：{token}"))
        };
        let get_i64 = |k: &str| -> Result<i64, String> {
            kv.get(k).ok_or_else(|| format!("缺少参数 {k}：{token}"))?.parse::<i64>()
                .map_err(|_| format!("参数 {k} 解析失败：{token}"))
        };
        let get_pair = |k: &str| -> Result<(f32, f32), String> {
            let v = kv.get(k).ok_or_else(|| format!("缺少参数 {k}：{token}"))?;
            let mut it = v.split(',');
            let a = it.next().ok_or_else(|| format!("参数 {k} 解析失败：{token}"))?.trim().parse::<f32>()
                .map_err(|_| format!("参数 {k} 解析失败：{token}"))?;
            let b = it.next().ok_or_else(|| format!("参数 {k} 解析失败：{token}"))?.trim().parse::<f32>()
                .map_err(|_| format!("参数 {k} 解析失败：{token}"))?;
            Ok((a, b))
        };
        match kind {
            "push" => {
                let dir = get_pair("dir")?;
                // 归一化（纯 float32 语义：方向向量归一）
                let l = (dir.0 * dir.0 + dir.1 * dir.1).sqrt();
                if l < 1e-9 {
                    return Err(format!("push 方向为零向量：{token}"));
                }
                events.push(Injection {
                    kind: "push".into(), f,
                    dir: Some((dir.0 / l, dir.1 / l)),
                    dist: Some(get_f32("dist")?),
                    dur: get_i64("dur")?,
                    at: None, pos: None,
                });
            }
            "block" => {
                // dur = 部署帧到撤退帧的总帧数（dur=0 零长度事件，无效果），不得为负
                let dur = match kv.get("dur") {
                    Some(v) => v.parse::<i64>()
                        .map_err(|_| format!("参数 dur 解析失败：{token}"))?,
                    None => 6,
                };
                if dur < 0 {
                    return Err(format!("block dur 不得为负：{token}"));
                }
                let at = kv.get("at").map(|v| {
                    let mut it = v.split(',');
                    let a = it.next().unwrap_or("").trim().parse::<i64>().unwrap_or(0);
                    let b = it.next().unwrap_or("").trim().parse::<i64>().unwrap_or(0);
                    (a, b)
                });
                let pos = if kv.contains_key("pos") { Some(get_pair("pos")?) } else { None };
                if at.is_none() && pos.is_none() {
                    return Err(format!("block 需要 at 或 pos：{token}"));
                }
                if at.is_some() && pos.is_some() {
                    return Err(format!("block 的 at 与 pos 互斥：{token}"));
                }
                events.push(Injection { kind: "block".into(), f, dir: None, dist: None, dur, at, pos });
            }
            _ => return Err(format!("未知注入类型：{kind}")),
        }
    }
    events.sort_by_key(|e| e.f);
    // 同帧校验：同一帧最多一个 push；同一帧多个 block 时 at 不得同格（pos 模式无部署位，不校验）
    let mut i = 0;
    while i < events.len() {
        let mut j = i;
        while j + 1 < events.len() && events[j + 1].f == events[i].f {
            j += 1;
        }
        let pushes = events[i..=j].iter().filter(|e| e.kind == "push").count();
        if pushes > 1 {
            return Err(format!("同帧多个推拉注入：帧 {}", events[i].f));
        }
        for a in i..=j {
            for b in a + 1..=j {
                if let (Some(at1), Some(at2)) = (events[a].at, events[b].at) {
                    if at1 == at2 {
                        return Err(format!("同帧同位置阻挡注入：帧 {} at={:?}", events[i].f, at1));
                    }
                }
            }
        }
        i = j + 1;
    }
    Ok(events)
}

// ═══════════════════════════════════════════════════════
// 寻路层：SPFA + 平整化 + nextNode
// ═══════════════════════════════════════════════════════

type NavMap = HashMap<(i64, i64), (i64, i64)>;

fn spfa(goal: (i64, i64), passable: &[Vec<bool>], obstacles: &std::collections::HashSet<(i64, i64)>, width: i64, height: i64) -> NavMap {
    let mut next_node = NavMap::new();
    let mut dist: HashMap<(i64, i64), i64> = HashMap::new();
    let mut in_queue = std::collections::HashSet::new();
    let mut q = VecDeque::new();
    dist.insert(goal, 0);
    in_queue.insert(goal);
    q.push_back(goal);
    while let Some(g) = q.pop_front() {
        in_queue.remove(&g);
        for nb in [(g.0, g.1 + 1), (g.0 + 1, g.1), (g.0, g.1 - 1), (g.0 - 1, g.1)] {
            if nb.0 < 0 || nb.0 >= width || nb.1 < 0 || nb.1 >= height {
                continue;
            }
            if !passable[nb.1 as usize][nb.0 as usize] {
                continue;
            }
            let cost = if obstacles.contains(&nb) { HAZARD_COST } else { 1 };
            let nd = dist[&g] + cost;
            let better = match dist.get(&nb) {
                Some(&old) => nd < old,
                None => true,
            };
            if better {
                dist.insert(nb, nd);
                next_node.insert(nb, g);
                if !in_queue.contains(&nb) {
                    in_queue.insert(nb);
                    q.push_back(nb);
                }
            }
        }
    }
    next_node
}

fn bresenham_cells(a: (i64, i64), c: (i64, i64)) -> Vec<(i64, i64)> {
    let mut cells = Vec::new();
    let dx = (c.0 - a.0).abs();
    let dy = (c.1 - a.1).abs();
    let sx = if c.0 > a.0 { 1 } else { -1 };
    let sy = if c.1 > a.1 { 1 } else { -1 };
    let mut err = dx - dy;
    let (mut x, mut y) = (a.0, a.1);
    loop {
        cells.push((x, y));
        if x == c.0 && y == c.1 {
            break;
        }
        let e2 = 2 * err;
        if e2 > -dy {
            err -= dy;
            x += sx;
        }
        if e2 < dx {
            err += dx;
            y += sy;
        }
    }
    cells
}

fn check_cells(a: (i64, i64), c: (i64, i64)) -> std::collections::HashSet<(i64, i64)> {
    let mut cells: std::collections::HashSet<(i64, i64)> = bresenham_cells(a, c).into_iter().collect();
    let dx = (c.0 - a.0).abs();
    let dy = (c.1 - a.1).abs();
    let (r0, r1) = (a.1.min(c.1), a.1.max(c.1));
    let (c0, c1) = (a.0.min(c.0), a.0.max(c.0));
    if dy == 1 || dx == 1 {
        for r in r0..=r1 {
            for cc in c0..=c1 {
                cells.insert((cc, r));
            }
        }
    }
    let path = bresenham_cells(a, c);
    for w in path.windows(2) {
        let (px, py) = w[0];
        let (qx, qy) = w[1];
        if px != qx && py != qy {
            cells.insert((qx, py));
            cells.insert((px, qy));
        }
    }
    cells
}

fn is_clear(a: (i64, i64), c: (i64, i64), passable: &[Vec<bool>], obstacles: &std::collections::HashSet<(i64, i64)>, width: i64, height: i64) -> bool {
    for cell in check_cells(a, c) {
        if cell.0 < 0 || cell.0 >= width || cell.1 < 0 || cell.1 >= height {
            return false;
        }
        if !passable[cell.1 as usize][cell.0 as usize] {
            return false;
        }
        if obstacles.contains(&cell) {
            return false;
        }
    }
    true
}

fn flatten(goal: (i64, i64), passable: &[Vec<bool>], obstacles: &std::collections::HashSet<(i64, i64)>, width: i64, height: i64, next_node: &mut NavMap) {
    let mut cells: Vec<(i64, i64)> = next_node.keys().copied().filter(|c| *c != goal).collect();
    cells.sort_by_key(|c| (c.1, c.0));
    for a in cells {
        loop {
            let b = match next_node.get(&a) {
                Some(&b) => b,
                None => break,
            };
            let c = match next_node.get(&b) {
                Some(&c) => c,
                None => break,
            };
            if is_clear(a, c, passable, obstacles, width, height) {
                next_node.insert(a, c);
            } else {
                break;
            }
        }
    }
}

fn build_nav_map(model: &MapModel, goal: (i64, i64), obstacles: &std::collections::HashSet<(i64, i64)>) -> NavMap {
    let mut nn = spfa(goal, &model.passable, obstacles, model.width, model.height);
    flatten(goal, &model.passable, obstacles, model.width, model.height, &mut nn);
    nn
}

// ═══════════════════════════════════════════════════════
// 模拟层
// ═══════════════════════════════════════════════════════

struct MapModel {
    width: i64,
    height: i64,
    passable: Vec<Vec<bool>>,
    tile_key: Vec<Vec<String>>,
}

struct SimEvent {
    f: i64,
    text: String,
}

struct FrameRec {
    f: i64,
    x: f32,
    y: f32,
    cx: f32,
    cy: f32,
    cell: (i64, i64),
    given: Option<(f32, f32)>,
    vel: Option<(f32, f32)>,
    avoid: (f32, f32),
    cp: usize,
    nav: i64,
    phase: i64,
    note: Option<String>,
}

struct SimOut {
    events: Vec<SimEvent>,
    frames: Vec<FrameRec>,
    cp_done: Vec<i64>,
    end_frame: i64,
    end_reason: String,
}

struct Simulator<'a> {
    model: &'a MapModel,
    obstacles: &'a std::collections::HashSet<(i64, i64)>,
    checkpoints: Vec<Checkpoint>,
    end: (i64, i64),
    k: i64,
    theo: f32,
    t_off: HashMap<&'static str, i64>,
    max_frames: i64,
    nav_maps: HashMap<(i64, i64), NavMap>,

    // 纯 float32 数值层（对齐游戏几何语义 / sim_all.py：全链 f32）
    entity: (f32, f32),
    cursor: (f32, f32),
    spawn_offset: (f32, f32),
    inertia: (f32, f32),
    avoid: (f32, f32),
    nav: i64,
    cp_idx: usize,
    wait_left: f32,
    disappeared: bool,
    last_cell: (i64, i64),
    out: SimOut,
}

impl<'a> Simulator<'a> {
    fn current_cp(&self) -> &Checkpoint {
        if self.cp_idx < self.checkpoints.len() {
            &self.checkpoints[self.cp_idx]
        } else {
            // END 哨兵：终点判定用 self.end
            &self.checkpoints[self.checkpoints.len() - 1]
        }
    }

    fn goal_point(&self, pos: (i64, i64), offset: (f32, f32)) -> (f32, f32) {
        // 脚本 goal_point：F(pos[0] + offset[0]) → f32
        (pos.0 as f32 + offset.0, pos.1 as f32 + offset.1)
    }

    fn nav_of(&mut self, goal: (i64, i64)) -> &NavMap {
        if !self.nav_maps.contains_key(&goal) {
            let nn = build_nav_map(self.model, goal, self.obstacles);
            self.nav_maps.insert(goal, nn);
        }
        &self.nav_maps[&goal]
    }

    fn target_of(&mut self, cell: (i64, i64)) -> Option<(f32, f32)> {
        let (kind, pos, offset) = {
            let cp = self.current_cp();
            (cp.kind.clone(), cp.pos, cp.offset)
        };
        match kind.as_str() {
            "MOVE" => {
                let goal = pos.unwrap();
                let nn = self.nav_of(goal);
                match nn.get(&cell) {
                    Some(&n) if n != goal => Some((n.0 as f32, n.1 as f32)),
                    _ => Some(self.goal_point(goal, offset.unwrap_or((0.0, 0.0)))),
                }
            }
            "END" => {
                let end = self.end;
                let nn = self.nav_of(end);
                match nn.get(&cell) {
                    Some(&n) if n != end => Some((n.0 as f32, n.1 as f32)),
                    _ => Some((end.0 as f32, end.1 as f32)),
                }
            }
            _ => None,
        }
    }

    fn given_direction(&mut self) -> Option<(f32, f32)> {
        let cp = self.current_cp();
        if cp.kind == "MOVE" || cp.kind == "END" {
            if let Some(t) = self.target_of(cell32(self.cursor.0, self.cursor.1)) {
                // 纯 float32（sim_all.py）：dx = t - cursor；dl = sqrt(dx*dx+dy*dy) f32；given = dx/dl
                let dx = t.0 - self.cursor.0;
                let dy = t.1 - self.cursor.1;
                let dl = (dx * dx + dy * dy).sqrt();
                if dl > EPS {
                    return Some((dx / dl, dy / dl));
                }
            }
        }
        None
    }

    /// 避障力（纯 float32，对齐游戏语义 / sim_all.py avoid32）。
    fn avoid32(&self, given: Option<(f32, f32)>) -> (f32, f32) {
        let passable = &self.model.passable;
        let (w, h) = (self.model.width, self.model.height);
        let (cx, cy) = cell32(self.cursor.0, self.cursor.1);
        let in_map = cx >= 0 && cx < w && cy >= 0 && cy < h;
        if !in_map || !passable[cy as usize][cx as usize] {
            // 光标在不可通行格：8 邻域最近可通行格方向（正上优先、顺时针递减）
            let order = [(0, 1), (1, 1), (1, 0), (1, -1), (0, -1), (-1, -1), (-1, 0), (-1, 1)];
            let mut best_dir: Option<(f32, f32)> = None;
            let mut best_d = f32::MAX;
            for (dx, dy) in order {
                let (ox, oy) = (cx + dx, cy + dy);
                if ox >= 0 && ox < w && oy >= 0 && oy < h
                    && passable[oy as usize][ox as usize] && !self.obstacles.contains(&(ox, oy)) {
                    // sim_all.py：d = np.sqrt(F(dx)*F(dx) + F(dy)*F(dy))（f32）
                    let d = (dx as f32 * dx as f32 + dy as f32 * dy as f32).sqrt();
                    if d < best_d {
                        best_d = d;
                        best_dir = Some((dx as f32, dy as f32));
                    }
                }
            }
            return match best_dir {
                Some((bx, by)) => {
                    let l = (bx * bx + by * by).sqrt();
                    project32((bx / l, by / l), given)
                }
                None => (0.0, 0.0),
            };
        }
        // foot = (entity[0], entity[1] - 0.2)
        let foot_x = self.entity.0;
        let foot_y = self.entity.1 - HALF_BODY;
        let (mut push_x, mut push_y) = (0.0f32, 0.0f32);
        for dx in -1i64..=1 {
            for dy in -1i64..=1 {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let (ox, oy) = (cx + dx, cy + dy);
                if ox < 0 || ox >= w || oy < 0 || oy >= h {
                    continue;
                }
                if passable[oy as usize][ox as usize] && !self.obstacles.contains(&(ox, oy)) {
                    continue;
                }
                let nx = if dx < 0 { foot_x - HALF_BODY } else if dx > 0 { foot_x + HALF_BODY } else { foot_x };
                let ny = foot_y;
                let (fdx, fdy) = (dx as f32, dy as f32);
                let app_x = ((nx - cx as f32) * fdx).max(0.0);
                let app_y = ((ny - cy as f32) * fdy).max(0.0);
                let eff_x = (app_x - 0.25) * fdx.abs();
                let eff_y = (app_y - 0.25) * fdy.abs();
                if dx != 0 && dy != 0 {
                    if eff_x > 0.0 && eff_y > 0.0 {
                        let avg = (eff_x + eff_y) / 2.0;
                        push_x += -avg * fdx;
                        push_y += -avg * fdy;
                    }
                } else {
                    if eff_x > 0.0 {
                        push_x += -eff_x * fdx;
                    }
                    if eff_y > 0.0 {
                        push_y += -eff_y * fdy;
                    }
                }
            }
        }
        let l = (push_x * push_x + push_y * push_y).sqrt();
        if l < EPS {
            return (0.0, 0.0);
        }
        project32((push_x / l, push_y / l), given)
    }

    fn check_hole(&self) -> bool {
        let (hx, hy) = cell32(self.entity.0, self.entity.1);
        hx >= 0 && hx < self.model.width && hy >= 0 && hy < self.model.height
            && self.model.tile_key[hy as usize][hx as usize] == "tile_hole"
    }

    fn finish_cp(&mut self, f: i64, label: &str) {
        self.out.cp_done.push(f);
        self.out.events.push(SimEvent { f, text: format!("检查点完成 {label}，位置 ({:.3},{:.3})", self.entity.0, self.entity.1) });
        self.cp_idx += 1;
        if self.cp_idx < self.checkpoints.len() && self.checkpoints[self.cp_idx].kind == "WAIT" {
            self.wait_left = self.checkpoints[self.cp_idx].time as f32;
            self.out.events.push(SimEvent {
                f, text: format!("停驻开始 {}s，位置 ({:.3},{:.3})", self.checkpoints[self.cp_idx].time, self.entity.0, self.entity.1) });
        }
    }

    fn check_cp(&mut self, f: i64) {
        let (kind, time, pos, offset) = {
            let cp = self.current_cp();
            (cp.kind.clone(), cp.time, cp.pos, cp.offset)
        };
        match kind.as_str() {
            "MOVE" => {
                let p = pos.unwrap();
                let gp = self.goal_point(p, offset.unwrap_or((0.0, 0.0)));
                // 纯 float32（sim_all.py）：np.sqrt((cursor-gp)**2 + ...) < GOAL_R
                let dx = self.cursor.0 - gp.0;
                let dy = self.cursor.1 - gp.1;
                if (dx * dx + dy * dy).sqrt() < GOAL_R {
                    self.finish_cp(f, &format!("MOVE{:?}", p));
                    return;
                }
                let (cx, cy) = cell32(self.cursor.0, self.cursor.1);
                if cx < 0 || cx >= self.model.width || cy < 0 || cy >= self.model.height
                    || !self.model.passable[cy as usize][cx as usize] {
                    self.finish_cp(f, "MOVE 卡墙跳");
                    return;
                }
            }
            "WAIT" => {
                self.wait_left -= DELTA;
                if self.wait_left <= 0.0 {
                    self.finish_cp(f, "WAIT");
                }
            }
            "WAIT_GLOBAL" | "WAIT_FRAGMENT" | "WAIT_WAVE" => {
                let limit = self.t_off[kind.as_str()] + (time * FPS as f64) as i64;
                if f >= limit {
                    self.finish_cp(f, &kind);
                }
            }
            "END" => {
                // 纯 float32：sqrt((cursor-end)²) < GOAL_R
                let dx = self.cursor.0 - self.end.0 as f32;
                let dy = self.cursor.1 - self.end.1 as f32;
                if (dx * dx + dy * dy).sqrt() < GOAL_R {
                    self.out.end_reason = format!("抵达终点，位置 ({:.3},{:.3})", self.entity.0, self.entity.1);
                    self.out.end_frame = f;
                }
            }
            "DISAPPEAR" => {
                self.disappeared = true;
                self.finish_cp(f, "DISAPPEAR");
            }
            "APPEAR" => {
                let p = pos.unwrap();
                // 纯 float32：entity = F(pos + spawn_offset)
                self.entity = (p.0 as f32 + self.spawn_offset.0, p.1 as f32 + self.spawn_offset.1);
                self.cursor = (p.0 as f32, p.1 as f32);
                self.disappeared = false;
                self.finish_cp(f, &format!("APPEAR{:?}", p));
            }
            _ => {}
        }
    }

    fn record_frame(&mut self, f: i64, given: Option<(f32, f32)>, vel: Option<(f32, f32)>, avoid: (f32, f32), note: Option<&str>) {
        let cell = cell32(self.cursor.0, self.cursor.1);
        self.out.frames.push(FrameRec {
            f, x: self.entity.0, y: self.entity.1,
            cx: self.cursor.0, cy: self.cursor.1, cell,
            given, vel, avoid,
            cp: self.cp_idx, nav: self.nav, phase: (self.nav - self.k).rem_euclid(3),
            note: note.map(|s| s.to_string()),
        });
    }

    /// 正常移动判定（给定方向 → 避障 → 加速度 → 速度 → 位移）。
    /// 纯 float32（对齐游戏语义 / sim_all.py）：全链 f32。
    fn move_frame(&mut self) -> (Option<(f32, f32)>, (f32, f32), (f32, f32)) {
        self.nav += 1;
        let given = self.given_direction();
        if (self.nav - self.k).rem_euclid(3) == 0 {
            self.avoid = self.avoid32(given);
        }
        // sim_all.py：inert_len = np.sqrt(inertia[0]*inertia[0] + inertia[1]*inertia[1])（f32）
        let inert_len = (self.inertia.0 * self.inertia.0 + self.inertia.1 * self.inertia.1).sqrt();
        let av = if self.avoid != (0.0, 0.0) {
            // sim_all.py：coef = np.maximum(inert_len/THEO, F(0.5))（f32）
            let coef = (inert_len / self.theo).max(0.5);
            (self.avoid.0 * coef, self.avoid.1 * coef)
        } else {
            (0.0, 0.0)
        };
        let sb = match given {
            Some((gx, gy)) => (gx * self.theo - self.inertia.0, gy * self.theo - self.inertia.1),
            None => (-self.inertia.0, -self.inertia.1),
        };
        let steer = (sb.0 * STEERING + av.0, sb.1 * STEERING + av.1);
        let accel = clamp32(steer.0, steer.1, MAX_STEER);
        let vel = clamp32(accel.0 * DELTA + self.inertia.0, accel.1 * DELTA + self.inertia.1, self.theo);
        let disp = (vel.0 * DELTA, vel.1 * DELTA);
        self.entity = (self.entity.0 + disp.0, self.entity.1 + disp.1);
        self.cursor = (self.entity.0 - self.spawn_offset.0, self.entity.1 - self.spawn_offset.1);
        self.inertia = vel;
        (given, vel, disp)
    }

    /// 正常寻路帧（move_frame + 进入格事件 + 帧记录）
    fn nav_frame(&mut self, f: i64) {
        let (given, vel, _disp) = self.move_frame();
        let cell = cell32(self.cursor.0, self.cursor.1);
        if cell != self.last_cell {
            self.last_cell = cell;
            self.out.events.push(SimEvent {
                f, text: format!("进入格 [{}, {}]，位置 ({:.3},{:.3})", cell.0, cell.1, self.cursor.0, self.cursor.1) });
        }
        self.record_frame(f, given, Some(vel), self.avoid, None);
    }

    fn run(&mut self, injections: &[Injection]) -> Result<(), String> {
                self.out.events.push(SimEvent {
            f: 0,
            text: format!("生成于 ({:.3},{:.3}) [格{:?}]", self.entity.0, self.entity.1, self.last_cell),
        });
        if self.spawn_offset != (0.0, 0.0) {
            self.out.events.push(SimEvent { f: 0, text: format!("生成偏移 {:?}", self.spawn_offset) });
        }
        let mut inj_idx = 0usize;
        // 失衡状态（push 设定帧下一帧起生效 dur 帧；失衡覆盖一切）
        let mut push_state: Option<PushState> = None;
        // 登记的阻挡事件（部署先后顺序；生命周期内每帧帧首判定）
        let mut block_events: Vec<BlockEvent> = Vec::new();
        // 当前阻挡归属（偏移中 / 稳定）
        let mut blocking: Option<BlockState> = None;

        for f in 0..self.max_frames {
            if f == 0 {
                self.record_frame(0, None, None, self.avoid, Some("生成帧"));
                continue;
            }

            // ── 帧首：注入登记（设定帧不影响本帧，下一帧起生效）──
            // 同帧若含 push（无论登记顺序），pos 模式的触发帧必失衡 → 无效
            let frame_has_push = injections[inj_idx..]
                .iter()
                .take_while(|e| e.f == f)
                .any(|e| e.kind == "push");
            while inj_idx < injections.len() && injections[inj_idx].f == f {
                let inj = &injections[inj_idx];
                match inj.kind.as_str() {
                    "push" => {
                        push_state = Some(PushState {
                            dir: inj.dir.unwrap(),
                            dist: inj.dist.unwrap(),
                            dur: inj.dur.max(1),
                            start_f: f + 1,
                        });
                        // 失衡覆盖阻挡归属：归属作废，失衡结束后重新判定
                        blocking = None;
                        self.out.events.push(SimEvent {
                            f,
                            text: format!("推拉开始：方向{:?} 距离{} 持续{}帧，位置 ({:.3},{:.3})",
                                inj.dir.unwrap(), inj.dist.unwrap(), inj.dur, self.entity.0, self.entity.1),
                        });
                    }
                    "block" => {
                        // pos 模式要求触发帧（设定帧下一帧）不处于失衡，否则无效（事件流说明，注入作废）
                        let pos_invalid = inj.pos.is_some()
                            && (frame_has_push
                                || push_state.as_ref().map_or(false, |ps| {
                                    f + 1 >= ps.start_f && f + 1 < ps.start_f + ps.dur
                                }));
                        if pos_invalid {
                            self.out.events.push(SimEvent {
                                f,
                                text: "pos 偏移无效：触发帧处于失衡状态，注入忽略".to_string(),
                            });
                        } else {
                            block_events.push(BlockEvent {
                                at: inj.at,
                                pos: inj.pos,
                                deploy_f: f,
                                end_frame: f + inj.dur,
                            });
                            let desc = match inj.at {
                                Some(a) => format!("at={:?}", a),
                                None => format!("pos={:?}", inj.pos.unwrap()),
                            };
                            self.out.events.push(SimEvent {
                                f,
                                text: format!("部署阻挡 {} dur={}，位置 ({:.3},{:.3})", desc, inj.dur, self.entity.0, self.entity.1),
                            });
                        }
                    }
                    _ => {}
                }
                inj_idx += 1;
            }

            // ── 帧首：状态判定（基于上一帧末坐标）──
            // 消失状态：不移动、不寻路、不判阻挡，仅检查点判定
            if self.disappeared {
                self.check_cp(f);
                self.record_frame(f, None, Some(self.inertia), self.avoid, Some("消失"));
                if !self.out.end_reason.is_empty() {
                    break;
                }
                continue;
            }

            // 失衡（最高优先级）：设定帧下一帧起持续 dur 帧
            let in_push = push_state
                .as_ref()
                .map_or(false, |ps| f >= ps.start_f && f < ps.start_f + ps.dur);

            if in_push {
                let ps = push_state.clone().unwrap();
                let total = ps.dist;
                let dur = ps.dur.max(1);
                let step = if f == ps.start_f + dur - 1 {
                    total - (dur - 1) as f32 * (total / dur as f32)
                } else {
                    total / dur as f32
                };
                let disp = (ps.dir.0 * step, ps.dir.1 * step);
                self.entity = (self.entity.0 + disp.0, self.entity.1 + disp.1);
                self.cursor = (self.entity.0 - self.spawn_offset.0, self.entity.1 - self.spawn_offset.1);
                self.record_frame(f, None, Some((disp.0 * FPS, disp.1 * FPS)), self.avoid, Some("推拉"));
                if f == ps.start_f + dur - 1 {
                    // 推拉结束（失衡末帧的帧末结果事件）
                    self.out.events.push(SimEvent {
                        f,
                        text: format!("推拉结束，位置 ({:.3},{:.3})", self.entity.0, self.entity.1),
                    });
                    push_state = None;
                }
            } else if let Some(bs) = blocking.clone() {
                // 阻挡归属中：偏移优先完成；撤退帧（end_frame = 阻挡状态最后一帧）帧末宣告撤退
                let ev_end = block_events[bs.ev_idx].end_frame;
                if bs.offset_rem > 0 {
                    // 偏移过程帧（线性匀速，纯 float32）
                    let progress = (7.0 - bs.offset_rem as f32) / 6.0;
                    let pos = (bs.offset_from.0 + (bs.offset_to.0 - bs.offset_from.0) * progress,
                              bs.offset_from.1 + (bs.offset_to.1 - bs.offset_from.1) * progress);
                    self.entity = (pos.0 + self.spawn_offset.0, pos.1 + self.spawn_offset.1);
                    self.cursor = (pos.0, pos.1);
                    blocking.as_mut().unwrap().offset_rem -= 1;
                    self.record_frame(f, None, Some(self.inertia), self.avoid, Some("偏移"));
                    let done = blocking.as_ref().unwrap().offset_rem == 0;
                    if done {
                        // 偏移完成（帧末结果事件）：偏移收尾 + 稳定坐标宣告
                        self.out.events.push(SimEvent {
                            f,
                            text: format!("阻挡偏移完成，位置 ({:.3},{:.3})", self.entity.0, self.entity.1),
                        });
                    }
                    if f == ev_end {
                        // 撤退帧：阻挡状态最后一帧的末尾宣告撤退（事件发生帧，非生效首帧）；
                        // 偏移位移继续完成（撤退不打断偏移），偏移同时完成则一并解除归属
                        self.out.events.push(SimEvent {
                            f,
                            text: format!("撤退，位置 ({:.3},{:.3})", self.entity.0, self.entity.1),
                        });
                        if done {
                            blocking = None;
                        }
                    }
                } else if f == ev_end {
                    // 稳定态撤退帧：稳定冻结 + 帧末宣告撤退 + 解除归属
                    self.record_frame(f, None, Some(self.inertia), self.avoid, Some("阻挡"));
                    self.out.events.push(SimEvent {
                        f,
                        text: format!("撤退，位置 ({:.3},{:.3})", self.entity.0, self.entity.1),
                    });
                    blocking = None;
                } else if f > ev_end {
                    // 偏移中已宣告撤退后的收尾（撤退帧在偏移中）：直接解除归属，本帧寻路
                    blocking = None;
                    self.nav_frame(f);
                } else {
                    // 稳定冻结：位置不变
                    self.record_frame(f, None, Some(self.inertia), self.avoid, Some("阻挡"));
                }
            } else {
                // 无归属：按部署先后遍历未到期事件判定（十字五格 → 距离平方）
                let mut hit: Option<(usize, Option<(f32, f32)>)> = None;
                for (i, ev) in block_events.iter().enumerate() {
                    if f <= ev.deploy_f || f > ev.end_frame {
                        continue;
                    }
                    if let Some(at) = ev.at {
                        let (cx, cy) = cell32(self.entity.0, self.entity.1);
                        if (cx - at.0).abs() + (cy - at.1).abs() > 1 {
                            continue;
                        }
                        let (ex, ey) = (self.entity.0, self.entity.1);
                        let (bx, by) = (at.0 as f32, at.1 as f32);
                        let (dx, dy) = (ex - bx, ey - by);
                        let d2 = dx * dx + dy * dy;
                        if d2 < INNER_R2 {
                            // 内圈：径向偏移（重合不偏移 → 稳定）
                            let to = if d2 < 1e-10 {
                                None
                            } else {
                                let d = d2.sqrt();
                                let k = 0.5 / d;
                                Some((bx + dx * k, by + dy * k))
                            };
                            hit = Some((i, to));
                            break;
                        } else if d2 <= BLOCK_R2 {
                            // 外圈：稳定阻挡
                            hit = Some((i, None));
                            break;
                        }
                    } else {
                        // pos 模式：无条件触发偏移（登记时已验证触发帧非失衡）
                        hit = Some((i, ev.pos));
                        break;
                    }
                }
                if let Some((i, to)) = hit {
                    if let Some(t) = to {
                        blocking = Some(BlockState {
                            ev_idx: i,
                            offset_rem: 6,
                            offset_from: self.entity,
                            offset_to: t,
                        });
                        // 偏移开始（帧首判定事件，坐标 = 判定坐标）；本帧即偏移第 0 帧，执行第 1 步位移
                        self.out.events.push(SimEvent {
                            f,
                            text: format!("阻挡偏移开始，起点 ({:.3},{:.3})", self.entity.0, self.entity.1),
                        });
                        let from = self.entity;
                        let progress = 1.0 / 6.0;
                        let pos = (from.0 + (t.0 - from.0) * progress,
                                  from.1 + (t.1 - from.1) * progress);
                        self.entity = (pos.0 + self.spawn_offset.0, pos.1 + self.spawn_offset.1);
                        self.cursor = (pos.0, pos.1);
                        blocking.as_mut().unwrap().offset_rem -= 1;
                        self.record_frame(f, None, Some(self.inertia), self.avoid, Some("偏移"));
                    } else {
                        blocking = Some(BlockState {
                            ev_idx: i,
                            offset_rem: 0,
                            offset_from: self.entity,
                            offset_to: self.entity,
                        });
                        // 稳定开始（帧首判定事件，坐标 = 判定坐标）；本帧稳定冻结
                        self.out.events.push(SimEvent {
                            f,
                            text: format!("稳定阻挡开始，位置 ({:.3},{:.3})", self.entity.0, self.entity.1),
                        });
                        self.record_frame(f, None, Some(self.inertia), self.avoid, Some("阻挡"));
                    }
                } else {
                    self.nav_frame(f);
                }
            }

            // ── 帧末统一判定：入坑 / 检查点 / 终点（各状态一致）──
            if self.check_hole() {
                let (hx, hy) = cell32(self.entity.0, self.entity.1);
                self.out.end_reason = format!("入坑：实体坐标进入地穴格 ({},{})", hx, hy);
                self.out.end_frame = f;
                break;
            }
            self.check_cp(f);
            if !self.out.end_reason.is_empty() {
                break;
            }
        }
        if self.out.end_reason.is_empty() {
            self.out.end_reason = format!("超出帧上限（{}）", self.max_frames);
            self.out.end_frame = self.max_frames - 1;
        }
        self.out.events.push(SimEvent { f: self.out.end_frame, text: self.out.end_reason.clone() });
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════
// Tool trait 实现
// ═══════════════════════════════════════════════════════

/// 寻路模拟工具。工作根为帧级 log 的落点基准（**共享句柄，每次读当前值**）：
/// 默认写入 workspace_root/sim_logs/，log 参数覆盖时相对路径基于工作区解析。
/// 本工具只写自己的 log 文件，不走沙箱守卫（读写判定针对 LLM 文件
/// 能力；log 为工具内部副作用，落点已固定在工作区内）。
pub struct SimPath {
    /// 当前工作根（共享句柄，见 crate::workspace）。
    workspace_root: WorkspaceRoot,
}

impl SimPath {
    /// `workspace_root` 接受路径（单测）或共享句柄（生产装配）。
    pub fn new(workspace_root: impl Into<WorkspaceRoot>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
        }
    }
}

impl Tool for SimPath {
    fn name(&self) -> &str {
        "sim_path"
    }

    fn description(&self) -> &str {
        "逐帧寻路模拟：按关卡 ID 与路线序号模拟敌人沿路线移动（相对帧，f0=生成帧），输出格级事件流。基础用法：levelId + routeIndex + speed（speed 为敌人属性移速，先查波次确定该路线敌人再查其属性移速，工具自动按关卡倍率换算）。高级参数（k 避障相位、inject 行为注入、route 自定义路线、obstacle、timeAxis、log、maxFrames）语法见《行动推演》skill 正文。帧级 log 默认写入工作区 sim_logs/。仅地面行动（WALK）；FLY 路线、近地悬浮敌人与非常用检查点类型（传送外）不支持"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "levelId": { "type": "string", "description": "关卡 levelId，如 level_main_01-07" },
                "routeIndex": { "type": "integer", "description": "路线序号（0 起），如 5" },
                "speed": { "type": "number", "description": "敌人属性移速（如 1.3）。缺省 1.0" },
                "k": { "type": "integer", "description": "避障相位参数，默认 1" },
                "inject": { "type": "string", "description": "行为注入（push 推拉 / block 部署阻挡），分号分隔多条" },
                "route": { "type": "string", "description": "自定义路线 DSL。不填则用数据库路线" },
                "obstacle": { "type": "string", "description": "额外障碍物格，分号分隔，如 7,2;7,3。" },
                "timeAxis": { "type": "object", "description": "计时起点对象（默认全 0=f0），供 WAIT_GLOBAL/FRAGMENT/WAVE 使用；子键 global/fragment/wave，语义见《行动推演》skill 参数语法节"},
                "log": { "type": "string", "description": "帧级 log 输出路径（相对路径基于工作区解析），覆盖默认位置（工作区 sim_logs/）" },
                "maxFrames": { "type": "integer", "description": "模拟帧上限，默认 12000" }
            },
            "required": ["levelId", "routeIndex"]
        })
    }

    fn display(&self, _ctx: &ToolContext, params: &str) -> ToolDisplay {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(params) else {
            return ToolDisplay::default();
        };
        let level = v.get("levelId").and_then(|x| x.as_str()).unwrap_or("");
        let route = v.get("routeIndex").and_then(|x| x.as_i64());
        match (level.is_empty(), route) {
            (true, None) => ToolDisplay::default(),
            (false, None) => ToolDisplay::info(level),
            (true, Some(r)) => ToolDisplay::info(format!("路线 {r}")),
            (false, Some(r)) => ToolDisplay::info(format!("{level} · 路线 {r}")),
        }
    }

    fn execute(&self, ctx: &ToolContext, params: &str) -> Result<ToolOutput, ToolError> {
        let v: serde_json::Value = serde_json::from_str(params)
            .map_err(|e| ToolError::Parse(e.to_string()))?;
        let level_id = v["levelId"].as_str()
            .ok_or_else(|| ToolError::Parse("缺少必填参数: levelId".into()))?;
        let route_index = v["routeIndex"].as_i64()
            .ok_or_else(|| ToolError::Parse("缺少必填参数: routeIndex".into()))?;
        let speed = v["speed"].as_f64().unwrap_or(1.0) as f32;
        let k = v["k"].as_i64().unwrap_or(1);
        let max_frames = v["maxFrames"].as_i64().unwrap_or(12000);
        let t_global = v["timeAxis"]["global"].as_i64().unwrap_or(0);
        let t_fragment = v["timeAxis"]["fragment"].as_i64().unwrap_or(0);
        let t_wave = v["timeAxis"]["wave"].as_i64().unwrap_or(0);
        let log_override = v["log"].as_str().map(|s| s.to_string());

        let db = ctx.db.as_ref().ok_or_else(|| ToolError::Execution("数据库未连接".into()))?;
        let result = db.query_filtered(
            "levels",
            &["levelId".into(), "地图数据".into(), "路线".into()],
            &[Filter::new("levelId", FilterOp::Eq, level_id)],
            1,
        ).map_err(|e| ToolError::Execution(e.to_string()))?;
        let row = result.rows.into_iter().next()
            .ok_or_else(|| ToolError::Execution(format!("未找到关卡: {level_id}")))?;

        // 地图模型
        let map_raw = row.get("地图数据")
            .ok_or_else(|| ToolError::Execution("地图数据缺失".into()))?;
        let md: MapData = serde_json::from_str(map_raw)
            .map_err(|e| ToolError::Execution(format!("解析地图数据失败: {e}")))?;
        let (w, h) = (md.width, md.height);
        let mut passable = vec![vec![false; w as usize]; h as usize];
        let mut tile_key = vec![vec![String::new(); w as usize]; h as usize];
        let mut obstacles: std::collections::HashSet<(i64, i64)> = std::collections::HashSet::new();
        for (gi, row_vals) in md.grid.iter().enumerate() {
            let coord_row = (h - 1 - gi as i64) as usize;
            for (c, &idx) in row_vals.iter().enumerate() {
                if let Some(t) = md.tiles.get(idx) {
                    let key = &t.0;
                    tile_key[coord_row][c] = key.clone();
                    let p = t.3 == 3;
                    passable[coord_row][c] = p;
                    if p && key == "tile_hole" {
                        obstacles.insert((c as i64, coord_row as i64));
                    }
                }
            }
        }
        let model = MapModel { width: w, height: h, passable, tile_key };

        // 额外障碍物
        if let Some(obs) = v["obstacle"].as_str() {
            for token in obs.split(';') {
                let token = token.trim();
                if token.is_empty() { continue; }
                let mut it = token.split(',');
                let x = it.next().and_then(|s| s.trim().parse::<i64>().ok())
                    .ok_or_else(|| ToolError::Parse(format!("障碍物解析失败: {token}")))?;
                let y = it.next().and_then(|s| s.trim().parse::<i64>().ok())
                    .ok_or_else(|| ToolError::Parse(format!("障碍物解析失败: {token}")))?;
                obstacles.insert((x, y));
            }
        }

        // 路线：自定义 DSL 或数据库
        let (start_pos, end, spawn_offset, checkpoints, route_label) = if let Some(dsl) = v["route"].as_str() {
            let cps = parse_dsl(dsl).map_err(ToolError::Parse)?;
            let start = cps.iter().find(|c| c.kind == "START").and_then(|c| c.pos)
                .ok_or_else(|| ToolError::Parse("自定义路线需包含 START(x,y)".into()))?;
            let end = cps.iter().find(|c| c.kind == "END").and_then(|c| c.pos)
                .ok_or_else(|| ToolError::Parse("自定义路线需包含 END(x,y)".into()))?;
            // 生成偏移取 START 的偏移参数（过滤前）
            let spawn_offset = cps.iter().find(|c| c.kind == "START")
                .and_then(|c| c.offset).unwrap_or((0.0, 0.0));
            let mut cps: Vec<Checkpoint> = cps.into_iter()
                .filter(|c| c.kind != "START" && c.kind != "END").collect();
            cps.push(Checkpoint { kind: "END".into(), time: 0.0, pos: Some(end), offset: None });
            (start, end, spawn_offset, cps, "自定义".to_string())
        } else {
            let routes_raw = row.get("路线")
                .ok_or_else(|| ToolError::Execution("路线数据缺失".into()))?;
            let routes: serde_json::Value = serde_json::from_str(routes_raw)
                .map_err(|e| ToolError::Execution(format!("解析路线失败: {e}")))?;
            let arr = routes.as_array().ok_or_else(|| ToolError::Execution("路线格式错误".into()))?;
            if route_index < 0 || route_index as usize >= arr.len() {
                return Err(ToolError::Execution(format!("路线序号 {} 越界（共 {} 条）", route_index, arr.len())));
            }
            let (start_cp, end, spawn_offset, mut cps) = parse_db_route(&arr[route_index as usize])
                .map_err(ToolError::Parse)?;
            cps.push(Checkpoint { kind: "END".into(), time: 0.0, pos: Some(end), offset: None });
            (start_cp.pos.unwrap(), end, spawn_offset, cps, "数据库".to_string())
        };

        // 注入
        let injections = match v["inject"].as_str() {
            Some(spec) => parse_injections(spec).map_err(ToolError::Parse)?,
            None => Vec::new(),
        };
        // 前置检查：注入帧在帧上限内（判定与有效性在模拟中逐帧进行）
        for inj in &injections {
            if inj.f >= max_frames {
                return Err(ToolError::Parse(format!("注入帧 {} 超出帧上限", inj.f)));
            }
        }

        // 模拟
        let theo = speed * MOVE_MULTIPLIER;
        let checkpoints_json = json!(&checkpoints);
        let injections_json = json!(&injections);
        let mut t_off: HashMap<&'static str, i64> = HashMap::new();
        t_off.insert("WAIT_GLOBAL", t_global);
        t_off.insert("WAIT_FRAGMENT", t_fragment);
        t_off.insert("WAIT_WAVE", t_wave);
        let mut sim = Simulator {
            model: &model,
            obstacles: &obstacles,
            checkpoints,
            end,
            k, theo, t_off,
            max_frames,
            nav_maps: HashMap::new(),
            // 纯 float32：entity = F(start + spawn_offset)、cursor = F(start)
            entity: (start_pos.0 as f32 + spawn_offset.0, start_pos.1 as f32 + spawn_offset.1),
            cursor: (start_pos.0 as f32, start_pos.1 as f32),
            spawn_offset,
            inertia: (0.0, 0.0),
            avoid: (0.0, 0.0),
            nav: 0,
            cp_idx: 0,
            wait_left: 0.0,
            disappeared: false,
            last_cell: (start_pos.0, start_pos.1),
            out: SimOut { events: Vec::new(), frames: Vec::new(), cp_done: Vec::new(), end_frame: 0, end_reason: String::new() },
        };
        // 首检查点为 WAIT 时初始化计时
        if !sim.checkpoints.is_empty() && sim.checkpoints[0].kind == "WAIT" {
            sim.wait_left = sim.checkpoints[0].time as f32;
        }
        sim.run(&injections).map_err(ToolError::Parse)?;
        let out = sim.out;

        // summary：事件流
        let mut lines = Vec::new();
        lines.push("══════ 寻路模拟 ══════".to_string());
        lines.push(format!("关卡: {}  路线: {}（{}）  K={}  移速={}格/秒", level_id, route_index, route_label, k, speed));
        lines.push(format!("起点: {:?}  f0 生成  终点: {:?}", start_pos, end));
        if !injections.is_empty() {
            lines.push(format!("注入: {:?}", injections.iter().map(|i| format!("{}@{}", i.kind, i.f)).collect::<Vec<_>>()));
        }
        lines.push(String::new());
        for ev in &out.events {
            lines.push(format!("f{:<6}{}", ev.f, ev.text));
        }
        lines.push(String::new());
        let mut summary = lines.join("\n");

        // log 写入（M1 批 2：落点迁移到工作区）
        // log_override 相对路径 → workspace_root.join；绝对路径直接用。
        // 缺省 → workspace_root/sim_logs/sim_{level_id}_r{route_index}.jsonl
        let log_path = match log_override {
            Some(p) => {
                let op = PathBuf::from(p);
                if op.is_absolute() {
                    op
                } else {
                    self.workspace_root.current().join(op)
                }
            }
            None => {
                let dir = self.workspace_root.current().join("sim_logs");
                fs::create_dir_all(&dir).ok();
                dir.join(format!("sim_{}_r{}.jsonl", level_id, route_index))
            }
        };
        let meta = json!({
            "type": "meta", "levelId": level_id, "route": route_index,
            "k": k, "theo": theo, "start": [start_pos.0, start_pos.1],
            "end": [end.0, end.1], "spawn_offset": [spawn_offset.0, spawn_offset.1],
            "checkpoints": checkpoints_json,
            "injections": injections_json,
            "width": w, "height": h,
            "end_frame": out.end_frame, "end_reason": out.end_reason,
        });
        let mut log_content = format!("{}\n", serde_json::to_string(&meta).unwrap_or_default());
        for fr in &out.frames {
            let rec = json!({
                "type": "frame", "f": fr.f,
                "x": round6(fr.x), "y": round6(fr.y), "cx": round6(fr.cx), "cy": round6(fr.cy),
                "cell": [fr.cell.0, fr.cell.1],
                "given": fr.given.map(|(a, b)| [round6(a), round6(b)]),
                "vel": fr.vel.map(|(a, b)| [round6(a), round6(b)]),
                "avoid": [round6(fr.avoid.0), round6(fr.avoid.1)],
                "cp": fr.cp, "nav": fr.nav, "phase": fr.phase, "note": fr.note,
            });
            log_content.push_str(&format!("{}\n", serde_json::to_string(&rec).unwrap_or_default()));
        }
        for ev in &out.events {
            let rec = json!({ "type": "event", "f": ev.f, "text": ev.text });
            log_content.push_str(&format!("{}\n", serde_json::to_string(&rec).unwrap_or_default()));
        }
        fs::write(&log_path, log_content)
            .map_err(|e| ToolError::Execution(format!("log 写入失败: {e}")))?;
        summary.push_str(&format!("\nlog: {}", log_path.display()));

        // rows：结构化摘要
        let rows = json!([{
            "levelId": level_id, "routeIndex": route_index, "k": k, "speed": speed,
            "endFrame": out.end_frame, "endReason": out.end_reason,
            "cpDone": out.cp_done, "logPath": log_path,
        }]);
        Ok(ToolOutput::json(rows, summary))
    }
}

fn round6(v: f32) -> f64 {
    // log 保留 6 位小数（银行家舍入，与脚本 round(float(x), 6) 同值）
    round_half_even(f64::from(v) * 1e6) / 1e6
}

// ═══════════════════════════════════════════════════════
// 测试
// ═══════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    fn db_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/arknights.db")
    }

    /// 创建唯一临时目录（不引入 tempfile crate）。
    fn unique_temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "prts-sim-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_ctx() -> ToolContext {
        let db = crate::db::Database::open(db_path().to_str().unwrap()).unwrap();
        ToolContext::with_db(Arc::new(db))
    }

    /// 在指定工作区执行一次模拟并返回 summary（**不清理**——show_output
    /// 等持久场景用：log 是研究资产，必须留存供绘图/对拍）。
    fn run_in(ws: &Path, params: &str) -> String {
        let ctx = make_ctx();
        let tool = SimPath::new(ws.to_path_buf());
        match tool.execute(&ctx, params) {
            Ok(s) => s.summary,
            Err(e) => format!("ERROR: {e}"),
        }
    }

    /// 断言测试用：独立临时工作区，用后整体清理（测试不断言 log 路径，
    /// 仅断言 summary——log 为副作用，清除避免测试垃圾与并行互踩）。
    fn run(params: &str) -> String {
        let ws = unique_temp_dir("ws");
        let out = run_in(&ws, params);
        let _ = std::fs::remove_dir_all(&ws);
        out
    }

    #[test]
    fn show_output() {
        // 交互/研究入口（test_sim.py 驱动，log 供 plot_log.py 绘图）：
        // 环境变量驱动（与 test_level.py 同模式）：
        //   PRTS_SIM_LEVEL_ID="level_rogue5_t-10"
        //   PRTS_SIM_ROUTE_INDEX="5"
        //   PRTS_SIM_SPEED / PRTS_SIM_K / PRTS_SIM_INJECT / PRTS_SIM_ROUTE 可选
        //   PRTS_SIM_WS：持久工作区（log 落 {ws}/sim_logs/，不清理）；缺省用
        //   唯一临时目录且**不清理**（log 存活于系统 temp，供绘图脚本读取）。
        // 注意与 run() 的区别：run() 的清理是断言测试隔离，不适用于本入口。
        let ws = match std::env::var("PRTS_SIM_WS") {
            Ok(p) => {
                let dir = PathBuf::from(p);
                std::fs::create_dir_all(&dir)
                    .unwrap_or_else(|e| panic!("PRTS_SIM_WS 不可用: {e}"));
                dir
            }
            Err(_) => unique_temp_dir("show"),
        };
        if let Ok(level_id) = std::env::var("PRTS_SIM_LEVEL_ID") {
            let route_index = std::env::var("PRTS_SIM_ROUTE_INDEX").unwrap_or_else(|_| "5".into());
            let speed = std::env::var("PRTS_SIM_SPEED").unwrap_or_else(|_| "1.0".into());
            let k = std::env::var("PRTS_SIM_K").unwrap_or_else(|_| "1".into());
            let inject = std::env::var("PRTS_SIM_INJECT").unwrap_or_default();
            let route = std::env::var("PRTS_SIM_ROUTE").unwrap_or_default();
            let mut p = format!(r#"{{"levelId":"{level_id}","routeIndex":{route_index},"speed":{speed},"k":{k}}}"#);
            if !inject.is_empty() {
                p = format!(r#"{{"levelId":"{level_id}","routeIndex":{route_index},"speed":{speed},"k":{k},"inject":"{inject}"}}"#);
            }
            if !route.is_empty() {
                p = format!(r#"{{"levelId":"{level_id}","routeIndex":{route_index},"speed":{speed},"k":{k},"route":"{route}"}}"#);
            }
            println!("{}", run_in(&ws, &p));
            return;
        }
        // 默认演示：路线 5（沙滩车移速 1.3 → 理论 0.65）
        println!("===== 路线 5 基础 =====");
        println!("{}", run_in(&ws, r#"{"levelId":"level_rogue5_t-10","routeIndex":5,"speed":1.3}"#));
    }

    #[test]
    fn sim_route5_basic() {
        // 与 sim_tool.py 对拍：路线 5，属性移速 1.3（理论 0.65），K=1
        let s = run(r#"{"levelId":"level_rogue5_t-10","routeIndex":5,"speed":1.3}"#);
        assert!(s.contains("f93    检查点完成 MOVE(8, 2)"), "检查点完成事件不符\n{s}");
        assert!(s.contains("f994   检查点完成 WAIT"), "检查点完成事件不符\n{s}");
        assert!(s.contains("f1784  检查点完成 MOVE(2, 1)"), "检查点完成事件不符\n{s}");
        assert!(s.contains("抵达终点"), "应抵达终点\n{s}");
    }

    #[test]
    fn sim_route1_basic_k2() {
        // 与 sim_tool.py 对拍：路线 1，K=2，属性移速 1.3
        let s = run(r#"{"levelId":"level_rogue5_t-10","routeIndex":1,"speed":1.3,"k":2}"#);
        assert!(s.contains("f1801  检查点完成 WAIT"), "检查点完成事件不符\n{s}");
        assert!(s.contains("f2471  检查点完成 MOVE(9, 5)"), "检查点完成事件不符\n{s}");
        assert!(s.contains("f2796  检查点完成 MOVE(8, 5)"), "检查点完成事件不符\n{s}");
        assert!(s.contains("f3633  抵达终点"), "终点帧不符\n{s}");
    }

    #[test]
    fn sim_route3_basic_k1() {
        // 与 sim_tool.py 对拍：路线 3，K=1，属性移速 1.3
        let s = run(r#"{"levelId":"level_rogue5_t-10","routeIndex":3,"speed":1.3,"k":1}"#);
        assert!(s.contains("f3601  检查点完成 WAIT"), "检查点完成事件不符\n{s}");
        assert!(s.contains("f4364  检查点完成 MOVE(9, 5)"), "检查点完成事件不符\n{s}");
        assert!(s.contains("f4689  检查点完成 MOVE(8, 5)"), "检查点完成事件不符\n{s}");
        assert!(s.contains("f5525  抵达终点"), "终点帧不符\n{s}");
    }

    #[test]
    fn sim_route1_block_tune() {
        // 路线 1 调避障轴（K=2，属性移速 1.3，block@1930 at=(4,1) dur=6 纯偏移）：
        // 部署帧 1930（寻路照常），1931 起偏移 6 帧到 (3.5,1)，偏移后事件整体推后
        let s = run(r#"{"levelId":"level_rogue5_t-10","routeIndex":1,"speed":1.3,"k":2,"inject":"block@1930:at=(4,1),dur=6"}"#);
        assert!(s.contains("阻挡偏移完成，位置 (3.500,1.000)"), "偏移完成不符\n{s}");
        assert!(s.contains("f2488  检查点完成 MOVE(9, 5)"), "检查点完成事件不符\n{s}");
        assert!(s.contains("f2813  检查点完成 MOVE(8, 5)"), "检查点完成事件不符\n{s}");
        assert!(s.contains("f3649  抵达终点"), "终点帧不符\n{s}");
    }

    #[test]
    fn sim_route3_block_verify() {
        // 路线 3 调避障轴（K=1，block@4381 at=(9,5) dur=6）：
        // 部署帧 4381（寻路照常），4382 起偏移 6 帧，偏移目标 (9.0382,4.5015)（显示 3 位小数）
        let s = run(r#"{"levelId":"level_rogue5_t-10","routeIndex":3,"speed":1.3,"k":1,"inject":"block@4381:at=(9,5),dur=6"}"#);
        assert!(s.contains("阻挡偏移完成，位置 (9.038,4.501)"), "偏移坐标不符\n{s}");
        assert!(s.contains("f4387  撤退"), "撤退帧不符（应为偏移完成同帧）\n{s}");
    }

    #[test]
    fn sim_custom_route_dsl() {
        // 自定义路线 DSL：START/MOVE/WAIT/END
        let s = run(r#"{"levelId":"level_rogue5_t-10","routeIndex":5,"speed":1.3,"route":"START(10,2);MOVE(8,2);WAIT(10.0);END(2,7)"}"#);
        assert!(s.contains("（自定义）"), "应标记自定义路线\n{s}");
        assert!(s.contains("f93    检查点完成 MOVE(8, 2)"), "检查点完成事件不符\n{s}");
        assert!(s.contains("f394   检查点完成 WAIT"), "检查点完成事件不符\n{s}");
        assert!(s.contains("抵达终点"), "应抵达终点\n{s}");
    }

    #[test]
    fn sim_push_into_hole() {
        // 推拉入坑：路线 5 起点 (10,2) 向左推 3 格 → (7,2) 地穴
        let s = run(r#"{"levelId":"level_rogue5_t-10","routeIndex":5,"speed":1.3,"inject":"push@10:dir=(-1,0),dist=3.0,dur=30"}"#);
        assert!(s.contains("入坑：实体坐标进入地穴格 (7,2)"), "应入坑 (7,2)\n{s}");
    }

    #[test]
    fn sim_block_auto_radial() {
        // block pos 模式直给偏移坐标 + 入坑（偏移过程到位时入坑 (7,3)）
        let s = run(r#"{"levelId":"level_rogue5_t-10","routeIndex":5,"speed":1.3,"inject":"block@30:pos=(7.3,3.0),dur=6"}"#);
        assert!(s.contains("入坑：实体坐标进入地穴格 (7,3)"), "应入坑 (7,3)\n{s}");
    }

    #[test]
    fn sim_errors() {
        // 越界路线 / 缺少参数
        let s = run(r#"{"levelId":"level_rogue5_t-10","routeIndex":99}"#);
        assert!(s.contains("ERROR"), "应报错：路线越界\n{s}");
        let s = run(r#"{"levelId":"level_rogue5_t-10"}"#);
        assert!(s.contains("ERROR"), "应报错：缺 routeIndex\n{s}");
    }

    #[test]
    fn sim_log_override_relative_resolves_under_workspace() {
        // M1 批 2 遗留补测：log 参数相对路径 → workspace_root.join 解析；
        // summary 的 log: 指向工作区内且文件真实存在
        let ws = unique_temp_dir("logrel");
        std::fs::create_dir_all(ws.join("out")).unwrap(); // override 分支不自动建目录
        let ctx = make_ctx();
        let tool = SimPath::new(ws.clone());

        let s = tool
            .execute(&ctx, r#"{"levelId":"level_rogue5_t-10","routeIndex":5,"speed":1.3,"log":"out/a.jsonl"}"#)
            .unwrap()
            .summary;

        let log_line = s
            .lines()
            .find(|l| l.starts_with("log: "))
            .expect("summary 应附 log: 行");
        let log_path = PathBuf::from(log_line.trim_start_matches("log: "));
        assert!(
            log_path.starts_with(&ws),
            "log 应落在工作区内：{log_path:?} vs {ws:?}"
        );
        assert_eq!(log_path, ws.join("out").join("a.jsonl"), "相对路径应基于工作区解析");
        assert!(log_path.is_file(), "log 文件应真实存在");

        let _ = std::fs::remove_dir_all(&ws);
    }
}

