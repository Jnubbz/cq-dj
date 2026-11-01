//! CQ DJ's FT8 engine.
//!
//! * [`encode_tones`] packs a message into the 79 FT8 tones.
//! * [`synthesize`] turns tones into GFSK audio at a given audio frequency.
//! * [`build_tx_audio`] mixes several outgoing messages (one per QSO slot) into one stream.
//! * [`decode`] decodes a 15-second receive slot.
//!
//! FT8 coding comes from ft8_lib (vendored, MIT). The ft8_lib C code shares one callsign
//! hash table, so every call into it goes through a process-wide lock.

mod ffi;

use std::ffi::{CStr, CString};
use std::fmt;
use std::sync::Mutex;

/// Number of tones (symbols) in one FT8 transmission.
pub const FT8_NUM_TONES: usize = ffi::NUM_TONES;
/// Duration of one FT8 symbol, in seconds.
pub const FT8_SYMBOL_PERIOD: f32 = 0.160;
/// Tone spacing, in Hz.
pub const FT8_TONE_SPACING_HZ: f32 = 6.25;
/// Occupied bandwidth of one FT8 signal (8 tones x 6.25 Hz), in Hz.
pub const FT8_BANDWIDTH_HZ: f32 = 8.0 * FT8_TONE_SPACING_HZ;
/// Length of one T/R slot, in seconds.
pub const FT8_SLOT_SECONDS: f32 = 15.0;
/// Transmissions start this long after the slot boundary.
pub const FT8_TX_START_SECONDS: f32 = 0.5;
/// Default sample rate used throughout CQ DJ.
pub const SAMPLE_RATE: u32 = 12_000;

const GFSK_BT: f32 = 2.0;
const GFSK_CONST_K: f32 = 5.336446; // pi * sqrt(2 / ln 2)

/// Offset (dB) that maps the raw waterfall SNR estimate to WSJT-X style SNR in 2500 Hz.
/// Calibrated against synthetic signals with known SNR (see tests/roundtrip.rs).
const SNR_CALIBRATION_DB: f32 = -27.0;

/// ft8_lib reports candidate times one symbol late (its STFT frames span two symbols).
/// Measured against synthetic signals with known timing (see examples/dt_probe.rs).
const DT_CALIBRATION_SEC: f32 = -FT8_SYMBOL_PERIOD;

static FT8_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    FT8_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum EngineError {
    /// The message text could not be packed into an FT8 message.
    BadMessage { message: String, reason: &'static str },
    /// Two outgoing signals would overlap on the waterfall.
    Overlap { a_hz: f32, b_hz: f32 },
    /// A signal would fall outside the usable audio passband.
    OutOfBand { freq_hz: f32 },
    /// No signals were given.
    NothingToSend,
    /// The decoder failed internally.
    Decoder(i32),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::BadMessage { message, reason } => write!(f, "can't send \"{message}\": {reason}"),
            EngineError::Overlap { a_hz, b_hz } => {
                write!(f, "signals at {a_hz:.0} Hz and {b_hz:.0} Hz overlap (need at least {FT8_BANDWIDTH_HZ:.0} Hz apart)")
            }
            EngineError::OutOfBand { freq_hz } => write!(f, "{freq_hz:.0} Hz is outside the {MIN_AUDIO_HZ:.0}-{MAX_AUDIO_HZ:.0} Hz range"),
            EngineError::NothingToSend => write!(f, "no messages to send"),
            EngineError::Decoder(code) => write!(f, "decoder error {code}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// Lowest usable audio frequency for tone 0.
pub const MIN_AUDIO_HZ: f32 = 200.0;
/// Highest usable audio frequency for tone 0 (the signal extends 50 Hz above it).
pub const MAX_AUDIO_HZ: f32 = 3000.0 - FT8_BANDWIDTH_HZ;

// ---------------------------------------------------------------------------
// Encode
// ---------------------------------------------------------------------------

/// Packs a message like `"CQ KN0BLE EL98"` or `"JA1QRX KN0BLE -14"` into FT8 tones.
pub fn encode_tones(message: &str) -> Result<[u8; FT8_NUM_TONES], EngineError> {
    let text = message.trim().to_ascii_uppercase();
    let c = CString::new(text.clone()).map_err(|_| EngineError::BadMessage { message: text.clone(), reason: "contains a NUL byte" })?;
    let mut tones = [0u8; FT8_NUM_TONES];
    let rc = {
        let _g = lock();
        unsafe { ffi::cqdj_ft8_tones(c.as_ptr(), tones.as_mut_ptr()) }
    };
    match rc {
        0 => Ok(tones),
        1 => Err(EngineError::BadMessage { message: text, reason: "first callsign is not valid" }),
        2 => Err(EngineError::BadMessage { message: text, reason: "second callsign is not valid" }),
        3 => Err(EngineError::BadMessage { message: text, reason: "callsign suffix is not valid" }),
        4 => Err(EngineError::BadMessage { message: text, reason: "grid or report is not valid" }),
        _ => Err(EngineError::BadMessage { message: text, reason: "not a message FT8 can send" }),
    }
}

/// Synthesizes GFSK audio (peak amplitude 1.0) for the given tones, with tone 0 at `f0_hz`.
/// Returns `79 * 0.16 s` of audio (12.64 s) at `sample_rate`.
pub fn synthesize(tones: &[u8], f0_hz: f32, sample_rate: u32) -> Vec<f32> {
    use std::f32::consts::PI;
    let n_sym = tones.len();
    if n_sym == 0 {
        return Vec::new();
    }
    let sr = sample_rate as f32;
    let n_spsym = (0.5 + sr * FT8_SYMBOL_PERIOD) as usize;
    let n_wave = n_sym * n_spsym;

    // GFSK smoothing pulse, 3 symbols long.
    let pulse: Vec<f32> = (0..3 * n_spsym)
        .map(|i| {
            let t = i as f32 / n_spsym as f32 - 1.5;
            let a1 = GFSK_CONST_K * GFSK_BT * (t + 0.5);
            let a2 = GFSK_CONST_K * GFSK_BT * (t - 0.5);
            (erf(a1) - erf(a2)) / 2.0
        })
        .collect();

    let dphi_peak = 2.0 * PI / n_spsym as f32; // modulation index h = 1
    let mut dphi = vec![2.0 * PI * f0_hz / sr; n_wave + 2 * n_spsym];
    for (i, &tone) in tones.iter().enumerate() {
        let ib = i * n_spsym;
        for (j, p) in pulse.iter().enumerate() {
            dphi[ib + j] += dphi_peak * tone as f32 * p;
        }
    }
    // Extend the first and last symbols so the edges are smooth.
    for j in 0..2 * n_spsym {
        dphi[j] += dphi_peak * pulse[j + n_spsym] * tones[0] as f32;
        dphi[j + n_sym * n_spsym] += dphi_peak * pulse[j] * tones[n_sym - 1] as f32;
    }

    let mut out = Vec::with_capacity(n_wave);
    let mut phi = 0.0f64;
    for k in 0..n_wave {
        out.push(phi.sin() as f32);
        phi = (phi + dphi[k + n_spsym] as f64) % (2.0 * std::f64::consts::PI);
    }

    // Raised-cosine ramp on the first and last 1/8 symbol to avoid key clicks.
    let n_ramp = n_spsym / 8;
    for i in 0..n_ramp {
        let env = (1.0 - (2.0 * PI * i as f32 / (2 * n_ramp) as f32).cos()) / 2.0;
        out[i] *= env;
        out[n_wave - 1 - i] *= env;
    }
    out
}

/// Error function (Abramowitz and Stegun 7.1.26, max error 1.5e-7), so we don't depend on libm's erff.
fn erf(x: f32) -> f32 {
    let x = x as f64;
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let y = 1.0 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * (-x * x).exp();
    (sign * y) as f32
}

// ---------------------------------------------------------------------------
// Multi-signal transmit
// ---------------------------------------------------------------------------

/// One outgoing message on one audio frequency (one QSO slot).
#[derive(Debug, Clone, PartialEq)]
pub struct TxSignal {
    pub message: String,
    pub freq_hz: f32,
}

impl TxSignal {
    pub fn new(message: impl Into<String>, freq_hz: f32) -> Self {
        TxSignal { message: message.into(), freq_hz }
    }
}

/// How much weaker each signal is when `n` signals share a peak-power-limited transmitter.
/// Each stream gets 1/n of the peak amplitude, so 1/n^2 of the power: 20*log10(n) dB
/// (2 streams: 6 dB, 3: 9.5 dB, 4: 12 dB, 5: 14 dB).
pub fn per_signal_loss_db(n: usize) -> f32 {
    if n <= 1 {
        0.0
    } else {
        20.0 * (n as f32).log10()
    }
}

/// Builds the transmit audio for one slot: every message synthesized on its own frequency and
/// mixed together. Each signal gets an equal share of the peak amplitude, so the mix never
/// exceeds `level` (0.0..=1.0) and stays clean through a linear transmitter.
/// The result is 12.64 s long and should start playing at the slot boundary + 0.5 s.
pub fn build_tx_audio(signals: &[TxSignal], sample_rate: u32, level: f32) -> Result<Vec<f32>, EngineError> {
    if signals.is_empty() {
        return Err(EngineError::NothingToSend);
    }
    for s in signals {
        if !(MIN_AUDIO_HZ..=MAX_AUDIO_HZ).contains(&s.freq_hz) {
            return Err(EngineError::OutOfBand { freq_hz: s.freq_hz });
        }
    }
    let mut freqs: Vec<f32> = signals.iter().map(|s| s.freq_hz).collect();
    freqs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    for w in freqs.windows(2) {
        if w[1] - w[0] < FT8_BANDWIDTH_HZ {
            return Err(EngineError::Overlap { a_hz: w[0], b_hz: w[1] });
        }
    }

    let tones: Vec<[u8; FT8_NUM_TONES]> = signals.iter().map(|s| encode_tones(&s.message)).collect::<Result<_, _>>()?;
    let gain = level.clamp(0.0, 1.0) / signals.len() as f32;
    let mut mix: Vec<f32> = Vec::new();
    for (sig, t) in signals.iter().zip(&tones) {
        let wave = synthesize(t, sig.freq_hz, sample_rate);
        if mix.is_empty() {
            mix = vec![0.0; wave.len()];
        }
        for (m, w) in mix.iter_mut().zip(&wave) {
            *m += w * gain;
        }
    }
    Ok(mix)
}

// ---------------------------------------------------------------------------
// Decode
// ---------------------------------------------------------------------------

/// One decoded FT8 message.
#[derive(Debug, Clone, PartialEq)]
pub struct Decode {
    /// Message text, e.g. `"CQ JA1QRX PM95"`.
    pub text: String,
    /// Audio frequency of tone 0, in Hz.
    pub freq_hz: f32,
    /// Time offset from the nominal start (slot boundary + 0.5 s), like WSJT-X's DT.
    pub dt_sec: f32,
    /// Estimated SNR in a 2500 Hz bandwidth, like WSJT-X's report.
    pub snr_db: i32,
    /// Sync score from the candidate search (higher is better).
    pub score: i32,
}

/// Options for [`decode`].
#[derive(Debug, Clone, Copy)]
pub struct DecodeOptions {
    pub f_min_hz: f32,
    pub f_max_hz: f32,
    pub max_results: usize,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        DecodeOptions { f_min_hz: 100.0, f_max_hz: 3100.0, max_results: 100 }
    }
}

/// Decodes one receive slot. `samples` should start at the slot boundary and cover up to 15 s.
pub fn decode(samples: &[f32], sample_rate: u32, opts: DecodeOptions) -> Result<Vec<Decode>, EngineError> {
    let max = opts.max_results.max(1);
    let mut raw = vec![ffi::CqdjDecode::default(); max];
    let n = {
        let _g = lock();
        unsafe {
            ffi::cqdj_ft8_decode(
                samples.as_ptr(),
                samples.len().min(i32::MAX as usize) as i32,
                sample_rate as i32,
                opts.f_min_hz,
                opts.f_max_hz,
                raw.as_mut_ptr(),
                max as i32,
            )
        }
    };
    if n < 0 {
        return Err(EngineError::Decoder(n));
    }
    let mut out: Vec<Decode> = raw[..n as usize]
        .iter()
        .map(|r| {
            let text = unsafe { CStr::from_ptr(r.text.as_ptr()) }.to_string_lossy().trim().to_string();
            Decode {
                text,
                freq_hz: r.freq_hz,
                dt_sec: r.time_sec - FT8_TX_START_SECONDS + DT_CALIBRATION_SEC,
                snr_db: (r.snr_db + SNR_CALIBRATION_DB).round().clamp(-30.0, 40.0) as i32,
                score: r.score,
            }
        })
        .collect();
    out.sort_by(|a, b| a.freq_hz.partial_cmp(&b.freq_hz).unwrap());
    Ok(out)
}

/// Forgets callsigns remembered for decoding hashed (`<...>`) calls. Mostly useful in tests.
pub fn reset_callsign_memory() {
    let _g = lock();
    unsafe { ffi::cqdj_ft8_reset_hashes() }
}
