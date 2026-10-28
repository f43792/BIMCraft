//! A linked scan's points on disk, by level of detail: the octree of [`super::octree`] built a piece at a time, so a scan of
//! any size is built in bounded memory, and kept in the user's cache, where a project finds it by the fingerprint of the file
//! it was built from. Drawing loads the nodes a picture chooses ([`Store::block`]) and keeps the latest of them.
//!
//! The build reads the file twice: once to measure it, once to sort its points into the cubes of a coarse grid, each a file of
//! its own beside the cache. A cube with more points than [`CHUNK_POINTS`] is split into its eighths, again and again; one with
//! fewer is built in memory as [`octree::subtree`] builds any node. The nodes above the cubes are built from below: each keeps
//! one point in each square of its grid of what its eighths' roots kept, and those points leave the roots, as Potree's
//! converter builds its upper levels.
//!
//! The file: a header of [`HEADER`] bytes, the nodes' blocks of points (positions as little-endian `f32` from the cloud's
//! middle, then their colours as bytes where it has some, then the CRC-32 of both), and the table of nodes, root first, with
//! its own CRC-32 in the header, which has one too.

use super::octree::{self, MAX_LEVEL, NODE_MAX, Node, Octree};
use super::source::{self, Source};
use super::{Format, GREY, MAX_FILE_COORD, MAX_SPAN, Stats};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

/// What a cache file's layout is: a cache of another is built again.
pub const VERSION: u32 = 1;
const MAGIC: &[u8; 8] = b"BCOCTREE";
/// The header's bytes.
const HEADER: usize = 128;
/// A node's bytes in the table.
const NODE: usize = 88;
/// A point's bytes in the files a build sorts them into: its position and its colour.
const RECORD: usize = 15;
/// The most points built in memory at once, as one cube's subtree.
pub const CHUNK_POINTS: usize = 4_000_000;
/// The most points of a linked scan a project file keeps, the upper levels of its tree, to draw where its cache isn't.
pub const PREVIEW_POINTS: usize = 1_000_000;
/// The most points a linked scan may have.
pub const MAX_POINTS: u64 = 1 << 36;
/// The most nodes a cache's tree may have.
const MAX_NODES: usize = 1 << 24;
/// The bytes of points a store keeps loaded; past them, those drawn longest ago are let go.
pub const KEEP: usize = 1 << 30;
/// The bytes a build gathers for one cube before writing them to its file.
const BUFFER: usize = 32 << 10;
/// The finest grid the first sort uses, 2 to this along a side.
const MAX_TOP: u32 = 4;

/// Where the caches are kept when a front end says (and tests, which share one).
static FOLDER: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Keeps caches in `folder` from now on; none goes back to the user's own cache folder.
pub fn set_folder(folder: Option<PathBuf>) {
    if let Ok(mut f) = FOLDER.write() {
        *f = folder;
    }
}

/// Where the caches are kept: where [`set_folder`] said, else where `BIMCRAFT_SCAN_CACHE` says, else in the user's own cache
/// folder.
pub fn folder() -> Option<PathBuf> {
    if let Some(f) = FOLDER.read().ok().and_then(|f| f.clone()) {
        return Some(f);
    }
    if let Some(path) = std::env::var_os("BIMCRAFT_SCAN_CACHE").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let home = || std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
    let folder = if cfg!(target_os = "windows") {
        PathBuf::from(std::env::var_os("LOCALAPPDATA").filter(|p| !p.is_empty())?).join("BIMCraft")
    } else if cfg!(target_os = "macos") {
        home()?.join("Library").join("Caches").join("BIMCraft")
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".cache")))?
            .join("bimcraft")
    };
    Some(folder.join("scans"))
}

/// The cache of the file with `fingerprint`.
pub fn path_of(fingerprint: u64) -> Option<PathBuf> {
    folder().map(|f| f.join(format!("{fingerprint:016x}.bcoct")))
}

/// FNV-1a, 64 bits: what a fingerprint is made with.
struct Fnv(u64);

impl Fnv {
    fn add(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
}

/// What names a file's cache: its size, when it was written (`stamp`), its first and last megabyte, how it's read, and the
/// cache's [`VERSION`], so a file changed, or read another way, is built again.
pub fn fingerprint(src: &dyn Source, stamp: u64, format: Format, scale: f64) -> Result<u64, String> {
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    h.add(&VERSION.to_le_bytes());
    h.add(&src.size().to_le_bytes());
    h.add(&stamp.to_le_bytes());
    h.add(&[format as u8]);
    h.add(&scale.to_bits().to_le_bytes());
    let n = usize::try_from(src.size()).unwrap_or(usize::MAX).min(1 << 20);
    h.add(&source::take(src, 0, n)?);
    h.add(&source::take(src, src.size() - n as u64, n)?);
    Ok(h.0)
}

/// What a cache says of its scan.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub fingerprint: u64,
    /// Where the cloud's middle is in the file's coordinates, mm.
    pub origin: [f64; 3],
    /// The points the file holds: all of them are in the cache.
    pub read: u64,
    /// The box round its points, from its middle, mm.
    pub lo: [f32; 3],
    pub hi: [f32; 3],
    pub colours: bool,
}

/// One node's points.
#[derive(Debug, Default, PartialEq)]
pub struct Block {
    pub points: Vec<[f32; 3]>,
    pub colours: Option<Vec<[u8; 3]>>,
}

/// Builds the cache of the file `src` holds, of `format` in units of `scale` mm, at `out`, and says what it holds. It's
/// written beside `out` and put there whole, so a cache is never found half written.
pub fn build(src: &dyn Source, format: Format, scale: f64, fingerprint: u64, out: &Path) -> Result<Header, String> {
    build_in(src, format, scale, fingerprint, out, CHUNK_POINTS)
}

/// As [`build`], with no more than `chunk` points built in memory at once.
fn build_in(src: &dyn Source, format: Format, scale: f64, fingerprint: u64, out: &Path, chunk: usize) -> Result<Header, String> {
    if !(scale.is_finite() && scale > 0.0) {
        return Err(format!("a unit of {scale} mm isn't one"));
    }
    let mut stats = Stats::default();
    super::each(src, format, &mut |raw| stats.add(&raw, scale))?;
    if stats.count == 0 {
        return Err("it holds no points".into());
    }
    if stats.count as u64 > MAX_POINTS {
        return Err(format!("it holds {} points; a linked scan up to {MAX_POINTS}", stats.count));
    }
    let origin = stats.middle()?;
    let shade = stats.colour();
    let local = |v: [f64; 3]| -> [f32; 3] { std::array::from_fn(|k| (v[k] - origin[k]) as f32) };
    let (lo32, hi32) = (local(stats.min), local(stats.max));
    let (lo, edge) = octree::cube(lo32.map(f64::from), hi32.map(f64::from));
    let folder = out.parent().ok_or("a cache goes in a folder")?;
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let work = tempfile::Builder::new().prefix(".scan").tempdir_in(folder).map_err(|e| format!("{}: {e}", folder.display()))?;

    // The first sort: into cubes of a grid fine enough that, were the points spread evenly, a cube would hold half a chunk.
    let mut top = 0;
    while top < MAX_TOP && (stats.count as f64 / 8_f64.powi(top as i32)) > chunk as f64 / 2.0 {
        top += 1;
    }
    let side = 1_u32 << top;
    let mut sorted: HashMap<usize, (Vec<u8>, u64)> = HashMap::new();
    let cube_file = |c: usize| work.path().join(format!("c{c}"));
    let mut n = 0_u64;
    super::each(src, format, &mut |raw| {
        let p = local(raw.p.map(|v| v * scale));
        let colour = shade.as_ref().map_or([GREY; 3], |s| s.of(&raw));
        let c = octree::square(p, lo, edge, side);
        let (buf, count) = sorted.entry(c).or_default();
        record(buf, p, colour);
        *count += 1;
        n += 1;
        if buf.len() >= BUFFER {
            append(&cube_file(c), buf)?;
        }
        Ok(())
    })?;
    if n != stats.count as u64 {
        return Err("it changed while it was read".into());
    }
    let mut cubes: Vec<(usize, u64)> = Vec::with_capacity(sorted.len());
    for (c, (buf, count)) in &mut sorted {
        append(&cube_file(*c), buf)?;
        cubes.push((*c, *count));
    }
    drop(sorted);

    let mut file = tempfile::NamedTempFile::new_in(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let mut b = Builder {
        out: BufWriter::new(file.as_file_mut()),
        at: HEADER as u64,
        nodes: Vec::new(),
        colours: shade.is_some(),
        chunk,
        work: work.path().to_path_buf(),
        made: 0,
    };
    b.out.write_all(&[0; HEADER]).map_err(|e| e.to_string())?;
    let counts: HashMap<usize, u64> = cubes.into_iter().collect();
    // Each cube, and every cube it lies in, from the root down to the grid.
    let mut under: HashSet<(u32, [u32; 3])> = HashSet::new();
    for &c in counts.keys() {
        let s = side as usize;
        let mut at = [c % s, c / s % s, c / s / s].map(|v| v as u32);
        for level in (0..=top).rev() {
            under.insert((level, at));
            at = at.map(|v| v / 2);
        }
    }
    let part = if top == 0 { Part::File(cube_file(0), counts.get(&0).copied().unwrap_or(0)) } else { Part::Cells([0; 3]) };
    let ctx = Grid { top, side, counts: &counts, under: &under, cube: &cube_file };
    let root = b.part(&ctx, part, lo, edge, 0)?.ok_or("it holds no points")?;
    b.finish(root)?;
    let table = b.at;
    let nodes = reachable(std::mem::take(&mut b.nodes));
    let mut bytes = Vec::with_capacity(nodes.len() * NODE);
    for r in &nodes {
        r.write(&mut bytes);
    }
    b.out.write_all(&bytes).map_err(|e| e.to_string())?;
    b.out.flush().map_err(|e| e.to_string())?;
    drop(b);
    let header = Header { fingerprint, origin, read: n, lo: lo32, hi: hi32, colours: shade.is_some() };
    let head = header_bytes(&header, nodes.len() as u32, crc(&bytes), table);
    let f = file.as_file_mut();
    f.seek(SeekFrom::Start(0)).and_then(|_| f.write_all(&head)).and_then(|()| f.sync_all()).map_err(|e| e.to_string())?;
    file.persist(out).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(header)
}

/// The nodes the root reaches, in the order they were, numbered again: a leaf the node above took all of is left out.
fn reachable(nodes: Vec<Rec>) -> Vec<Rec> {
    let mut reached = vec![false; nodes.len()];
    if let Some(r) = reached.first_mut() {
        *r = true;
    }
    // A child comes after its node, so one walk in order finds them all.
    for (i, r) in nodes.iter().enumerate() {
        if reached.get(i).copied().unwrap_or(false) {
            for &c in r.children.iter().filter(|&&c| c != 0) {
                if let Some(seen) = reached.get_mut(c as usize) {
                    *seen = true;
                }
            }
        }
    }
    let mut number = vec![0_u32; nodes.len()];
    let mut next = 0;
    for (i, n) in number.iter_mut().enumerate() {
        if reached.get(i).copied().unwrap_or(false) {
            *n = next;
            next += 1;
        }
    }
    nodes
        .into_iter()
        .zip(reached)
        .filter(|(_, r)| *r)
        .map(|(mut r, _)| {
            for c in r.children.iter_mut().filter(|c| **c != 0) {
                *c = number.get(*c as usize).copied().unwrap_or(0);
            }
            r
        })
        .collect()
}

fn record(buf: &mut Vec<u8>, p: [f32; 3], c: [u8; 3]) {
    for v in p {
        buf.extend_from_slice(&v.to_le_bytes());
    }
    buf.extend_from_slice(&c);
}

fn unrecord(r: &[u8]) -> ([f32; 3], [u8; 3]) {
    let f = |i: usize| r.get(i..i + 4).and_then(|b| b.try_into().ok()).map_or(0.0, f32::from_le_bytes);
    let c = |i: usize| r.get(i).copied().unwrap_or(GREY);
    ([f(0), f(4), f(8)], [c(12), c(13), c(14)])
}

/// Writes `buf` on the end of the file at `path`, and empties it.
fn append(path: &Path, buf: &mut Vec<u8>) -> Result<(), String> {
    if buf.is_empty() {
        return Ok(());
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(buf).map_err(|e| format!("{}: {e}", path.display()))?;
    buf.clear();
    Ok(())
}

fn crc(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

/// A node as the table holds it.
#[derive(Clone, Debug, Default)]
struct Rec {
    lo: [f64; 3],
    edge: f64,
    spacing: f64,
    level: u8,
    len: u32,
    children: [u32; 8],
    block: u64,
}

impl Rec {
    fn write(&self, out: &mut Vec<u8>) {
        for v in self.lo {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&self.edge.to_le_bytes());
        out.extend_from_slice(&self.spacing.to_le_bytes());
        out.extend_from_slice(&[self.level, 0, 0, 0]);
        out.extend_from_slice(&self.len.to_le_bytes());
        for c in self.children {
            out.extend_from_slice(&c.to_le_bytes());
        }
        out.extend_from_slice(&self.block.to_le_bytes());
    }

    fn read(b: &[u8]) -> Option<Rec> {
        let f64_at = |i: usize| b.get(i..i + 8).and_then(|b| b.try_into().ok()).map(f64::from_le_bytes);
        let u32_at = |i: usize| b.get(i..i + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
        let mut children = [0; 8];
        for (k, c) in children.iter_mut().enumerate() {
            *c = u32_at(48 + 4 * k)?;
        }
        Some(Rec {
            lo: [f64_at(0)?, f64_at(8)?, f64_at(16)?],
            edge: f64_at(24)?,
            spacing: f64_at(32)?,
            level: *b.get(40)?,
            len: u32_at(44)?,
            children,
            block: b.get(80..88).and_then(|b| b.try_into().ok()).map(u64::from_le_bytes)?,
        })
    }
}

fn header_bytes(h: &Header, nodes: u32, table_crc: u32, table: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&u32::from(h.colours).to_le_bytes());
    out.extend_from_slice(&h.fingerprint.to_le_bytes());
    for v in h.origin {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&h.read.to_le_bytes());
    for v in h.lo.into_iter().chain(h.hi) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&nodes.to_le_bytes());
    out.extend_from_slice(&table_crc.to_le_bytes());
    out.extend_from_slice(&table.to_le_bytes());
    out.resize(HEADER - 4, 0);
    let c = crc(&out);
    out.extend_from_slice(&c.to_le_bytes());
    out
}

/// Where a part of the tree's points are: in one cube's file, or in the cubes of the first sort under the cube at this place
/// of its level's grid.
enum Part {
    File(PathBuf, u64),
    Cells([u32; 3]),
}

/// The first sort: its grid, how many points each of its cubes holds, every cube with points under it, and each one's file.
struct Grid<'a> {
    top: u32,
    side: u32,
    counts: &'a HashMap<usize, u64>,
    under: &'a HashSet<(u32, [u32; 3])>,
    cube: &'a dyn Fn(usize) -> PathBuf,
}

/// A node whose own points are still to be written, since the node above takes some of them: where it is in the table, and
/// the points.
struct Pending {
    index: usize,
    points: Vec<[f32; 3]>,
    colours: Vec<[u8; 3]>,
}

struct Builder<'a> {
    out: BufWriter<&'a mut std::fs::File>,
    /// Where the next block goes.
    at: u64,
    nodes: Vec<Rec>,
    colours: bool,
    chunk: usize,
    work: PathBuf,
    /// Files made by splitting, for their names.
    made: usize,
}

impl Builder<'_> {
    /// Writes the points of a node, and says where.
    fn block(&mut self, points: &[[f32; 3]], colours: &[[u8; 3]]) -> Result<u64, String> {
        let mut bytes = Vec::with_capacity(points.len() * RECORD + 4);
        for p in points {
            for v in p {
                bytes.extend_from_slice(&v.to_le_bytes());
            }
        }
        if self.colours {
            bytes.extend(colours.iter().flatten());
        }
        let c = crc(&bytes);
        bytes.extend_from_slice(&c.to_le_bytes());
        let at = self.at;
        self.out.write_all(&bytes).map_err(|e| e.to_string())?;
        self.at += bytes.len() as u64;
        Ok(at)
    }

    /// The root's points, which no node takes.
    fn finish(&mut self, root: Pending) -> Result<(), String> {
        let at = self.block(&root.points, &root.colours)?;
        if let Some(r) = self.nodes.get_mut(root.index) {
            (r.len, r.block) = (root.points.len() as u32, at);
        }
        Ok(())
    }

    /// The tree of `part`, whose cube is `lo`, `edge` wide, at `level`: none where it holds no points.
    fn part(&mut self, g: &Grid, part: Part, lo: [f64; 3], edge: f64, level: u8) -> Result<Option<Pending>, String> {
        let half = edge / 2.0;
        match part {
            Part::File(path, count) if count as usize <= self.chunk || level >= MAX_LEVEL - 1 => self.chunk(&path, lo, edge, level),
            Part::File(path, _) => {
                // Its eighths, each a file.
                let mut parts: [(Vec<u8>, u64, PathBuf); 8] = std::array::from_fn(|_| (Vec::new(), 0, PathBuf::new()));
                for (buf, _, p) in &mut parts {
                    self.made += 1;
                    *p = self.work.join(format!("s{}", self.made));
                    buf.reserve(BUFFER);
                }
                let src = source::FileSource::open(&path)?;
                let records = src.size() / RECORD as u64;
                source::records(&src, 0, (RECORD, records), source::CHUNK, &mut |r| {
                    let (p, _) = unrecord(r);
                    let k = (0..3).fold(0, |k, a| k | (usize::from(f64::from(p[a]) >= lo[a] + half) << a));
                    if let Some((buf, n, path)) = parts.get_mut(k) {
                        buf.extend_from_slice(r);
                        *n += 1;
                        if buf.len() >= BUFFER {
                            append(path, buf)?;
                        }
                    }
                    Ok(())
                })?;
                drop(src);
                let _ = std::fs::remove_file(&path);
                let mut kids: [Option<Part>; 8] = Default::default();
                for (k, (buf, n, p)) in parts.iter_mut().enumerate() {
                    append(p, buf)?;
                    if *n > 0 {
                        kids[k] = Some(Part::File(p.clone(), *n));
                    }
                }
                self.inner(g, kids, lo, edge, level)
            }
            Part::Cells(at) => {
                let mut kids: [Option<Part>; 8] = Default::default();
                let below = u32::from(level) + 1;
                for (k, kid) in kids.iter_mut().enumerate() {
                    let at: [u32; 3] = std::array::from_fn(|a| at[a] * 2 + (k as u32 >> a & 1));
                    if !g.under.contains(&(below, at)) {
                        continue;
                    }
                    *kid = Some(if below == g.top {
                        let c = (at[0] + g.side * (at[1] + g.side * at[2])) as usize;
                        Part::File((g.cube)(c), g.counts.get(&c).copied().unwrap_or(0))
                    } else {
                        Part::Cells(at)
                    });
                }
                self.inner(g, kids, lo, edge, level)
            }
        }
    }

    /// A node over the trees of `kids`, its eighths: it keeps one point in each square of its grid of what their roots keep,
    /// a grid made coarser while it would keep more than [`NODE_MAX`], and those points leave the roots.
    fn inner(&mut self, g: &Grid, kids: [Option<Part>; 8], lo: [f64; 3], edge: f64, level: u8) -> Result<Option<Pending>, String> {
        let index = self.nodes.len();
        self.nodes.push(Rec { lo, edge, level, ..Rec::default() });
        let half = edge / 2.0;
        let mut below: Vec<(usize, Pending)> = Vec::new();
        for (k, kid) in kids.into_iter().enumerate() {
            let Some(kid) = kid else { continue };
            let at: [f64; 3] = std::array::from_fn(|a| if k >> a & 1 == 1 { lo[a] + half } else { lo[a] });
            if let Some(p) = self.part(g, kid, at, half, level + 1)? {
                below.push((k, p));
            }
        }
        if below.is_empty() {
            self.nodes.truncate(index);
            return Ok(None);
        }
        let all: usize = below.iter().map(|(_, p)| p.points.len()).sum();
        let mut grid = octree::GRID;
        let taken = loop {
            let mut filled = vec![0_u64; (grid as usize).pow(3).div_ceil(64)];
            let mut taken: Vec<Vec<bool>> = Vec::with_capacity(below.len());
            let mut kept = 0;
            for (_, p) in &below {
                taken.push(
                    p.points
                        .iter()
                        .map(|&q| {
                            let c = octree::square(q, lo, edge, grid);
                            match filled.get_mut(c / 64) {
                                Some(word) if *word & (1 << (c % 64)) == 0 => {
                                    *word |= 1 << (c % 64);
                                    kept += 1;
                                    true
                                }
                                _ => false,
                            }
                        })
                        .collect(),
                );
            }
            if kept <= NODE_MAX || grid == 1 {
                break taken;
            }
            grid /= 2;
        };
        let (mut points, mut colours) = (Vec::with_capacity(all.min(NODE_MAX)), Vec::new());
        let mut children = [0_u32; 8];
        for ((k, p), taken) in below.into_iter().zip(taken) {
            let (mut left, mut left_colours) = (Vec::new(), Vec::new());
            for (i, (&q, t)) in p.points.iter().zip(&taken).enumerate() {
                let c = p.colours.get(i).copied().unwrap_or([GREY; 3]);
                if *t {
                    points.push(q);
                    colours.push(c);
                } else {
                    left.push(q);
                    left_colours.push(c);
                }
            }
            let leaf = self.nodes.get(p.index).is_none_or(|r| r.children.iter().all(|&c| c == 0));
            if left.is_empty() && leaf {
                // Nothing of it is left: the node above has all it had.
                continue;
            }
            let at = self.block(&left, &left_colours)?;
            if let Some(r) = self.nodes.get_mut(p.index) {
                (r.len, r.block) = (left.len() as u32, at);
            }
            children[k] = p.index as u32;
        }
        if let Some(r) = self.nodes.get_mut(index) {
            (r.spacing, r.children) = (edge / f64::from(grid), children);
        }
        Ok(Some(Pending { index, points, colours }))
    }

    /// The tree of the points in the file at `path`, built in memory as [`octree::subtree`] builds it: every node written but
    /// its root.
    fn chunk(&mut self, path: &Path, lo: [f64; 3], edge: f64, level: u8) -> Result<Option<Pending>, String> {
        let src = source::FileSource::open(path)?;
        let records = src.size() / RECORD as u64;
        let (mut points, mut colours) = (Vec::new(), Vec::new());
        source::records(&src, 0, (RECORD, records), source::CHUNK, &mut |r| {
            let (p, c) = unrecord(r);
            points.push(p);
            colours.push(c);
            Ok(())
        })?;
        drop(src);
        let _ = std::fs::remove_file(path);
        if points.is_empty() {
            return Ok(None);
        }
        let n = points.len().min(u32::MAX as usize) as u32;
        let tree = octree::subtree(&points, lo, edge, level, (0..n).collect());
        let base = self.nodes.len();
        for node in tree.nodes() {
            let mut children = [0; 8];
            for (k, c) in node.slots().iter().enumerate() {
                if *c != 0 {
                    children[k] = (*c as usize + base) as u32;
                }
            }
            self.nodes.push(Rec { lo: node.lo, edge: node.edge, spacing: node.spacing, level: node.level, children, ..Rec::default() });
        }
        for (i, _) in tree.nodes().iter().enumerate().skip(1) {
            let order = tree.points_of(i);
            let ps: Vec<[f32; 3]> = order.iter().filter_map(|&j| points.get(j as usize).copied()).collect();
            let cs: Vec<[u8; 3]> = order.iter().filter_map(|&j| colours.get(j as usize).copied()).collect();
            let at = self.block(&ps, &cs)?;
            if let Some(r) = self.nodes.get_mut(base + i) {
                (r.len, r.block) = (ps.len() as u32, at);
            }
        }
        let order = tree.points_of(0);
        Ok(Some(Pending {
            index: base,
            points: order.iter().filter_map(|&j| points.get(j as usize).copied()).collect(),
            colours: order.iter().filter_map(|&j| colours.get(j as usize).copied()).collect(),
        }))
    }
}

/// The blocks a store has loaded, and when each was last asked for.
#[derive(Default)]
struct Kept {
    blocks: HashMap<usize, (Arc<Block>, u64)>,
    bytes: usize,
    tick: u64,
}

/// A scan's cache, open: its tree, and its nodes' points loaded as they're asked for.
pub struct Store {
    file: source::FileSource,
    header: Header,
    tree: Octree,
    blocks: Vec<u64>,
    points: u64,
    kept: Mutex<Kept>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Store({:016x}, {} nodes)", self.header.fingerprint, self.blocks.len())
    }
}

impl Store {
    /// The cache at `path`, if it's whole and of the file with `fingerprint`; else why not.
    pub fn open(path: &Path, fingerprint: u64) -> Result<Store, String> {
        let file = source::FileSource::open(path)?;
        let size = file.size();
        let head = source::take(&file, 0, HEADER).map_err(|_| "it's cut short".to_string())?;
        let at = |i: usize, n: usize| head.get(i..i + n).unwrap_or_default();
        let u32_at = |i: usize| at(i, 4).try_into().map_or(0, u32::from_le_bytes);
        let u64_at = |i: usize| at(i, 8).try_into().map_or(0, u64::from_le_bytes);
        let f64_at = |i: usize| at(i, 8).try_into().map_or(f64::NAN, f64::from_le_bytes);
        let f32_at = |i: usize| at(i, 4).try_into().map_or(f32::NAN, f32::from_le_bytes);
        if at(0, 8) != MAGIC {
            return Err("it isn't a scan's cache".into());
        }
        if u32_at(8) != VERSION {
            return Err(format!("it's a cache of version {}; this one reads {VERSION}", u32_at(8)));
        }
        if crc(at(0, HEADER - 4)) != u32_at(HEADER - 4) {
            return Err("its header is damaged".into());
        }
        let header = Header {
            fingerprint: u64_at(16),
            origin: [f64_at(24), f64_at(32), f64_at(40)],
            read: u64_at(48),
            lo: [f32_at(56), f32_at(60), f32_at(64)],
            hi: [f32_at(68), f32_at(72), f32_at(76)],
            colours: u32_at(12) & 1 == 1,
        };
        if header.fingerprint != fingerprint {
            return Err("it's the cache of another file".into());
        }
        if !header.origin.iter().all(|v| v.is_finite() && v.abs() <= MAX_FILE_COORD)
            || !header.lo.iter().chain(&header.hi).all(|v| v.is_finite() && f64::from(v.abs()) <= MAX_SPAN)
        {
            return Err("its header holds a place BIMCraft can't".into());
        }
        let (count, table) = (u32_at(80) as usize, u64_at(88));
        if count == 0 || count > MAX_NODES {
            return Err(format!("it holds {count} nodes; 1 to {MAX_NODES}"));
        }
        let bytes = (count * NODE) as u64;
        if table < HEADER as u64 || table.checked_add(bytes).is_none_or(|end| end > size) {
            return Err("its table is cut short".into());
        }
        let table = source::take(&file, table, count * NODE)?;
        if crc(&table) != u32_at(84) {
            return Err("its table is damaged".into());
        }
        let per = RECORD as u64 - if header.colours { 0 } else { 3 };
        let (mut nodes, mut blocks, mut points) = (Vec::with_capacity(count), Vec::with_capacity(count), 0_u64);
        let mut parented = vec![false; count];
        for (i, b) in table.as_chunks::<NODE>().0.iter().enumerate() {
            let r = Rec::read(b).ok_or("its table is cut short")?;
            let fine = r.lo.iter().all(|v| v.is_finite() && v.abs() <= MAX_SPAN * 2.0)
                && r.edge.is_finite()
                && r.edge > 0.0
                && r.edge <= MAX_SPAN * 2.0
                && r.spacing.is_finite()
                && r.spacing >= 0.0
                && r.level <= MAX_LEVEL
                && (r.len as u64).checked_mul(per).and_then(|b| b.checked_add(r.block + 4)).is_some_and(|end| end <= size)
                && (r.len == 0 || r.block >= HEADER as u64);
            if !fine {
                return Err(format!("its node {i} isn't one"));
            }
            for &c in r.children.iter().filter(|&&c| c != 0) {
                // A child comes after its node, and has no other: the tree is a tree.
                match parented.get_mut(c as usize) {
                    Some(p) if (c as usize) > i && !*p => *p = true,
                    _ => return Err(format!("its node {i} points at node {c} wrongly")),
                }
            }
            points += u64::from(r.len);
            blocks.push(r.block);
            nodes.push(Node::kept(r.lo, r.edge, r.spacing, r.level, r.len as usize, r.children));
        }
        if points > MAX_POINTS {
            return Err(format!("it holds {points} points; up to {MAX_POINTS}"));
        }
        Ok(Store { file, header, tree: Octree::of_nodes(nodes), blocks, points, kept: Mutex::default() })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn tree(&self) -> &Octree {
        &self.tree
    }

    /// The points in all its nodes.
    pub fn points(&self) -> u64 {
        self.points
    }

    /// The points of `node`, loaded once and kept while there's room: none where its block can't be read or is damaged.
    pub fn block(&self, node: usize) -> Arc<Block> {
        if let Ok(mut k) = self.kept.lock() {
            k.tick += 1;
            let tick = k.tick;
            if let Some((b, t)) = k.blocks.get_mut(&node) {
                *t = tick;
                return Arc::clone(b);
            }
        }
        let b = Arc::new(self.load(node).unwrap_or_default());
        if let Ok(mut k) = self.kept.lock() {
            let bytes = b.points.len() * RECORD;
            while k.bytes + bytes > KEEP {
                let Some((&old, _)) = k.blocks.iter().min_by_key(|(_, (_, t))| *t) else { break };
                if let Some((gone, _)) = k.blocks.remove(&old) {
                    k.bytes = k.bytes.saturating_sub(gone.points.len() * RECORD);
                }
            }
            let tick = k.tick;
            k.bytes += bytes;
            k.blocks.insert(node, (Arc::clone(&b), tick));
        }
        b
    }

    fn load(&self, node: usize) -> Option<Block> {
        let len = self.tree.nodes().get(node)?.len();
        let at = *self.blocks.get(node)?;
        let colours = if self.header.colours { len * 3 } else { 0 };
        let bytes = source::take(&self.file, at, len * 12 + colours + 4).ok()?;
        let (body, sum) = bytes.split_at(len * 12 + colours);
        if sum.try_into().map(u32::from_le_bytes).ok()? != crc(body) {
            return None;
        }
        let (ps, cs) = body.split_at(len * 12);
        let points =
            ps.as_chunks::<12>().0.iter().map(|c| [0, 4, 8].map(|i| f32::from_le_bytes([c[i], c[i + 1], c[i + 2], c[i + 3]]))).collect();
        let colours = self.header.colours.then(|| cs.as_chunks::<3>().0.to_vec());
        Some(Block { points, colours })
    }

    /// The upper levels of its tree, no more than `most` points, root first: what a project file keeps of a linked scan.
    pub fn preview(&self, most: usize) -> Block {
        let mut out = Block { points: Vec::new(), colours: self.header.colours.then(Vec::new) };
        let mut queue = VecDeque::from([0_usize]);
        while let Some(n) = queue.pop_front() {
            let Some(node) = self.tree.nodes().get(n) else { continue };
            if n != 0 && out.points.len() + node.len() > most {
                continue;
            }
            let b = self.load(n).unwrap_or_default();
            out.points.extend_from_slice(&b.points);
            if let (Some(to), Some(from)) = (out.colours.as_mut(), b.colours.as_ref()) {
                to.extend_from_slice(from);
            }
            queue.extend(node.children());
        }
        out
    }
}

#[cfg(test)]
mod tests;
