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
            // Whole millimetres, so a position packed to quarter millimetres comes back to them.
            let p = [next() * wide + 1e5, next() * wide - 3e4, next() * wide * 0.3].map(f64::round);
            (p, [(i % 251) as u8, (i % 13) as u8, 200])
        })
        .collect()
}

fn xyz(points: &[([f64; 3], [u8; 3])]) -> Vec<u8> {
    points.iter().map(|(p, c)| format!("{:.3} {:.3} {:.3} {} {} {}\n", p[0], p[1], p[2], c[0], c[1], c[2])).collect::<String>().into_bytes()
}

/// A position from a cloud's middle (a whole or a half millimetre, the scan's being whole), in half millimetres: what a packed
/// one comes back as.
fn halves(p: [f32; 3]) -> [i64; 3] {
    p.map(|v| (f64::from(v) * 2.0).round() as i64)
}

/// Every node's points, as numbers to compare.
fn every(store: &Store) -> Vec<([i64; 3], [u8; 3])> {
    let mut all = Vec::new();
    for n in 0..store.tree().nodes().len() {
        let b = store.block(n);
        assert_eq!(b.points.len(), store.tree().nodes()[n].len(), "node {n}");
        let cs = b.colours.clone().unwrap();
        all.extend(b.points.iter().zip(cs).map(|(p, c)| (halves(*p), c)));
    }
    all.sort_unstable();
    all
}

/// A scan's bytes, noting how far the build said it had got each time it asked for some.
struct Noted<'a> {
    bytes: &'a [u8],
    progress: &'a Progress,
    seen: Mutex<Vec<f32>>,
}

impl Source for Noted<'_> {
    fn size(&self) -> u64 {
        self.bytes.size()
    }

    fn read_at(&self, at: u64, buf: &mut [u8]) -> Result<usize, String> {
        self.seen.lock().unwrap().push(self.progress.fraction());
        self.bytes.read_at(at, buf)
    }
}

#[test]
fn a_build_says_how_far_it_has_got_from_nothing_to_whole_and_never_goes_back() {
    let bytes = xyz(&scatter(60_000));
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("a.bcoct");
    let progress = Progress::default();
    assert_eq!(progress.fraction(), 0.0);
    let src = Noted { bytes: &bytes, progress: &progress, seen: Mutex::default() };
    build_in(&src, Format::Xyz, 1.0, 3, &out, 20_000, &progress).unwrap();
    let seen = src.seen.into_inner().unwrap();
    assert_eq!(seen.first(), Some(&0.0));
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
    // The file is read twice, the first time to measure it, before any point is built.
    assert!(seen.iter().any(|&f| f > 0.0) && seen.iter().all(|&f| f <= 0.6), "{seen:?}");
    assert_eq!(progress.fraction(), 1.0);
}

#[test]
fn a_small_scans_cache_is_the_tree_a_project_builds_in_memory() {
    let bytes = xyz(&scatter(60_000));
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("a.bcoct");
    let h = build(&bytes.as_slice(), Format::Xyz, 1.0, 7, &out, &Progress::default()).unwrap();
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
        let want: Vec<[f32; 3]> = order.iter().map(|&i| held.points[i as usize]).collect();
        assert_eq!(block.points.len(), want.len(), "node {n}");
        // As held, or within half a step where the node packs them.
        let off = block.points.iter().zip(&want).flat_map(|(a, b)| (0..3).map(move |k| (a[k] - b[k]).abs())).fold(0.0, f32::max);
        assert!(f64::from(off) <= if a.edge <= 65_535.0 * STEP { STEP / 2.0 + 1e-3 } else { 0.0 }, "node {n}: {off}");
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
    let mut want: Vec<([i64; 3], [u8; 3])> =
        held.points.iter().zip(held.colours.as_ref().unwrap()).map(|(p, c)| (halves(*p), *c)).collect();
    want.sort_unstable();
    let dir = tempfile::tempdir().unwrap();
    // Chunks smaller than a node's inflow, and larger: cubes split again and again, and built in memory with levels of their own.
    for chunk in [9_000, 60_000] {
        let out = dir.path().join(format!("{chunk}.bcoct"));
        build_in(&bytes.as_slice(), Format::Xyz, 1.0, 1, &out, chunk, &Progress::default()).unwrap();
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
    build(&bytes.as_slice(), Format::Xyz, 1.0, 5, &out, &Progress::default()).unwrap();
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

/// A LAS 1.2 file of `n` coloured points, made as it's read and never held: a city block, mm, with its storeys.
struct Synthetic {
    n: u64,
    header: Vec<u8>,
}

impl Synthetic {
    const RECORD: u64 = 26;

    fn new(n: u64) -> Synthetic {
        let mut b = vec![0u8; 227];
        b[0..4].copy_from_slice(b"LASF");
        (b[24], b[25]) = (1, 2);
        b[94..96].copy_from_slice(&227_u16.to_le_bytes());
        b[96..100].copy_from_slice(&227_u32.to_le_bytes());
        b[104] = 2;
        b[105..107].copy_from_slice(&26_u16.to_le_bytes());
        b[107..111].copy_from_slice(&(n as u32).to_le_bytes());
        for k in 0..3 {
            b[131 + 8 * k..139 + 8 * k].copy_from_slice(&0.001_f64.to_le_bytes());
        }
        Synthetic { n, header: b }
    }

    fn record(i: u64) -> [u8; 26] {
        let h = (i ^ 0x9e37_79b9_7f4a_7c15).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        let h = (h ^ (h >> 31)).wrapping_mul(0x94d0_49bb_1331_11eb);
        let [x, y, z] = [h & 0x1f_ffff, (h >> 21) & 0x1f_ffff, ((h >> 42) & 0xffff) * 10 % 400_000];
        let mut r = [0u8; 26];
        for (k, v) in [x * 100, y * 100, z].into_iter().enumerate() {
            r[4 * k..4 * k + 4].copy_from_slice(&(v as i32).to_le_bytes());
        }
        r[20..26].copy_from_slice(&[(h >> 8) as u8, 0, (h >> 16) as u8, 0, 200, 0]);
        r
    }
}

impl Source for Synthetic {
    fn size(&self) -> u64 {
        227 + self.n * Self::RECORD
    }

    fn read_at(&self, at: u64, buf: &mut [u8]) -> Result<usize, String> {
        let end = (at + buf.len() as u64).min(self.size());
        let mut put = 0;
        let mut at = at;
        while at < end {
            let n = if at < 227 {
                let part = &self.header[at as usize..(end.min(227)) as usize];
                buf[put..put + part.len()].copy_from_slice(part);
                part.len()
            } else {
                let (i, off) = ((at - 227) / Self::RECORD, ((at - 227) % Self::RECORD) as usize);
                let r = Self::record(i);
                let take = (26 - off).min((end - at) as usize);
                buf[put..put + take].copy_from_slice(&r[off..off + take]);
                take
            };
            (put, at) = (put + n, at + n as u64);
        }
        Ok(put)
    }
}

#[test]
#[ignore = "builds a hundred million points: run it to measure, with --ignored"]
fn a_hundred_million_points_are_built_a_piece_at_a_time() {
    let n = 100_000_000;
    let src = Synthetic::new(n);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("big.bcoct");
    let started = std::time::Instant::now();
    build(&src, Format::Las, 1.0, 9, &out, &Progress::default()).unwrap();
    let built = started.elapsed();
    let store = Store::open(&out, 9).unwrap();
    assert_eq!((store.points(), store.header().read), (n, n));
    let started = std::time::Instant::now();
    let chosen = octree::select(&[store.tree()], 2_000_000, |_, _| Some(0.05));
    let points: usize = chosen[0].iter().map(|&k| store.block(k).points.len()).sum();
    println!(
        "{n} points built in {built:?} ({} nodes, {} MB); {} nodes, {points} points chosen and loaded in {:?}",
        store.tree().nodes().len(),
        std::fs::metadata(&out).unwrap().len() >> 20,
        chosen[0].len(),
        started.elapsed()
    );
    assert!(points <= 2_000_000 && points > 1_000_000);
}
