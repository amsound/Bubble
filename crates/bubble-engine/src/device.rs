//! Audio output through cpal. The stream keeps running (outputting silence when
//! idle) so the first trigger never waits for device start-up.

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc;
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, FromSample, SampleFormat, SizedSample, StreamConfig};

use crate::engine::{Engine, EngineConfig, EngineHandle};
use crate::mixer::Mixer;
use crate::status::EngineStatus;

#[derive(Debug, thiserror::Error)]
pub enum DeviceError {
    #[error("no output device available")]
    NoDevice,
    #[error("output device '{0}' not found")]
    NotFound(String),
    #[error("audio backend error: {0}")]
    Backend(String),
    #[error("unsupported sample format {0}")]
    UnsupportedFormat(String),
}

fn backend<E: std::fmt::Display>(e: E) -> DeviceError {
    DeviceError::Backend(e.to_string())
}

/// One available output device.
#[derive(Clone, Debug)]
pub struct OutputDeviceInfo {
    pub name: String,
    pub id: String,
    pub is_default: bool,
    pub default_sample_rate: Option<u32>,
    pub default_channels: Option<u16>,
    /// Most output channels any supported configuration offers.
    pub max_channels: u16,
    /// Whether 48 kHz is among the supported rates.
    pub supports_48k: bool,
}

/// Lists output devices of the default host.
pub fn list_output_devices() -> Result<Vec<OutputDeviceInfo>, DeviceError> {
    let host = cpal::default_host();
    let default_id = host
        .default_output_device()
        .and_then(|d| d.id().ok())
        .map(|id| id.to_string());
    let mut out = Vec::new();
    for dev in host.output_devices().map_err(backend)? {
        let id = dev.id().map(|i| i.to_string()).unwrap_or_default();
        let name = dev
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| id.clone());
        let cfg = dev.default_output_config().ok();
        let ranges: Vec<_> = dev
            .supported_output_configs()
            .map(|c| c.collect())
            .unwrap_or_default();
        let max_channels = ranges
            .iter()
            .map(|c| c.channels())
            .max()
            .or(cfg.as_ref().map(|c| c.channels()))
            .unwrap_or(2);
        let supports_48k = ranges
            .iter()
            .any(|c| c.min_sample_rate() <= 48_000 && 48_000 <= c.max_sample_rate());
        out.push(OutputDeviceInfo {
            max_channels,
            supports_48k,
            is_default: Some(&id) == default_id.as_ref(),
            name,
            id,
            default_sample_rate: cfg.as_ref().map(|c| c.sample_rate()),
            default_channels: cfg.as_ref().map(|c| c.channels()),
        });
    }
    Ok(out)
}

/// Requested output settings. `None` fields use the device defaults.
#[derive(Clone, Debug, Default)]
pub struct OutputRequest {
    /// Device name (as listed) or `None` for the system default.
    pub device_name: Option<String>,
    pub sample_rate: Option<u32>,
    /// At least this many output channels (for outputs beyond channels 1–2).
    pub min_channels: Option<u16>,
    /// Fixed callback size in frames; `None` = backend default.
    pub buffer_frames: Option<u32>,
    pub voices: Option<usize>,
}

/// Asynchronous stream problems reported by the backend.
#[derive(Clone, Debug)]
pub enum StreamProblem {
    /// The device disappeared or the stream died; the output must be reopened.
    Fatal(String),
    /// Recoverable glitch (underrun, route change).
    Warning(String),
}

/// A running output stream with its engine. Dropping it stops audio.
pub struct AudioOutput {
    _stream: cpal::Stream,
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub buffer: BufferSize,
    pub sample_format: String,
    /// Windows: the device's rate, or its mix rate if only that is off (Windows converts
    /// Bubble's audio when this differs from `sample_rate`). `None` elsewhere: macOS sets the device
    /// to the stream's rate itself.
    pub device_rate: Option<u32>,
    /// Windows: what happened when the device's rate was set to Bubble's (for the log).
    pub rate_note: Option<String>,
    problems: mpsc::Receiver<StreamProblem>,
}

impl AudioOutput {
    /// Opens the device, builds the engine and starts the stream.
    pub fn open(req: &OutputRequest) -> Result<(AudioOutput, EngineHandle), DeviceError> {
        let host = cpal::default_host();
        let device = match &req.device_name {
            None => host.default_output_device().ok_or(DeviceError::NoDevice)?,
            Some(name) => host
                .output_devices()
                .map_err(backend)?
                .find(|d| {
                    d.description().map(|x| x.name() == name).unwrap_or(false)
                        || d.id().map(|i| i.to_string() == *name).unwrap_or(false)
                })
                .ok_or_else(|| DeviceError::NotFound(name.clone()))?,
        };
        let device_name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "unknown".into());

        // Windows: run the device at Bubble's rate, so Windows does not convert again.
        #[cfg(windows)]
        let (device_rate, rate_note) = match (req.sample_rate, device.id()) {
            (Some(want), Ok(id)) => {
                // Whichever of the two is off, or the device's when neither is.
                let off = |(dev, mix): (u32, u32)| if dev != want { dev } else { mix };
                let id = id.id().to_string();
                match crate::device_rate::device_rates(&id) {
                    Some((dev, mix)) if dev != want || mix != want => {
                        let before = format!("device format {dev} Hz, mix format {mix} Hz");
                        match crate::device_rate::set_device_rate(&id, want) {
                            Ok((d, m)) => (
                                Some(off((d, m))),
                                Some(format!(
                                    "{before}; asked for {want} Hz; now device format {d} Hz, \
                                     mix format {m} Hz"
                                )),
                            ),
                            Err(e) => (Some(off((dev, mix))), Some(format!("{before}; {e}"))),
                        }
                    }
                    Some((dev, mix)) => (
                        Some(dev),
                        Some(format!(
                            "device format {dev} Hz, mix format {mix} Hz; as wanted"
                        )),
                    ),
                    None => (None, Some("Windows did not give its formats".to_string())),
                }
            }
            _ => (None, None),
        };
        #[cfg(not(windows))]
        let (device_rate, rate_note): (Option<u32>, Option<String>) = (None, None);

        let default = device.default_output_config().map_err(backend)?;
        let mut sample_format = default.sample_format();
        let mut config: StreamConfig = default.config();
        let want_ch = req.min_channels.unwrap_or(0).max(1);
        if req.sample_rate.is_some() || want_ch > config.channels {
            let rate = req.sample_rate.unwrap_or(config.sample_rate);
            // Fewest channels that are enough (the default count when it is), then
            // 32-bit float, at the requested rate.
            let mut candidates: Vec<_> = device
                .supported_output_configs()
                .map_err(backend)?
                .filter(|c| {
                    c.channels() >= want_ch
                        && c.min_sample_rate() <= rate
                        && rate <= c.max_sample_rate()
                })
                .collect();
            candidates.sort_by_key(|c| {
                (
                    c.channels() != config.channels.max(want_ch),
                    c.channels(),
                    c.sample_format() != SampleFormat::F32,
                )
            });
            let chosen = candidates.into_iter().next().ok_or_else(|| {
                DeviceError::Backend(format!(
                    "{device_name} cannot run {want_ch} channel(s) at {rate} Hz"
                ))
            })?;
            sample_format = chosen.sample_format();
            config = chosen.with_sample_rate(rate).config();
        }
        if let Some(frames) = req.buffer_frames {
            config.buffer_size = BufferSize::Fixed(frames);
        }

        let mut ecfg = EngineConfig::new(config.sample_rate, config.channels);
        if let Some(v) = req.voices {
            ecfg.voices = v;
        }
        let (handle, mixer) = Engine::new(ecfg);
        let status = handle.status().clone();
        let (ptx, prx) = mpsc::channel();

        let stream = match sample_format {
            SampleFormat::F32 => build::<f32>(&device, &config, mixer, status, ptx),
            SampleFormat::I16 => build::<i16>(&device, &config, mixer, status, ptx),
            SampleFormat::I32 => build::<i32>(&device, &config, mixer, status, ptx),
            SampleFormat::U16 => build::<u16>(&device, &config, mixer, status, ptx),
            SampleFormat::F64 => build::<f64>(&device, &config, mixer, status, ptx),
            other => return Err(DeviceError::UnsupportedFormat(other.to_string())),
        }?;
        stream.play().map_err(backend)?;
        Ok((
            AudioOutput {
                _stream: stream,
                device_name,
                sample_rate: config.sample_rate,
                channels: config.channels,
                buffer: config.buffer_size,
                sample_format: sample_format.to_string(),
                device_rate,
                rate_note,
                problems: prx,
            },
            handle,
        ))
    }

    /// Returns stream problems reported since the last call.
    pub fn take_problems(&self) -> Vec<StreamProblem> {
        self.problems.try_iter().collect()
    }
}

/// Scratch size for non-f32 devices; larger callbacks are processed in chunks.
const SCRATCH_FRAMES: usize = 8192;

fn build<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mut mixer: Mixer,
    status: Arc<EngineStatus>,
    problems: mpsc::Sender<StreamProblem>,
) -> Result<cpal::Stream, DeviceError>
where
    T: SizedSample + FromSample<f32> + 'static,
{
    let channels = config.channels as usize;
    let mut scratch = vec![0.0f32; SCRATCH_FRAMES * channels];
    let err_status = status.clone();
    let data_cb = move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
        let t0 = Instant::now();
        let ts = info.timestamp();
        if let Some(d) = ts.playback.checked_duration_since(ts.callback) {
            status
                .output_latency_us
                .store(d.as_micros() as u64, Relaxed);
        }
        if let Some(out) = cast_f32(data) {
            mixer.process(out);
        } else {
            for chunk in data.chunks_mut(SCRATCH_FRAMES * channels) {
                let s = &mut scratch[..chunk.len()];
                mixer.process(s);
                for (d, v) in chunk.iter_mut().zip(s.iter()) {
                    *d = T::from_sample(*v);
                }
            }
        }
        status
            .max_process_ns
            .fetch_max(t0.elapsed().as_nanos() as u64, Relaxed);
    };
    let err_cb = move |e: cpal::Error| {
        use cpal::ErrorKind::*;
        let problem = match e.kind() {
            Xrun => {
                err_status.xruns.fetch_add(1, Relaxed);
                StreamProblem::Warning(e.to_string())
            }
            DeviceChanged | RealtimeDenied => StreamProblem::Warning(e.to_string()),
            _ => StreamProblem::Fatal(e.to_string()),
        };
        let _ = problems.send(problem);
    };
    device
        .build_output_stream(*config, data_cb, err_cb, None)
        .map_err(backend)
}

/// Reinterprets the slice as `f32` when `T` is `f32` (no copy needed).
fn cast_f32<T: 'static>(data: &mut [T]) -> Option<&mut [f32]> {
    if std::any::TypeId::of::<T>() == std::any::TypeId::of::<f32>() {
        // SAFETY: T is f32, so the layout is identical.
        Some(unsafe { std::slice::from_raw_parts_mut(data.as_mut_ptr() as *mut f32, data.len()) })
    } else {
        None
    }
}
