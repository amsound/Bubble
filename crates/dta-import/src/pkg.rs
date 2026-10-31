//! Read-only parser for reference packages (`*.pkg`, package version 2): a session
//! plus the audio files it uses, in one file.
//!
//! Layout (`TForm1.SavePackageToFile`, checked against a real package):
//!
//! | Field | Encoding |
//! |---|---|
//! | package id | Int64 (random; repeated at the end) |
//! | package version | Int = 2 |
//! | session version | Int = 99 |
//! | button count | Int |
//! | total audio length | Int64 (sum of the distinct files' data lengths) |
//! | header length, header | Int, then `FileSavePre` + a 32-byte block (first/last button, …) |
//! | per button | Int button index (0-based), Str original path, Int64 file size + the whole file (0 = same path stored earlier), Int record length + cart record (as in a `.dta`) |
//! | images | `SaveBmpsToPackage` (not decoded) |
//! | session options | `FileSavePost`, as in a `.dta` |
//! | converter info | `SavePkgConvToPackage` (not decoded) |
//! | trailer | Int64 file length, Int64 package id |

use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;

use crate::dta::{
    Cart, FORMAT_VERSION, ParseError, Reader, Session, SessionOptions, decode_cp1252, parse_cart,
    parse_post, parse_pre,
};

/// The only package version this parser understands.
pub const PACKAGE_VERSION: i32 = 2;

/// One audio file carried in the package.
#[derive(Clone, Debug, PartialEq)]
pub struct PackedFile {
    /// Path the file had on the machine that saved the package.
    pub original_path: String,
    /// Byte range of the complete file inside the package.
    pub data: Range<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Package {
    pub id: i64,
    /// The session, built from the package header, cart records and options.
    pub session: Session,
    /// Distinct audio files, in package order.
    pub files: Vec<PackedFile>,
    /// Whether the session options block was found and decoded.
    pub options_found: bool,
}

impl Package {
    /// The bytes of a packed file.
    pub fn bytes<'a>(&self, pkg: &'a [u8], file: &PackedFile) -> &'a [u8] {
        &pkg[file.data.clone()]
    }
}

/// Whether `d` looks like a package (version and session-version fields).
pub fn is_package(d: &[u8]) -> bool {
    d.len() >= 32
        && i32::from_le_bytes(d[8..12].try_into().expect("4 bytes")) == PACKAGE_VERSION
        && i32::from_le_bytes(d[12..16].try_into().expect("4 bytes")) == FORMAT_VERSION
}

/// Parses a whole package in memory (tests and small inputs; see [`parse_from`]).
pub fn parse(d: &[u8]) -> Result<Package, ParseError> {
    parse_from(&mut std::io::Cursor::new(d)).map_err(|e| match e {
        ReadError::Parse(e) => e,
        // Reading memory does not fail.
        ReadError::Io(_) => ParseError::Truncated {
            at: 0,
            want: 0,
            end: d.len(),
        },
    })
}

/// Why reading a package failed.
#[derive(Debug)]
pub enum ReadError {
    Parse(ParseError),
    Io(std::io::Error),
}

impl From<ParseError> for ReadError {
    fn from(e: ParseError) -> Self {
        ReadError::Parse(e)
    }
}

impl From<std::io::Error> for ReadError {
    fn from(e: std::io::Error) -> Self {
        ReadError::Io(e)
    }
}

/// The package being read, for its small parts: audio is skipped, not read.
struct Source<'a, R> {
    r: &'a mut R,
    p: usize,
    len: usize,
}

impl<R: Read + Seek> Source<'_, R> {
    fn take(&mut self, n: usize) -> Result<Vec<u8>, ReadError> {
        self.check(n)?;
        let mut b = vec![0; n];
        self.r.read_exact(&mut b)?;
        self.p += n;
        Ok(b)
    }

    fn skip(&mut self, n: usize) -> Result<(), ReadError> {
        self.check(n)?;
        self.p += n;
        self.r.seek(SeekFrom::Start(self.p as u64))?;
        Ok(())
    }

    fn check(&self, n: usize) -> Result<(), ParseError> {
        if n > self.len.saturating_sub(self.p) {
            return Err(ParseError::Truncated {
                at: self.p,
                want: n,
                end: self.len,
            });
        }
        Ok(())
    }

    fn i32(&mut self) -> Result<i32, ReadError> {
        Ok(i32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }

    fn i64(&mut self) -> Result<i64, ReadError> {
        Ok(i64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }
}

/// Parses a package from a file, reading only its small parts into memory (the session
/// and the end of the file); the audio, nearly all of it, is stepped over. The input is
/// only read.
pub fn parse_from<R: Read + Seek>(input: &mut R) -> Result<Package, ReadError> {
    let len = input.seek(SeekFrom::End(0))? as usize;
    input.seek(SeekFrom::Start(0))?;
    let mut r = Source {
        r: input,
        p: 0,
        len,
    };
    let id = r.i64()?;
    let version = r.i32()?;
    if version != PACKAGE_VERSION {
        return Err(ParseError::UnsupportedVersion(version).into());
    }
    let session_version = r.i32()?;
    if session_version != FORMAT_VERSION {
        return Err(ParseError::UnsupportedVersion(session_version).into());
    }
    let count = r.i32()?;
    if !(0..=10_000).contains(&count) {
        return Err(ParseError::Implausible {
            what: "button count",
            value: count.into(),
        }
        .into());
    }
    r.i64()?; // total audio length

    let header_len = r.i32()?;
    let header_len = usize::try_from(header_len).unwrap_or(usize::MAX);
    let header = r.take(header_len)?;
    let mut h = Reader::new(&header, 0, header.len());
    let pre = parse_pre(&mut h)?;

    let mut files: Vec<PackedFile> = Vec::new();
    let mut carts: Vec<Cart> = Vec::new();
    for _ in 0..count {
        let index = r.i32()?;
        let index = u16::try_from(index).map_err(|_| ParseError::Implausible {
            what: "button index",
            value: index.into(),
        })?;
        let path_len = r.i32()?;
        let path_len = usize::try_from(path_len).map_err(|_| ParseError::Implausible {
            what: "string length",
            value: path_len.into(),
        })?;
        let original_path = decode_cp1252(&r.take(path_len)?);
        let size = r.i64()?;
        let size = usize::try_from(size).map_err(|_| ParseError::Implausible {
            what: "file size",
            value: size,
        })?;
        if size > 0 {
            let start = r.p;
            r.skip(size)?;
            files.push(PackedFile {
                original_path: original_path.clone(),
                data: start..start + size,
            });
        } else if !files.iter().any(|f| f.original_path == original_path) {
            return Err(ParseError::Implausible {
                what: "reference to a file not stored earlier in the package",
                value: index.into(),
            }
            .into());
        }
        let cart_len = r.i32()?;
        let cart_len = usize::try_from(cart_len).map_err(|_| ParseError::Implausible {
            what: "cart record length",
            value: cart_len.into(),
        })?;
        let record = r.take(cart_len)?;
        carts.push(parse_cart(&record, 0, cart_len, index)?);
    }

    // What follows the last cart (images, options, the trailer) holds no audio.
    let tail = r.take(len - r.p)?;

    // Images precede the options block and are not decoded: find the options by
    // trying each position, accepting the first that decodes cleanly.
    let tail_end = tail.len().saturating_sub(16);
    let (options, options_found) = (0..(1usize << 16).min(tail_end))
        .find_map(|p| {
            let mut t = Reader::new(&tail, p, tail_end);
            parse_post(&mut t)
                .ok()
                .filter(|o| o.link_names.len() == 256)
        })
        .map_or((SessionOptions::default(), false), |o| (o, true));

    // The trailer is the file's last bytes, whether or not the tail reaches that far.
    let mut trailer = [0u8; 16];
    let mut trailer_ok = false;
    if len >= 24 {
        input.seek(SeekFrom::Start(len as u64 - 16))?;
        input.read_exact(&mut trailer)?;
        let size = i64::from_le_bytes(trailer[..8].try_into().expect("8 bytes"));
        let last = i64::from_le_bytes(trailer[8..].try_into().expect("8 bytes"));
        trailer_ok = size == len as i64 && last == id;
    }

    Ok(Package {
        id,
        session: Session {
            format_version: session_version,
            trailer_ok,
            wav_load_folder: pre.wav_load_folder,
            session_folder: pre.session_folder,
            page_names: pre.page_names,
            grid_columns: pre.grid_columns,
            grid_rows: pre.grid_rows,
            gain_offset: pre.gain_offset,
            carts,
            options,
        },
        files,
        options_found,
    })
}

/// File name for an extracted file: the last component of a Windows or POSIX path,
/// with characters that are invalid on common file systems replaced.
pub fn file_name_of(original: &str) -> String {
    let name = original.rsplit(['\\', '/']).next().unwrap_or(original);
    let clean: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let clean = clean.trim().trim_end_matches('.').to_string();
    if clean.is_empty() {
        "audio.wav".into()
    } else {
        clean
    }
}
