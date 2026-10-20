//! Asset fingerprints: BLAKE3 over `size || first 1 MiB || last 1 MiB`.
//! Fast enough on load, and enough to tell an edited or different file apart.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::model::Fingerprint;

pub const ALGO: &str = "blake3-head-tail-v1";
const CHUNK: u64 = 1 << 20;

pub fn fingerprint_file(path: &Path) -> std::io::Result<Fingerprint> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let mut h = blake3::Hasher::new();
    h.update(&size.to_le_bytes());
    let mut buf = vec![0u8; CHUNK.min(size) as usize];
    f.read_exact(&mut buf)?;
    h.update(&buf);
    let tail_len = CHUNK.min(size);
    f.seek(SeekFrom::Start(size - tail_len))?;
    buf.resize(tail_len as usize, 0);
    f.read_exact(&mut buf)?;
    h.update(&buf);
    Ok(Fingerprint {
        algo: ALGO.into(),
        size,
        head_tail: h.finalize().to_hex().to_string(),
        full: None,
    })
}

/// Result of comparing a file on disk with a stored fingerprint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    Exact,
    /// Different content (edited, replaced, or a different file of the same name).
    Changed,
    /// No stored fingerprint or unknown algorithm.
    Unverified,
}

pub fn check(path: &Path, expected: Option<&Fingerprint>) -> std::io::Result<Match> {
    let Some(exp) = expected else {
        return Ok(Match::Unverified);
    };
    if exp.algo != ALGO {
        return Ok(Match::Unverified);
    }
    if std::fs::metadata(path)?.len() != exp.size {
        return Ok(Match::Changed);
    }
    let fp = fingerprint_file(path)?;
    Ok(if fp.head_tail == exp.head_tail {
        Match::Exact
    } else {
        Match::Changed
    })
}
