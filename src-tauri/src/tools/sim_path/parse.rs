// 路线 DSL、数据库路线与注入参数的解析。
//
// 只做文本/JSON 到内部结构的转换，不涉及模拟帧进。

use std::collections::HashMap;

use super::{Checkpoint, Injection};

/// 数据库路线数组 → (起点, 终点, 生成偏移, 检查点列表)
pub(super) fn parse_db_route(
    route: &serde_json::Value,
) -> Result<(Checkpoint, (i64, i64), (f32, f32), Vec<Checkpoint>), String> {
    let motion = route[0].as_i64().unwrap_or(0);
    if motion == 1 {
        return Err("FLY 路线暂不支持（飞行移动未实现）".into());
    }
    if motion == 2 {
        return Err("E_NUM 路线无意义".into());
    }
    let start = (
        route[1][1].as_i64().unwrap_or(0),
        route[1][0].as_i64().unwrap_or(0),
    );
    let end = (
        route[2][1].as_i64().unwrap_or(0),
        route[2][0].as_i64().unwrap_or(0),
    );
    let spawn_offset = if route.get(4).is_some() {
        (
            route[4][0].as_f64().unwrap_or(0.0) as f32,
            route[4][1].as_f64().unwrap_or(0.0) as f32,
        )
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
                7 => "ALERT", // 纯演出，跳过
                8 => "MOVE",  // PATROL_MOVE 同 MOVE
                _ => return Err(format!("检查点类型 {} 暂不支持", t)),
            };
            if kind == "ALERT" {
                continue;
            }
            let time = cp[1].as_f64().unwrap_or(0.0);
            let pos = (
                cp[2][1].as_i64().unwrap_or(0),
                cp[2][0].as_i64().unwrap_or(0),
            );
            let offset = (
                cp[3][0].as_f64().unwrap_or(0.0) as f32,
                cp[3][1].as_f64().unwrap_or(0.0) as f32,
            );
            match kind {
                "MOVE" => cps.push(Checkpoint {
                    kind: kind.into(),
                    time: 0.0,
                    pos: Some(pos),
                    offset: Some(offset),
                }),
                "WAIT" | "WAIT_GLOBAL" | "WAIT_FRAGMENT" | "WAIT_WAVE" => cps.push(Checkpoint {
                    kind: kind.into(),
                    time,
                    pos: None,
                    offset: None,
                }),
                "DISAPPEAR" => cps.push(Checkpoint::new("DISAPPEAR")),
                "APPEAR" => cps.push(Checkpoint {
                    kind: "APPEAR".into(),
                    time: 0.0,
                    pos: Some(pos),
                    offset: None,
                }),
                _ => {}
            }
        }
    }
    let start_cp = Checkpoint {
        kind: "START".into(),
        time: 0.0,
        pos: Some(start),
        offset: None,
    };
    Ok((start_cp, end, spawn_offset, cps))
}

/// 自定义路线 DSL → 检查点列表（START/MOVE/WAIT/.../END，分号分隔）
pub(super) fn parse_dsl(dsl: &str) -> Result<Vec<Checkpoint>, String> {
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
        let parts: Vec<&str> = args
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        match kind {
            "START" | "MOVE" | "PATROL" | "APPEAR" | "END" => {
                let n = if kind == "MOVE" {
                    if parts.len() == 4 {
                        4
                    } else {
                        2
                    }
                } else {
                    2
                };
                if parts.len() != n {
                    return Err(format!("{kind} 需要 {} 个参数：{token}", n));
                }
                let pos = (
                    parts[0]
                        .parse::<i64>()
                        .map_err(|_| format!("坐标解析失败：{token}"))?,
                    parts[1]
                        .parse::<i64>()
                        .map_err(|_| format!("坐标解析失败：{token}"))?,
                );
                let offset = if parts.len() == 4 {
                    Some((
                        parts[2]
                            .parse::<f32>()
                            .map_err(|_| format!("偏移解析失败：{token}"))?,
                        parts[3]
                            .parse::<f32>()
                            .map_err(|_| format!("偏移解析失败：{token}"))?,
                    ))
                } else {
                    None
                };
                let k = match kind {
                    "PATROL" => "MOVE",
                    _ => kind,
                };
                out.push(Checkpoint {
                    kind: k.to_string(),
                    time: 0.0,
                    pos: Some(pos),
                    offset,
                });
            }
            "WAIT" | "WAIT_GLOBAL" | "WAIT_FRAGMENT" | "WAIT_WAVE" => {
                if parts.len() != 1 {
                    return Err(format!("{kind} 需要 1 个参数：{token}"));
                }
                let time = parts[0]
                    .parse::<f64>()
                    .map_err(|_| format!("时间解析失败：{token}"))?;
                out.push(Checkpoint {
                    kind: kind.to_string(),
                    time,
                    pos: None,
                    offset: None,
                });
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

pub(super) fn parse_injections(spec: &str) -> Result<Vec<Injection>, String> {
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
        let f = f_str
            .parse::<i64>()
            .map_err(|_| format!("注入帧号解析失败：{token}"))?;
        let kv = parse_kv(body);
        let get_f32 = |k: &str| -> Result<f32, String> {
            kv.get(k)
                .ok_or_else(|| format!("缺少参数 {k}：{token}"))?
                .parse::<f32>()
                .map_err(|_| format!("参数 {k} 解析失败：{token}"))
        };
        let get_i64 = |k: &str| -> Result<i64, String> {
            kv.get(k)
                .ok_or_else(|| format!("缺少参数 {k}：{token}"))?
                .parse::<i64>()
                .map_err(|_| format!("参数 {k} 解析失败：{token}"))
        };
        let get_pair = |k: &str| -> Result<(f32, f32), String> {
            let v = kv.get(k).ok_or_else(|| format!("缺少参数 {k}：{token}"))?;
            let mut it = v.split(',');
            let a = it
                .next()
                .ok_or_else(|| format!("参数 {k} 解析失败：{token}"))?
                .trim()
                .parse::<f32>()
                .map_err(|_| format!("参数 {k} 解析失败：{token}"))?;
            let b = it
                .next()
                .ok_or_else(|| format!("参数 {k} 解析失败：{token}"))?
                .trim()
                .parse::<f32>()
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
                    kind: "push".into(),
                    f,
                    dir: Some((dir.0 / l, dir.1 / l)),
                    dist: Some(get_f32("dist")?),
                    dur: get_i64("dur")?,
                    at: None,
                    pos: None,
                });
            }
            "block" => {
                // dur = 部署帧到撤退帧的总帧数（dur=0 零长度事件，无效果），不得为负
                let dur = match kv.get("dur") {
                    Some(v) => v
                        .parse::<i64>()
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
                let pos = if kv.contains_key("pos") {
                    Some(get_pair("pos")?)
                } else {
                    None
                };
                if at.is_none() && pos.is_none() {
                    return Err(format!("block 需要 at 或 pos：{token}"));
                }
                if at.is_some() && pos.is_some() {
                    return Err(format!("block 的 at 与 pos 互斥：{token}"));
                }
                events.push(Injection {
                    kind: "block".into(),
                    f,
                    dir: None,
                    dist: None,
                    dur,
                    at,
                    pos,
                });
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
        let pushes = events[i..=j]
            .iter()
            .filter(|e| e.kind == "push")
            .count();
        if pushes > 1 {
            return Err(format!("同帧多个推拉注入：帧 {}", events[i].f));
        }
        for a in i..=j {
            for b in a + 1..=j {
                if let (Some(at1), Some(at2)) = (events[a].at, events[b].at) {
                    if at1 == at2 {
                        return Err(format!(
                            "同帧同位置阻挡注入：帧 {} at={:?}",
                            events[i].f, at1
                        ));
                    }
                }
            }
        }
        i = j + 1;
    }
    Ok(events)
}
