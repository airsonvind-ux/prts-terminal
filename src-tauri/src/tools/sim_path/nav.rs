// 寻路层：SPFA + 平整化 + nextNode。

use std::collections::{HashMap, HashSet, VecDeque};

use super::{MapModel, HAZARD_COST};

pub(super) type NavMap = HashMap<(i64, i64), (i64, i64)>;

fn spfa(
    goal: (i64, i64),
    passable: &[Vec<bool>],
    obstacles: &HashSet<(i64, i64)>,
    width: i64,
    height: i64,
) -> NavMap {
    let mut next_node = NavMap::new();
    let mut dist: HashMap<(i64, i64), i64> = HashMap::new();
    let mut in_queue = HashSet::new();
    let mut q = VecDeque::new();
    dist.insert(goal, 0);
    in_queue.insert(goal);
    q.push_back(goal);
    while let Some(g) = q.pop_front() {
        in_queue.remove(&g);
        for nb in [
            (g.0, g.1 + 1),
            (g.0 + 1, g.1),
            (g.0, g.1 - 1),
            (g.0 - 1, g.1),
        ] {
            if nb.0 < 0 || nb.0 >= width || nb.1 < 0 || nb.1 >= height {
                continue;
            }
            if !passable[nb.1 as usize][nb.0 as usize] {
                continue;
            }
            let cost = if obstacles.contains(&nb) {
                HAZARD_COST
            } else {
                1
            };
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

fn check_cells(a: (i64, i64), c: (i64, i64)) -> HashSet<(i64, i64)> {
    let mut cells: HashSet<(i64, i64)> = bresenham_cells(a, c).into_iter().collect();
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

fn is_clear(
    a: (i64, i64),
    c: (i64, i64),
    passable: &[Vec<bool>],
    obstacles: &HashSet<(i64, i64)>,
    width: i64,
    height: i64,
) -> bool {
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

fn flatten(
    goal: (i64, i64),
    passable: &[Vec<bool>],
    obstacles: &HashSet<(i64, i64)>,
    width: i64,
    height: i64,
    next_node: &mut NavMap,
) {
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

pub(super) fn build_nav_map(
    model: &MapModel,
    goal: (i64, i64),
    obstacles: &HashSet<(i64, i64)>,
) -> NavMap {
    let mut nn = spfa(goal, &model.passable, obstacles, model.width, model.height);
    flatten(
        goal,
        &model.passable,
        obstacles,
        model.width,
        model.height,
        &mut nn,
    );
    nn
}
