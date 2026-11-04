//! Sections marked in a WAV file: cue points and a play list (`cue ` and `plst`
//! chunks), as an editor such as Audition writes them. Bubble reads the shapes a cart
//! in sections can play: an optional Intro played once, Main repeated, and an optional
//! Outro played once. Anything else (more parts, repeats on the Intro or Outro) is
//! left alone.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// Source frames: each part's start and end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileSections {
    pub intro: Option<(u64, u64)>,
    pub main: (u64, u64),
    pub outro: Option<(u64, u64)>,
}

/// Biggest `cue ` or `plst` chunk read (a few thousand points); larger is not a
/// play list Bubble can use.
const MAX_CHUNK: u32 = 64 * 1024;

/// The sections marked in `path`, if it is a WAV with a play list Bubble can play.
/// Reads only the chunk headers and the two small chunks.
pub fn read(path: &Path) -> Option<FileSections> {
    let mut f = BufReader::new(File::open(path).ok()?);
    let mut head = [0u8; 12];
    f.read_exact(&mut head).ok()?;
    if !matches!(&head[..4], b"RIFF" | b"RF64") || &head[8..12] != b"WAVE" {
        return None;
    }
    let (mut cues, mut list) = (None, None);
    // RF64 keeps the data chunk's real size in `ds64`.
    let mut data_size = None;
    loop {
        let mut h = [0u8; 8];
        if f.read_exact(&mut h).is_err() {
            break;
        }
        let id = [h[0], h[1], h[2], h[3]];
        let mut size = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as u64;
        match &id {
            b"cue " | b"plst" | b"ds64" if size <= MAX_CHUNK as u64 => {
                let mut body = vec![0u8; size as usize];
                f.read_exact(&mut body).ok()?;
                match &id {
                    b"cue " => cues = Some(body),
                    b"plst" => list = Some(body),
                    // ds64: RIFF size, then the data chunk's size.
                    _ => {
                        data_size = body
                            .get(8..16)
                            .and_then(|b| b.try_into().ok())
                            .map(u64::from_le_bytes)
                    }
                }
            }
            _ => {
                if &id == b"data" && size == u32::MAX as u64 {
                    size = data_size?;
                }
                f.seek(SeekFrom::Current(size as i64)).ok()?;
            }
        }
        if size % 2 == 1 {
            f.seek(SeekFrom::Current(1)).ok()?;
        }
        if cues.is_some() && list.is_some() {
            break;
        }
    }
    parse(&cues?, &list?)
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Turns a `cue ` and a `plst` chunk into sections.
pub fn parse(cue: &[u8], plst: &[u8]) -> Option<FileSections> {
    // Each cue point: id, position, chunk id, chunk start, block start, sample offset.
    let n = u32_at(cue, 0)? as usize;
    let point = |id: u32| {
        (0..n).find_map(|i| {
            let at = 4 + i * 24;
            (u32_at(cue, at)? == id).then(|| u32_at(cue, at + 20))?
        })
    };
    // Each play list entry: cue id, length (frames), repeats.
    let m = u32_at(plst, 0)? as usize;
    let mut parts = Vec::with_capacity(m.min(8));
    for i in 0..m {
        let at = 4 + i * 12;
        let start = point(u32_at(plst, at)?)? as u64;
        let len = u32_at(plst, at + 4)? as u64;
        let repeats = u32_at(plst, at + 8)?;
        if len == 0 || repeats == 0 {
            return None;
        }
        parts.push(((start, start + len), repeats));
    }
    let main = parts.iter().position(|p| p.1 > 1)?;
    let once = |p: &((u64, u64), u32)| (p.1 == 1).then_some(p.0);
    let intro = match main {
        0 => None,
        1 => Some(once(&parts[0])?),
        _ => return None,
    };
    let outro = match parts.len() - main - 1 {
        0 => None,
        1 => Some(once(&parts[main + 1])?),
        _ => return None,
    };
    Some(FileSections {
        intro,
        main: parts[main].0,
        outro,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(points: &[(u32, u32)]) -> Vec<u8> {
        let mut v = (points.len() as u32).to_le_bytes().to_vec();
        for (id, at) in points {
            for x in [*id, *at, u32::from_le_bytes(*b"data"), 0, 0, *at] {
                v.extend(x.to_le_bytes());
            }
        }
        v
    }

    fn plst(entries: &[(u32, u32, u32)]) -> Vec<u8> {
        let mut v = (entries.len() as u32).to_le_bytes().to_vec();
        for (id, len, rep) in entries {
            for x in [*id, *len, *rep] {
                v.extend(x.to_le_bytes());
            }
        }
        v
    }

    #[test]
    fn reads_an_intro_and_a_repeated_main() {
        // As Audition writes it: Intro once, Main 999 times.
        let s = parse(
            &cue(&[(1, 0), (2, 25_647)]),
            &plst(&[(1, 25_647, 1), (2, 1_097_151, 999)]),
        )
        .unwrap();
        assert_eq!(s.intro, Some((0, 25_647)));
        assert_eq!(s.main, (25_647, 1_122_798));
        assert_eq!(s.outro, None);
    }

    #[test]
    fn reads_main_alone_and_an_outro() {
        let c = cue(&[(1, 100), (2, 500), (3, 900)]);
        let s = parse(&c, &plst(&[(1, 400, 9999)])).unwrap();
        assert_eq!((s.intro, s.main, s.outro), (None, (100, 500), None));
        let s = parse(&c, &plst(&[(1, 400, 1), (2, 400, 2), (3, 50, 1)])).unwrap();
        assert_eq!(s.outro, Some((900, 950)));
    }

    #[test]
    fn leaves_shapes_it_cannot_play() {
        let c = cue(&[(1, 0), (2, 100), (3, 200), (4, 300)]);
        // Nothing repeats; a repeated Intro; four parts; an unknown cue.
        assert!(parse(&c, &plst(&[(1, 100, 1), (2, 100, 1)])).is_none());
        assert!(parse(&c, &plst(&[(1, 100, 2), (2, 100, 3)])).is_none());
        assert!(
            parse(
                &c,
                &plst(&[(1, 100, 1), (2, 100, 1), (3, 100, 5), (4, 9, 1)])
            )
            .is_none()
        );
        assert!(parse(&c, &plst(&[(9, 100, 5)])).is_none());
    }

    #[test]
    fn reads_the_chunks_from_a_wav() {
        let dir = std::env::temp_dir().join(format!("bubble-markers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("m.wav");
        let mut body = b"WAVE".to_vec();
        let chunk = |v: &mut Vec<u8>, id: &[u8; 4], b: &[u8]| {
            v.extend(id);
            v.extend((b.len() as u32).to_le_bytes());
            v.extend(b);
            if b.len() % 2 == 1 {
                v.push(0);
            }
        };
        chunk(
            &mut body,
            b"fmt ",
            &[1, 0, 1, 0, 0x80, 0xBB, 0, 0, 0, 0x77, 1, 0, 2, 0, 16, 0],
        );
        chunk(&mut body, b"data", &[0; 7]);
        chunk(&mut body, b"cue ", &cue(&[(1, 0), (2, 3)]));
        chunk(&mut body, b"plst", &plst(&[(1, 3, 1), (2, 4, 999)]));
        let mut file = b"RIFF".to_vec();
        file.extend((body.len() as u32).to_le_bytes());
        file.extend(body);
        std::fs::write(&p, file).unwrap();
        let s = read(&p).unwrap();
        assert_eq!((s.intro, s.main), (Some((0, 3)), (3, 7)));
        std::fs::remove_dir_all(&dir).ok();
    }
}
