//! `bubble-cli` — exercise the Bubble engine without the UI.
//!
//! ```text
//! bubble-cli devices
//! bubble-cli play sting.wav tone.wav --seconds 3
//! bubble-cli console *.wav          # interactive: p 1, s 1, l 1, g 1 -6, x, q
//! bubble-cli render a.wav b.wav --out mix.wav  # offline, no device
//! ```

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bubble_engine::device::{AudioOutput, OutputRequest, list_output_devices};
use bubble_engine::loader::load_for_engine;
use bubble_engine::{
    CartKey, Engine, EngineCommand, EngineConfig, EngineEvent, EngineHandle, Fade, FadeCurve,
    PlayParams, Playable, Retrigger, StopTarget, db_to_gain, gain_to_db,
};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about = "Bubble engine test harness")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args, Clone)]
struct DeviceOpts {
    /// Output device name (see `devices`); default device if omitted.
    #[arg(long)]
    device: Option<String>,
    /// Requested sample rate.
    #[arg(long)]
    rate: Option<u32>,
    /// Fixed callback size in frames (any value the backend accepts, e.g. 480).
    #[arg(long)]
    buffer: Option<u32>,
}

#[derive(Subcommand)]
enum Cmd {
    /// List output devices.
    Devices,
    /// Play files simultaneously on the output device.
    Play {
        files: Vec<PathBuf>,
        #[command(flatten)]
        dev: DeviceOpts,
        /// Gain in dB applied to every file.
        #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
        gain_db: f32,
        /// Loop each file.
        #[arg(long)]
        r#loop: bool,
        /// Stop (with a 500 ms fade) after this many seconds.
        #[arg(long, default_value_t = 5.0)]
        seconds: f32,
        /// Delay between starting successive files (ms).
        #[arg(long, default_value_t = 0)]
        stagger_ms: u64,
    },
    /// Interactive trigger console.
    Console {
        files: Vec<PathBuf>,
        #[command(flatten)]
        dev: DeviceOpts,
    },
    /// Render files through the mixer offline into a WAV file.
    Render {
        files: Vec<PathBuf>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 48_000)]
        rate: u32,
        #[arg(long, default_value_t = 0.0, allow_hyphen_values = true)]
        gain_db: f32,
        /// Callback size used for rendering (frames).
        #[arg(long, default_value_t = 480)]
        block: usize,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Devices => devices(),
        Cmd::Play {
            files,
            dev,
            gain_db,
            r#loop,
            seconds,
            stagger_ms,
        } => play(&files, &dev, gain_db, r#loop, seconds, stagger_ms),
        Cmd::Console { files, dev } => console(&files, &dev),
        Cmd::Render {
            files,
            out,
            rate,
            gain_db,
            block,
        } => render(&files, &out, rate, gain_db, block),
    }
}

fn devices() -> Result<()> {
    for d in list_output_devices()? {
        println!(
            "{} {}  [{} Hz, {} ch]  id={}",
            if d.is_default { "*" } else { " " },
            d.name,
            d.default_sample_rate.map_or("?".into(), |r| r.to_string()),
            d.default_channels.map_or("?".into(), |c| c.to_string()),
            d.id
        );
    }
    Ok(())
}

fn open(dev: &DeviceOpts) -> Result<(AudioOutput, EngineHandle)> {
    let (out, handle) = AudioOutput::open(&OutputRequest {
        device_name: dev.device.clone(),
        sample_rate: dev.rate,
        buffer_frames: dev.buffer,
        ..Default::default()
    })?;
    println!(
        "Output: {} — {} Hz, {} ch, {} samples, buffer {:?}",
        out.device_name, out.sample_rate, out.channels, out.sample_format, out.buffer
    );
    Ok((out, handle))
}

/// Loads files whole into memory (the CLI does not stream).
fn load_all(files: &[PathBuf], rate: u32) -> Result<Vec<Arc<Playable>>> {
    if files.is_empty() {
        bail!("no files given");
    }
    files
        .iter()
        .map(|f| {
            let t = Instant::now();
            let b = load_for_engine(f, rate).with_context(|| f.display().to_string())?;
            println!(
                "Loaded {} — {:.2} s, {} ch, peak {:.1} dBFS ({} ms)",
                f.display(),
                b.duration_secs(),
                b.channels(),
                gain_to_db(b.peak()),
                t.elapsed().as_millis()
            );
            Ok(Playable::resident(
                Arc::try_unwrap(b).expect("freshly loaded buffer"),
            ))
        })
        .collect()
}

fn print_events(h: &mut EngineHandle, t0: Instant) {
    h.poll_events(|e| match e {
        EngineEvent::VoiceStarted { cart, voice } => {
            println!(
                "{:>8.3}s  started  cart {} voice {}",
                t0.elapsed().as_secs_f32(),
                cart.0,
                voice.0
            )
        }
        EngineEvent::VoiceEnded {
            cart,
            voice,
            reason,
        } => println!(
            "{:>8.3}s  ended    cart {} voice {} ({reason:?})",
            t0.elapsed().as_secs_f32(),
            cart.0,
            voice.0
        ),
        EngineEvent::PlayIgnored { cart } => println!("          ignored  cart {}", cart.0),
    });
}

fn play(
    files: &[PathBuf],
    dev: &DeviceOpts,
    gain_db: f32,
    looping: bool,
    seconds: f32,
    stagger_ms: u64,
) -> Result<()> {
    let (out, mut h) = open(dev)?;
    let bufs = load_all(files, out.sample_rate)?;
    let t0 = Instant::now();
    for (i, b) in bufs.iter().enumerate() {
        h.send(EngineCommand::Play {
            cart: CartKey(i as u32 + 1),
            audio: b.clone(),
            stream: None,
            params: PlayParams {
                gain: db_to_gain(gain_db),
                looping,
                fade_out: Fade::from_ms(500.0, out.sample_rate, FadeCurve::EqualPower),
                retrigger: Retrigger::Overlap,
                ..Default::default()
            },
        })?;
        if stagger_ms > 0 {
            std::thread::sleep(Duration::from_millis(stagger_ms));
        }
    }
    while t0.elapsed().as_secs_f32() < seconds {
        std::thread::sleep(Duration::from_millis(20));
        print_events(&mut h, t0);
        for p in out.take_problems() {
            println!("stream: {p:?}");
        }
        if h.status().active_voices() == 0 && t0.elapsed().as_millis() > 200 {
            break;
        }
    }
    h.send(EngineCommand::Stop {
        target: StopTarget::All,
        fade: None,
    })?;
    std::thread::sleep(Duration::from_millis(600));
    print_events(&mut h, t0);
    report(&h);
    Ok(())
}

fn report(h: &EngineHandle) {
    let d = h.status().diagnostics();
    println!(
        "Diagnostics: {} callbacks, callback frames min/last/max {}/{}/{}, max process {:.3} ms, \
         device output latency {:.2} ms, xruns {}, events dropped {}, voices stolen {}",
        d.callbacks,
        d.min_callback_frames,
        d.last_callback_frames,
        d.max_callback_frames,
        d.max_process_ns as f64 / 1e6,
        d.output_latency_us as f64 / 1e3,
        d.xruns,
        d.events_dropped,
        d.voices_stolen
    );
}

fn console(files: &[PathBuf], dev: &DeviceOpts) -> Result<()> {
    let (out, mut h) = open(dev)?;
    let bufs = load_all(files, out.sample_rate)?;
    let mut gains = vec![1.0f32; bufs.len()];
    let mut loops = vec![false; bufs.len()];
    println!(
        "Commands: p N (play) | o N (overlap) | s N (stop) | l N (toggle loop) | g N dB | x (stop all) | d (diagnostics) | q"
    );
    let t0 = Instant::now();
    let stdin = std::io::stdin();
    print!("> ");
    std::io::stdout().flush()?;
    for line in stdin.lock().lines() {
        let line = line?;
        let parts: Vec<&str> = line.split_whitespace().collect();
        let idx = |i: usize| -> Option<usize> {
            parts
                .get(i)?
                .parse::<usize>()
                .ok()
                .filter(|n| *n >= 1 && *n <= bufs.len())
                .map(|n| n - 1)
        };
        match parts.first().copied() {
            Some("q") => break,
            Some("x") => h.send(EngineCommand::Stop {
                target: StopTarget::All,
                fade: None,
            })?,
            Some("d") => report(&h),
            Some(c @ ("p" | "o")) => {
                if let Some(n) = idx(1) {
                    h.send(EngineCommand::Play {
                        cart: CartKey(n as u32 + 1),
                        audio: bufs[n].clone(),
                        stream: None,
                        params: PlayParams {
                            gain: gains[n],
                            looping: loops[n],
                            retrigger: if c == "o" {
                                Retrigger::Overlap
                            } else {
                                Retrigger::Restart
                            },
                            fade_out: Fade::from_ms(300.0, out.sample_rate, FadeCurve::EqualPower),
                            ..Default::default()
                        },
                    })?;
                }
            }
            Some("s") => {
                if let Some(n) = idx(1) {
                    h.send(EngineCommand::Stop {
                        target: StopTarget::Cart(CartKey(n as u32 + 1)),
                        fade: None,
                    })?;
                }
            }
            Some("l") => {
                if let Some(n) = idx(1) {
                    loops[n] = !loops[n];
                    h.send(EngineCommand::SetLoop {
                        cart: CartKey(n as u32 + 1),
                        looping: loops[n],
                    })?;
                    println!("cart {} loop {}", n + 1, loops[n]);
                }
            }
            Some("g") => {
                if let (Some(n), Some(db)) =
                    (idx(1), parts.get(2).and_then(|s| s.parse::<f32>().ok()))
                {
                    gains[n] = db_to_gain(db);
                    h.send(EngineCommand::SetGain {
                        cart: CartKey(n as u32 + 1),
                        gain: gains[n],
                    })?;
                }
            }
            _ => println!("?"),
        }
        std::thread::sleep(Duration::from_millis(5));
        print_events(&mut h, t0);
        print!("> ");
        std::io::stdout().flush()?;
    }
    Ok(())
}

fn render(files: &[PathBuf], out: &PathBuf, rate: u32, gain_db: f32, block: usize) -> Result<()> {
    let bufs = load_all(files, rate)?;
    let (mut h, mut mixer) = Engine::new(EngineConfig::new(rate, 2));
    let longest = bufs.iter().map(|b| b.frames()).max().unwrap_or(0);
    for (i, b) in bufs.iter().enumerate() {
        h.send(EngineCommand::Play {
            cart: CartKey(i as u32 + 1),
            audio: b.clone(),
            stream: None,
            params: PlayParams {
                gain: db_to_gain(gain_db),
                retrigger: Retrigger::Overlap,
                ..Default::default()
            },
        })?;
    }
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(out, spec)?;
    let mut buf = vec![0.0f32; block.max(1) * 2];
    let mut done = 0;
    while done < longest + rate as usize / 10 {
        mixer.process(&mut buf);
        for s in &buf {
            w.write_sample(*s)?;
        }
        done += block.max(1);
    }
    w.finalize()?;
    let t0 = Instant::now();
    print_events(&mut h, t0);
    println!(
        "Wrote {} ({:.2} s)",
        out.display(),
        done as f64 / rate as f64
    );
    Ok(())
}
