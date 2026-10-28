use super::*;
use crate::cloud::{Options, read_from};

/// Points of a scan, the same every time: most in a tight cluster, the rest spread wide, each with a colour.
fn scatter(n: usize) -> Vec<([f64; 3], [u8; 3])> {
    let mut s = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        (s >> 11) as f64 / (1_u64 << 53) as f64
    };
    (0..n)
        .map(|i| {
            let wide = if i % 10 < 7 { 2_000.0 } else { 60_000.0 };
            let p = [next() * wide + 1e5, next() * wide - 3e4, next() * wide * 0.3];
            (p, [(i % 251) as u8, (i % 13) as u8, 200])
        })
        .collect()
}

fn xyz(points: &[([f64; 3], [u8; 3])]) -> Vec<u8> {
    points.iter().map(|(p, c)| format!("{:.3} {:.3} {:.3} {} {} {}\n", p[0], p[1], p[2], c[0], c[1], c[2])).collect::<String>().into_bytes()
}

/// Every node's points, as numbers to compare.
fn every(store: &Store) -> Vec<([u32; 3], [u8; 3])> {
    let mut all = Vec::new();
    for n in 0..store.tree().nodes().len() {
        let b = store.block(n);
        assert_eq!(b.points.len(), store.tree().nodes()[n].len(), "node {n}");
        let cs = b.colours.clone().unwrap();
        all.extend(b.points.iter().zip(cs).map(|(p, c)| (p.map(f32::to_bits), c)));
    }
    all.sort_unstable();
    all
}

#[test]
fn a_small_scans_cache_is_the_tree_a_project_builds_in_memory() {
    let bytes = xyz(&scatter(60_000));
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("a.bcoct");
    let h = build(&bytes.as_slice(), Format::Xyz, 1.0, 7, &out).unwrap();
    let held = read_from(&bytes.as_slice(), Format::Xyz, &Options { scale: 1.0, spacing: 0.0, cap: usize::MAX }).unwrap();
    assert_eq!((h.read, h.origin, h.colours), (60_000, held.origin, true));
    let store = Store::open(&out, 7).unwrap();
    let tree = Octree::build(&held.points);
    assert!(tree.nodes().len() > 1);
    assert_eq!(store.tree().nodes().len(), tree.nodes().len());
    let colours = held.colours.unwrap();
    for (n, (a, b)) in store.tree().nodes().iter().zip(tree.nodes()).enumerate() {
        assert_eq!((a.lo, a.edge, a.spacing, a.level, a.len()), (b.lo, b.edge, b.spacing, b.level, b.len()), "node {n}");
        assert_eq!(a.children().collect::<Vec<_>>(), b.children().collect::<Vec<_>>(), "node {n}");
        let block = store.block(n);
        let order = tree.points_of(n);
        assert_eq!(block.points, order.iter().map(|&i| held.points[i as usize]).collect::<Vec<_>>(), "node {n}");
        assert_eq!(block.colours.as_ref().unwrap(), &order.iter().map(|&i| colours[i as usize]).collect::<Vec<_>>());
    }
    assert_eq!(store.points(), 60_000);
    assert_eq!(store.preview(PREVIEW_POINTS).points.len(), 60_000);
    let few = store.preview(30_000);
    assert!(few.points.len() <= 30_000 && few.points.len() >= tree.nodes()[0].len(), "{}", few.points.len());
    assert_eq!(few.colours.unwrap().len(), few.points.len());
}

#[test]
fn a_scan_built_a_piece_at_a_time_keeps_every_point_once_each_in_its_own_node() {
    let pts = scatter(240_000);
    let bytes = xyz(&pts);
    let held = read_from(&bytes.as_slice(), Format::Xyz, &Options { scale: 1.0, spacing: 0.0, cap: usize::MAX }).unwrap();
    let mut want: Vec<([u32; 3], [u8; 3])> =
        held.points.iter().zip(held.colours.as_ref().unwrap()).map(|(p, c)| (p.map(f32::to_bits), *c)).collect();
    want.sort_unstable();
    let dir = tempfile::tempdir().unwrap();
    // Chunks smaller than a node's inflow, and larger: cubes split again and again, and built in memory with levels of their own.
    for chunk in [9_000, 60_000] {
        let out = dir.path().join(format!("{chunk}.bcoct"));
        build_in(&bytes.as_slice(), Format::Xyz, 1.0, 1, &out, chunk).unwrap();
        let store = Store::open(&out, 1).unwrap();
        let nodes = store.tree().nodes();
        assert!(nodes.len() > 8, "{chunk}: {} nodes", nodes.len());
        assert_eq!(every(&store), want, "{chunk}");
        for (n, node) in nodes.iter().enumerate() {
            let b = store.block(n);
            let slack = node.edge * 1e-5;
            for p in &b.points {
                assert!(
                    (0..3).all(|k| f64::from(p[k]) >= node.lo[k] - slack && f64::from(p[k]) <= node.hi()[k] + slack),
                    "{chunk}: node {n}"
                );
            }
            if node.children().next().is_some() {
                assert!(node.len() <= octree::NODE_MAX && node.spacing > 0.0, "{chunk}: node {n}");
            }
            for c in node.children() {
                let child = &nodes[c];
                assert_eq!((child.level, child.edge), (node.level + 1, node.edge / 2.0), "{chunk}: node {c}");
                assert!(!child.is_empty() || child.children().next().is_some(), "{chunk}: node {c} holds nothing");
            }
        }
        // A picture of all of it chooses the root and what it can afford.
        let chosen = octree::select(&[store.tree()], 200_000, |_, _| Some(1.0));
        assert!(chosen[0].first() == Some(&0) && chosen[0].len() > 1);
        // No file of the build is left beside it.
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert!(left.iter().all(|f| f.to_string_lossy().ends_with(".bcoct")), "{left:?}");
    }
}

#[test]
fn a_damaged_or_foreign_cache_is_refused_and_a_damaged_block_is_drawn_as_nothing() {
    let bytes = xyz(&scatter(40_000));
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("a.bcoct");
    build(&bytes.as_slice(), Format::Xyz, 1.0, 5, &out).unwrap();
    let good = std::fs::read(&out).unwrap();
    let store = Store::open(&out, 5).unwrap();
    let nodes = store.tree().nodes().len();
    assert!(nodes > 1);
    drop(store);
    let try_with = |name: &str, file: &[u8]| {
        let p = dir.path().join(name);
        std::fs::write(&p, file).unwrap();
        Store::open(&p, 5)
    };
    assert!(Store::open(&out, 6).unwrap_err().contains("another file"));
    assert!(try_with("empty", &[]).unwrap_err().contains("cut short"));
    assert!(try_with("garbage", &[7; 4096]).unwrap_err().contains("isn't a scan's cache"));
    let mut v = good.clone();
    v[8] = 99;
    assert!(try_with("version", &v).unwrap_err().contains("version 99"));
    let mut h = good.clone();
    h[30] ^= 1;
    assert!(try_with("header", &h).unwrap_err().contains("header"));
    let table = u64::from_le_bytes(good[88..96].try_into().unwrap()) as usize;
    let mut t = good.clone();
    t[table + 5] ^= 1;
    assert!(try_with("table", &t).unwrap_err().contains("table"));
    assert!(try_with("short", &good[..good.len() - 10]).unwrap_err().contains("cut short"));
    // A table made to point a node back at the root, its sums made good: still refused.
    let mut c = good.clone();
    let child = table + NODE + 48;
    c[child..child + 4].copy_from_slice(&0_u32.to_le_bytes());
    let mut slots: Vec<u32> = (0..8).map(|k| u32::from_le_bytes(c[child + 4 * k..child + 4 * k + 4].try_into().unwrap())).collect();
    slots[0] = 1;
    for (k, s) in slots.iter().enumerate() {
        c[child + 4 * k..child + 4 * k + 4].copy_from_slice(&s.to_le_bytes());
    }
    let sum = crc(&c[table..table + nodes * NODE]);
    c[84..88].copy_from_slice(&sum.to_le_bytes());
    let hsum = crc(&c[..HEADER - 4]);
    c[HEADER - 4..HEADER].copy_from_slice(&hsum.to_le_bytes());
    assert!(try_with("cycle", &c).unwrap_err().contains("wrongly"));
    // A block damaged: that node is nothing, the others are as they were.
    let mut b = good.clone();
    b[HEADER + 3] ^= 0xff;
    let store = try_with("block", &b).unwrap();
    let damaged = (0..nodes).filter(|&n| store.block(n).points.is_empty() && !store.tree().nodes()[n].is_empty()).count();
    assert_eq!(damaged, 1);
}

#[test]
fn a_fingerprint_changes_with_the_file_and_with_how_its_read() {
    let a = xyz(&scatter(3_000));
    let mut b = a.clone();
    *b.last_mut().unwrap() = b' ';
    let f = |bytes: &[u8], stamp, format, scale| fingerprint(&bytes, stamp, format, scale).unwrap();
    let base = f(&a, 1, Format::Xyz, 1.0);
    assert_eq!(base, f(&a, 1, Format::Xyz, 1.0));
    for other in [f(&b, 1, Format::Xyz, 1.0), f(&a, 2, Format::Xyz, 1.0), f(&a, 1, Format::Pts, 1.0), f(&a, 1, Format::Xyz, 1000.0)] {
        assert_ne!(base, other);
    }
    assert!(path_of(base).is_some_and(|p| p.to_string_lossy().ends_with(&format!("{base:016x}.bcoct"))));
}
