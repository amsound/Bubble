//! Read-only parser for reference SpotOn session files (`*.dta`, format version 99).
//!
//! Every cart record and the `FileSavePost` block must end exactly where the framing says, otherwise the
//! file is rejected. Fields the importer does not use are read and discarded so that
//! the alignment checks still cover them.

use std::fmt;

/// The only session format this parser understands.
pub const FORMAT_VERSION: i32 = 99;

/// `SaveToStream_Strs` replaces a CRLF inside each line with these bytes.
const CRLF_ESC: [u8; 2] = [0xCB, 0xC8];

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("read past end at byte {at:#x} (+{want}, end {end:#x})")]
    Truncated { at: usize, want: usize, end: usize },
    #[error("unsupported session format version {0} (only {FORMAT_VERSION} is supported)")]
    UnsupportedVersion(i32),
    #[error("button {button}: {left} bytes left over in the cart record")]
    CartMisaligned { button: u16, left: isize },
    #[error("session options ended at byte {at:#x}, expected {expected:#x}")]
    TrailerMisaligned { at: usize, expected: usize },
    #[error("implausible {what} ({value})")]
    Implausible { what: &'static str, value: i64 },
}

/// Delphi `TColor`: `0x00BBGGRR`, or a system-colour index when the high byte is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TColor(pub i32);

impl TColor {
    /// `#RRGGBB`, or `None` for a system colour (no fixed RGB value).
    pub fn to_hex(self) -> Option<String> {
        let v = self.0 as u32;
        if v >> 24 != 0 {
            return None;
        }
        Some(format!(
            "#{:02X}{:02X}{:02X}",
            v & 0xff,
            (v >> 8) & 0xff,
            (v >> 16) & 0xff
        ))
    }
}

impl fmt::Display for TColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.to_hex() {
            Some(h) => f.write_str(&h),
            None => write!(f, "sys:{:#010x}", self.0 as u32),
        }
    }
}

/// `MidiMsgRec`, 8 bytes: Any-channel flag, All flag, 2 zero bytes, raw short message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiMsg(pub [u8; 8]);

impl MidiMsg {
    /// `MidiMsgValid`: status byte has bit 7 set and the data bytes do not.
    pub fn is_valid(&self) -> bool {
        let r = &self.0;
        r[4] & 0x80 != 0 && (r[5] | r[6] | r[7]) & 0x80 == 0
    }
    pub fn any_channel(&self) -> bool {
        self.0[0] != 0
    }
    pub fn all(&self) -> bool {
        self.0[1] != 0
    }
    pub fn status(&self) -> u8 {
        self.0[4]
    }
    /// 1–16.
    pub fn channel(&self) -> u8 {
        (self.0[4] & 0x0f) + 1
    }
    pub fn data1(&self) -> u8 {
        self.0[5]
    }
    pub fn data2(&self) -> u8 {
        self.0[6]
    }
    pub fn hex(&self) -> String {
        hex(&self.0)
    }
}

/// `HotKeyRec`, 4 bytes: Word Windows virtual-key code, Byte flag, Byte `TShiftState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct HotKey {
    pub vk: u16,
    pub flag: u8,
    /// Bit 0 Shift, bit 1 Alt, bit 2 Ctrl.
    pub shift: u8,
}

impl HotKey {
    pub fn is_set(&self) -> bool {
        self.vk != 0
    }
}

/// `FileSavePre` and the session options the importer uses.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub format_version: i32,
    /// The random token at offset 0 matches the one at EOF and the stored size matches.
    pub trailer_ok: bool,
    pub wav_load_folder: String,
    pub session_folder: String,
    pub page_names: Vec<String>,
    pub grid_columns: i32,
    pub grid_rows: i32,
    /// dB of headroom: 10 = the reference's "Gain offset −10 dB" mode.
    pub gain_offset: i32,
    /// Cart records in file order (play-count order); use [`Cart::index`] for the slot.
    pub carts: Vec<Cart>,
    pub options: SessionOptions,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct SessionOptions {
    pub use_midi_in: bool,
    pub use_midi_out: bool,
    pub hotkeys_on: bool,
    pub use_master_slave_links: bool,
    pub use_gpi_in: bool,
    /// Tenths of a second.
    pub fade_out_default: u16,
    /// 256 link names, indexed by link id; empty = default "Link n".
    pub link_names: Vec<String>,
    pub use_udp_midi: bool,
    pub use_tcp_midi: bool,
    pub use_pbus: bool,
    pub use_play_delay: bool,
    pub network_target: String,
    pub computer_name: String,
    pub user_name: String,
}

/// One cart record (`TTrackFrameObj.SaveToStream`, version 99).
#[derive(Clone, Debug, PartialEq)]
pub struct Cart {
    /// 0-based button (slot) index from the record header.
    pub index: u16,
    pub channel_mask: u16,
    /// `(valid, nAvgBytesPerSec)` from the format BoolInts, pair 0.
    pub avg_bytes_per_sec: Option<u32>,
    pub output_device_index: i32,
    pub output_device_guid: String,
    pub filename: String,
    pub alt_filename: String,
    pub remote_filename: String,
    pub track_name: String,
    pub panel_colour: TColor,
    pub looped: bool,
    /// dB, rounded.
    pub preset_gain_db: i32,
    pub comment: String,
    pub original_local_filename: String,
    pub original_remote_filename: String,
    pub button_image: String,
    pub hotkey_on: bool,
    /// Byte offset into the data chunk.
    pub trim_start_bytes: i64,
    /// Byte offset; equals the data-chunk length when untrimmed.
    pub trim_end_bytes: i64,
    pub pitch: i32,
    pub stop_all_on_play: bool,
    /// Tenths of a second.
    pub fade_out_ds: i32,
    /// Tenths of a second.
    pub fade_in_ds: i32,
    pub play_next: bool,
    pub midi_play_in: MidiMsg,
    pub midi_play_out: MidiMsg,
    pub stop_row_on_play: bool,
    pub muted: bool,
    pub locked_midi_play_in: MidiMsg,
    pub locked_midi_play_out: MidiMsg,
    pub hotkey: HotKey,
    /// Link ids this cart is master / play slave / stop slave of.
    pub master_links: Vec<u8>,
    pub play_slave_links: Vec<u8>,
    pub stop_slave_links: Vec<u8>,
    /// 0 = none, 1–25.
    pub play_group: u16,
    pub locked_hotkey: HotKey,
    pub midi_stop_in: MidiMsg,
    pub locked_midi_stop_in: MidiMsg,
    pub play_delay_max: u16,
    /// 1 = logarithmic (default), 0 = linear.
    pub fade_in_law: u16,
    pub fade_out_law: u16,
    pub slow_down_time: i16,
    pub play_seq_count: u16,
    pub alternate_pan: i16,
    pub fast_midi_mute: bool,
    pub pan: f32,
    pub left_to_both: bool,
    pub right_to_both: bool,
    pub left_phase: bool,
    pub right_phase: bool,
    pub active_effects: u32,
    pub mouse_up_stop: bool,
    pub stop_page_on_play: bool,
    pub high_pass_filter: u16,
    pub low_pass_filter: u16,
    pub midi_stop_out: MidiMsg,
    pub locked_midi_stop_out: MidiMsg,
}

impl Cart {
    /// 1-based button number as shown by the reference app.
    pub fn button(&self) -> u16 {
        self.index + 1
    }

    /// The path the cart plays: the current file, else its alternate, else the
    /// original local / remote source.
    pub fn source_path(&self) -> Option<&str> {
        [
            &self.filename,
            &self.alt_filename,
            &self.original_local_filename,
            &self.remote_filename,
            &self.original_remote_filename,
        ]
        .into_iter()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
    }

    /// Same rule as the reference parser: anything that identifies a used slot.
    pub fn is_used(&self) -> bool {
        self.source_path().is_some() || !self.track_name.is_empty() || !self.comment.is_empty()
    }
}

/// Parses a whole session. The input is only read.
pub fn parse(d: &[u8]) -> Result<Session, ParseError> {
    let mut r = Reader::new(d, 0, d.len());
    let magic = r.take(8)?;
    r.i32()?;
    let format_version = r.i32()?;
    if format_version != FORMAT_VERSION {
        return Err(ParseError::UnsupportedVersion(format_version));
    }
    let pre = parse_pre(&mut r)?;
    // SaveDataToStream middle: markers, build date, reserved, loaded carts, total length, 32 bools.
    r.skip(3 * 4 + 5 * 4 + 4 + 8 + 32)?;

    let mut carts = Vec::new();
    loop {
        let at = r.p;
        let hdr = r.u32()? as usize;
        let (len, idx) = (hdr >> 12, (hdr & 0xfff) as u16);
        // FileSavePost starts with bools, which never form a plausible header.
        if !(200..=1 << 20).contains(&len) || r.p + len > d.len().saturating_sub(16) {
            r.p = at;
            break;
        }
        carts.push(parse_cart(d, r.p, len, idx)?);
        r.p += len;
    }

    let options = parse_post(&mut r)?;
    let expected = d.len().saturating_sub(16);
    if r.p != expected {
        return Err(ParseError::TrailerMisaligned { at: r.p, expected });
    }
    let size_ok = d.len() >= 24
        && i64::from_le_bytes(d[d.len() - 16..d.len() - 8].try_into().expect("8 bytes"))
            == d.len() as i64;
    let trailer_ok = size_ok && &d[d.len() - 8..] == magic;
    Ok(Session {
        format_version,
        trailer_ok,
        wav_load_folder: pre.wav_load_folder,
        session_folder: pre.session_folder,
        page_names: pre.page_names,
        grid_columns: pre.grid_columns,
        grid_rows: pre.grid_rows,
        gain_offset: pre.gain_offset,
        carts,
        options,
    })
}

/// The `FileSavePre` block: folders, page names, grid and gain offset.
pub(crate) struct Pre {
    pub wav_load_folder: String,
    pub session_folder: String,
    pub page_names: Vec<String>,
    pub grid_columns: i32,
    pub grid_rows: i32,
    pub gain_offset: i32,
}

pub(crate) fn parse_pre(r: &mut Reader) -> Result<Pre, ParseError> {
    let wav_load_folder = r.s()?;
    let session_folder = r.s()?;
    r.s()?; // text export folder
    let n_pages = r.i32()?;
    if !(0..=64).contains(&n_pages) {
        return Err(ParseError::Implausible {
            what: "page count",
            value: n_pages.into(),
        });
    }
    let page_names = (0..n_pages).map(|_| r.s()).collect::<Result<_, _>>()?;
    let grid_columns = r.i32()?;
    let grid_rows = r.i32()?;
    r.s()?; // form string +0xd80
    let gain_offset = r.i32()?;
    r.skip(2 * 4 + 5 * 4)?; // virtual screen size, reserved
    Ok(Pre {
        wav_load_folder,
        session_folder,
        page_names,
        grid_columns,
        grid_rows,
        gain_offset,
    })
}

pub(crate) fn parse_cart(d: &[u8], off: usize, len: usize, index: u16) -> Result<Cart, ParseError> {
    let mut r = Reader::new(d, off, off + len);
    let channel_mask = r.u16()?;
    let format = r.boolints()?;
    let avg_bytes_per_sec = format
        .first()
        .filter(|(valid, v)| *valid && *v > 0)
        .map(|(_, v)| *v as u32);
    r.boolints()?; // reserved
    r.skip(8)?; // file date/time (TDateTime, UTC)
    let output_device_index = r.i32()?;
    let output_device_guid = r.guid()?;
    let fa = r.s()?;
    let (filename, alt_filename) = match fa.split_once("?**?") {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (fa, String::new()),
    };
    let remote_filename = r.s()?;
    let track_name = r.s()?;
    let panel_colour = TColor(r.i32()?);
    let looped = r.b()?;
    let preset_gain_db = r.i32()?;
    let pan_int = r.i32()?;
    r.skip(3 * 40 + 4)?; // three FontAttr, loop-line width
    let mut lines = r.strs()?;
    lines.resize(8, String::new());
    let mut lines = lines.into_iter();
    let mut next = || lines.next().unwrap_or_default();
    let (comment, original_local_filename, original_remote_filename, button_image) =
        (next(), next(), next(), next());
    let hotkey_on = r.b()?;
    let trim_start_bytes = r.i64()?;
    let trim_end_bytes = r.i64()?;
    let pitch = r.i32()?;
    let stop_all_on_play = r.b()?;
    let fade_out_ds = r.i32()?;
    let fade_in_ds = r.i32()?;
    r.i32()?; // unassigned panel colour
    let play_next = r.b()?;
    r.skip(1 + 1 + 20)?; // const true, auto-size fonts, auto-size reference
    let midi_play_in = r.midi()?;
    let midi_play_out = r.midi()?;
    r.b()?; // track name centred
    let stop_row_on_play = r.b()?;
    let muted = r.b()?;
    r.b()?; // flag +0x149
    let mut master = r.take(4)?.to_vec();
    let mut play_slave = r.take(4)?.to_vec();
    let mut stop_slave = r.take(4)?.to_vec();
    let locked_midi_play_in = r.midi()?;
    let locked_midi_play_out = r.midi()?;
    let hotkey = r.hotkey()?;
    r.skip(16)?; // play-list record
    let n = r.i32()?;
    if n < 0 {
        return Err(ParseError::Implausible {
            what: "waveform size",
            value: n.into(),
        });
    }
    r.skip(n as usize)?;

    // Reserved block 1: 128-byte budget, 124 bytes on disk.
    let b1 = r.p;
    master.extend_from_slice(r.take(28)?);
    play_slave.extend_from_slice(r.take(28)?);
    stop_slave.extend_from_slice(r.take(28)?);
    let play_group = r.u16()?;
    let locked_hotkey = r.hotkey()?;
    r.b()?; // const false
    let midi_stop_in = r.midi()?;
    let locked_midi_stop_in = r.midi()?;
    r.b()?; // colour locked
    r.u16()?; // channel mask copy
    let play_delay_max = r.u16()?;
    r.i16()?; // VO fade depth
    let fade_in_law = r.u16()?;
    let fade_out_law = r.u16()?;
    r.u16()?; // user play delay max
    let slow_down_time = r.i16()?;
    r.pad_to(b1, 124)?;
    r.boolints()?; // reserved
    let play_seq_count = r.u16()?;
    r.skip(play_seq_count as usize * 20)?;

    // Reserved block 2: 256-byte budget, 252 bytes on disk.
    let b2 = r.p;
    r.skip(2 + 8 + 3 * 2)?; // comment-field index, comment hash, user VO gain / fades
    let alternate_pan = r.i16()?;
    r.i32()?; // bitmap panel colour
    let fast_midi_mute = r.b()?;
    r.i32()?; // fast MIDI mute timeout
    let pan = pan_int as f32 + r.f32()?;
    let left_to_both = r.b()?;
    let right_to_both = r.b()?;
    let left_phase = r.b()?;
    let right_phase = r.b()?;
    let active_effects = r.u32()?;
    r.skip(1 + 4 * 2 + 1)?; // echo
    r.skip(1 + 5 * 2 + 2 + 2)?; // flanger
    let mouse_up_stop = r.b()?;
    let stop_page_on_play = r.b()?;
    r.skip(4 + 1)?;
    r.skip(1 + 4 + 2)?; // reverb
    r.skip(1 + 2 + 1)?; // gargle
    r.skip(4 + 2 + 2)?; // frame bookkeeping
    r.i32()?; // fade-in copy
    r.skip(3 * (2 + 4 + 2 + 4))?; // virtual play-list segments
    r.skip(16)?; // play-list record copy
    let high_pass_filter = r.u16()?;
    let low_pass_filter = r.u16()?;
    let midi_stop_out = r.midi()?;
    let locked_midi_stop_out = r.midi()?;
    r.pad_to(b2, 252)?;
    if r.p != r.end {
        return Err(ParseError::CartMisaligned {
            button: index + 1,
            left: r.end as isize - r.p as isize,
        });
    }

    Ok(Cart {
        index,
        channel_mask,
        avg_bytes_per_sec,
        output_device_index,
        output_device_guid,
        filename,
        alt_filename,
        remote_filename,
        track_name,
        panel_colour,
        looped,
        preset_gain_db,
        comment,
        original_local_filename,
        original_remote_filename,
        button_image,
        hotkey_on,
        trim_start_bytes,
        trim_end_bytes,
        pitch,
        stop_all_on_play,
        fade_out_ds,
        fade_in_ds,
        play_next,
        midi_play_in,
        midi_play_out,
        stop_row_on_play,
        muted,
        locked_midi_play_in,
        locked_midi_play_out,
        hotkey,
        master_links: bitset(&master),
        play_slave_links: bitset(&play_slave),
        stop_slave_links: bitset(&stop_slave),
        play_group,
        locked_hotkey,
        midi_stop_in,
        locked_midi_stop_in,
        play_delay_max,
        fade_in_law,
        fade_out_law,
        slow_down_time,
        play_seq_count,
        alternate_pan,
        fast_midi_mute,
        pan,
        left_to_both,
        right_to_both,
        left_phase,
        right_phase,
        active_effects,
        mouse_up_stop,
        stop_page_on_play,
        high_pass_filter,
        low_pass_filter,
        midi_stop_out,
        locked_midi_stop_out,
    })
}

pub(crate) fn parse_post(r: &mut Reader) -> Result<SessionOptions, ParseError> {
    let mut o = SessionOptions {
        use_midi_in: r.b()?,
        use_midi_out: r.b()?,
        hotkeys_on: r.b()?,
        ..Default::default()
    };
    r.skip(1 + 4 + 1)?;
    r.s()?; // custom colours
    r.skip(4 + 4 + 1)?; // MIDI device ids, bool
    o.use_master_slave_links = r.b()?;
    r.skip(1 + 1 + 4 * 4 + 4 + 2)?; // flags, window rect, gain offset, joystick id
    o.use_gpi_in = r.b()?;
    r.skip(8 * 2 + 1 + 1 + 1 + 1 + 1)?; // GPI buttons, flags, edge width, hardware mixer

    // Block A: exactly 128 bytes.
    let a = r.p;
    r.skip(2 + 1 + 1 + 1)?;
    o.fade_out_default = r.u16()?;
    r.skip(1 + 4 + 4 + 1 + 8 * 8 + 1 + 1 + 1 + 1 + 1 + 2 + 1 + 1 + 4 + 4 + 1 + 2 + 8)?;
    r.skip(1 + 1 + 2 + 1 + 1)?;
    o.use_udp_midi = r.b()?;
    r.skip(1 + 1)?;
    o.use_play_delay = r.b()?;
    r.skip(1 + 1 + 4 + 1)?;
    r.pad_to(a, 128)?;

    o.link_names = (0..256).map(|_| r.s()).collect::<Result<_, _>>()?;
    for _ in 0..17 + 4 {
        r.s()?;
    }
    r.skip(16 * 4 + 16 * 8 + 16 * 2)?;

    // Block B: exactly 4096 bytes.
    let b = r.p;
    r.skip(8 * 4 + 2 + 2 + 4 + 8 * 4 + 4 + 4 + 1 + 7 * 8 + 3 * 8)?;
    r.skip(64 * 4 + 64 * 8 + 64 * 2 + 64 * 2)?; // GPI table
    r.skip(1 + 8 + 1)?;
    o.use_tcp_midi = r.b()?;
    r.skip(1 + 8 + 1 + 4 + 1)?;
    o.use_pbus = r.b()?;
    r.skip(8 * 4 + 32 * 4 + 1 + 1 + 1 + 1)?; // pause links, output colours, flags
    o.network_target = r.s()?;
    r.skip(4 + 6)?; // Int, 6 flags
    r.skip(1)?; // trigger size
    let bars = r.u8()?;
    r.skip(bars as usize * 24)?;
    r.skip(2 + 2 + 3 * 40 + 1 + 1 + 1 + 1)?;
    r.s()?; // editor filename
    r.skip(1 + 1 + 8 + 1 + 32 * 4 + 1 + 4)?;
    o.computer_name = r.s()?;
    o.user_name = r.s()?;
    r.skip(4 + 2 + 1)?;
    r.pad_to(b, 4096)?;
    r.skip(8)?; // total track length (again)
    Ok(o)
}

fn bitset(raw: &[u8]) -> Vec<u8> {
    (0..raw.len() * 8)
        .filter(|i| raw[i >> 3] >> (i & 7) & 1 != 0)
        .map(|i| i as u8)
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Decodes Windows-1252 (the reference app writes ANSI strings).
pub fn decode_cp1252(b: &[u8]) -> String {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    b.iter()
        .map(|&c| match c {
            0x80..=0x9f => HIGH[(c - 0x80) as usize],
            _ => c as char,
        })
        .collect()
}

pub(crate) struct Reader<'a> {
    d: &'a [u8],
    pub(crate) p: usize,
    end: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(d: &'a [u8], p: usize, end: usize) -> Self {
        Reader { d, p, end }
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], ParseError> {
        if n > self.end.saturating_sub(self.p) {
            return Err(ParseError::Truncated {
                at: self.p,
                want: n,
                end: self.end,
            });
        }
        let b = &self.d[self.p..self.p + n];
        self.p += n;
        Ok(b)
    }

    pub(crate) fn skip(&mut self, n: usize) -> Result<(), ParseError> {
        self.take(n).map(|_| ())
    }

    fn arr<const N: usize>(&mut self) -> Result<[u8; N], ParseError> {
        Ok(self.take(N)?.try_into().expect("length checked"))
    }

    fn u8(&mut self) -> Result<u8, ParseError> {
        Ok(self.take(1)?[0])
    }
    fn b(&mut self) -> Result<bool, ParseError> {
        Ok(self.u8()? != 0)
    }
    pub(crate) fn u16(&mut self) -> Result<u16, ParseError> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    fn i16(&mut self) -> Result<i16, ParseError> {
        Ok(i16::from_le_bytes(self.arr()?))
    }
    pub(crate) fn i32(&mut self) -> Result<i32, ParseError> {
        Ok(i32::from_le_bytes(self.arr()?))
    }
    fn u32(&mut self) -> Result<u32, ParseError> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    pub(crate) fn i64(&mut self) -> Result<i64, ParseError> {
        Ok(i64::from_le_bytes(self.arr()?))
    }
    fn f32(&mut self) -> Result<f32, ParseError> {
        Ok(f32::from_le_bytes(self.arr()?))
    }

    fn len_prefix(&mut self) -> Result<usize, ParseError> {
        let n = self.i32()?;
        usize::try_from(n).map_err(|_| ParseError::Implausible {
            what: "string length",
            value: n.into(),
        })
    }

    pub(crate) fn s(&mut self) -> Result<String, ParseError> {
        let n = self.len_prefix()?;
        Ok(decode_cp1252(self.take(n)?))
    }

    /// `SaveToStream_Strs`: one Str holding `TStringList.Text` (lines ending CRLF).
    fn strs(&mut self) -> Result<Vec<String>, ParseError> {
        let n = self.len_prefix()?;
        let raw = self.take(n)?;
        let mut lines: Vec<&[u8]> = split_crlf(raw);
        if lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        Ok(lines
            .into_iter()
            .map(|l| {
                let mut v = Vec::with_capacity(l.len());
                let mut i = 0;
                while i < l.len() {
                    if l[i..].starts_with(&CRLF_ESC) {
                        v.extend_from_slice(b"\r\n");
                        i += 2;
                    } else {
                        v.push(l[i]);
                        i += 1;
                    }
                }
                decode_cp1252(&v)
            })
            .collect())
    }

    fn boolints(&mut self) -> Result<Vec<(bool, i32)>, ParseError> {
        let n = self.i32()?;
        if !(0..=64).contains(&n) {
            return Err(ParseError::Implausible {
                what: "BoolInts count",
                value: n.into(),
            });
        }
        (0..n).map(|_| Ok((self.b()?, self.i32()?))).collect()
    }

    fn guid(&mut self) -> Result<String, ParseError> {
        let g = self.take(16)?;
        // Windows mixed-endian: Data1 (LE u32), Data2/Data3 (LE u16), Data4 bytes.
        Ok(format!(
            "{:08X}-{:04X}-{:04X}-{}-{}",
            u32::from_le_bytes(g[0..4].try_into().expect("4")),
            u16::from_le_bytes(g[4..6].try_into().expect("2")),
            u16::from_le_bytes(g[6..8].try_into().expect("2")),
            hex(&g[8..10]).to_uppercase(),
            hex(&g[10..16]).to_uppercase()
        ))
    }

    fn midi(&mut self) -> Result<MidiMsg, ParseError> {
        Ok(MidiMsg(self.arr()?))
    }

    fn hotkey(&mut self) -> Result<HotKey, ParseError> {
        let [a, b, flag, shift] = self.arr()?;
        Ok(HotKey {
            vk: u16::from_le_bytes([a, b]),
            flag,
            shift,
        })
    }

    /// Reserved blocks: zero padding up to `size` bytes after `start`.
    fn pad_to(&mut self, start: usize, size: usize) -> Result<(), ParseError> {
        let used = self.p - start;
        if used > size {
            return Err(ParseError::Implausible {
                what: "reserved block overrun",
                value: (used - size) as i64,
            });
        }
        self.skip(size - used)
    }
}

fn split_crlf(raw: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i + 1 < raw.len() {
        if raw[i] == b'\r' && raw[i + 1] == b'\n' {
            out.push(&raw[start..i]);
            i += 2;
            start = i;
        } else {
            i += 1;
        }
    }
    out.push(&raw[start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcolor_is_bgr() {
        assert_eq!(TColor(0x00A56E3A).to_hex().as_deref(), Some("#3A6EA5"));
        assert_eq!(TColor(0x0000FF).to_hex().as_deref(), Some("#FF0000"));
        assert_eq!(TColor(0x8000_000Fu32 as i32).to_hex(), None);
    }

    #[test]
    fn midi_sentinel_is_invalid() {
        assert!(!MidiMsg([1, 0, 0, 0, 0, 0x80, 0, 0]).is_valid());
        let m = MidiMsg([0, 0, 0, 0, 0x92, 60, 100, 0]);
        assert!(m.is_valid());
        assert_eq!((m.channel(), m.data1()), (3, 60));
    }

    #[test]
    fn cp1252_high_range() {
        assert_eq!(decode_cp1252(b"caf\xe9 \x80"), "café €");
    }

    #[test]
    fn strs_unescape_and_split() {
        let text = b"a\xcb\xc8b\r\nY:\\x.wav\r\n\r\n\r\n";
        let mut data = (text.len() as i32).to_le_bytes().to_vec();
        data.extend_from_slice(text);
        let mut r = Reader::new(&data, 0, data.len());
        assert_eq!(r.strs().unwrap(), vec!["a\r\nb", "Y:\\x.wav", "", ""]);
    }

    #[test]
    fn rejects_other_versions() {
        let mut d = vec![0u8; 16];
        d[12..16].copy_from_slice(&98i32.to_le_bytes());
        assert_eq!(parse(&d), Err(ParseError::UnsupportedVersion(98)));
    }
}
