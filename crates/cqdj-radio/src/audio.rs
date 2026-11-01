//! Sound card input and output (cpal: CoreAudio on macOS, WASAPI on Windows, ALSA on Linux).
//!
//! Receive audio is mixed to mono, resampled to 12 kHz and kept in a rolling buffer.
//! The output stream runs continuously and plays silence until we queue a transmission,
//! so a transmission starts within one audio buffer of when we ask for it.

use crate::resample::Resampler;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

/// What the station loop needs from a sound card. [`Audio`] is the real one; tests use a fake.
pub trait AudioIo {
    fn output_rate(&self) -> u32;
    /// Starts sending `samples` (mono, at `output_rate`) right away, replacing anything queued.
    fn play(&self, samples: Vec<f32>);
    fn stop(&self);
    fn is_playing(&self) -> bool;
    /// Received audio (12 kHz) from wall-clock time `start` to now, at most `max_secs`.
    fn rx_since(&self, start: f64, max_secs: f64) -> Vec<f32>;
    /// Receive level over the last half second, dBFS.
    fn rx_dbfs(&self) -> f32;
    /// The newest `n` received samples (12 kHz), fewer if not that many yet.
    fn rx_latest(&self, n: usize) -> Vec<f32>;
    fn device_names(&self) -> (String, String);
}
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

pub const RX_RATE: u32 = cqdj_engine::SAMPLE_RATE;
const RX_KEEP_SECONDS: usize = 40;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AudioDeviceList {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// Best guess at the radio's USB sound card, if one is plugged in.
    pub suggested_input: Option<String>,
    pub suggested_output: Option<String>,
}

fn device_name(d: &cpal::Device) -> Option<String> {
    d.description().ok().map(|desc| desc.name().to_string())
}

fn looks_like_radio(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("usb audio codec") || n.contains("ft-710") || n.contains("yaesu") || n.contains("codec")
}

pub fn list_devices() -> AudioDeviceList {
    let host = cpal::default_host();
    let inputs: Vec<String> = host.input_devices().map(|it| it.filter_map(|d| device_name(&d)).collect()).unwrap_or_default();
    let outputs: Vec<String> = host.output_devices().map(|it| it.filter_map(|d| device_name(&d)).collect()).unwrap_or_default();
    AudioDeviceList {
        suggested_input: inputs.iter().find(|n| looks_like_radio(n)).cloned(),
        suggested_output: outputs.iter().find(|n| looks_like_radio(n)).cloned(),
        inputs,
        outputs,
    }
}

/// Rolling buffer of received audio at 12 kHz.
pub struct RxBuffer {
    samples: VecDeque<f32>,
    /// Total samples ever received (index of the sample after the newest one).
    pub total: u64,
    /// Wall-clock time (Unix seconds) when the newest sample arrived.
    pub newest_at: f64,
    /// Peak level of the most recent block, in dBFS.
    pub level_dbfs: f32,
}

impl RxBuffer {
    fn new() -> Self {
        RxBuffer { samples: VecDeque::with_capacity(RX_KEEP_SECONDS * RX_RATE as usize), total: 0, newest_at: 0.0, level_dbfs: -120.0 }
    }

    fn push(&mut self, block: &[f32], now: f64) {
        let cap = RX_KEEP_SECONDS * RX_RATE as usize;
        for &s in block {
            if self.samples.len() == cap {
                self.samples.pop_front();
            }
            self.samples.push_back(s);
        }
        self.total += block.len() as u64;
        self.newest_at = now;
        let peak = block.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        self.level_dbfs = if peak > 0.0 { 20.0 * peak.log10() } else { -120.0 };
    }

    /// Audio from wall-clock time `start` up to the newest sample (at most `max_secs`).
    pub fn since(&self, start: f64, max_secs: f64) -> Vec<f32> {
        let back = ((self.newest_at - start) * RX_RATE as f64).round().max(0.0) as usize;
        let back = back.min(self.samples.len()).min((max_secs * RX_RATE as f64) as usize);
        self.samples.iter().skip(self.samples.len() - back).copied().collect()
    }

    /// The newest `n` samples.
    pub fn latest(&self, n: usize) -> Vec<f32> {
        let n = n.min(self.samples.len());
        self.samples.iter().skip(self.samples.len() - n).copied().collect()
    }

    /// RMS level over the last `secs` seconds, in dBFS.
    pub fn rms_dbfs(&self, secs: f64) -> f32 {
        let n = ((secs * RX_RATE as f64) as usize).min(self.samples.len());
        if n == 0 {
            return -120.0;
        }
        let sum: f32 = self.samples.iter().rev().take(n).map(|s| s * s).sum();
        let rms = (sum / n as f32).sqrt();
        if rms > 0.0 {
            20.0 * rms.log10()
        } else {
            -120.0
        }
    }
}

/// Audio waiting to go out to the radio.
#[derive(Default)]
struct TxQueue {
    samples: Vec<f32>,
    pos: usize,
}

/// Open input and output streams. Keep this on the thread that created it.
pub struct Audio {
    _input: cpal::Stream,
    _output: cpal::Stream,
    pub rx: Arc<Mutex<RxBuffer>>,
    tx: Arc<Mutex<TxQueue>>,
    pub output_rate: u32,
    pub input_name: String,
    pub output_name: String,
}

#[derive(Debug)]
pub struct AudioError(pub String);

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for AudioError {}

fn find_device(input: bool, name: &str) -> Result<cpal::Device, AudioError> {
    let host = cpal::default_host();
    if name.is_empty() {
        let d = if input { host.default_input_device() } else { host.default_output_device() };
        return d.ok_or_else(|| AudioError(format!("no default {} device", if input { "input" } else { "output" })));
    }
    let devices = if input { host.input_devices() } else { host.output_devices() }.map_err(|e| AudioError(e.to_string()))?;
    for d in devices {
        if device_name(&d).as_deref() == Some(name) {
            return Ok(d);
        }
    }
    Err(AudioError(format!("sound device \"{name}\" not found (is the radio's USB cable plugged in?)")))
}

impl AudioIo for Audio {
    fn output_rate(&self) -> u32 {
        self.output_rate
    }
    fn play(&self, samples: Vec<f32>) {
        Audio::play(self, samples)
    }
    fn stop(&self) {
        Audio::stop(self)
    }
    fn is_playing(&self) -> bool {
        Audio::is_playing(self)
    }
    fn rx_since(&self, start: f64, max_secs: f64) -> Vec<f32> {
        self.rx.lock().map(|b| b.since(start, max_secs)).unwrap_or_default()
    }
    fn rx_dbfs(&self) -> f32 {
        self.rx.lock().map(|b| b.rms_dbfs(0.5)).unwrap_or(-120.0)
    }
    fn rx_latest(&self, n: usize) -> Vec<f32> {
        self.rx.lock().map(|b| b.latest(n)).unwrap_or_default()
    }
    fn device_names(&self) -> (String, String) {
        (self.input_name.clone(), self.output_name.clone())
    }
}

impl Audio {
    pub fn open(input_name: &str, output_name: &str) -> Result<Audio, AudioError> {
        let in_dev = find_device(true, input_name)?;
        let out_dev = find_device(false, output_name)?;

        let rx = Arc::new(Mutex::new(RxBuffer::new()));
        let in_cfg = in_dev.default_input_config().map_err(|e| AudioError(format!("input: {e}")))?;
        let input = build_input(&in_dev, in_cfg, rx.clone())?;

        let tx = Arc::new(Mutex::new(TxQueue::default()));
        let out_cfg = out_dev.default_output_config().map_err(|e| AudioError(format!("output: {e}")))?;
        let output_rate = out_cfg.sample_rate();
        let output = build_output(&out_dev, out_cfg, tx.clone())?;

        input.play().map_err(|e| AudioError(format!("can't start input: {e}")))?;
        output.play().map_err(|e| AudioError(format!("can't start output: {e}")))?;
        Ok(Audio {
            _input: input,
            _output: output,
            rx,
            tx,
            output_rate,
            input_name: device_name(&in_dev).unwrap_or_default(),
            output_name: device_name(&out_dev).unwrap_or_default(),
        })
    }

    /// Starts sending `samples` (mono, at `output_rate`) to the radio right away.
    pub fn play(&self, samples: Vec<f32>) {
        let mut q = self.tx.lock().unwrap();
        q.samples = samples;
        q.pos = 0;
    }

    pub fn stop(&self) {
        let mut q = self.tx.lock().unwrap();
        q.samples.clear();
        q.pos = 0;
    }

    pub fn is_playing(&self) -> bool {
        let q = self.tx.lock().unwrap();
        q.pos < q.samples.len()
    }
}

fn build_input(dev: &cpal::Device, cfg: cpal::SupportedStreamConfig, rx: Arc<Mutex<RxBuffer>>) -> Result<cpal::Stream, AudioError> {
    let format = cfg.sample_format();
    let config: cpal::StreamConfig = cfg.into();
    macro_rules! go {
        ($t:ty) => {
            input_stream::<$t>(dev, config, rx)
        };
    }
    match format {
        cpal::SampleFormat::F32 => go!(f32),
        cpal::SampleFormat::I16 => go!(i16),
        cpal::SampleFormat::I32 => go!(i32),
        cpal::SampleFormat::U16 => go!(u16),
        cpal::SampleFormat::I8 => go!(i8),
        cpal::SampleFormat::U8 => go!(u8),
        other => Err(AudioError(format!("unsupported input sample format {other}"))),
    }
}

fn input_stream<T>(dev: &cpal::Device, config: cpal::StreamConfig, rx: Arc<Mutex<RxBuffer>>) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = config.channels.max(1) as usize;
    let mut resampler = Resampler::new(config.sample_rate, RX_RATE);
    let mut mono = Vec::new();
    let mut out = Vec::new();
    dev.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            mono.clear();
            for frame in data.chunks(channels) {
                let sum: f32 = frame.iter().map(|s| s.to_sample::<f32>()).sum();
                mono.push(sum / channels as f32);
            }
            out.clear();
            resampler.process(&mono, &mut out);
            if let Ok(mut b) = rx.lock() {
                b.push(&out, crate::clock::now_secs());
            }
        },
        |e| eprintln!("audio input error: {e}"),
        None,
    )
    .map_err(|e| AudioError(format!("can't open input: {e}")))
}

fn build_output(dev: &cpal::Device, cfg: cpal::SupportedStreamConfig, tx: Arc<Mutex<TxQueue>>) -> Result<cpal::Stream, AudioError> {
    let format = cfg.sample_format();
    let config: cpal::StreamConfig = cfg.into();
    macro_rules! go {
        ($t:ty) => {
            output_stream::<$t>(dev, config, tx)
        };
    }
    match format {
        cpal::SampleFormat::F32 => go!(f32),
        cpal::SampleFormat::I16 => go!(i16),
        cpal::SampleFormat::I32 => go!(i32),
        cpal::SampleFormat::U16 => go!(u16),
        cpal::SampleFormat::I8 => go!(i8),
        cpal::SampleFormat::U8 => go!(u8),
        other => Err(AudioError(format!("unsupported output sample format {other}"))),
    }
}

fn output_stream<T>(dev: &cpal::Device, config: cpal::StreamConfig, tx: Arc<Mutex<TxQueue>>) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels.max(1) as usize;
    dev.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let mut q = match tx.try_lock() {
                Ok(q) => q,
                Err(_) => {
                    for s in data.iter_mut() {
                        *s = T::from_sample(0.0f32);
                    }
                    return;
                }
            };
            for frame in data.chunks_mut(channels) {
                let v = if q.pos < q.samples.len() {
                    let v = q.samples[q.pos];
                    q.pos += 1;
                    v
                } else {
                    0.0
                };
                for s in frame.iter_mut() {
                    *s = T::from_sample(v);
                }
            }
        },
        |e| eprintln!("audio output error: {e}"),
        None,
    )
    .map_err(|e| AudioError(format!("can't open output: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rx_buffer_returns_audio_since_a_time() {
        let mut b = RxBuffer::new();
        let block = vec![0.5f32; RX_RATE as usize]; // 1 s
        for i in 0..3 {
            b.push(&block, 100.0 + i as f64 + 1.0);
        }
        // Newest sample at t=103; audio since t=101.5 is the last 1.5 s.
        let got = b.since(101.5, 15.0);
        assert_eq!(got.len(), (1.5 * RX_RATE as f64) as usize);
        assert!((b.level_dbfs - (-6.02)).abs() < 0.1);
        assert!((b.rms_dbfs(1.0) - (-6.02)).abs() < 0.1);
    }

    #[test]
    fn spots_the_radio_sound_card() {
        assert!(looks_like_radio("USB Audio CODEC"));
        assert!(!looks_like_radio("MacBook Pro Microphone"));
    }
}
