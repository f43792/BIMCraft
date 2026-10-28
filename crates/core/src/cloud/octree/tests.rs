use super::*;

/// `n` points the same every time, spread through a box `size` mm wide, `tall` high.
fn scatter(n: usize, size: f32, tall: f32) -> Vec<[f32; 3]> {
    let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s >> 11) as f32 / (1_u64 << 53) as f32
    };
    (0..n).map(|_| [(next() - 0.5) * size, (next() - 0.5) * size, (next() - 0.5) * tall]).collect()
}

/// Every node's points, by node.
fn kept(t: &Octree) -> Vec<Vec<u32>> {
    (0..t.nodes().len()).map(|i| t.points_of(i).to_vec()).collect()
}

fn inside(p: [f32; 3], n: &Node) -> bool {
    let (lo, hi) = (n.lo, n.hi());
    (0..3).all(|k| f64::from(p[k]) >= lo[k] - 1e-3 && f64::from(p[k]) <= hi[k] + 1e-3)
}

#[test]
fn every_point_is_in_one_node_once_inside_its_cube_and_the_tree_is_the_same_every_time() {
    let points = scatter(300_000, 40_000.0, 3_000.0);
    let t = Octree::build(&points);
    assert!(t.nodes().len() > 8, "{}", t.nodes().len());
    let mut all: Vec<u32> = kept(&t).into_iter().flatten().collect();
    all.sort_unstable();
    assert_eq!(all, (0..points.len() as u32).collect::<Vec<_>>());
    for (i, n) in t.nodes().iter().enumerate() {
        assert!(t.points_of(i).iter().all(|&p| inside(points[p as usize], n)), "node {i}");
        for c in n.children() {
            let k = &t.nodes()[c];
            assert_eq!((k.level, k.edge), (n.level + 1, n.edge / 2.0));
            assert!((0..3).all(|a| k.lo[a] >= n.lo[a] && k.hi()[a] <= n.hi()[a] + 1e-6));
        }
    }
    assert_eq!(t, Octree::build(&points));
}

#[test]
fn a_node_keeps_one_point_in_each_square_of_its_grid_and_no_more_than_it_may() {
    let points = scatter(400_000, 20_000.0, 20_000.0);
    let t = Octree::build(&points);
    for (i, n) in t.nodes().iter().enumerate() {
        let mine = t.points_of(i);
        assert!(mine.len() <= NODE_MAX, "node {i}: {}", mine.len());
        if n.spacing > 0.0 {
            let mut cells: Vec<[i64; 3]> =
                mine.iter().map(|&p| std::array::from_fn(|k| ((f64::from(points[p as usize][k]) - n.lo[k]) / n.spacing) as i64)).collect();
            let all = cells.len();
            cells.sort_unstable();
            cells.dedup();
            assert_eq!(cells.len(), all, "node {i} keeps two points in one square");
        } else {
            assert!(mine.len() <= NODE_POINTS && n.children().next().is_none(), "node {i}");
        }
    }
    // The root sees the whole scan, not the corner the file began with.
    let root: Vec<[f32; 3]> = t.points_of(0).iter().map(|&p| points[p as usize]).collect();
    for octant in 0..8 {
        let here = |p: &&[f32; 3]| (0..3).all(|k| (p[k] >= 0.0) == (octant >> k & 1 == 1));
        assert!(root.iter().filter(here).count() > root.len() / 16, "octant {octant}");
    }
}

#[test]
fn a_few_points_are_one_node_and_none_are_no_tree() {
    let t = Octree::build(&scatter(1000, 100.0, 100.0));
    assert_eq!((t.nodes().len(), t.points_of(0).len(), t.nodes()[0].spacing), (1, 1000, 0.0));
    assert!(Octree::build(&[]).nodes().is_empty());
    assert!(select(&[&Octree::build(&[])], 100, |_, _| Some(1.0)).iter().all(Vec::is_empty));
}

#[test]
fn points_all_in_one_place_or_one_far_from_the_rest_build_without_trouble() {
    let same = vec![[1.5_f32, -2.0, 3.0]; 200_000];
    let t = Octree::build(&same);
    assert_eq!(kept(&t).iter().map(Vec::len).sum::<usize>(), same.len());
    assert!(t.nodes().iter().all(|n| n.level <= MAX_LEVEL));
    let mut far = scatter(100_000, 1000.0, 1000.0);
    far.push([1.9e7, -1.9e7, 0.0]);
    let t = Octree::build(&far);
    assert_eq!(kept(&t).iter().map(Vec::len).sum::<usize>(), far.len());
    assert!(t.nodes().iter().all(|n| n.level <= MAX_LEVEL && n.len() <= NODE_MAX || n.level == MAX_LEVEL));
}

#[test]
fn a_selection_keeps_to_its_budget_leaves_out_what_isnt_seen_and_looks_closer_where_it_is_bigger() {
    let points = scatter(500_000, 40_000.0, 3_000.0);
    let t = Octree::build(&points);
    let count = |chosen: &[usize]| chosen.iter().map(|&n| t.nodes()[n].len()).sum::<usize>();
    // All of it when the budget allows and every square is seen large.
    let all = select(&[&t], usize::MAX, |_, _| Some(10.0));
    assert_eq!(count(&all[0]), points.len());
    // No more than the budget.
    let some = select(&[&t], 150_000, |_, _| Some(10.0));
    assert!(count(&some[0]) <= 150_000 && count(&some[0]) > 100_000, "{}", count(&some[0]));
    assert!(some[0].contains(&0));
    // What isn't seen isn't chosen (the root always is): only the west half here.
    let west = select(&[&t], usize::MAX, |_, n| (n.lo[0] < 0.0).then_some(10.0));
    assert!(west[0].iter().all(|&n| n == 0 || t.nodes()[n].lo[0] < 0.0));
    assert!(count(&west[0]) < points.len());
    // Drawn small, the root's detail is all a picture needs.
    let far = select(&[&t], usize::MAX, |_, _| Some(1e-6));
    assert_eq!(far[0], vec![0]);
    // Seen ten times larger in the west, the budget goes there first.
    let near = select(&[&t], 200_000, |_, n| Some(if n.lo[0] < 0.0 { 1.0 } else { 0.1 }));
    let side =
        |west: bool| near[0].iter().filter(|&&n| n != 0 && (t.nodes()[n].lo[0] < 0.0) == west).map(|&n| t.nodes()[n].len()).sum::<usize>();
    assert!(side(true) > 2 * side(false), "{} {}", side(true), side(false));
}

#[test]
fn a_selection_shares_its_budget_among_scans_and_keeps_each_root() {
    let (a, b) = (Octree::build(&scatter(300_000, 30_000.0, 3_000.0)), Octree::build(&scatter(300_000, 30_000.0, 3_000.0)));
    let chosen = select(&[&a, &b], 200_000, |_, _| Some(10.0));
    assert!(chosen[0].contains(&0) && chosen[1].contains(&0));
    let total: usize =
        chosen[0].iter().map(|&n| a.nodes()[n].len()).sum::<usize>() + chosen[1].iter().map(|&n| b.nodes()[n].len()).sum::<usize>();
    assert!(total <= 200_000, "{total}");
    // Hidden, a scan gives nothing.
    let one = select(&[&a, &b], usize::MAX, |t, _| (t == 0).then_some(10.0));
    assert!(one[1].is_empty() && !one[0].is_empty());
}

/// What a tree of twenty million points costs: `cargo test -p bimcraft-core twenty_million -- --ignored --nocapture`.
#[test]
#[ignore]
fn twenty_million_points_build_and_select() {
    let points = scatter(20_000_000, 200_000.0, 10_000.0);
    let started = std::time::Instant::now();
    let t = Octree::build(&points);
    let built = started.elapsed();
    let started = std::time::Instant::now();
    let chosen = select(&[&t], 2_000_000, |_, n| (n.lo[0] < 0.0 || n.level == 0).then_some(0.05));
    let picked = started.elapsed();
    let drawn: usize = chosen[0].iter().map(|&n| t.nodes()[n].len()).sum();
    println!(
        "{} nodes, deepest {}, built in {built:?}; {drawn} points chosen in {picked:?}",
        t.nodes().len(),
        t.nodes().iter().map(|n| n.level).max().unwrap_or(0)
    );
}
