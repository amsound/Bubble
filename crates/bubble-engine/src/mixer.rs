//! The real-time mixer. `Mixer::process` is called from the audio callback (or from
//! tests/offline rendering). It must not allocate, free, lock, or block.
//!
//! Rules enforced here:
//! * Voices live in a fixed, preallocated pool.
//! * Audio (`Arc<Playable>`) and disk streams leaving the audio thread are pushed onto
//!   the garbage ring (or parked in a preallocated list) so their memory is never
//!   freed on this thread.
//! * A streamed voice that finds no data plays silence and holds its place; it never
//!   waits (see [`crate::stream`]).
//! * Large callbacks are split into blocks of at most [`MAX_BLOCK`] frames so commands
//!   and ramps stay fine-grained. Any callback size works, including non powers of two.

use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use crate::command::{
    CartKey, EndReason, EngineCommand, EngineEvent, Fade, FadeCurve, PlayParams, Retrigger,
    StopOthers, StopTarget, VoiceId,
};
use crate::engine::Garbage;
use crate::playable::{MAX_CHANNELS, Playable};
use crate::status::{EngineStatus, VoicePhase};
use crate::stream::{Fetch, VoiceStream};

/// Highest playback speed (4 × normal, as in the reference).
pub const MAX_SPEED: f32 = 4.0;
/// Largest number of frames processed between command checks.
pub const MAX_BLOCK: usize = 256;
/// Parameter smoothing time for gain/pan changes.
const PARAM_RAMP_MS: f32 = 10.0;
/// Minimum ramp used for "immediate" stops, restarts and steals to avoid clicks.
const KILL_RAMP_MS: f32 = 2.0;
/// Safety limiter ceiling (−0.3 dBFS).
const LIMITER_CEILING: f32 = 0.966_051;
const LIMITER_RELEASE_MS: f32 = 50.0;

#[derive(Clone, Copy, Debug, Default)]
struct Ramp {
    cur: f32,
    target: f32,
    step: f32,
    remaining: u32,
}

impl Ramp {
    fn new(v: f32) -> Self {
        Ramp {
            cur: v,
            target: v,
            step: 0.0,
            remaining: 0,
        }
    }

    fn set(&mut self, target: f32, frames: u32) {
        if frames == 0 {
            *self = Ramp::new(target);
        } else {
            self.target = target;
            self.remaining = frames;
            self.step = (target - self.cur) / frames as f32;
        }
    }

    #[inline]
    fn next(&mut self) -> f32 {
        if self.remaining > 0 {
            self.remaining -= 1;
            self.cur = if self.remaining == 0 {
                self.target
            } else {
                self.cur + self.step
            };
        }
        self.cur
    }
}

#[inline]
fn shape(curve: FadeCurve, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match curve {
        FadeCurve::Linear => t,
        FadeCurve::EqualPower => (t * FRAC_PI_2).sin(),
        FadeCurve::Exponential => {
            if t <= 0.0 {
                0.0
            } else {
                // −60 dB … 0 dB, linear in dB, with the floor pulled to exactly 0.
                let g = 10f32.powf(-3.0 * (1.0 - t));
                ((g - 0.001) / 0.999).max(0.0)
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum EnvState {
    Sustain,
    In {
        pos: u32,
        total: u32,
        curve: FadeCurve,
    },
    Out {
        pos: u32,
        total: u32,
        curve: FadeCurve,
        from: f32,
    },
}

#[derive(Clone, Copy, Debug)]
struct Envelope {
    state: EnvState,
    level: f32,
}

impl Envelope {
    fn start(fade_in: Fade) -> Self {
        if fade_in.frames == 0 {
            Envelope {
                state: EnvState::Sustain,
                level: 1.0,
            }
        } else {
            Envelope {
                state: EnvState::In {
                    pos: 0,
                    total: fade_in.frames,
                    curve: fade_in.curve,
                },
                level: 0.0,
            }
        }
    }

    fn stopping(&self) -> bool {
        matches!(self.state, EnvState::Out { .. })
    }

    fn remaining_out(&self) -> Option<u32> {
        match self.state {
            EnvState::Out { pos, total, .. } => Some(total - pos),
            _ => None,
        }
    }

    /// Begin fading out over `frames` (≥ 1). An existing fade-out is only shortened.
    fn release(&mut self, frames: u32, curve: FadeCurve) {
        let frames = frames.max(1);
        if let Some(rem) = self.remaining_out()
            && rem <= frames
        {
            return;
        }
        self.state = EnvState::Out {
            pos: 0,
            total: frames,
            curve,
            from: self.level,
        };
    }

    /// Advances one frame. Returns the gain and whether the voice has finished.
    #[inline]
    fn next(&mut self) -> (f32, bool) {
        match &mut self.state {
            EnvState::Sustain => (1.0, false),
            EnvState::In { pos, total, curve } => {
                *pos += 1;
                self.level = shape(*curve, *pos as f32 / *total as f32);
                if *pos >= *total {
                    self.state = EnvState::Sustain;
                    self.level = 1.0;
                }
                (self.level, false)
            }
            EnvState::Out {
                pos,
                total,
                curve,
                from,
            } => {
                *pos += 1;
                let t = *pos as f32 / *total as f32;
                self.level = *from * shape(*curve, 1.0 - t);
                (self.level, *pos >= *total)
            }
        }
    }
}

struct Voice {
    active: bool,
    cart: CartKey,
    id: VoiceId,
    audio: Option<Arc<Playable>>,
    /// Disk stream of a streamed source.
    stream: Option<Box<VoiceStream>>,
    /// A seek's new position waiting for the stream of the voice it replaces.
    heir_of: Option<VoiceId>,
    /// Out of stream data since the last frame (for counting underruns once).
    starved: bool,
    pos: usize,
    /// Fractional position between `pos` and the next frame (variable speed).
    frac: f64,
    /// Playback rate (1 = normal, 0 = stopped in place).
    rate: Ramp,
    /// Output peaks of this voice since the last status publish.
    peak_l: f32,
    peak_r: f32,
    start: usize,
    end: usize,
    looping: bool,
    gain: Ramp,
    pan_l: Ramp,
    pan_r: Ramp,
    env: Envelope,
    fade_out: Fade,
    group: u32,
    page: u32,
    started_seq: u64,
    pending_reason: EndReason,
    /// Device channels this voice writes to: `out_off..out_off + out_w`.
    out_off: usize,
    out_w: usize,
}

impl Voice {
    fn empty() -> Self {
        Voice {
            active: false,
            cart: CartKey(0),
            id: VoiceId(0),
            audio: None,
            stream: None,
            heir_of: None,
            starved: false,
            pos: 0,
            frac: 0.0,
            rate: Ramp::new(1.0),
            peak_l: 0.0,
            peak_r: 0.0,
            start: 0,
            end: 0,
            looping: false,
            gain: Ramp::new(0.0),
            pan_l: Ramp::new(0.0),
            pan_r: Ramp::new(0.0),
            env: Envelope::start(Fade::NONE),
            fade_out: Fade::NONE,
            group: 0,
            page: 0,
            started_seq: 0,
            pending_reason: EndReason::Stopped,
            out_off: 0,
            out_w: 2,
        }
    }

    fn stopping(&self) -> bool {
        self.env.stopping()
    }

    /// A copy for a seek: everything but the stream, which follows later.
    fn seek_copy(&self) -> Voice {
        Voice {
            active: self.active,
            cart: self.cart,
            id: self.id,
            audio: self.audio.clone(),
            stream: None,
            heir_of: (self.stream.is_some() || self.heir_of.is_some()).then_some(self.id),
            starved: false,
            pos: self.pos,
            frac: self.frac,
            rate: self.rate,
            peak_l: self.peak_l,
            peak_r: self.peak_r,
            start: self.start,
            end: self.end,
            looping: self.looping,
            gain: self.gain,
            pan_l: self.pan_l,
            pan_r: self.pan_r,
            env: self.env,
            fade_out: self.fade_out,
            group: self.group,
            page: self.page,
            started_seq: self.started_seq,
            pending_reason: self.pending_reason,
            out_off: self.out_off,
            out_w: self.out_w,
        }
    }

    /// Asks the disk for data to follow a jump to `self.pos` (loop, seek, trim).
    fn follow_jump(&mut self) {
        if let (Some(s), Some(a)) = (self.stream.as_mut(), self.audio.as_ref()) {
            let from = a.stream_from(self.pos);
            s.request(from, from > self.pos);
        }
    }
}

/// Reads frame `pos` into `out`: from memory, else from the voice's stream.
#[inline]
fn fetch(
    audio: &Playable,
    stream: &mut Option<Box<VoiceStream>>,
    heir: bool,
    pos: usize,
    out: &mut [f32; MAX_CHANNELS],
    may_request: bool,
) -> Fetch {
    if audio.memory_frame(pos, out) {
        return Fetch::Ok;
    }
    match stream {
        Some(s) => s.fetch(pos, out, may_request),
        // A seek's copy before the stream reaches it.
        None if heir => Fetch::Wait { starved: false },
        None => Fetch::End,
    }
}

/// Pan gains for a source with `channels` channels.
fn pan_gains(pan: f32, channels: u16) -> (f32, f32) {
    let pan = pan.clamp(-1.0, 1.0);
    if channels == 1 {
        // Constant-power (−3 dB at centre).
        let a = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
        (a.cos(), a.sin())
    } else {
        // Balance for stereo and above: unity at centre.
        ((1.0 - pan).min(1.0), (1.0 + pan).min(1.0))
    }
}

/// Reads one source frame (`n` channels) as a stereo pair.
#[inline]
fn source_lr(frame: &[f32; MAX_CHANNELS], n: usize) -> (f32, f32) {
    const M3DB: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let s = |c: usize| frame[c];
    match n {
        1 => (s(0), s(0)),
        2 => (s(0), s(1)),
        // Quad (L R Ls Rs): fold surrounds at −3 dB.
        4 => (s(0) + s(2) * M3DB, s(1) + s(3) * M3DB),
        // 3 ch (L R C) and 5.1/7.1 in WAV order (L R C LFE Ls Rs …): centre and the first
        // surround pair at −3 dB, LFE and further channels dropped.
        n => {
            let c = s(2) * M3DB;
            let (ls, rs) = if n >= 6 {
                (s(4) * M3DB, s(5) * M3DB)
            } else {
                (0.0, 0.0)
            };
            (s(0) + c + ls, s(1) + c + rs)
        }
    }
}

/// The real-time mixer. Construct via [`crate::Engine::new`].
pub struct Mixer {
    sample_rate: u32,
    channels: usize,
    voices: Box<[Voice]>,
    commands: rtrb::Consumer<EngineCommand>,
    events: rtrb::Producer<EngineEvent>,
    garbage: rtrb::Producer<Garbage>,
    parked: Box<[Option<Garbage>]>,
    status: Arc<EngineStatus>,
    next_voice_id: u64,
    seq: u64,
    master: Ramp,
    limiter_enabled: bool,
    limiter_gain: f32,
    limiter_release: f32,
    param_ramp: u32,
    kill_ramp: u32,
    /// Start ramp (frames) for voices that begin on a non-silent sample.
    declick: u32,
}

impl Mixer {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        sample_rate: u32,
        channels: u16,
        voices: usize,
        commands: rtrb::Consumer<EngineCommand>,
        events: rtrb::Producer<EngineEvent>,
        garbage: rtrb::Producer<Garbage>,
        status: Arc<EngineStatus>,
        declick_ms: f32,
    ) -> Self {
        let sr = sample_rate as f32;
        Mixer {
            sample_rate,
            channels: channels as usize,
            voices: (0..voices).map(|_| Voice::empty()).collect(),
            commands,
            events,
            garbage,
            parked: (0..voices * 4).map(|_| None).collect(),
            status,
            next_voice_id: 1,
            seq: 0,
            master: Ramp::new(1.0),
            limiter_enabled: true,
            limiter_gain: 1.0,
            limiter_release: 1.0 - (-1.0 / (LIMITER_RELEASE_MS * 0.001 * sr)).exp(),
            param_ramp: ((PARAM_RAMP_MS * sr / 1000.0) as u32).max(1),
            kill_ramp: ((KILL_RAMP_MS * sr / 1000.0) as u32).max(1),
            declick: (declick_ms.max(0.0) * sr / 1000.0) as u32,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Renders interleaved audio for `out.len() / channels` frames, overwriting `out`.
    pub fn process(&mut self, out: &mut [f32]) {
        let ch = self.channels;
        let frames = out.len() / ch;
        out.fill(0.0);
        self.retry_parked();

        let mut offset = 0;
        while offset < frames {
            let n = (frames - offset).min(MAX_BLOCK);
            self.drain_commands();
            self.render_block(&mut out[offset * ch..(offset + n) * ch], n);
            offset += n;
        }

        let st = &self.status;
        st.callbacks.fetch_add(1, Relaxed);
        st.frames_rendered.fetch_add(frames as u64, Relaxed);
        st.last_callback_frames.store(frames as u32, Relaxed);
        st.max_callback_frames.fetch_max(frames as u32, Relaxed);
        st.min_callback_frames.fetch_min(frames as u32, Relaxed);
        self.publish_status();
    }

    fn drain_commands(&mut self) {
        while let Ok(cmd) = self.commands.pop() {
            self.apply(cmd);
        }
    }

    fn apply(&mut self, cmd: EngineCommand) {
        match cmd {
            EngineCommand::Play {
                cart,
                audio,
                stream,
                params,
            } => self.play(cart, audio, stream, params),
            EngineCommand::Stop { target, fade } => {
                let kill = self.kill_ramp;
                for v in self.voices.iter_mut().filter(|v| v.active) {
                    if matches_target(v, target) {
                        let f = fade.unwrap_or(v.fade_out);
                        v.pending_reason = EndReason::Stopped;
                        v.env.release(f.frames.max(kill), f.curve);
                    }
                }
            }
            EngineCommand::Kill { target } => {
                let kill = self.kill_ramp;
                for v in self.voices.iter_mut().filter(|v| v.active) {
                    if matches_target(v, target) {
                        v.pending_reason = EndReason::Stopped;
                        v.env.release(kill, FadeCurve::Linear);
                    }
                }
            }
            EngineCommand::SetGain { cart, gain } => {
                let r = self.param_ramp;
                for v in self
                    .voices
                    .iter_mut()
                    .filter(|v| v.active && v.cart == cart)
                {
                    v.gain.set(gain.max(0.0), r);
                }
            }
            EngineCommand::SetPan { cart, pan } => {
                let r = self.param_ramp;
                for v in self
                    .voices
                    .iter_mut()
                    .filter(|v| v.active && v.cart == cart)
                {
                    let ch = v.audio.as_ref().map_or(2, |b| b.channels());
                    let (l, rr) = pan_gains(pan, ch);
                    v.pan_l.set(l, r);
                    v.pan_r.set(rr, r);
                }
            }
            EngineCommand::SetSpeed {
                cart,
                speed,
                ramp_frames,
            } => {
                for v in self
                    .voices
                    .iter_mut()
                    .filter(|v| v.active && v.cart == cart)
                {
                    v.rate.set(speed.clamp(0.0, MAX_SPEED), ramp_frames);
                }
            }
            EngineCommand::SetLoop { cart, looping } => {
                for v in self
                    .voices
                    .iter_mut()
                    .filter(|v| v.active && v.cart == cart)
                {
                    v.looping = looping;
                }
            }
            EngineCommand::Seek { cart, frame } => self.seek(cart, frame),
            EngineCommand::SetRegion { cart, start, end } => {
                for v in self
                    .voices
                    .iter_mut()
                    .filter(|v| v.active && v.cart == cart)
                {
                    let frames = v.audio.as_ref().map_or(0, |b| b.frames());
                    let end = end.min(frames);
                    if start < end {
                        v.start = start;
                        v.end = end;
                        // Before the new In, or (looping) past the new Out: go to In.
                        if v.pos < start || (v.looping && v.pos >= end) {
                            v.pos = start;
                            v.frac = 0.0;
                            v.follow_jump();
                        }
                    }
                }
            }
            EngineCommand::SetMaster { gain } => self.master.set(gain.max(0.0), self.param_ramp),
            EngineCommand::SetLimiter { enabled } => {
                self.limiter_enabled = enabled;
                self.limiter_gain = 1.0;
            }
        }
    }

    fn play(
        &mut self,
        cart: CartKey,
        audio: Arc<Playable>,
        stream: Option<Box<VoiceStream>>,
        p: PlayParams,
    ) {
        let stream = match stream {
            Some(s) if !audio.is_streamed() => {
                self.discard(Garbage::Stream(s));
                None
            }
            other => other,
        };
        let frames = audio.frames();
        let end = p.end_frame.unwrap_or(frames).min(frames);
        let start = if p.start_frame < end {
            p.start_frame
        } else {
            0
        };
        if frames == 0 || start >= end {
            self.emit(EngineEvent::PlayIgnored { cart });
            self.discard_play(audio, stream);
            return;
        }

        let playing = self
            .voices
            .iter()
            .any(|v| v.active && v.cart == cart && !v.stopping());
        match p.retrigger {
            Retrigger::Ignore if playing => {
                self.emit(EngineEvent::PlayIgnored { cart });
                self.discard_play(audio, stream);
                return;
            }
            Retrigger::ToggleStop if playing => {
                for v in self
                    .voices
                    .iter_mut()
                    .filter(|v| v.active && v.cart == cart)
                {
                    v.pending_reason = EndReason::Stopped;
                    v.env
                        .release(v.fade_out.frames.max(self.kill_ramp), v.fade_out.curve);
                }
                self.discard_play(audio, stream);
                return;
            }
            Retrigger::Restart => {
                let kill = self.kill_ramp;
                for v in self
                    .voices
                    .iter_mut()
                    .filter(|v| v.active && v.cart == cart)
                {
                    v.pending_reason = EndReason::Restarted;
                    v.env.release(kill, FadeCurve::Linear);
                }
            }
            _ => {}
        }

        // Exclusive group and stop-others act on *other* carts.
        let kill = self.kill_ramp;
        for v in self
            .voices
            .iter_mut()
            .filter(|v| v.active && v.cart != cart)
        {
            let in_group = p.group != 0 && v.group == p.group;
            let by_scope = match p.stop_others {
                StopOthers::None => false,
                StopOthers::All => true,
                StopOthers::Page => v.page == p.page,
            };
            if in_group || by_scope {
                let use_fade = in_group || p.stop_others_fade;
                let f = if use_fade {
                    v.fade_out.frames.max(kill)
                } else {
                    kill
                };
                v.pending_reason = EndReason::Stopped;
                v.env.release(f, v.fade_out.curve);
            }
        }

        let slot = match self.voices.iter().position(|v| !v.active) {
            Some(s) => s,
            None => self.steal(),
        };
        let id = VoiceId(self.next_voice_id);
        self.next_voice_id += 1;
        self.seq += 1;
        let (pl, pr) = pan_gains(p.pan, audio.channels());
        // De-click: a start on a non-silent sample (e.g. an In point mid-waveform)
        // gets a 1.5 ms ramp unless the cart already fades in. A start the disk has
        // not reached yet counts as non-silent.
        let mut first = [0.0; MAX_CHANNELS];
        let n = (audio.channels() as usize).min(MAX_CHANNELS);
        let loud =
            !audio.memory_frame(start, &mut first) || first[..n].iter().any(|x| x.abs() > 1e-3);
        let v = &mut self.voices[slot];
        v.active = true;
        v.cart = cart;
        v.id = id;
        v.audio = Some(audio);
        v.stream = stream;
        v.heir_of = None;
        v.starved = false;
        v.pos = start;
        v.frac = 0.0;
        v.rate = Ramp::new(p.speed.clamp(0.0, MAX_SPEED));
        v.peak_l = 0.0;
        v.peak_r = 0.0;
        v.start = start;
        v.end = end;
        v.looping = p.looping;
        v.gain = Ramp::new(p.gain.max(0.0));
        v.pan_l = Ramp::new(pl);
        v.pan_r = Ramp::new(pr);
        v.follow_jump();
        let declick = self.declick;
        let fade_in = if declick > 0 && loud && p.fade_in.frames < declick {
            Fade {
                frames: declick,
                curve: FadeCurve::Linear,
            }
        } else {
            p.fade_in
        };
        v.env = Envelope::start(fade_in);
        v.fade_out = p.fade_out;
        v.group = p.group;
        v.page = p.page;
        v.started_seq = self.seq;
        v.pending_reason = EndReason::Stopped;
        // A route outside this device falls back to its first channels.
        let mut w = (p.output_channels.max(1) as usize).min(self.channels);
        let mut off = p.output_offset as usize;
        if off + w > self.channels {
            off = 0;
            w = w.min(self.channels);
        }
        v.out_off = off;
        v.out_w = w;
        self.emit(EngineEvent::VoiceStarted { cart, voice: id });
    }

    /// See [`EngineCommand::Seek`].
    fn seek(&mut self, cart: CartKey, frame: usize) {
        let Some(old) = self
            .voices
            .iter()
            .enumerate()
            .filter(|(_, v)| v.active && v.cart == cart && !v.stopping())
            .max_by_key(|(_, v)| v.started_seq)
            .map(|(i, _)| i)
        else {
            return;
        };
        let ramp = self.declick.max(self.kill_ramp);
        let kill = self.kill_ramp;
        let target = {
            let v = &self.voices[old];
            frame.clamp(v.start, v.end.saturating_sub(1).max(v.start))
        };
        // A slot for the copy; never the old voice, which still owns the stream.
        let slot = match self.voices.iter().position(|v| !v.active) {
            Some(s) => s,
            None => match self.steal_except(Some(old)) {
                Some(s) => s,
                None => {
                    // A pool of one voice: jump in place.
                    let v = &mut self.voices[old];
                    v.pos = target;
                    v.frac = 0.0;
                    v.env = Envelope::start(Fade {
                        frames: ramp,
                        curve: FadeCurve::Linear,
                    });
                    v.follow_jump();
                    return;
                }
            },
        };
        let mut copy = self.voices[old].seek_copy();
        let v = &mut self.voices[old];
        v.pending_reason = EndReason::Restarted;
        v.env.release(kill, FadeCurve::Linear);
        let id = VoiceId(self.next_voice_id);
        self.next_voice_id += 1;
        self.seq += 1;
        copy.id = id;
        copy.pos = target;
        copy.frac = 0.0;
        copy.peak_l = 0.0;
        copy.peak_r = 0.0;
        copy.started_seq = self.seq;
        copy.pending_reason = EndReason::Stopped;
        copy.env = Envelope::start(Fade {
            frames: ramp,
            curve: FadeCurve::Linear,
        });
        // The slot is free (or was just emptied by a steal), so nothing is dropped.
        self.voices[slot] = copy;
        self.emit(EngineEvent::VoiceStarted { cart, voice: id });
    }

    /// Frees a slot by ending the least valuable voice: one already stopping (quietest
    /// first), otherwise the oldest.
    fn steal(&mut self) -> usize {
        self.steal_except(None).expect("a voice to steal")
    }

    /// Like [`Self::steal`], sparing voice `keep`; `None` if it is the only one.
    fn steal_except(&mut self, keep: Option<usize>) -> Option<usize> {
        let mut best = None;
        let mut best_key = (u8::MAX, f32::MAX, u64::MAX);
        for (i, v) in self.voices.iter().enumerate() {
            if Some(i) == keep {
                continue;
            }
            let key = if v.stopping() {
                (0, v.env.level, v.started_seq)
            } else if !v.looping {
                (1, 0.0, v.started_seq)
            } else {
                (2, 0.0, v.started_seq)
            };
            if key.0 < best_key.0
                || (key.0 == best_key.0
                    && (key.1 < best_key.1 || (key.1 == best_key.1 && key.2 < best_key.2)))
            {
                best = Some(i);
                best_key = key;
            }
        }
        let best = best?;
        self.status.voices_stolen.fetch_add(1, Relaxed);
        self.finish(best, EndReason::Stolen);
        Some(best)
    }

    fn render_block(&mut self, out: &mut [f32], frames: usize) {
        let ch = self.channels;
        let (mut underruns, mut underrun_frames, mut wait_frames) = (0u64, 0u64, 0u64);
        for idx in 0..self.voices.len() {
            if !self.voices[idx].active {
                continue;
            }
            let mut finished: Option<EndReason> = None;
            {
                let v = &mut self.voices[idx];
                let Some(audio) = v.audio.take() else {
                    // Invariant: active voices always hold audio.
                    v.active = false;
                    continue;
                };
                let n = (audio.channels() as usize).min(MAX_CHANNELS);
                // 5.1 output with a 5.1 (or wider) source: channel for channel.
                let direct = v.out_w >= 6 && n >= 6;
                let mut a = [0.0f32; MAX_CHANNELS];
                let mut b = [0.0f32; MAX_CHANNELS];
                for i in 0..frames {
                    let heir = v.heir_of.is_some();
                    match fetch(&audio, &mut v.stream, heir, v.pos, &mut a, true) {
                        Fetch::Ok => v.starved = false,
                        Fetch::Wait { starved } => {
                            // Silence, holding the place (and the fade) until data comes;
                            // a voice that is stopping still fades out and ends.
                            if starved {
                                underrun_frames += 1;
                                if !v.starved {
                                    v.starved = true;
                                    underruns += 1;
                                }
                            } else {
                                wait_frames += 1;
                            }
                            if v.stopping() && v.env.next().1 {
                                finished = Some(v.pending_reason);
                                break;
                            }
                            continue;
                        }
                        Fetch::End => {
                            // The file ends early (shorter than stated, or unreadable):
                            // as at the Out point.
                            if v.looping && !v.stopping() && v.pos > v.start {
                                v.pos = v.start;
                                v.frac = 0.0;
                                if let Some(s) = v.stream.as_mut() {
                                    let from = audio.stream_from(v.pos);
                                    s.request(from, from > v.pos);
                                }
                                continue;
                            }
                            finished = Some(if v.stopping() {
                                v.pending_reason
                            } else {
                                EndReason::Completed
                            });
                            break;
                        }
                    }
                    // Linear interpolation towards the next frame (speed ≠ 1).
                    let f = v.frac as f32;
                    if f > 0.0 {
                        let next = if v.pos + 1 < v.end {
                            v.pos + 1
                        } else if v.looping {
                            v.start
                        } else {
                            v.pos
                        };
                        if next == v.pos
                            || fetch(&audio, &mut v.stream, heir, next, &mut b, false) != Fetch::Ok
                        {
                            b = a;
                        }
                    }
                    let (env, done) = v.env.next();
                    let g = v.gain.next() * env;
                    if direct {
                        // Keep the pan ramps moving; balance does not apply to 5.1.
                        v.pan_l.next();
                        v.pan_r.next();
                        let o = &mut out[i * ch + v.out_off..i * ch + v.out_off + 6];
                        for (c, s) in o.iter_mut().enumerate() {
                            let x = if f > 0.0 {
                                a[c] + (b[c] - a[c]) * f
                            } else {
                                a[c]
                            };
                            *s += x * g;
                        }
                        v.peak_l = v.peak_l.max((a[0] * g).abs());
                        v.peak_r = v.peak_r.max((a[1] * g).abs());
                    } else {
                        let (mut l, mut r) = source_lr(&a, n);
                        if f > 0.0 {
                            let (l2, r2) = source_lr(&b, n);
                            l += (l2 - l) * f;
                            r += (r2 - r) * f;
                        }
                        let (ol, or) = (l * g * v.pan_l.next(), r * g * v.pan_r.next());
                        let o = &mut out[i * ch + v.out_off..i * ch + v.out_off + v.out_w];
                        if v.out_w == 1 {
                            o[0] += 0.5 * (ol + or);
                        } else {
                            o[0] += ol;
                            o[1] += or;
                        }
                        v.peak_l = v.peak_l.max(ol.abs());
                        v.peak_r = v.peak_r.max(or.abs());
                    }
                    if done {
                        finished = Some(v.pending_reason);
                        break;
                    }
                    v.frac += v.rate.next() as f64;
                    let step = v.frac as usize; // floor, frac ≥ 0
                    v.frac -= step as f64;
                    v.pos += step;
                    if v.pos >= v.end {
                        // A loop that is fading out plays to its Out point and ends.
                        if v.looping && !v.env.stopping() {
                            let len = (v.end - v.start).max(1);
                            v.pos = v.start + (v.pos - v.end) % len;
                            if let Some(s) = v.stream.as_mut() {
                                let from = audio.stream_from(v.pos);
                                s.request(from, from > v.pos);
                            }
                        } else {
                            finished = Some(if v.env.stopping() {
                                v.pending_reason
                            } else {
                                EndReason::Completed
                            });
                            break;
                        }
                    }
                }
                v.audio = Some(audio);
            }
            if let Some(reason) = finished {
                self.finish(idx, reason);
            }
        }

        // Master gain and safety limiter (no look-ahead: instant attack, smooth release).
        let mut peak_l = 0.0f32;
        let mut peak_r = 0.0f32;
        let mut limited = false;
        for i in 0..frames {
            let m = self.master.next();
            let o = &mut out[i * ch..i * ch + ch];
            for s in o.iter_mut() {
                *s *= m;
            }
            if self.limiter_enabled {
                let p = o.iter().fold(0.0f32, |a, s| a.max(s.abs()));
                let needed = if p > LIMITER_CEILING {
                    LIMITER_CEILING / p
                } else {
                    1.0
                };
                self.limiter_gain = if needed < self.limiter_gain {
                    needed
                } else {
                    // Release towards unity, but never above what this frame allows.
                    needed.min(self.limiter_gain + (1.0 - self.limiter_gain) * self.limiter_release)
                };
                if self.limiter_gain < 0.9999 {
                    limited = true;
                    for s in o.iter_mut() {
                        *s *= self.limiter_gain;
                    }
                }
            }
            peak_l = peak_l.max(o[0].abs());
            peak_r = peak_r.max(o[if ch > 1 { 1 } else { 0 }].abs());
        }
        let st = &self.status;
        if underrun_frames > 0 {
            st.stream_underruns.fetch_add(underruns, Relaxed);
            st.stream_underrun_frames
                .fetch_add(underrun_frames, Relaxed);
        }
        if wait_frames > 0 {
            st.stream_wait_frames.fetch_add(wait_frames, Relaxed);
        }
        st.peak_l.fetch_max(peak_l.to_bits(), Relaxed);
        st.peak_r.fetch_max(peak_r.to_bits(), Relaxed);
        if limited {
            st.limiter_active_blocks.fetch_add(1, Relaxed);
        }
    }

    fn finish(&mut self, idx: usize, reason: EndReason) {
        let v = &mut self.voices[idx];
        if !v.active {
            return;
        }
        v.active = false;
        let inherit = v.heir_of.take();
        let (cart, id) = (v.cart, v.id);
        let stream = v.stream.take();
        if let Some(audio) = v.audio.take() {
            self.discard(Garbage::Audio(audio));
        }
        if let Some(s) = stream {
            // A seek's copy takes the stream over; otherwise it is released.
            match self
                .voices
                .iter_mut()
                .find(|w| w.active && w.heir_of == Some(id))
            {
                Some(w) => {
                    w.stream = Some(s);
                    w.heir_of = None;
                    w.follow_jump();
                }
                None => self.discard(Garbage::Stream(s)),
            }
        } else if inherit.is_some() {
            // Ended before its own stream arrived (quick seeks): whoever waits for it
            // now waits for the stream's holder instead.
            for w in self.voices.iter_mut().filter(|w| w.heir_of == Some(id)) {
                w.heir_of = inherit;
            }
        }
        self.emit(EngineEvent::VoiceEnded {
            cart,
            voice: id,
            reason,
        });
    }

    /// Releases a Play that did not start.
    fn discard_play(&mut self, audio: Arc<Playable>, stream: Option<Box<VoiceStream>>) {
        self.discard(Garbage::Audio(audio));
        if let Some(s) = stream {
            self.discard(Garbage::Stream(s));
        }
    }

    /// Moves audio or a stream off the audio thread without freeing it here.
    fn discard(&mut self, buf: Garbage) {
        if let Err(rtrb::PushError::Full(buf)) = self.garbage.push(buf) {
            if let Some(slot) = self.parked.iter_mut().find(|s| s.is_none()) {
                *slot = Some(buf);
            } else {
                // Last resort: dropping here may free memory on the audio thread.
                self.status.garbage_overflow.fetch_add(1, Relaxed);
                drop(buf);
            }
        }
    }

    fn retry_parked(&mut self) {
        for slot in self.parked.iter_mut() {
            if let Some(buf) = slot.take()
                && let Err(rtrb::PushError::Full(buf)) = self.garbage.push(buf)
            {
                *slot = Some(buf);
                return;
            }
        }
    }

    fn emit(&mut self, ev: EngineEvent) {
        if self.events.push(ev).is_err() {
            self.status.events_dropped.fetch_add(1, Relaxed);
        }
    }

    fn publish_status(&mut self) {
        let mut active = 0;
        for (i, v) in self.voices.iter_mut().enumerate() {
            if v.active {
                active += 1;
                let phase = match v.env.state {
                    EnvState::Out { .. } => VoicePhase::Stopping,
                    EnvState::In { .. } => VoicePhase::FadingIn,
                    EnvState::Sustain => VoicePhase::Playing,
                };
                self.status.publish_voice(
                    i,
                    v.cart,
                    phase,
                    (v.pos - v.start) as u64,
                    (v.end - v.start) as u64,
                    v.looping,
                );
                self.status
                    .publish_voice_meter(i, v.peak_l, v.peak_r, v.rate.cur);
                v.peak_l = 0.0;
                v.peak_r = 0.0;
            } else {
                self.status.clear_voice(i);
            }
        }
        self.status.active_voices.store(active, Relaxed);
    }
}

fn matches_target(v: &Voice, target: StopTarget) -> bool {
    match target {
        StopTarget::Cart(c) => v.cart == c,
        StopTarget::Page(p) => v.page == p,
        StopTarget::Group(g) => g != 0 && v.group == g,
        StopTarget::All => true,
    }
}
