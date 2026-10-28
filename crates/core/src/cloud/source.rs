//! Where a file of points is read from: a piece at a time, so a scan on disk is never held whole. The readers ask for the
//! bytes at an offset ([`Source::read_at`]); text and records go by in pieces of a few megabytes ([`lines`], [`records`]),
//! and E57 reads the pages it's pointed at.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

/// How many bytes a reader asks for at a time.
pub const CHUNK: usize = 8 << 20;

/// The longest line a text file of points may have, bytes: a point is a few dozen.
pub const MAX_LINE: usize = 64 << 10;

/// Bytes read where they're asked for.
pub trait Source {
    /// How many there are.
    fn size(&self) -> u64;

    /// Fills `buf` from byte `at`, and says how many it put there: fewer only where the bytes end.
    fn read_at(&self, at: u64, buf: &mut [u8]) -> Result<usize, String>;
}

impl Source for &[u8] {
    fn size(&self) -> u64 {
        self.len() as u64
    }

    fn read_at(&self, at: u64, buf: &mut [u8]) -> Result<usize, String> {
        let from = usize::try_from(at).unwrap_or(usize::MAX).min(self.len());
        let part = self.get(from..).unwrap_or_default();
        let n = part.len().min(buf.len());
        if let (Some(to), Some(from)) = (buf.get_mut(..n), part.get(..n)) {
            to.copy_from_slice(from);
        }
        Ok(n)
    }
}

/// A file on disk, read where it's asked.
pub struct FileSource {
    file: Mutex<std::fs::File>,
    size: u64,
    stamp: u64,
}

impl FileSource {
    pub fn open(path: &Path) -> Result<FileSource, String> {
        let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let meta = file.metadata().map_err(|e| format!("{}: {e}", path.display()))?;
        // When it was last written, in nanoseconds; 0 where the system doesn't say.
        let stamp = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        Ok(FileSource { file: Mutex::new(file), size: meta.len(), stamp })
    }

    /// When it was last written: part of what tells a scan's cache it's still the file's.
    pub fn stamp(&self) -> u64 {
        self.stamp
    }
}

impl Source for FileSource {
    fn size(&self) -> u64 {
        self.size
    }

    fn read_at(&self, at: u64, buf: &mut [u8]) -> Result<usize, String> {
        let mut file = self.file.lock().map_err(|_| "the file was left half read".to_string())?;
        file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
        let mut n = 0;
        while n < buf.len() {
            match file.read(buf.get_mut(n..).unwrap_or_default()) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(n)
    }
}

/// Exactly `n` bytes from `at`, or why not.
pub fn take(src: &dyn Source, at: u64, n: usize) -> Result<Vec<u8>, String> {
    let mut out = vec![0; n];
    if src.read_at(at, &mut out)? < n {
        return Err(format!("it's cut short: {n} bytes from byte {at} don't fit in its {}", src.size()));
    }
    Ok(out)
}

/// What [`lines`] hands each line to: its number and its bytes; it says whether to go on.
pub type Line<'a> = dyn FnMut(usize, &[u8]) -> Result<bool, String> + 'a;

/// Hands each line of `src` from byte `from` to `f`, without its `\n` and numbered from 1, as splitting the whole at each `\n`
/// would (the last, after the last `\n`, too): `chunk` bytes are read at a time. `f` says whether to go on.
pub fn lines(src: &dyn Source, from: u64, chunk: usize, f: &mut Line) -> Result<(), String> {
    let mut buf = vec![0; chunk.max(1)];
    let mut carry: Vec<u8> = Vec::new();
    let (mut at, mut n) = (from, 0_usize);
    loop {
        let got = src.read_at(at, &mut buf)?;
        if got == 0 {
            break;
        }
        at += got as u64;
        let mut rest = buf.get(..got).unwrap_or_default();
        while let Some(i) = rest.iter().position(|b| *b == b'\n') {
            n += 1;
            let (line, after) = (rest.get(..i).unwrap_or_default(), rest.get(i + 1..).unwrap_or_default());
            let go = if carry.is_empty() {
                f(n, line)?
            } else {
                carry.extend_from_slice(line);
                let go = f(n, &carry)?;
                carry.clear();
                go
            };
            if !go {
                return Ok(());
            }
            rest = after;
        }
        carry.extend_from_slice(rest);
        if carry.len() > MAX_LINE {
            return Err(format!("line {} is longer than {MAX_LINE} bytes", n + 1));
        }
    }
    f(n + 1, &carry)?;
    Ok(())
}

/// Hands `count` records of `stride` bytes each, from byte `from`, to `f`, reading about `chunk` bytes at a time; a file that
/// ends before the last says so.
pub fn records(
    src: &dyn Source,
    from: u64,
    (stride, count): (usize, u64),
    chunk: usize,
    f: &mut dyn FnMut(&[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let stride = stride.max(1);
    let per = (chunk / stride).max(1);
    let mut buf = vec![0; per * stride];
    let (mut at, mut left) = (from, count);
    while left > 0 {
        let k = usize::try_from(left).map_or(per, |l| l.min(per));
        let want = buf.get_mut(..k * stride).unwrap_or_default();
        if src.read_at(at, want)? < want.len() {
            return Err(format!("it's cut short: {left} of its {count} records are missing"));
        }
        for row in want.chunks_exact(stride) {
            f(row)?;
        }
        at += want.len() as u64;
        left -= k as u64;
    }
    Ok(())
}
