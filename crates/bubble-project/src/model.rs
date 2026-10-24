//! Project format v1. Field names follow the published JSON shape.
//! Every struct keeps unknown fields in `extra` so files written by newer minor
//! versions round-trip without data loss.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use uuid::Uuid;

pub const FORMAT_NAME: &str = "bubble-project";
pub const CURRENT_FORMAT_VERSION: u32 = 1;
/// The first output. Carts name their output by letter: "A", "B", …; each machine
/// maps letters to devices and channels, so a project is portable.
pub const MAIN_OUTPUT: &str = "A";
/// Most outputs a machine can define (A–Z).
pub const MAX_OUTPUTS: usize = 26;

/// "A" → 0, "B" → 1, …; `None` for anything that is not one letter A–Z.
pub fn output_index(letter: &str) -> Option<usize> {
    let mut c = letter.chars();
    match (c.next(), c.next()) {
        (Some(ch @ 'A'..='Z'), None) => Some(ch as usize - 'A' as usize),
        _ => None,
    }
}

/// 0 → "A", 1 → "B", …
pub fn output_letter(index: usize) -> String {
    char::from(b'A' + (index.min(MAX_OUTPUTS - 1) as u8)).to_string()
}

pub type Extra = Map<String, Value>;

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub format: String,
    pub format_version: u32,
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_with: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_with: Option<String>,
    #[serde(default = "default_search_paths")]
    pub asset_search_paths: Vec<String>,
    #[serde(default)]
    pub settings: ProjectSettings,
    #[serde(default)]
    pub assets: Vec<AssetRef>,
    pub pages: Vec<Page>,
    #[serde(default)]
    pub groups: Vec<ExclusiveGroup>,
    #[serde(default)]
    pub bindings: Vec<Binding>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    #[serde(flatten)]
    pub extra: Extra,
}

fn default_search_paths() -> Vec<String> {
    vec![".".into()]
}

impl ExclusiveGroup {
    /// Tile label: "G1", or "G1 Name" when named.
    pub fn label(&self, index: usize) -> String {
        if self.name.trim().is_empty() {
            format!("G{}", index + 1)
        } else {
            format!("G{} {}", index + 1, self.name.trim())
        }
    }

    /// In lists and menus: "Group 1", or "Group 1: Name" when named.
    pub fn display_name(&self, index: usize) -> String {
        if self.name.trim().is_empty() {
            format!("Group {}", index + 1)
        } else {
            format!("Group {}: {}", index + 1, self.name.trim())
        }
    }
}

impl Project {
    /// A new empty project with one 4 × 6 page.
    pub fn new(name: impl Into<String>) -> Self {
        Project {
            format: FORMAT_NAME.into(),
            format_version: CURRENT_FORMAT_VERSION,
            id: Uuid::new_v4(),
            name: name.into(),
            created_with: Some(app_version()),
            modified_with: Some(app_version()),
            asset_search_paths: default_search_paths(),
            settings: ProjectSettings::default(),
            assets: Vec::new(),
            pages: vec![Page::new("Page 1", 4, 6)],
            groups: default_groups(),
            bindings: Vec::new(),
            notes: String::new(),
            extra: Extra::new(),
        }
    }

    pub fn asset(&self, id: Uuid) -> Option<&AssetRef> {
        self.assets.iter().find(|a| a.id == id)
    }

    pub fn asset_mut(&mut self, id: Uuid) -> Option<&mut AssetRef> {
        self.assets.iter_mut().find(|a| a.id == id)
    }

    /// Iterates over all carts with their page index.
    pub fn carts(&self) -> impl Iterator<Item = (usize, &Cell)> {
        self.pages
            .iter()
            .enumerate()
            .flat_map(|(pi, p)| p.cells.iter().map(move |c| (pi, c)))
    }

    pub fn cart(&self, id: Uuid) -> Option<&Cart> {
        self.carts().map(|(_, c)| &c.cart).find(|c| c.id == id)
    }

    pub fn cart_mut(&mut self, id: Uuid) -> Option<&mut Cart> {
        self.pages
            .iter_mut()
            .flat_map(|p| p.cells.iter_mut())
            .map(|c| &mut c.cart)
            .find(|c| c.id == id)
    }

    /// (page index, row, col) of a cart.
    pub fn cart_position(&self, id: Uuid) -> Option<(usize, u16, u16)> {
        self.carts()
            .find(|(_, c)| c.cart.id == id)
            .map(|(p, c)| (p, c.row, c.col))
    }

    /// Removes assets no cart references.
    pub fn prune_unused_assets(&mut self) -> usize {
        let used: std::collections::HashSet<Uuid> =
            self.carts().filter_map(|(_, c)| c.cart.asset).collect();
        let before = self.assets.len();
        self.assets.retain(|a| used.contains(&a.id));
        before - self.assets.len()
    }
}

/// Number of exclusive groups a new project starts with.
pub const DEFAULT_GROUPS: usize = 8;

/// Eight unnamed exclusive groups (shown as G1…G8).
pub fn default_groups() -> Vec<ExclusiveGroup> {
    (0..DEFAULT_GROUPS)
        .map(|_| ExclusiveGroup {
            id: Uuid::new_v4(),
            name: String::new(),
            color: None,
            extra: Extra::new(),
        })
        .collect()
}

pub fn app_version() -> String {
    format!("Bubble {}", env!("CARGO_PKG_VERSION"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retrigger {
    #[default]
    Restart,
    Overlap,
    ToggleStop,
    Ignore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseMode {
    /// Left click plays a stopped cart and stops a playing one.
    #[default]
    Toggle,
    /// Left click plays (per the cart's retrigger policy), right click stops.
    LeftPlayRightStop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicatePolicy {
    /// All carts bound to the same input play together.
    #[default]
    Ganged,
    /// Each trigger plays the next bound cart that is not playing (round-robin).
    Overlapped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    #[default]
    Global,
    #[serde(alias = "page")]
    CurrentPage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Numbering {
    #[default]
    PerPage,
    Continuous,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectSettings {
    pub default_fade_out_ms: u32,
    pub default_retrigger: Retrigger,
    pub mouse_mode: MouseMode,
    pub duplicate_binding_policy: DuplicatePolicy,
    pub binding_scope: Scope,
    pub numbering: Numbering,
    pub ending_warning_s: f32,
    /// Global output attenuation (REF "gain offset"). Default −10 dB leaves headroom
    /// so individual carts can be raised by up to +10 dB.
    pub master_gain_db: f32,
    pub limiter: bool,
    /// Audio brought into the session is converted to WAV at the engine's sample
    /// rate, so opening the session needs no decoding or resampling.
    pub convert_to_wav: bool,
    /// How cart tiles are drawn.
    pub display: Display,
    #[serde(flatten)]
    pub extra: Extra,
}

pub const DEFAULT_MASTER_GAIN_DB: f32 = -10.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HAlign {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VAlign {
    Top,
    #[default]
    Center,
    Bottom,
}

/// Tile appearance (project-wide).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    /// Typeface name for cart labels (`None` = built-in default).
    pub font: Option<String>,
    /// Label size multiplier (0.5–3).
    pub label_size: f32,
    /// Size multiplier for the small text: number, time, triggers, icons (0.5–3).
    pub small_size: f32,
    pub h_align: HAlign,
    pub v_align: VAlign,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Default for Display {
    fn default() -> Self {
        Display {
            font: None,
            label_size: 1.0,
            small_size: 1.0,
            h_align: HAlign::Center,
            v_align: VAlign::Center,
            extra: Extra::new(),
        }
    }
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            default_fade_out_ms: 100,
            default_retrigger: Retrigger::Restart,
            mouse_mode: MouseMode::Toggle,
            duplicate_binding_policy: DuplicatePolicy::Ganged,
            binding_scope: Scope::Global,
            numbering: Numbering::PerPage,
            ending_warning_s: 3.0,
            master_gain_db: DEFAULT_MASTER_GAIN_DB,
            limiter: true,
            convert_to_wav: false,
            display: Display::default(),
            extra: Extra::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub id: Uuid,
    pub name: String,
    pub rows: u16,
    pub cols: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub cells: Vec<Cell>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Page {
    pub fn new(name: impl Into<String>, rows: u16, cols: u16) -> Self {
        Page {
            id: Uuid::new_v4(),
            name: name.into(),
            rows,
            cols,
            color: None,
            cells: Vec::new(),
            extra: Extra::new(),
        }
    }

    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        self.cells.iter().find(|c| c.row == row && c.col == col)
    }

    pub fn cell_mut(&mut self, row: u16, col: u16) -> Option<&mut Cell> {
        self.cells.iter_mut().find(|c| c.row == row && c.col == col)
    }

    /// Returns the cart at (row, col), creating an empty cart there if needed.
    pub fn ensure_cart(&mut self, row: u16, col: u16) -> &mut Cart {
        if let Some(i) = self.cells.iter().position(|c| c.row == row && c.col == col) {
            return &mut self.cells[i].cart;
        }
        self.cells.push(Cell {
            row,
            col,
            cart: Cart::new(),
        });
        &mut self.cells.last_mut().expect("just pushed").cart
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub row: u16,
    pub col: u16,
    pub cart: Cart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Curve {
    Linear,
    #[default]
    EqualPower,
    Exponential,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FadeSpec {
    pub ms: u32,
    #[serde(default)]
    pub curve: Curve,
    #[serde(default = "default_depth")]
    pub depth_db: f32,
}

fn default_depth() -> f32 {
    -80.0
}

impl FadeSpec {
    pub fn ms(ms: u32) -> Self {
        FadeSpec {
            ms,
            curve: Curve::EqualPower,
            depth_db: default_depth(),
        }
    }
}

impl Default for FadeSpec {
    fn default() -> Self {
        FadeSpec::ms(0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trim {
    pub in_frames: u64,
    pub out_frames: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopScope {
    #[default]
    None,
    All,
    Page,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnPlay {
    #[serde(default)]
    pub stop: StopScope,
    #[serde(default = "yes")]
    pub stop_fade: bool,
}

fn yes() -> bool {
    true
}

impl Default for OnPlay {
    fn default() -> Self {
        OnPlay {
            stop: StopScope::None,
            stop_fade: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cart {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default)]
    pub asset: Option<Uuid>,
    #[serde(default = "main_output", skip_serializing_if = "is_main")]
    pub output: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub gain_db: f32,
    #[serde(default, skip_serializing_if = "is_default")]
    pub muted: bool,
    #[serde(default, skip_serializing_if = "is_default")]
    pub pan: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trim: Option<Trim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_in: Option<FadeSpec>,
    /// `None` = project default fade-out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_out: Option<FadeSpec>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub fade_out_at_end: bool,
    #[serde(rename = "loop", default, skip_serializing_if = "is_default")]
    pub looping: bool,
    /// `None` = project default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrigger: Option<Retrigger>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub on_play: OnPlay,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub play_next: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<Uuid>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub hold_to_play: bool,
    #[serde(default, skip_serializing_if = "is_default")]
    pub play_delay_ms: u32,
    /// Playback speed preset (1 = normal, 0.25–4).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub speed: f32,
    /// Duration of the speed bar's `x0` slow-down to a stop (ms).
    #[serde(
        default = "default_slowdown",
        skip_serializing_if = "is_default_slowdown"
    )]
    pub slowdown_ms: u32,
    /// Duration of the speed bar's `x1` return to normal speed (ms; 0 = instant).
    #[serde(
        default = "default_slowdown",
        skip_serializing_if = "is_default_slowdown"
    )]
    pub speedup_ms: u32,
    /// A control button instead of an audio cart (no audio; label, colour, group and
    /// triggers still apply).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<Control>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// What a control button does when triggered (click, touch, key or MIDI).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Control {
    /// Fade out every cart in the button's exclusive group.
    StopGroup,
    /// Fade out every cart on the button's page.
    StopPage,
    /// Fade out everything.
    StopAll,
}

impl Control {
    pub fn name(self) -> &'static str {
        match self {
            Control::StopGroup => "Stop Group",
            Control::StopPage => "Stop Page",
            Control::StopAll => "Stop All",
        }
    }
}

fn one() -> f32 {
    1.0
}

fn is_one(v: &f32) -> bool {
    *v == 1.0
}

pub const DEFAULT_SLOWDOWN_MS: u32 = 2000;

fn default_slowdown() -> u32 {
    DEFAULT_SLOWDOWN_MS
}

fn is_default_slowdown(v: &u32) -> bool {
    *v == DEFAULT_SLOWDOWN_MS
}

fn main_output() -> String {
    MAIN_OUTPUT.into()
}

fn is_main(s: &str) -> bool {
    s == MAIN_OUTPUT
}

impl Cart {
    pub fn new() -> Self {
        Cart {
            id: Uuid::new_v4(),
            label: String::new(),
            color: None,
            asset: None,
            output: main_output(),
            gain_db: 0.0,
            muted: false,
            pan: 0.0,
            trim: None,
            fade_in: None,
            fade_out: None,
            fade_out_at_end: false,
            looping: false,
            retrigger: None,
            on_play: OnPlay::default(),
            play_next: None,
            group: None,
            hold_to_play: false,
            play_delay_ms: 0,
            speed: 1.0,
            slowdown_ms: DEFAULT_SLOWDOWN_MS,
            speedup_ms: DEFAULT_SLOWDOWN_MS,
            control: None,
            notes: String::new(),
            extra: Extra::new(),
        }
    }
}

impl Default for Cart {
    fn default() -> Self {
        Cart::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub algo: String,
    pub size: u64,
    pub head_tail: String,
    #[serde(default)]
    pub full: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AssetMeta {
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Macos,
    Windows,
    Linux,
}

impl Platform {
    pub fn current() -> Platform {
        if cfg!(target_os = "windows") {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::Macos
        } else {
            Platform::Linux
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbsoluteHint {
    pub platform: Platform,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssetRef {
    pub id: Uuid,
    /// POSIX-style path, relative to the project directory when possible.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absolute_hint: Option<AbsoluteHint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<Fingerprint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<AssetMeta>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExclusiveGroup {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// MIDI channel 1–16 or any.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct MidiChannel(pub Option<u8>);

impl Serialize for MidiChannel {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Some(c) => s.serialize_u8(c),
            None => s.serialize_str("*"),
        }
    }
}

impl<'de> Deserialize<'de> for MidiChannel {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::String(s) if s == "*" => Ok(MidiChannel(None)),
            Value::Number(n) => n
                .as_u64()
                .filter(|c| (1..=16).contains(c))
                .map(|c| MidiChannel(Some(c as u8)))
                .ok_or_else(|| serde::de::Error::custom("MIDI channel must be 1–16 or \"*\"")),
            _ => Err(serde::de::Error::custom(
                "MIDI channel must be 1–16 or \"*\"",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MidiMessage {
    NoteOn,
    NoteOff,
    Cc,
    Program,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BindingSource {
    Key {
        /// Physical key code name (W3C `KeyboardEvent.code` style, e.g. "F1", "KeyA", "Numpad5").
        key: String,
        #[serde(default)]
        modifiers: Vec<String>,
    },
    Midi {
        /// Port name or "*" for any.
        #[serde(default = "any")]
        device: String,
        #[serde(default)]
        channel: MidiChannel,
        message: MidiMessage,
        number: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        threshold: Option<u8>,
    },
}

fn any() -> String {
    "*".into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Play,
    Stop,
    Toggle,
    Pause,
    StopPage,
    StopAll,
    Panic,
    NextPage,
    PrevPage,
    GotoPage,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub id: Uuid,
    pub source: BindingSource,
    pub action: Action,
    #[serde(default)]
    pub targets: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<DuplicatePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub velocity_to_gain: bool,
    #[serde(flatten)]
    pub extra: Extra,
}
