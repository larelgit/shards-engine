//! Routes on the navmesh: [`NavRegion::path`] (and how far a straight walk goes,
//! [`NavRegion::reach`]).
//!
//! 1. **Ends**: the start is the node the walker stands on (within a meter); the goal the
//!    node at the goal (its own column first, then within two meters).
//! 2. **Areas**: two ends whose areas never meet fail at once, with the reason.
//! 3. **Corridor**: A* over the tile graph (portals joined inside tiles by their baked
//!    costs and across tiles by the stitches and navlinks), with the start and the goal
//!    joined to their tiles' portals by a search inside those tiles.
//! 4. **Grid A***: over the nodes of the corridor's tiles only (all tiles, within a budget,
//!    if the corridor finds nothing).
//! 5. **String pulling**: each run of walking becomes straight lines where the grid says a
//!    straight walk works (every column crossed joined by a walking link, no corner cut)
//!    and a body-wide sweep through the physics world agrees. Climbs, drops and swims stay
//!    where the search put them.
//!
//! Costs are integers and every tie goes to the lower id, so a route is the same on every
//! run.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use ai_core::spatial::{compass, fmt_pos};
use bevy::prelude::*;
use shards_motion::perception::{Leg, PathPlan, StepKind, COST_PER_METER};
use shards_motion::{LayerMask, PhysicsWorld, QueryFilter, Shape};

use super::region::{NavRegion, NodeId};

/// Two steps whose headings have a dot product above this are one straight run (under 1°).
const STRAIGHT: f32 = 0.9999;
/// How far from the walker's feet its node may be (m).
const START_REACH: f32 = 1.0;
/// How far from the goal its node may be (m).
const GOAL_REACH: f32 = 2.0;
/// A body's goal (another body) is a floor within this height of its feet (m).
const GOAL_BAND: f32 = 2.5;
/// Nodes a search over the whole region may expand.
const FULL_BUDGET: usize = 120_000;
/// How many nodes ahead string pulling looks for a straight line.
const PULL_WINDOW: usize = 48;

/// What to route.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NavQuery {
    /// The walker's feet.
    pub from: Vec3,
    pub to: Vec3,
    /// The goal is a body standing somewhere: its floor is near `to.y`. Otherwise any floor
    /// of the goal's column will do, the one nearest `to.y` first.
    pub goal_level: bool,
    pub radius: f32,
    pub height: f32,
    pub step_height: f32,
}

/// A route on the navmesh.
#[derive(Clone, Debug, PartialEq)]
pub struct NavPath {
    pub start: Vec3,
    /// Where each leg ends; the last at the goal.
    pub points: Vec<Vec3>,
    pub legs: Vec<Leg>,
    /// Horizontal length (m).
    pub length: f32,
    /// The region cells of the tiles it crosses, in order.
    pub tiles: Vec<[i32; 2]>,
    /// Nodes the grid search expanded.
    pub explored: usize,
}

impl NavPath {
    /// As the motion planner reports a route: `path 23.5m, 4 turns: ...`.
    pub fn to_plan(&self) -> PathPlan {
        PathPlan { start: self.start, legs: self.legs.clone(), length: self.length, explored: self.explored }
    }
}

/// Why the navmesh gives no route.
#[derive(Clone, Debug, PartialEq)]
pub enum NavError {
    /// Not the navmesh's to answer: outside the region, a bigger body than the one baked, a
    /// walker off the ground. The local planner answers instead.
    NotCovered(String),
    /// There is no route; why, in words an agent can act on.
    NoRoute(String),
}

impl std::fmt::Display for NavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NavError::NotCovered(why) | NavError::NoRoute(why) => f.write_str(why),
        }
    }
}

fn flat(a: Vec3, b: Vec3) -> f32 {
    Vec2::new(a.x - b.x, a.z - b.z).length()
}

/// A tile's links turned around, by target: `edges[first[n]..first[n + 1]]` are the
/// `(from, cost)` of the links into node `n`.
type Reversed = (Vec<u32>, Vec<(u32, u32)>);

/// One step of a found route: the node, how it is entered, how much it climbs or falls.
#[derive(Clone, Copy, Debug)]
struct Hop {
    node: NodeId,
    pos: Vec3,
    kind: StepKind,
    rise: f32,
}

impl NavRegion {
    /// The route for a body from `query.from` to `query.to` (see the module docs).
    pub fn path(&self, physics: &PhysicsWorld, query: &NavQuery) -> Result<NavPath, NavError> {
        if !self.fits(query.radius, query.height) {
            return Err(NavError::NotCovered(format!(
                "a body of {:.2} m radius is bigger than the {:.2} m the navmesh was baked for",
                query.radius, self.agent_radius
            )));
        }
        let finite = |v: Vec3| v.is_finite();
        if !finite(query.from) || !finite(query.to) {
            return Err(NavError::NoRoute("the start and the goal must be finite coordinates".into()));
        }
        if self.instance_at(self.column_of(query.from)).is_none() {
            return Err(NavError::NotCovered("the start is outside the region".into()));
        }
        if self.instance_at(self.column_of(query.to)).is_none() {
            return Err(NavError::NotCovered("the goal is outside the region".into()));
        }
        let start = self.start_node(query.from).ok_or_else(|| {
            NavError::NotCovered(format!("{} is not on ground the navmesh knows (in the air, on a prop, in a wall)", fmt_pos(query.from)))
        })?;
        let goal = self.goal_node(query).ok_or_else(|| {
            NavError::NoRoute(format!(
                "the goal {} is not on walkable ground: it is inside or on top of something solid, over a drop, or too \
                 tight for the {:.1} m wide body; aim for open ground next to it",
                fmt_pos(query.to),
                2.0 * self.agent_radius
            ))
        })?;
        let tile = |n: NodeId| self.instances[self.split(n).0 as usize].cell;
        if self.component(start) != self.component(goal) {
            let (a, b) = (tile(start), tile(goal));
            // A voxel region is cut into chunks, whose edges are all open; a region of tiles has sockets.
            let (noun, between) = if self.chunked() { ("chunk", "walls, cliffs or water") } else { ("tile", "walls, cliffs, water or tile edges whose sockets do not connect") };
            return Err(NavError::NoRoute(format!(
                "the goal {} is on ground the start cannot reach at all ({noun} ({}, {}) from {noun} ({}, {})): {between} lie between; aim for a point on the start's side",
                fmt_pos(query.to),
                b[0],
                b[1],
                a[0],
                a[1]
            )));
        }
        let corridor = self.corridor(start, goal);
        let found = match &corridor {
            Some(tiles) => self.search(start, goal, Some(&self.widened(tiles)), usize::MAX),
            None => None,
        };
        let found = found.or_else(|| (self.node_count() as usize <= 4 * FULL_BUDGET).then(|| self.search(start, goal, None, FULL_BUDGET)).flatten());
        let Some((hops, explored)) = found else {
            return Err(NavError::NoRoute(format!(
                "no way to {}: the ground there is only reached the other way (drops and ledges that cannot be climbed back) \
                 or through spots that are blocked now",
                fmt_pos(query.to)
            )));
        };
        Ok(self.finish(physics, query, start, goal, hops, explored))
    }

    /// Whether the tile graph has an edge between the graph nodes of `a` and `b` (the two ends
    /// of a stitch, a hop or a navlink). A check that the graph and the components agree.
    #[doc(hidden)]
    pub fn tile_graph_joins(&self, a: NodeId, b: NodeId) -> bool {
        let at = |n: NodeId| self.graph.nodes.iter().position(|g| g.node == Some(n) && g.portal.is_none());
        let Some((a, b)) = at(a).zip(at(b)) else { return false };
        self.graph.edges[self.graph.first[a] as usize..self.graph.first[a + 1] as usize].iter().any(|e| e.0 as usize == b)
    }

    /// The tile graph as one text row per node: equal for equal graphs.
    #[doc(hidden)]
    pub fn tile_graph_digest(&self) -> String {
        let g = &self.graph;
        let rows = (0..g.nodes.len()).map(|n| (g.nodes[n].node, g.nodes[n].portal, &g.edges[g.first[n] as usize..g.first[n + 1] as usize]));
        rows.map(|row| format!("{row:?}")).collect::<Vec<_>>().join("\n")
    }

    /// Whether a body walks from `query.from` straight to `query.to` on the navmesh: every
    /// column on the line entered by a walking link (no drop, no ledge, nothing switched off)
    /// and a body-wide sweep through the physics world meets nothing, ending in the goal's
    /// own column. `None` where the navmesh does not cover the walk (see [`path`](Self::path)).
    pub fn straight_walk(&self, physics: &PhysicsWorld, query: &NavQuery) -> Option<bool> {
        if !self.fits(query.radius, query.height) || !query.from.is_finite() || !query.to.is_finite() {
            return None;
        }
        self.instance_at(self.column_of(query.from))?;
        self.instance_at(self.column_of(query.to))?;
        let start = self.start_node(query.from)?;
        let Some(goal) = self.goal_node(query) else { return Some(false) };
        if self.component(start) != self.component(goal) || self.column_of(query.to) != self.node_column(goal) {
            return Some(false);
        }
        let a = Hop { node: start, pos: query.from, kind: StepKind::Walk, rise: 0.0 };
        let b = Hop { node: goal, pos: Vec3::new(query.to.x, self.node_pos(goal).y, query.to.z), kind: StepKind::Walk, rise: 0.0 };
        Some(self.line_clear(physics, query, a, b))
    }

    /// How far a body standing at `from` goes on the navmesh in compass step `(dx, dz)` (m,
    /// at most `range`): column by column over walking links and drops (and, for a swimmer,
    /// swimming ones), to the far side of the last column it enters. A climb, a step into
    /// water, a tile edge that does not connect or a spot switched off ends it (for a diagonal
    /// step, a side column switched off too). `None` where the navmesh does not know `from`
    /// (outside the region, in the air, on a prop) or the way leaves the region (it knows
    /// nothing beyond); also for a step that is no compass step (`dx` and `dz` each -1, 0 or
    /// 1, not both 0).
    pub fn reach(&self, from: Vec3, (dx, dz): (i32, i32), range: f32) -> Option<f32> {
        let compass = (-1..=1).contains(&dx) && (-1..=1).contains(&dz) && (dx, dz) != (0, 0);
        if !from.is_finite() || !range.is_finite() || !compass {
            return None;
        }
        let dir = Vec2::new(dx as f32, dz as f32).normalize();
        self.instance_at(self.column_of(from))?;
        let mut node = self.start_node(from)?;
        let swimmer = self.is_swim(node);
        let takes = |kind: StepKind| matches!(kind, StepKind::Walk | StepKind::Drop) || (swimmer && kind == StepKind::Swim);
        // From a column's center to its side (or corner) ahead.
        let half = 0.5 * self.cell * Vec2::new(dx as f32, dz as f32).length();
        loop {
            let at = self.node_pos(node);
            let reached = Vec2::new(at.x - from.x, at.z - from.z).dot(dir) + half;
            if reached >= range {
                return Some(range);
            }
            let [cx, cz] = self.node_column(node);
            // A diagonal step passes between two side columns, and a ray that does not run
            // through the corner enters one of them: both must still be open (a bake's diagonal
            // link knows they were, not that nothing switched one off since).
            let sides_open = dx == 0
                || dz == 0
                || (self.move_toward(node, (dx, 0), takes).is_some() && self.move_toward(node, (0, dz), takes).is_some());
            match self.move_toward(node, (dx, dz), takes).filter(|_| sides_open) {
                Some(next) => node = next,
                None if self.instance_at([cx + dx, cz + dz]).is_none() => return None,
                None => return Some(reached.max(0.0)),
            }
        }
    }

    /// The walker's node: in its own column, a floor at most a meter below its feet (or half
    /// a meter above); else the nearest such within a meter.
    fn start_node(&self, from: Vec3) -> Option<NodeId> {
        let fits = |nav: &Self, n: NodeId| {
            let dy = from.y - nav.node_pos(n).y;
            !nav.is_disabled(n) && (-0.5..=1.0).contains(&dy)
        };
        let own = self.nodes_at(self.column_of(from)).filter(|n| fits(self, *n)).min_by(|a, b| {
            let (da, db) = ((self.node_pos(*a).y - from.y).abs(), (self.node_pos(*b).y - from.y).abs());
            da.total_cmp(&db).then(a.cmp(b))
        });
        own.or_else(|| self.nearest_node(from, START_REACH, 1.0).filter(|n| fits(self, *n)))
    }

    /// The goal's node: its own column first (a body's: within its height band; a point's:
    /// the floor nearest its height), else the nearest within two meters.
    fn goal_node(&self, query: &NavQuery) -> Option<NodeId> {
        let band = if query.goal_level { GOAL_BAND } else { f32::INFINITY };
        let to = query.to;
        let own = self
            .nodes_at(self.column_of(to))
            .filter(|n| !self.is_disabled(*n) && (self.node_pos(*n).y - to.y).abs() <= band)
            .min_by(|a, b| {
                let (da, db) = ((self.node_pos(*a).y - to.y).abs(), (self.node_pos(*b).y - to.y).abs());
                da.total_cmp(&db).then(a.cmp(b))
            });
        own.or_else(|| self.nearest_node(to, GOAL_REACH, if query.goal_level { GOAL_BAND } else { 1e6 }))
    }

    /// Cheapest costs from (or, `reverse`, to) local node `n` inside instance `inst`.
    fn inside(&self, inst: u32, n: u32, reverse: bool) -> Vec<u32> {
        let instance = &self.instances[inst as usize];
        let nav = &instance.nav;
        let mut dist = vec![u32::MAX; nav.nodes.len()];
        // Reversed links: who reaches whom.
        let reversed: Option<Reversed> = reverse.then(|| {
            let mut first = vec![0u32; nav.nodes.len() + 1];
            for link in &nav.links {
                first[link.to as usize + 1] += 1;
            }
            for k in 0..nav.nodes.len() {
                first[k + 1] += first[k];
            }
            let mut fill = first.clone();
            let mut edges = vec![(0u32, 0u32); nav.links.len()];
            for from in 0..nav.nodes.len() as u32 {
                for link in nav.links_of(from) {
                    edges[fill[link.to as usize] as usize] = (from, link.cost);
                    fill[link.to as usize] += 1;
                }
            }
            (first, edges)
        });
        if instance.is_disabled(n) {
            return dist;
        }
        dist[n as usize] = 0;
        let mut open = BinaryHeap::new();
        open.push(Reverse((0u32, n)));
        while let Some(Reverse((d, at))) = open.pop() {
            if d != dist[at as usize] {
                continue;
            }
            let mut relax = |to: u32, cost: u32| {
                if instance.is_disabled(to) {
                    return;
                }
                let next = d.saturating_add(cost);
                if next < dist[to as usize] {
                    dist[to as usize] = next;
                    open.push(Reverse((next, to)));
                }
            };
            match &reversed {
                Some((first, edges)) => {
                    for &(from, cost) in &edges[first[at as usize] as usize..first[at as usize + 1] as usize] {
                        relax(from, cost);
                    }
                }
                None => {
                    for link in nav.links_of(at) {
                        relax(link.to, link.cost);
                    }
                }
            }
        }
        dist
    }

    /// The corridor with the chunks around it, in a voxel region: the tile graph measures from
    /// each portal's middle node, so the corridor it picks may leave out the chunk a cheaper
    /// crossing of a seam is in, and a chunk is only a seam in the ground, not a place a route
    /// has a reason to stay in. A region of tiles keeps the corridor as it is: its tiles are
    /// separate places. Sorted, once each.
    fn widened(&self, tiles: &[u32]) -> Vec<u32> {
        if !self.chunked() {
            return tiles.to_vec();
        }
        let mut out = tiles.to_vec();
        for &t in tiles {
            let [cx, cz] = self.instances[t as usize].cell;
            for dz in -1..=1 {
                for dx in -1..=1 {
                    let (x, z) = (cx + dx, cz + dz);
                    if x >= 0 && z >= 0 && (x as u32) < self.size[0] && (z as u32) < self.size[1] {
                        out.extend(self.instance_at([x * self.per as i32, z * self.per as i32]));
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The tiles a route goes through, by A* over the tile graph; `None`: the graph knows no way.
    fn corridor(&self, start: NodeId, goal: NodeId) -> Option<Vec<u32>> {
        let (s_inst, s_n) = self.split(start);
        let (g_inst, g_n) = self.split(goal);
        let graph = &self.graph;
        let from_start = self.inside(s_inst, s_n, false);
        let to_goal = self.inside(g_inst, g_n, true);
        let local = |inst: u32, node: NodeId| node - self.global(inst, 0);
        let n = graph.nodes.len();
        let (start_id, goal_id) = (n as u32, n as u32 + 1);
        let goal_pos = self.node_pos(goal);
        let h = |id: u32| -> u32 {
            match graph.nodes.get(id as usize).and_then(|g| g.node) {
                Some(node) => (flat(self.node_pos(node), goal_pos) * COST_PER_METER) as u32,
                None => 0,
            }
        };
        let mut best = vec![u32::MAX; n + 2];
        let mut parent = vec![u32::MAX; n + 2];
        let mut open = BinaryHeap::new();
        best[start_id as usize] = 0;
        open.push(Reverse((h(start_id).max((flat(self.node_pos(start), goal_pos) * COST_PER_METER) as u32), 0u32, start_id)));
        while let Some(Reverse((_, cost, at))) = open.pop() {
            if cost != best[at as usize] {
                continue;
            }
            if at == goal_id {
                break;
            }
            let mut next: Vec<(u32, u32)> = Vec::new();
            if at == start_id {
                for &k in &graph.of_instance[s_inst as usize] {
                    if let Some(node) = graph.nodes[k as usize].node {
                        let c = from_start[local(s_inst, node) as usize];
                        if c != u32::MAX {
                            next.push((k, c));
                        }
                    }
                }
                if s_inst == g_inst && from_start[g_n as usize] != u32::MAX {
                    next.push((goal_id, from_start[g_n as usize]));
                }
            } else {
                let a = at as usize;
                next.extend(graph.edges[graph.first[a] as usize..graph.first[a + 1] as usize].iter().copied());
                let node = &graph.nodes[a];
                if node.inst == g_inst
                    && let Some(at_node) = node.node
                {
                    let c = to_goal[local(g_inst, at_node) as usize];
                    if c != u32::MAX {
                        next.push((goal_id, c));
                    }
                }
            }
            for (to, c) in next {
                let total = cost.saturating_add(c);
                if total < best[to as usize] {
                    best[to as usize] = total;
                    parent[to as usize] = at;
                    open.push(Reverse((total.saturating_add(h(to)), total, to)));
                }
            }
        }
        if best[goal_id as usize] == u32::MAX {
            return None;
        }
        let mut tiles = vec![s_inst, g_inst];
        let mut at = parent[goal_id as usize];
        while at != start_id && at != u32::MAX {
            tiles.push(graph.nodes[at as usize].inst);
            at = parent[at as usize];
        }
        tiles.sort_unstable();
        tiles.dedup();
        Some(tiles)
    }

    /// Grid A* from `start` to `goal` over the nodes of `tiles` (all with `None`), within
    /// `budget` expansions: the hops after the start, and how many nodes it expanded.
    fn search(&self, start: NodeId, goal: NodeId, tiles: Option<&[u32]>, budget: usize) -> Option<(Vec<Hop>, usize)> {
        // Dense state over the searched tiles: slot of each instance, first index of each slot.
        let count = self.instances.len();
        let mut slot_base = vec![u32::MAX; count];
        let mut size = 0u32;
        for inst in 0..count as u32 {
            if tiles.is_none_or(|t| t.binary_search(&inst).is_ok()) {
                slot_base[inst as usize] = size;
                size += self.instances[inst as usize].nav.nodes.len() as u32;
            }
        }
        let index = |node: NodeId| -> Option<usize> {
            let (inst, n) = self.split(node);
            let base = slot_base[inst as usize];
            (base != u32::MAX).then(|| (base + n) as usize)
        };
        let mut cost = vec![u32::MAX; size as usize];
        let mut parent = vec![u32::MAX; size as usize];
        let mut via: Vec<(StepKind, f32)> = vec![(StepKind::Walk, 0.0); size as usize];
        let mut closed = vec![false; size as usize];
        let goal_pos = self.node_pos(goal);
        let h = |node: NodeId| (flat(self.node_pos(node), goal_pos) * COST_PER_METER) as u32;
        let mut open = BinaryHeap::new();
        cost[index(start)?] = 0;
        open.push(Reverse((h(start), 0u32, start)));
        let mut moves = Vec::with_capacity(16);
        let mut expanded = 0usize;
        while let Some(Reverse((_, g, at))) = open.pop() {
            let i = index(at)?;
            if closed[i] || g != cost[i] {
                continue;
            }
            closed[i] = true;
            if at == goal {
                let mut hops = Vec::new();
                let mut node = goal;
                while node != start {
                    let k = index(node)?;
                    let (kind, rise) = via[k];
                    hops.push(Hop { node, pos: self.node_pos(node), kind, rise });
                    node = parent[k];
                }
                hops.reverse();
                return Some((hops, expanded));
            }
            expanded += 1;
            if expanded > budget {
                return None;
            }
            self.moves(at, &mut moves);
            for &(to, kind, c, rise) in &moves {
                let Some(k) = index(to) else { continue };
                let total = g.saturating_add(c);
                if closed[k] || total >= cost[k] {
                    continue;
                }
                cost[k] = total;
                parent[k] = at;
                via[k] = (kind, rise);
                open.push(Reverse((total.saturating_add(h(to)), total, to)));
            }
        }
        None
    }

    /// From the found hops to legs: the exact goal, straight lines, compass headings.
    fn finish(&self, physics: &PhysicsWorld, query: &NavQuery, start: NodeId, goal: NodeId, mut hops: Vec<Hop>, explored: usize) -> NavPath {
        let goal_pos = self.node_pos(goal);
        // A goal in the goal node's own column: end on it rather than on the column's center.
        if self.column_of(query.to) == self.node_column(goal) && flat(query.to, goal_pos) >= 0.05 {
            let end = Vec3::new(query.to.x, goal_pos.y, query.to.z);
            hops.push(Hop { node: goal, pos: end, kind: StepKind::Walk, rise: 0.0 });
        }
        let start_pos = if self.column_of(query.from) == self.node_column(start) {
            Vec3::new(query.from.x, self.node_pos(start).y, query.from.z)
        } else {
            self.node_pos(start)
        };
        let pulled = self.pull(physics, query, Hop { node: start, pos: start_pos, kind: StepKind::Walk, rise: 0.0 }, &hops);
        let mut tiles: Vec<[i32; 2]> = Vec::new();
        for node in std::iter::once(start).chain(hops.iter().map(|h| h.node)) {
            let cell = self.instances[self.split(node).0 as usize].cell;
            if tiles.last() != Some(&cell) {
                tiles.push(cell);
            }
        }
        let mut legs: Vec<Leg> = Vec::new();
        let mut at = query.from;
        let mut heading = Vec3::ZERO;
        let mut length = 0.0;
        for hop in pulled {
            let delta = Vec3::new(hop.pos.x - at.x, 0.0, hop.pos.z - at.z);
            let len = delta.length();
            if len < 1e-4 {
                continue;
            }
            length += len;
            at = hop.pos;
            // The few centimeters from where the walker stands to its first cell center are
            // no leg of their own.
            if legs.is_empty() && len < 0.3 && hop.kind == StepKind::Walk && hop.rise == 0.0 {
                continue;
            }
            let way = delta / len;
            match legs.last_mut() {
                Some(leg) if leg.kind == hop.kind && matches!(hop.kind, StepKind::Walk | StepKind::Swim) && heading.dot(way) > STRAIGHT => {
                    leg.length += len;
                    leg.end = hop.pos;
                }
                _ => legs.push(Leg { dir: compass(delta), length: len, kind: hop.kind, rise: hop.rise, end: hop.pos }),
            }
            heading = way;
        }
        let points = legs.iter().map(|l| l.end).collect();
        NavPath { start: query.from, points, legs, length, tiles, explored }
    }

    /// String pulling: runs of walking become the fewest straight lines that stay walkable.
    fn pull(&self, physics: &PhysicsWorld, query: &NavQuery, start: Hop, hops: &[Hop]) -> Vec<Hop> {
        let mut out = Vec::with_capacity(hops.len());
        let (mut anchor, mut run): (Hop, Vec<Hop>) = (start, Vec::new());
        for hop in hops {
            if hop.kind == StepKind::Walk {
                run.push(*hop);
                continue;
            }
            out.extend(self.shorten(physics, query, anchor, &run));
            run.clear();
            out.push(*hop);
            anchor = *hop;
        }
        out.extend(self.shorten(physics, query, anchor, &run));
        out
    }

    /// The hops of `run` (walked in order after `anchor`) a straight-line version still
    /// needs: from each kept one, the farthest a clear line reaches (looking a window ahead).
    fn shorten(&self, physics: &PhysicsWorld, query: &NavQuery, anchor: Hop, run: &[Hop]) -> Vec<Hop> {
        let mut kept = Vec::new();
        let (mut from, mut next) = (anchor, 0usize);
        while next < run.len() {
            let mut far = (next + PULL_WINDOW).min(run.len() - 1);
            while far > next && !self.line_clear(physics, query, from, run[far]) {
                far -= 1;
            }
            kept.push(run[far]);
            from = run[far];
            next = far + 1;
        }
        kept
    }

    /// Whether walking straight from `a` to `b` stays on the navmesh (every column crossed
    /// is entered by a walking link, corners only by a diagonal one) and a body-wide sweep
    /// through the physics world at step height meets nothing.
    fn line_clear(&self, physics: &PhysicsWorld, query: &NavQuery, a: Hop, b: Hop) -> bool {
        let mut node = a.node;
        let mut col = self.column_of(a.pos);
        let origin = if col == self.node_column(node) { a.pos } else { self.node_pos(node) };
        col = self.node_column(node);
        let target = self.node_column(b.node);
        let (dx, dz) = (b.pos.x - origin.x, b.pos.z - origin.z);
        let cell = self.cell;
        let step_x = if dx > 0.0 { 1 } else { -1 };
        let step_z = if dz > 0.0 { 1 } else { -1 };
        // Column edges in the world: the grid starts at the region's corner.
        let next_edge = |c: i32, s: i32, corner: f32| corner + (c + if s > 0 { 1 } else { 0 }) as f32 * cell;
        let t_of = |edge: f32, o: f32, d: f32| if d.abs() < 1e-9 { f32::INFINITY } else { (edge - o) / d };
        let mut t_x = t_of(next_edge(col[0], step_x, self.origin.x), origin.x, dx);
        let mut t_z = t_of(next_edge(col[1], step_z, self.origin.y), origin.z, dz);
        let dt_x = if dx.abs() < 1e-9 { f32::INFINITY } else { cell / dx.abs() };
        let dt_z = if dz.abs() < 1e-9 { f32::INFINITY } else { cell / dz.abs() };
        let limit = (target[0] - col[0]).abs() + (target[1] - col[1]).abs() + 2;
        for _ in 0..limit {
            if col == target {
                break;
            }
            let (mx, mz) = if (t_x - t_z).abs() < 1e-6 {
                t_x += dt_x;
                t_z += dt_z;
                (step_x, step_z)
            } else if t_x < t_z {
                t_x += dt_x;
                (step_x, 0)
            } else {
                t_z += dt_z;
                (0, step_z)
            };
            let Some(to) = self.walk_toward(node, (mx, mz)) else { return false };
            node = to;
            col = [col[0] + mx, col[1] + mz];
        }
        if node != b.node {
            return false;
        }
        let lift = Vec3::Y * (query.step_height + query.radius + 0.05);
        let delta = b.pos - origin;
        let Ok(dir) = Dir3::new(delta) else { return true };
        let length = delta.length();
        let ball = Shape::sphere((query.radius * 0.95).max(0.05));
        physics
            .shapecast(&ball, origin + lift, Quat::IDENTITY, dir, length, &QueryFilter::new(LayerMask::WORLD))
            .is_none_or(|hit| hit.distance >= length - 0.05)
    }
}
