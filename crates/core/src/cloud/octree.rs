//! A scan's points as an octree of levels of detail, as Potree made standard. The root keeps one point in each square of a
//! grid over the scan's cube, a coarse picture of all of it; each node passes what it doesn't keep to the eighth of its cube
//! the point lies in, whose grid is twice as fine; a node given no more than [`NODE_POINTS`] keeps them all. A picture draws
//! the nodes [`select`] chooses: the largest on screen first, until its budget is spent, so a view of millions of points
//! draws the detail that shows. The points stay where the cloud keeps them; the tree holds their indices, grouped by node.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// An inflow of this many points or fewer stays in one node, which keeps them all.
pub const NODE_POINTS: usize = 25_000;
/// The most a node keeps of a larger inflow: past it, its grid is made coarser.
pub const NODE_MAX: usize = 65_536;
/// Squares of a node's grid along each side of its cube.
const GRID: u32 = 64;
/// The deepest level: a node there keeps all it is given, so points in one place end the tree.
pub const MAX_LEVEL: u8 = 20;
/// Detail finer than this, px, adds nothing to a picture: below a node drawn with its squares smaller, a selection doesn't look.
pub const FINEST: f64 = 1.0;

/// A cube of the tree, from the cloud's middle, mm.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub lo: [f64; 3],
    pub edge: f64,
    /// How wide its grid's squares are, mm, each holding one of its points; 0 for a node that keeps all it is given.
    pub spacing: f64,
    pub level: u8,
    start: usize,
    len: usize,
    /// Each eighth's node, 0 for none (the root is no one's child).
    children: [u32; 8],
}

impl Node {
    pub fn hi(&self) -> [f64; 3] {
        self.lo.map(|v| v + self.edge)
    }

    /// How many points it keeps.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn children(&self) -> impl Iterator<Item = usize> + '_ {
        self.children.iter().filter(|&&c| c != 0).map(|&c| c as usize)
    }

    /// Its eight corners.
    pub fn corners(&self) -> [[f64; 3]; 8] {
        let (lo, hi) = (self.lo, self.hi());
        std::array::from_fn(|k| std::array::from_fn(|a| if k >> a & 1 == 0 { lo[a] } else { hi[a] }))
    }
}

/// A cloud's points by node: the root first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Octree {
    /// The cloud's point indices, each node's together.
    order: Vec<u32>,
    nodes: Vec<Node>,
}

impl Octree {
    /// The tree of `points`, the same every time. Points past `u32::MAX` are left out (a cloud holds far fewer); one that isn't
    /// finite goes in the square its clamped coordinates name.
    pub fn build(points: &[[f32; 3]]) -> Octree {
        let n = points.len().min(u32::MAX as usize);
        if n == 0 {
            return Octree::default();
        }
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for p in &points[..n] {
            for k in 0..3 {
                let v = f64::from(p[k]);
                if v.is_finite() {
                    (lo[k], hi[k]) = (lo[k].min(v), hi[k].max(v));
                }
            }
        }
        let lo = lo.map(|v| if v.is_finite() { v } else { 0.0 });
        // A cube a millimetre wide at least, a little past the farthest point so it lies inside.
        let edge = (0..3).map(|k| hi[k] - lo[k]).filter(|v| v.is_finite()).fold(0.0, f64::max).max(1.0) * (1.0 + 1e-6);
        subtree(points, lo, edge, 0, (0..n as u32).collect())
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// The indices of the cloud's points node `node` keeps.
    pub fn points_of(&self, node: usize) -> &[u32] {
        self.nodes.get(node).and_then(|n| self.order.get(n.start..n.start + n.len)).unwrap_or_default()
    }
}

/// The tree under the node of cube `lo`, `edge` wide, at `level`, given `inflow`: the node first, then each eighth's tree, built
/// on the threads there are. A node keeps one point in each square of its grid, first come first kept, and passes the rest to the
/// eighth of its cube each lies in; a grid with more squares filled than a node may keep is made coarser.
fn subtree(points: &[[f32; 3]], lo: [f64; 3], edge: f64, level: u8, inflow: Vec<u32>) -> Octree {
    let mut node = Node { lo, edge, spacing: 0.0, level, start: 0, len: inflow.len(), children: [0; 8] };
    if inflow.len() <= NODE_POINTS || level >= MAX_LEVEL {
        return Octree { order: inflow, nodes: vec![node] };
    }
    let half = edge / 2.0;
    let mut grid = GRID;
    let (kept, eighths) = loop {
        let mut taken = vec![0_u64; (grid as usize).pow(3).div_ceil(64)];
        let mut kept = Vec::with_capacity(NODE_MAX.min(inflow.len()));
        let mut eighths: Vec<Vec<u32>> = vec![Vec::new(); 8];
        for &i in &inflow {
            let p = points.get(i as usize).copied().unwrap_or_default();
            let c = square(p, lo, edge, grid);
            match taken.get_mut(c / 64) {
                Some(word) if *word & (1 << (c % 64)) == 0 => {
                    *word |= 1 << (c % 64);
                    kept.push(i);
                }
                _ => {
                    let k = (0..3).fold(0, |k, a| k | (usize::from(f64::from(p[a]) >= lo[a] + half) << a));
                    if let Some(eighth) = eighths.get_mut(k) {
                        eighth.push(i);
                    }
                }
            }
        }
        if kept.len() <= NODE_MAX || grid == 1 {
            break (kept, eighths);
        }
        grid /= 2;
    };
    drop(inflow);
    (node.len, node.spacing) = (kept.len(), edge / f64::from(grid));
    use rayon::prelude::*;
    let subs: Vec<(usize, Octree)> = eighths
        .into_par_iter()
        .enumerate()
        .filter(|(_, inflow)| !inflow.is_empty())
        .map(|(k, inflow)| {
            let lo: [f64; 3] = std::array::from_fn(|a| if k >> a & 1 == 1 { lo[a] + half } else { lo[a] });
            (k, subtree(points, lo, half, level + 1, inflow))
        })
        .collect();
    let mut t = Octree { order: kept, nodes: vec![node] };
    for (k, sub) in subs {
        let (base, start) = (t.nodes.len() as u32, t.order.len());
        if let Some(c) = t.nodes.first_mut().and_then(|n| n.children.get_mut(k)) {
            *c = base;
        }
        t.nodes.extend(sub.nodes.into_iter().map(|mut n| {
            n.start += start;
            for c in n.children.iter_mut().filter(|c| **c != 0) {
                *c += base;
            }
            n
        }));
        t.order.extend(sub.order);
    }
    t
}

/// Which of `grid`³ squares of the cube at `lo`, `edge` wide, `p` lies in.
fn square(p: [f32; 3], lo: [f64; 3], edge: f64, grid: u32) -> usize {
    let g = f64::from(grid);
    let at = |k: usize| ((f64::from(p[k]) - lo[k]) / edge * g).floor().clamp(0.0, g - 1.0) as usize;
    let g = grid as usize;
    at(0) + g * (at(1) + g * at(2))
}

/// A node waiting to be chosen: how large it's drawn, its tree, and itself; the larger first, then the earlier.
struct Ranked(f64, usize, usize);

impl PartialEq for Ranked {
    fn eq(&self, o: &Ranked) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}

impl Eq for Ranked {}

impl PartialOrd for Ranked {
    fn partial_cmp(&self, o: &Ranked) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Ranked {
    fn cmp(&self, o: &Ranked) -> Ordering {
        self.0.total_cmp(&o.0).then_with(|| o.1.cmp(&self.1)).then_with(|| o.2.cmp(&self.2))
    }
}

/// The nodes of each of `trees` a picture draws: `seen` says how many pixels a millimetre of a node is drawn (`None` where the
/// picture doesn't see it), and the nodes drawn largest are chosen first until their points would pass `budget`. Each seen tree's
/// root is always chosen; a node is looked into only while its squares are drawn [`FINEST`] or wider.
pub fn select(trees: &[&Octree], budget: usize, mut seen: impl FnMut(usize, &Node) -> Option<f64>) -> Vec<Vec<usize>> {
    let mut chosen = vec![Vec::new(); trees.len()];
    let mut queue = BinaryHeap::new();
    let mut spent = 0_usize;
    let mut seen = |t: usize, node: &Node| seen(t, node).filter(|v| v.is_finite() && *v > 0.0);
    let open = |queue: &mut BinaryHeap<Ranked>, t: usize, node: &Node, px: f64, seen: &mut dyn FnMut(usize, &Node) -> Option<f64>| {
        if node.spacing * px < FINEST {
            return;
        }
        for c in node.children() {
            if let Some(child) = trees[t].nodes.get(c)
                && let Some(px) = seen(t, child)
            {
                queue.push(Ranked(child.edge * px, t, c));
            }
        }
    };
    for (t, tree) in trees.iter().enumerate() {
        if let Some(root) = tree.nodes.first()
            && let Some(px) = seen(t, root)
        {
            chosen[t].push(0);
            spent = spent.saturating_add(root.len);
            open(&mut queue, t, root, px, &mut seen);
        }
    }
    while let Some(Ranked(size, t, n)) = queue.pop() {
        let Some(node) = trees[t].nodes.get(n) else { continue };
        if spent.saturating_add(node.len) > budget {
            continue;
        }
        spent += node.len;
        chosen[t].push(n);
        open(&mut queue, t, node, size / node.edge, &mut seen);
    }
    chosen
}

#[cfg(test)]
mod tests;
