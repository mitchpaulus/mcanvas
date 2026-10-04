//! Geometry in canvas units, independent of zoom and the UI.
use crate::doc::{Arrow, Routing, Side, Trunk};

pub const MERGE_DISTANCE: f64 = 12.0;
const CLEARANCE: f64 = 24.0;
pub type Point = (f64, f64);
#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}
// Reject unusable geometry before building grids or SVG dimensions.
const MAX_COORDINATE: f64 = 10_000_000.0;
fn valid_point(p: Point) -> bool {
    p.0.is_finite() && p.1.is_finite() && p.0.abs() <= MAX_COORDINATE && p.1.abs() <= MAX_COORDINATE
}
impl Rect {
    pub fn valid(self) -> bool {
        self.w.is_finite()
            && self.h.is_finite()
            && self.w > 0.0
            && self.h > 0.0
            && valid_point((self.x, self.y))
            && valid_point((self.x + self.w, self.y + self.h))
    }

    pub fn port(self, side: Side) -> Point {
        match side {
            Side::North => (self.x + self.w / 2.0, self.y),
            Side::East => (self.x + self.w, self.y + self.h / 2.0),
            Side::South => (self.x + self.w / 2.0, self.y + self.h),
            Side::West => (self.x, self.y + self.h / 2.0),
        }
    }
    fn blocks(self, a: Point, b: Point) -> bool {
        if a.0 == b.0 {
            a.0 > self.x
                && a.0 < self.x + self.w
                && a.1.min(b.1) < self.y + self.h
                && a.1.max(b.1) > self.y
        } else {
            a.1 > self.y
                && a.1 < self.y + self.h
                && a.0.min(b.0) < self.x + self.w
                && a.0.max(b.0) > self.x
        }
    }
}
fn direction(a: Point, b: Point) -> usize {
    if b.0 > a.0 {
        1
    } else if b.0 < a.0 {
        3
    } else if b.1 > a.1 {
        2
    } else {
        0
    }
}
fn side_direction(side: Side) -> usize {
    match side {
        Side::North => 0,
        Side::East => 1,
        Side::South => 2,
        Side::West => 3,
    }
}
fn opposite(side: Side) -> Side {
    match side {
        Side::North => Side::South,
        Side::East => Side::West,
        Side::South => Side::North,
        Side::West => Side::East,
    }
}
fn outward_by(p: Point, side: Side, distance: f64) -> Point {
    match side {
        Side::North => (p.0, p.1 - distance),
        Side::East => (p.0 + distance, p.1),
        Side::South => (p.0, p.1 + distance),
        Side::West => (p.0 - distance, p.1),
    }
}
fn port_stubs(start: Point, sa: Side, end: Point, sb: Side) -> (Point, Point) {
    // Facing ports in a narrow gap must not overshoot each other's bends.
    let gap = match sa {
        Side::North => start.1 - end.1,
        Side::East => end.0 - start.0,
        Side::South => end.1 - start.1,
        Side::West => start.0 - end.0,
    };
    let distance = if sb == opposite(sa) && gap > 0.0 {
        CLEARANCE.min(gap / 2.0)
    } else {
        CLEARANCE
    };
    (
        outward_by(start, sa, distance),
        outward_by(end, sb, distance),
    )
}
#[derive(Clone, Debug)]
pub struct Route {
    pub points: Vec<Point>,
    pub obstacles: [Rect; 2],
    pub orthogonal: bool,
    pub manual: bool,
    pub suspended: bool,
}

pub fn route(a: Rect, sa: Side, b: Rect, sb: Side, style: Routing) -> Route {
    let start = a.port(sa);
    let end = b.port(sb);
    let mut result = Route {
        points: vec![start, end],
        obstacles: [a, b],
        orthogonal: style == Routing::Orthogonal,
        manual: false,
        suspended: false,
    };
    if !a.valid() || !b.valid() {
        result.points.clear();
        return result;
    }
    if !result.orthogonal {
        return result;
    }
    let (s, t) = port_stubs(start, sa, end, sb);
    let middle =
        connector(s, t, [a, b], sa, Some(opposite(sb))).unwrap_or_else(|| vec![s, (t.0, s.1), t]);
    result.points = vec![start];
    result.points.extend(middle);
    result.points.push(end);
    simplify(&mut result.points);
    result
}
/// Shortest orthogonal connector between two points outside the endpoint nodes.
fn connector(
    s: Point,
    t: Point,
    obstacles: [Rect; 2],
    sa: Side,
    end_direction: Option<Side>,
) -> Option<Vec<Point>> {
    let [a, b] = obstacles;
    if !a.valid() || !b.valid() || !valid_point(s) || !valid_point(t) {
        return None;
    }
    let mut xs = vec![s.0, t.0, (s.0 + t.0) / 2.0];
    let mut ys = vec![s.1, t.1, (s.1 + t.1) / 2.0];
    for r in [a, b] {
        xs.extend([r.x - CLEARANCE, r.x + r.w + CLEARANCE]);
        ys.extend([r.y - CLEARANCE, r.y + r.h + CLEARANCE]);
    }
    xs.sort_by(f64::total_cmp);
    xs.dedup();
    ys.sort_by(f64::total_cmp);
    ys.dedup();
    let points: Vec<Point> = ys
        .iter()
        .flat_map(|y| xs.iter().map(move |x| (*x, *y)))
        .collect();
    let si = points.iter().position(|p| *p == s)?;
    let ti = points.iter().position(|p| *p == t)?;
    // Dijkstra over a small visibility grid. Direction is part of the state,
    // with a bend penalty to prefer a few long runs over many short ones.
    // Break equal-cost ties by distance from the midpoint, integrated along
    // each segment. This is additive (even when an edge is subdivided), so
    // centered elbows win without accepting a longer or more complex route.
    let n = points.len() * 4;
    let mut dist = vec![f64::INFINITY; n];
    let mut centering = vec![f64::INFINITY; n];
    let midpoint = ((s.0 + t.0) / 2.0, (s.1 + t.1) / 2.0);
    let mut prev = vec![None; n];
    let mut done = vec![false; n];
    let sd = side_direction(sa);
    dist[si * 4 + sd] = 0.0;
    centering[si * 4 + sd] = 0.0;
    while let Some(u) = (0..n)
        .filter(|i| !done[*i] && dist[*i].is_finite())
        .min_by(|i, j| {
            dist[*i]
                .total_cmp(&dist[*j])
                .then_with(|| centering[*i].total_cmp(&centering[*j]))
        })
    {
        done[u] = true;
        let p = points[u / 4];
        for (j, &q) in points.iter().enumerate() {
            if p == q || (p.0 != q.0 && p.1 != q.1) || [a, b].iter().any(|r| r.blocks(p, q)) {
                continue;
            }
            let d = direction(p, q);
            if d == (u % 4 + 2) % 4 {
                continue;
            }
            let v = j * 4 + d;
            let cost = dist[u]
                + (p.0 - q.0).abs()
                + (p.1 - q.1).abs()
                + if d != u % 4 { 16.0 } else { 0.0 };
            let center_cost = centering[u]
                + if p.0 == q.0 {
                    (p.1 - q.1).abs() * (p.0 - midpoint.0).abs()
                } else {
                    (p.0 - q.0).abs() * (p.1 - midpoint.1).abs()
                };
            if cost < dist[v] || (cost == dist[v] && center_cost < centering[v]) {
                dist[v] = cost;
                centering[v] = center_cost;
                prev[v] = Some(u);
            }
        }
    }
    let terminal_cost = |u: usize| {
        dist[u]
            + if end_direction.is_some_and(|side| side_direction(side) != u % 4) {
                16.0
            } else {
                0.0
            }
    };
    let mut u = (ti * 4..ti * 4 + 4)
        .filter(|u| end_direction.is_none_or(|side| *u % 4 != (side_direction(side) + 2) % 4))
        .min_by(|a, b| {
            terminal_cost(*a)
                .total_cmp(&terminal_cost(*b))
                .then_with(|| centering[*a].total_cmp(&centering[*b]))
        })?;
    let mut middle = Vec::new();
    if dist[u].is_finite() {
        loop {
            middle.push(points[u / 4]);
            if let Some(v) = prev[u] {
                u = v;
            } else {
                break;
            }
        }
        middle.reverse();
    } else {
        return None;
    }
    Some(middle)
}

/// Keep the exact shared segment; route both ends around the connected nodes.
/// If a node covers the shared segment, callers retain the saved link and show
/// the ordinary route until the node is moved out of the way.
pub fn route_through(a: Rect, sa: Side, b: Rect, sb: Side, trunk: &Trunk) -> Option<Route> {
    if !a.valid()
        || !b.valid()
        || !trunk.coordinate.is_finite()
        || !trunk.start.is_finite()
        || !trunk.end.is_finite()
        || trunk.start >= trunk.end
    {
        return None;
    }
    let point = |v| {
        if trunk.vertical {
            (trunk.coordinate, v)
        } else {
            (v, trunk.coordinate)
        }
    };
    let (u, v) = (point(trunk.start), point(trunk.end));
    if !valid_point(u) || !valid_point(v) {
        return None;
    }
    let start = a.port(sa);
    let end = b.port(sb);
    let (s, t) = port_stubs(start, sa, end, sb);
    let obstacles = [a, b];
    if [(start, s), (t, end), (u, v)]
        .iter()
        .any(|&(p, q)| obstacles.iter().any(|r| r.blocks(p, q)))
    {
        return None;
    }
    let mut best: Option<(f64, Vec<Point>)> = None;
    for (u, v) in [(u, v), (v, u)] {
        let travel = match direction(u, v) {
            0 => Side::North,
            1 => Side::East,
            2 => Side::South,
            _ => Side::West,
        };
        let Some(left) = connector(s, u, obstacles, sa, Some(travel)) else {
            continue;
        };
        let Some(right) = connector(v, t, obstacles, travel, Some(opposite(sb))) else {
            continue;
        };
        let mut points = vec![start];
        points.extend(left);
        points.extend(right);
        points.push(end);
        simplify(&mut points);
        if !simple(&points) {
            continue;
        }
        let cost = points
            .windows(2)
            .map(|w| (w[0].0 - w[1].0).abs() + (w[0].1 - w[1].1).abs())
            .sum::<f64>()
            + points.len() as f64 * 16.0;
        if best.as_ref().is_none_or(|(c, _)| cost < *c) {
            best = Some((cost, points));
        }
    }
    best.map(|(_, points)| Route {
        points,
        obstacles,
        orthogonal: true,
        manual: true,
        suspended: false,
    })
}

fn length(points: &[Point]) -> f64 {
    points
        .windows(2)
        .map(|w| (w[0].0 - w[1].0).abs() + (w[0].1 - w[1].1).abs())
        .sum()
}

/// Shared routes must not retrace themselves or form loops.
fn simple(points: &[Point]) -> bool {
    if points.len() < 2 || !points.iter().copied().all(valid_point) {
        return false;
    }
    for w in points.windows(3) {
        let (a, b, c) = (w[0], w[1], w[2]);
        if (b.0 - a.0) * (c.0 - b.0) + (b.1 - a.1) * (c.1 - b.1) < 0.0 {
            return false;
        }
    }
    for (i, a) in points.windows(2).enumerate() {
        for b in points.windows(2).skip(i + 2) {
            if a[0].0.min(a[1].0) <= b[0].0.max(b[1].0)
                && b[0].0.min(b[1].0) <= a[0].0.max(a[1].0)
                && a[0].1.min(a[1].1) <= b[0].1.max(b[1].1)
                && b[0].1.min(b[1].1) <= a[0].1.max(a[1].1)
            {
                return false;
            }
        }
    }
    true
}

/// Resolve every saved group atomically from the current node geometry. A group
/// either shares one feasible run or all of its visible arrows remain ordinary
/// routes marked suspended. Never mutate the saved intent during routing.
pub fn resolve_links(arrows: &[Arrow], routes: &mut [(usize, Route)]) {
    let mut groups = std::collections::BTreeMap::<&str, Vec<usize>>::new();
    for (i, e) in arrows.iter().enumerate() {
        if let Some(t) = &e.trunk {
            groups.entry(&t.id).or_default().push(i);
        }
    }
    for (id, members) in groups {
        let positions: Vec<usize> = routes
            .iter()
            .enumerate()
            .filter(|(_, (i, _))| members.contains(i))
            .map(|(j, _)| j)
            .collect();
        for &j in &positions {
            routes[j].1.suspended = true;
        }
        // UI creates pairs. Bound work for malformed or hand-authored files.
        if id.is_empty()
            || members.len() < 2
            || members.len() > 16
            || positions.len() != members.len()
        {
            continue;
        }
        let intent = arrows[members[0]].trunk.as_ref().unwrap();
        if !intent.coordinate.is_finite()
            || !intent.start.is_finite()
            || !intent.end.is_finite()
            || intent.start >= intent.end
            || members.iter().any(|&i| {
                arrows[i].routing != Routing::Orthogonal || arrows[i].trunk.as_ref() != Some(intent)
            })
        {
            continue;
        }
        let mut runs = Vec::new();
        for &j in &positions {
            let p = &routes[j].1.points;
            let segments: Vec<(f64, f64, f64)> = (1..p.len().saturating_sub(2))
                .filter_map(|k| {
                    let (a, b) = (p[k], p[k + 1]);
                    if a == b || (a.0 == b.0) != intent.vertical {
                        return None;
                    }
                    Some(if intent.vertical {
                        (a.0, a.1.min(b.1), a.1.max(b.1))
                    } else {
                        (a.1, a.0.min(b.0), a.0.max(b.0))
                    })
                })
                .collect();
            runs.push(segments);
        }
        if runs.iter().any(Vec::is_empty) {
            continue;
        }
        let obstacles: Vec<Rect> = positions
            .iter()
            .flat_map(|&j| routes[j].1.obstacles)
            .collect();
        let preferred = positions
            .iter()
            .map(|&j| {
                let p = &routes[j].1.points;
                let (a, b) = (p[0], p[p.len() - 1]);
                if intent.vertical {
                    (a.0 + b.0) / 2.0
                } else {
                    (a.1 + b.1) / 2.0
                }
            })
            .sum::<f64>()
            / positions.len() as f64;
        let lane = lane_toward(
            preferred,
            positions.iter().map(|&j| &routes[j].1),
            intent.vertical,
        );
        let mut best: Option<(f64, f64, f64, Vec<Route>)> = None;
        // Generate a bounded set of candidates from current runs; old absolute
        // coordinates are only the saved selection, never an immovable anchor.
        let mut candidates = Vec::new();
        for &(coord, lo, hi) in &runs[0] {
            let mut low = lo;
            let mut high = hi;
            let mut coords = vec![coord];
            for other in &runs[1..] {
                if let Some(&(c, l, h)) = other
                    .iter()
                    .filter(|(_, l, h)| low.max(*l) <= high.min(*h))
                    .max_by(|a, b| {
                        (high.min(a.2) - low.max(a.1)).total_cmp(&(high.min(b.2) - low.max(b.1)))
                    })
                {
                    low = low.max(l);
                    high = high.min(h);
                    coords.push(c);
                } else {
                    high = low - 1.0;
                    break;
                }
            }
            if high < low {
                continue;
            }
            coords.push(coords.iter().sum::<f64>() / coords.len() as f64);
            coords.push(preferred);
            coords.extend(lane);
            for c in coords {
                candidates.push((c, low, high));
            }
        }
        for (coordinate, start, end) in candidates.into_iter().take(64) {
            let trunk = Trunk {
                id: id.into(),
                vertical: intent.vertical,
                coordinate,
                start,
                end,
            };
            let mut proposed = Vec::new();
            let mut score = 0.0;
            for &j in &positions {
                let (i, base) = &routes[j];
                let e = &arrows[*i];
                // Prefer moving the chosen run directly. This also supports
                // branches which meet at a shared stem without overlapping.
                let aligned = (1..base.points.len().saturating_sub(2))
                    .filter_map(|k| {
                        let (a, b) = (base.points[k], base.points[k + 1]);
                        if (a.0 == b.0) != intent.vertical {
                            return None;
                        }
                        let (lo, hi) = if intent.vertical {
                            (a.1.min(b.1), a.1.max(b.1))
                        } else {
                            (a.0.min(b.0), a.0.max(b.0))
                        };
                        if lo > start || hi < end {
                            return None;
                        }
                        let mut r = base.clone();
                        if intent.vertical {
                            r.points[k].0 = coordinate;
                            r.points[k + 1].0 = coordinate;
                        } else {
                            r.points[k].1 = coordinate;
                            r.points[k + 1].1 = coordinate;
                        }
                        if !simple(&r.points)
                            || r.points
                                .windows(2)
                                .any(|w| obstacles.iter().any(|o| o.blocks(w[0], w[1])))
                            || !endpoint_directions_preserved(&base.points, &r.points)
                        {
                            return None;
                        }
                        r.manual = true;
                        r.suspended = false;
                        Some(r)
                    })
                    .min_by(|a, b| length(&a.points).total_cmp(&length(&b.points)));
                let Some(r) = aligned.or_else(|| {
                    route_through(
                        base.obstacles[0],
                        e.from_side,
                        base.obstacles[1],
                        e.to_side,
                        &trunk,
                    )
                }) else {
                    break;
                };
                let cost = length(&r.points);
                if !simple(&r.points)
                    || cost > length(&base.points) * 1.5 + 2.0 * CLEARANCE
                    || r.points
                        .windows(2)
                        .any(|w| obstacles.iter().any(|o| o.blocks(w[0], w[1])))
                {
                    break;
                }
                score += cost;
                proposed.push(r);
            }
            let center_distance = (coordinate - preferred).abs();
            if proposed.len() == positions.len()
                && best
                    .as_ref()
                    .is_none_or(|(s, c, l, _)| (score, center_distance, coordinate) < (*s, *c, *l))
            {
                best = Some((score, center_distance, coordinate, proposed));
            }
        }
        if let Some((_, _, _, proposed)) = best {
            for (&j, r) in positions.iter().zip(proposed) {
                routes[j].1 = r;
            }
        }
    }
}

/// The lane closest to `preferred` that lies between every route's ports, kept
/// a clearance away from them when the common band is wide enough. The mean of
/// the midpoints can fall outside a narrow overlap; this keeps a usable middle.
fn lane_toward<'a>(
    preferred: f64,
    routes: impl Iterator<Item = &'a Route>,
    vertical: bool,
) -> Option<f64> {
    let (mut lo, mut hi) = (f64::NEG_INFINITY, f64::INFINITY);
    for r in routes {
        let (a, b) = (r.points[0], r.points[r.points.len() - 1]);
        let (a, b) = if vertical { (a.0, b.0) } else { (a.1, b.1) };
        lo = lo.max(a.min(b));
        hi = hi.min(a.max(b));
    }
    (lo < hi).then(|| {
        let inset = CLEARANCE.min((hi - lo) / 2.0);
        preferred.clamp(lo + inset, hi - inset)
    })
}

/// Parallel middle runs must have a common span to form a shared segment.
pub fn shared_trunk(a: &Route, ka: usize, b: &Route, kb: usize) -> Option<Trunk> {
    if !a.orthogonal
        || !b.orthogonal
        || ka == 0
        || kb == 0
        || ka >= a.points.len().saturating_sub(2)
        || kb >= b.points.len().saturating_sub(2)
    {
        return None;
    }
    let (p, q, r, s) = (
        a.points[ka],
        a.points[ka + 1],
        b.points[kb],
        b.points[kb + 1],
    );
    let vertical = p.0 == q.0;
    if p == q || r == s || vertical != (r.0 == s.0) {
        return None;
    }
    let (start, end, coordinate) = if vertical {
        (
            p.1.min(q.1).max(r.1.min(s.1)),
            p.1.max(q.1).min(r.1.max(s.1)),
            p.0,
        )
    } else {
        (
            p.0.min(q.0).max(r.0.min(s.0)),
            p.0.max(q.0).min(r.0.max(s.0)),
            p.1,
        )
    };
    (end >= start).then(|| Trunk {
        id: crate::doc::new_id(),
        vertical,
        coordinate,
        start: if end == start { start - 0.5 } else { start },
        end: if end == start { end + 0.5 } else { end },
    })
}

fn endpoint_directions_preserved(old: &[Point], new: &[Point]) -> bool {
    old.len() == new.len()
        && old.len() >= 2
        && [0, old.len() - 2].into_iter().all(|j| {
            (new[j + 1].0 - new[j].0) * (old[j + 1].0 - old[j].0)
                + (new[j + 1].1 - new[j].1) * (old[j + 1].1 - old[j].1)
                > 0.0
        })
}

fn simplify(p: &mut Vec<Point>) {
    p.dedup();
    let mut i = 1;
    while i + 1 < p.len() {
        let (a, b, c) = (p[i - 1], p[i], p[i + 1]);
        if ((a.0 == b.0 && b.0 == c.0) || (a.1 == b.1 && b.1 == c.1))
            && (b.0 - a.0) * (c.0 - b.0) + (b.1 - a.1) * (c.1 - b.1) >= 0.0
        {
            p.remove(i);
        } else {
            i += 1;
        }
    }
}
fn shifted_run(route: &Route, k: usize, vertical: bool, coordinate: f64) -> Option<Vec<Point>> {
    let mut p = route.points.clone();
    if vertical {
        p[k].0 = coordinate;
        p[k + 1].0 = coordinate;
    } else {
        p[k].1 = coordinate;
        p[k + 1].1 = coordinate;
    }
    (endpoint_directions_preserved(&route.points, &p)
        && simple(&p)
        && p.windows(2)
            .all(|w| !route.obstacles.iter().any(|r| r.blocks(w[0], w[1]))))
    .then_some(p)
}

/// Give a shared-port fan-out one central lane before proximity snapping. For
/// each orientation, center all its interior runs together, or leave the safe
/// individual routes intact when a common midpoint is obstructed.
fn center_branches(routes: &mut [Route]) {
    for vertical in [false, true] {
        for from_start in [true, false] {
            let mut groups = std::collections::BTreeMap::<(u64, u64), Vec<(usize, usize)>>::new();
            for (i, r) in routes.iter().enumerate() {
                if !r.orthogonal || r.manual || r.suspended || r.points.len() != 4 {
                    continue;
                }
                let (a, b) = (r.points[1], r.points[2]);
                if a == b || (a.0 == b.0) != vertical {
                    continue;
                }
                let port = if from_start { r.points[0] } else { r.points[3] };
                groups
                    .entry((port.0.to_bits(), port.1.to_bits()))
                    .or_default()
                    .push((i, 1));
            }
            for members in groups.values().filter(|m| m.len() > 1) {
                let coordinate = members
                    .iter()
                    .map(|&(i, _)| {
                        let p = &routes[i].points;
                        if vertical {
                            (p[0].0 + p[3].0) / 2.0
                        } else {
                            (p[0].1 + p[3].1) / 2.0
                        }
                    })
                    .sum::<f64>()
                    / members.len() as f64;
                let shift = |coordinate| -> Option<Vec<_>> {
                    members
                        .iter()
                        .map(|&(i, k)| shifted_run(&routes[i], k, vertical, coordinate))
                        .collect()
                };
                let proposals = shift(coordinate).or_else(|| {
                    lane_toward(
                        coordinate,
                        members.iter().map(|&(i, _)| &routes[i]),
                        vertical,
                    )
                    .and_then(shift)
                });
                if let Some(proposals) = proposals {
                    for (&(i, _), p) in members.iter().zip(proposals) {
                        routes[i].points = p;
                    }
                }
            }
        }
    }
}

/// Align overlapping, parallel interior segments to earlier routes. End segments
/// stay pinned to their cardinal ports; proposed moves cannot cross either node.
pub fn normalize(routes: &mut [Route]) {
    center_branches(routes);
    for i in 0..routes.len() {
        if !routes[i].orthogonal || routes[i].manual || routes[i].suspended {
            continue;
        }
        for k in 1..routes[i].points.len().saturating_sub(2) {
            let (a, b) = (routes[i].points[k], routes[i].points[k + 1]);
            let vertical = a.0 == b.0;
            let coord = if vertical { a.0 } else { a.1 };
            let mut candidates = Vec::new();
            for earlier in &routes[..i] {
                if !earlier.orthogonal || earlier.suspended {
                    continue;
                }
                for pair in earlier.points.windows(2) {
                    let (c, d) = (pair[0], pair[1]);
                    if (c.0 == d.0) != vertical {
                        continue;
                    }
                    let (lo, hi, lo2, hi2, v) = if vertical {
                        (a.1.min(b.1), a.1.max(b.1), c.1.min(d.1), c.1.max(d.1), c.0)
                    } else {
                        (a.0.min(b.0), a.0.max(b.0), c.0.min(d.0), c.0.max(d.0), c.1)
                    };
                    let shared_port = routes[i].points.first() == earlier.points.first()
                        || routes[i].points.last() == earlier.points.last();
                    if (lo.max(lo2) < hi.min(hi2) && (coord - v).abs() <= MERGE_DISTANCE)
                        || (shared_port && lo.max(lo2) <= hi.min(hi2))
                    {
                        candidates.push(v);
                    }
                }
            }
            candidates.sort_by(|a, b| (coord - a).abs().total_cmp(&(coord - b).abs()));
            for v in candidates {
                let mut p = routes[i].points.clone();
                if vertical {
                    p[k].0 = v;
                    p[k + 1].0 = v;
                } else {
                    p[k].1 = v;
                    p[k + 1].1 = v;
                }
                let old = &routes[i].points;
                let direction_ok = [0, p.len() - 2].into_iter().all(|j| {
                    (p[j + 1].0 - p[j].0) * (old[j + 1].0 - old[j].0)
                        + (p[j + 1].1 - p[j].1) * (old[j + 1].1 - old[j].1)
                        > 0.0
                });
                if direction_ok
                    && simple(&p)
                    && p.windows(2)
                        .all(|w| !routes[i].obstacles.iter().any(|r| r.blocks(w[0], w[1])))
                {
                    routes[i].points = p;
                    break;
                }
            }
        }
    }
}
pub fn distance(p: Point, points: &[Point]) -> f64 {
    points
        .windows(2)
        .map(|w| {
            let (a, b) = (w[0], w[1]);
            let d = (b.0 - a.0, b.1 - a.1);
            let len = d.0 * d.0 + d.1 * d.1;
            let t = if len == 0.0 {
                0.0
            } else {
                ((p.0 - a.0) * d.0 + (p.1 - a.1) * d.1) / len
            }
            .clamp(0.0, 1.0);
            (p.0 - a.0 - t * d.0).hypot(p.1 - a.1 - t * d.1)
        })
        .fold(f64::INFINITY, f64::min)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: f64, y: f64) -> Rect {
        Rect {
            x,
            y,
            w: 100.0,
            h: 80.0,
        }
    }
    #[test]
    fn all_cardinal_pairs_remain_orthogonal_and_outside_nodes() {
        for b in [
            rect(300.0, 200.0),
            rect(-300.0, -200.0),
            rect(0.0, 300.0),
            rect(300.0, 0.0),
        ] {
            for sa in [Side::North, Side::East, Side::South, Side::West] {
                for sb in [Side::North, Side::East, Side::South, Side::West] {
                    let a = rect(0.0, 0.0);
                    let r = route(a, sa, b, sb, Routing::Orthogonal);
                    assert_eq!(r.points[0], a.port(sa));
                    assert_eq!(*r.points.last().unwrap(), b.port(sb));
                    for w in r.points.windows(2) {
                        assert!(w[0].0 == w[1].0 || w[0].1 == w[1].1, "{r:?}");
                        assert!(!a.blocks(w[0], w[1]) && !b.blocks(w[0], w[1]), "{r:?}");
                    }
                }
            }
        }
    }
    #[test]
    fn screenshot_shared_south_port_has_no_backtrack() {
        // Reconstructed from Screenshot_20261003_191232.png: the left
        // destination is only 32 canvas units below the source's bottom.
        let source = Rect {
            x: 464.0,
            y: 184.0,
            w: 320.0,
            h: 160.0,
        };
        let left = Rect {
            x: 304.0,
            y: 376.0,
            w: 320.0,
            h: 46.0,
        };
        let right = Rect {
            x: 776.0,
            y: 408.0,
            w: 320.0,
            h: 52.0,
        };
        let mut routes = vec![
            route(source, Side::South, left, Side::North, Routing::Orthogonal),
            route(source, Side::South, right, Side::North, Routing::Orthogonal),
        ];
        normalize(&mut routes);
        for r in &routes {
            assert!(simple(&r.points), "{:?}", r.points);
            assert_eq!(r.points[0], source.port(Side::South));
            assert_eq!(r.points[1].0, source.port(Side::South).0);
            assert!(
                r.points.windows(2).all(|w| w[1].1 >= w[0].1),
                "{:?}",
                r.points
            );
            assert_eq!(r.points.len(), 4, "should be a clean three-segment elbow");
        }
        assert_eq!(routes[0].points[1].0, routes[1].points[1].0);
    }

    #[test]
    fn shared_port_branches_align_even_when_spans_only_meet() {
        let source = Rect {
            x: 464.0,
            y: 184.0,
            w: 320.0,
            h: 160.0,
        };
        let left = Rect {
            x: 216.0,
            y: 432.0,
            w: 320.0,
            h: 46.0,
        };
        let right = Rect {
            x: 720.0,
            y: 432.0,
            w: 320.0,
            h: 52.0,
        };
        let make = || {
            vec![
                route(source, Side::South, left, Side::North, Routing::Orthogonal),
                route(source, Side::South, right, Side::North, Routing::Orthogonal),
            ]
        };
        let mut routes = make();
        normalize(&mut routes);
        assert_eq!(routes[0].points[1].1, routes[1].points[1].1);
        assert!(routes.iter().all(|r| simple(&r.points)));
        let raw = make();
        let trunk = shared_trunk(&raw[0], 1, &raw[1], 1).expect("touching spans can link");
        let arrows = (0..2)
            .map(|i| Arrow {
                id: format!("e{i}"),
                from: "source".into(),
                to: format!("target{i}"),
                from_side: Side::South,
                to_side: Side::North,
                routing: Routing::Orthogonal,
                trunk: Some(trunk.clone()),
            })
            .collect::<Vec<_>>();
        let mut linked = raw.into_iter().enumerate().collect::<Vec<_>>();
        resolve_links(&arrows, &mut linked);
        assert!(linked.iter().all(|(_, r)| r.manual && !r.suspended));
        assert_eq!(linked[0].1.points[1].1, linked[1].1.points[1].1);
    }

    #[test]
    fn facing_ports_adapt_to_narrow_gaps_in_every_direction() {
        for gap in [1.0, 8.0, 16.0, 32.0, 47.0, 48.0, 100.0] {
            for sideways in [-200.0, 0.0, 200.0] {
                for side in [Side::North, Side::East, Side::South, Side::West] {
                    let a = rect(0.0, 0.0);
                    let b = match side {
                        Side::North => rect(sideways, -80.0 - gap),
                        Side::East => rect(100.0 + gap, sideways),
                        Side::South => rect(sideways, 80.0 + gap),
                        Side::West => rect(-100.0 - gap, sideways),
                    };
                    let r = route(a, side, b, opposite(side), Routing::Orthogonal);
                    assert!(
                        simple(&r.points),
                        "side {side:?}, gap {gap}: {:?}",
                        r.points
                    );
                    assert!(r
                        .points
                        .windows(2)
                        .all(|w| !a.blocks(w[0], w[1]) && !b.blocks(w[0], w[1])));
                }
            }
        }
    }

    #[test]
    fn straight_is_direct() {
        let a = rect(0.0, 0.0);
        let b = rect(300.0, 200.0);
        assert_eq!(
            route(a, Side::East, b, Side::North, Routing::Straight).points,
            vec![a.port(Side::East), b.port(Side::North)]
        );
    }
    #[test]
    fn normalization_merges_nearby_runs_without_moving_ports() {
        let make = |x| Route {
            points: vec![(100.0, 40.0), (x, 40.0), (x, 240.0), (300.0, 240.0)],
            obstacles: [rect(0.0, 0.0), rect(300.0, 200.0)],
            orthogonal: true,
            manual: false,
            suspended: false,
        };
        let mut distant = make(230.0);
        distant.points = vec![(100.0, 60.0), (230.0, 60.0), (230.0, 260.0), (300.0, 260.0)];
        let mut routes = vec![make(200.0), make(208.0), distant];
        normalize(&mut routes);
        assert_eq!(routes[0].points, routes[1].points);
        assert_eq!(routes[2].points[1].0, 230.0);
        let first = routes[1].points.clone();
        normalize(&mut routes);
        assert_eq!(first, routes[1].points);
    }
    #[test]
    fn normalization_does_not_merge_disjoint_or_straight_lines() {
        let obstacles = [rect(-500.0, -500.0); 2];
        let a = Route {
            points: vec![(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (200.0, 100.0)],
            obstacles,
            orthogonal: true,
            manual: false,
            suspended: false,
        };
        let b = Route {
            points: vec![(0.0, 200.0), (108.0, 200.0), (108.0, 300.0), (200.0, 300.0)],
            obstacles,
            orthogonal: true,
            manual: false,
            suspended: false,
        };
        let mut routes = vec![a, b.clone()];
        normalize(&mut routes);
        assert_eq!(routes[1].points, b.points);
        routes[0].orthogonal = false;
        routes[1].points = vec![(0.0, 0.0), (108.0, 0.0), (108.0, 100.0), (200.0, 100.0)];
        normalize(&mut routes);
        assert_eq!(routes[1].points[1].0, 108.0);
    }
    #[test]
    fn manual_link_survives_movement_and_automatic_normalization() {
        let trunk = Trunk {
            id: "link".into(),
            vertical: true,
            coordinate: 200.0,
            start: 100.0,
            end: 180.0,
        };
        let a = rect(0.0, 0.0);
        let b = rect(400.0, 240.0);
        let c = rect(24.0, 40.0);
        let d = rect(424.0, 280.0);
        let first = route_through(a, Side::East, b, Side::West, &trunk).unwrap();
        let second = route_through(c, Side::East, d, Side::West, &trunk).unwrap();
        let mut routes = vec![first.clone(), second.clone()];
        normalize(&mut routes);
        assert_eq!(routes[0].points, first.points);
        assert_eq!(routes[1].points, second.points);
        for r in &routes {
            assert!(r.manual);
            assert_eq!(distance((200.0, 100.0), &r.points), 0.0);
            assert_eq!(distance((200.0, 140.0), &r.points), 0.0);
            assert_eq!(distance((200.0, 180.0), &r.points), 0.0);
            for w in r.points.windows(2) {
                assert!(w[0].0 == w[1].0 || w[0].1 == w[1].1);
                assert!(r.obstacles.iter().all(|o| !o.blocks(w[0], w[1])));
            }
        }
        assert_eq!(routes[1].points[0], c.port(Side::East));
        assert_eq!(*routes[1].points.last().unwrap(), d.port(Side::West));
    }

    #[test]
    fn manual_links_work_horizontally_and_report_obstructions() {
        let trunk = Trunk {
            id: "link".into(),
            vertical: false,
            coordinate: 160.0,
            start: 140.0,
            end: 220.0,
        };
        let r = route_through(
            rect(0.0, 0.0),
            Side::South,
            rect(300.0, 300.0),
            Side::North,
            &trunk,
        )
        .unwrap();
        assert_eq!(distance((180.0, 160.0), &r.points), 0.0);
        assert!(route_through(
            rect(140.0, 120.0),
            Side::South,
            rect(300.0, 300.0),
            Side::North,
            &trunk
        )
        .is_none());
    }

    #[test]
    fn middle_selection_and_explicit_link_ignore_distance_limit() {
        let make = |x| Route {
            points: vec![(100.0, 40.0), (x, 40.0), (x, 240.0), (400.0, 240.0)],
            obstacles: [rect(0.0, 0.0), rect(400.0, 200.0)],
            orthogonal: true,
            manual: false,
            suspended: false,
        };
        let a = make(180.0);
        let b = make(300.0);
        let trunk = shared_trunk(&a, 1, &b, 1).unwrap();
        assert_eq!(trunk.coordinate, 180.0);
        assert_eq!((trunk.start, trunk.end), (40.0, 240.0));
        assert!(shared_trunk(&a, 0, &b, 1).is_none());
        let horizontal = Route {
            points: vec![(0.0, 0.0), (0.0, 100.0), (200.0, 100.0), (200.0, 200.0)],
            ..make(100.0)
        };
        assert!(shared_trunk(&a, 1, &horizontal, 1).is_none());
        let disjoint = Route {
            points: vec![
                (100.0, 300.0),
                (250.0, 300.0),
                (250.0, 400.0),
                (400.0, 400.0),
            ],
            ..make(250.0)
        };
        assert!(shared_trunk(&a, 1, &disjoint, 1).is_none());
    }

    fn linked_pair() -> Vec<Arrow> {
        let trunk = Trunk {
            id: "pair".into(),
            vertical: true,
            coordinate: 200.0,
            start: 100.0,
            end: 180.0,
        };
        (0..2)
            .map(|i| Arrow {
                id: format!("e{i}"),
                from: format!("a{i}"),
                to: format!("b{i}"),
                from_side: Side::East,
                to_side: Side::West,
                routing: Routing::Orthogonal,
                trunk: Some(trunk.clone()),
            })
            .collect()
    }
    fn pair_routes(offset: Point) -> Vec<(usize, Route)> {
        vec![
            (
                0,
                route(
                    rect(offset.0, offset.1),
                    Side::East,
                    rect(400.0 + offset.0, 240.0 + offset.1),
                    Side::West,
                    Routing::Orthogonal,
                ),
            ),
            (
                1,
                route(
                    rect(24.0 + offset.0, 40.0 + offset.1),
                    Side::East,
                    rect(424.0 + offset.0, 280.0 + offset.1),
                    Side::West,
                    Routing::Orthogonal,
                ),
            ),
        ]
    }
    #[test]
    fn intent_moves_suspends_and_resumes_without_mutating_document() {
        let arrows = linked_pair();
        let saved = serde_json::to_string(&arrows).unwrap();
        let mut initial = pair_routes((0.0, 0.0));
        resolve_links(&arrows, &mut initial);
        assert!(initial.iter().all(|(_, r)| r.manual && !r.suspended));
        let mut moved = pair_routes((4000.0, -3000.0));
        resolve_links(&arrows, &mut moved);
        for ((_, a), (_, b)) in initial.iter().zip(&moved) {
            assert!(b.manual);
            assert_eq!(
                a.points
                    .iter()
                    .map(|p| (p.0 + 4000.0, p.1 - 3000.0))
                    .collect::<Vec<_>>(),
                b.points
            );
        }
        let mut apart = pair_routes((0.0, 0.0));
        apart[1].1 = route(
            rect(24.0, 2040.0),
            Side::East,
            rect(424.0, 2280.0),
            Side::West,
            Routing::Orthogonal,
        );
        resolve_links(&arrows, &mut apart);
        assert!(apart.iter().all(|(_, r)| r.suspended && !r.manual));
        let reloaded: Vec<Arrow> = serde_json::from_str(&saved).unwrap();
        let mut restored = pair_routes((0.0, 0.0));
        resolve_links(&reloaded, &mut restored);
        assert!(restored.iter().all(|(_, r)| r.manual));
        assert_eq!(saved, serde_json::to_string(&arrows).unwrap());
    }
    #[test]
    fn whole_group_waits_for_missing_or_incompatible_partner() {
        let mut arrows = linked_pair();
        let mut missing = pair_routes((0.0, 0.0));
        missing.pop();
        resolve_links(&arrows, &mut missing);
        assert!(missing[0].1.suspended && !missing[0].1.manual);
        arrows[1].routing = Routing::Straight;
        let mut routes = pair_routes((0.0, 0.0));
        resolve_links(&arrows, &mut routes);
        assert!(routes.iter().all(|(_, r)| r.suspended && !r.manual));
        arrows[1].routing = Routing::Orthogonal;
        let mut routes = pair_routes((0.0, 0.0));
        resolve_links(&arrows, &mut routes);
        assert!(routes.iter().all(|(_, r)| r.manual));
        arrows[1].trunk.as_mut().unwrap().vertical = false;
        let mut routes = pair_routes((0.0, 0.0));
        resolve_links(&arrows, &mut routes);
        assert!(routes.iter().all(|(_, r)| r.suspended && !r.manual));
    }
    #[test]
    fn horizontal_intent_and_enclosed_ports_recover() {
        let mut arrows = linked_pair();
        for e in &mut arrows {
            e.from_side = Side::South;
            e.to_side = Side::North;
            e.trunk.as_mut().unwrap().vertical = false;
        }
        let transposed = || {
            pair_routes((0.0, 0.0))
                .into_iter()
                .map(|(i, r)| {
                    let swap = |a: Rect| Rect {
                        x: a.y,
                        y: a.x,
                        w: a.h,
                        h: a.w,
                    };
                    (
                        i,
                        route(
                            swap(r.obstacles[0]),
                            Side::South,
                            swap(r.obstacles[1]),
                            Side::North,
                            Routing::Orthogonal,
                        ),
                    )
                })
                .collect::<Vec<_>>()
        };
        let mut routes = transposed();
        resolve_links(&arrows, &mut routes);
        assert!(routes.iter().all(|(_, r)| r.manual));
        let mut blocked = transposed();
        blocked[0].1 = route(
            rect(0.0, 0.0),
            Side::South,
            Rect {
                x: -50.0,
                y: -50.0,
                w: 300.0,
                h: 300.0,
            },
            Side::North,
            Routing::Orthogonal,
        );
        resolve_links(&arrows, &mut blocked);
        assert!(blocked.iter().all(|(_, r)| !r.manual && r.suspended));
        let mut recovered = transposed();
        resolve_links(&arrows, &mut recovered);
        assert!(recovered.iter().all(|(_, r)| r.manual));
    }

    #[test]
    fn invalid_geometry_and_indices_never_enter_the_router() {
        let a = rect(0.0, 0.0);
        let b = rect(400.0, 240.0);
        let good = route(a, Side::East, b, Side::West, Routing::Orthogonal);
        assert!(shared_trunk(&good, usize::MAX, &good, 1).is_none());
        for bad in [
            Rect { x: f64::NAN, ..a },
            Rect {
                x: f64::INFINITY,
                ..a
            },
            Rect { x: f64::MAX, ..a },
            Rect { w: 0.0, ..a },
            Rect { h: -1.0, ..a },
        ] {
            assert!(route(bad, Side::East, b, Side::West, Routing::Orthogonal)
                .points
                .is_empty());
            assert!(connector((0.0, 0.0), (1.0, 1.0), [bad, b], Side::East, None).is_none());
        }
        assert!(connector((f64::NAN, 0.0), (0.0, 0.0), [a, b], Side::East, None).is_none());
        assert!(distance((0.0, 0.0), &[]).is_infinite());
        assert_eq!(distance((0.0, 0.0), &[(0.0, 0.0), (0.0, 0.0)]), 0.0);
        let mut arrows = linked_pair();
        arrows[0].trunk.as_mut().unwrap().start = f64::NAN;
        let mut routes = pair_routes((0.0, 0.0));
        resolve_links(&arrows, &mut routes);
        assert!(routes.iter().all(|(_, r)| r.suspended && !r.manual));
    }
    #[test]
    fn varied_layouts_are_finite_atomic_and_safe() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        let sides = [Side::North, Side::East, Side::South, Side::West];
        for _ in 0..160 {
            let mut arrows = linked_pair();
            let mut routes = Vec::new();
            for (i, e) in arrows.iter_mut().enumerate() {
                e.from_side = sides[rng.gen_range(0..4)];
                e.to_side = sides[rng.gen_range(0..4)];
                let a = rect(
                    rng.gen_range(-400..600) as f64,
                    rng.gen_range(-400..600) as f64,
                );
                let b = rect(
                    rng.gen_range(-400..600) as f64,
                    rng.gen_range(-400..600) as f64,
                );
                routes.push((i, route(a, e.from_side, b, e.to_side, e.routing)));
            }
            resolve_links(&arrows, &mut routes);
            assert_eq!(routes[0].1.manual, routes[1].1.manual);
            for (_, r) in &routes {
                assert!(r.points.iter().copied().all(valid_point));
                assert!(r
                    .points
                    .windows(2)
                    .all(|w| w[0].0 == w[1].0 || w[0].1 == w[1].1));
                if r.manual {
                    assert!(simple(&r.points));
                    assert!(r.points.windows(2).all(|w| routes
                        .iter()
                        .all(|(_, other)| other.obstacles.iter().all(|o| !o.blocks(w[0], w[1])))));
                }
            }
        }
    }

    #[test]
    fn hit_testing_uses_segments_not_bounding_boxes() {
        let p = [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)];
        assert_eq!(distance((50.0, 4.0), &p), 4.0);
        assert_eq!(distance((50.0, 50.0), &p), 50.0);
    }

    #[test]
    fn narrow_overlap_link_uses_reachable_middle() {
        // The mean of the midpoints (337.5) is past the first arrow's target,
        // but both arrows can share a lane in the narrow overlap near x = 275.
        let rects = [
            (rect(0.0, 0.0), rect(300.0, 240.0)),
            (rect(150.0, 40.0), rect(700.0, 280.0)),
        ];
        let raw: Vec<Route> = rects
            .iter()
            .map(|(a, b)| route(*a, Side::East, *b, Side::West, Routing::Orthogonal))
            .collect();
        let trunk = shared_trunk(&raw[0], 1, &raw[1], 1).unwrap();
        let arrows = (0..2)
            .map(|i| Arrow {
                id: format!("e{i}"),
                from: format!("a{i}"),
                to: format!("b{i}"),
                from_side: Side::East,
                to_side: Side::West,
                routing: Routing::Orthogonal,
                trunk: Some(trunk.clone()),
            })
            .collect::<Vec<_>>();
        let mut linked = raw.into_iter().enumerate().collect::<Vec<_>>();
        resolve_links(&arrows, &mut linked);
        for (_, r) in &linked {
            assert!(r.manual && !r.suspended, "{:?}", r.points);
            assert_eq!(r.points[1].0, 276.0, "{:?}", r.points);
            assert!(simple(&r.points));
        }
    }

    #[test]
    fn fan_out_centers_within_reachable_band() {
        // The near branch can only bend between y = 160 and 200; the mean of
        // the branch midpoints (280) is unreachable for it.
        let source = Rect {
            x: 0.0,
            y: 0.0,
            w: 320.0,
            h: 160.0,
        };
        let near = rect(-400.0, 200.0);
        let far = rect(400.0, 600.0);
        let mut routes = vec![
            route(source, Side::South, near, Side::North, Routing::Orthogonal),
            route(source, Side::South, far, Side::North, Routing::Orthogonal),
        ];
        center_branches(&mut routes);
        for r in &routes {
            assert_eq!(r.points.len(), 4, "{:?}", r.points);
            assert_eq!(r.points[1].1, 180.0, "{:?}", r.points);
            assert!(simple(&r.points));
        }
    }
}
