//! Waterfall lines: a windowed FFT of the newest receive audio, 0-3000 Hz.

use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::sync::Arc;

/// FFT length at 12 kHz: 4096 samples = 0.34 s window, 2.93 Hz per bin.
pub const FFT_LEN: usize = 4096;
/// Highest frequency shown.
pub const MAX_HZ: f32 = 3000.0;

pub struct Spectrum {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    sample_rate: u32,
}

/// One waterfall line. `bins[i]` is the power of bin i in dB, stored as `dB + 200` (1 dB steps,
/// clamped to 0-255) so it can travel as small integers.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SpectrumLine {
    pub hz_per_bin: f32,
    pub bins: Vec<u8>,
}

impl Spectrum {
    pub fn new(sample_rate: u32) -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT_LEN);
        let window = (0..FFT_LEN)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_LEN - 1) as f32).cos())
            .collect();
        Spectrum { fft, window, sample_rate }
    }

    /// Computes a line from the newest audio (`samples` should hold FFT_LEN samples), with
    /// `gain_db` added so the waterfall tracks the receive gain setting.
    pub fn line(&self, samples: &[f32], gain_db: f32) -> Option<SpectrumLine> {
        if samples.len() < FFT_LEN {
            return None;
        }
        let tail = &samples[samples.len() - FFT_LEN..];
        let mut buf: Vec<Complex<f32>> = tail.iter().zip(&self.window).map(|(s, w)| Complex::new(s * w, 0.0)).collect();
        self.fft.process(&mut buf);
        let hz_per_bin = self.sample_rate as f32 / FFT_LEN as f32;
        let n_bins = (MAX_HZ / hz_per_bin).ceil() as usize;
        // Normalize so a full-scale sine reads about 0 dB in its bin.
        let norm = 2.0 / self.window.iter().sum::<f32>();
        let bins = buf[..n_bins]
            .iter()
            .map(|c| {
                let mag = c.norm() * norm;
                let db = 20.0 * mag.max(1e-12).log10() + gain_db;
                (db + 200.0).round().clamp(0.0, 255.0) as u8
            })
            .collect();
        Some(SpectrumLine { hz_per_bin, bins })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_shows_up_in_the_right_bin() {
        let sr = 12_000;
        let tone: Vec<f32> = (0..FFT_LEN).map(|i| 0.5 * (2.0 * std::f32::consts::PI * 1500.0 * i as f32 / sr as f32).sin()).collect();
        let line = Spectrum::new(sr).line(&tone, 0.0).unwrap();
        assert_eq!(line.bins.len(), (3000.0 / line.hz_per_bin).ceil() as usize);
        let (peak_bin, peak) = line.bins.iter().enumerate().max_by_key(|(_, v)| **v).unwrap();
        let peak_hz = peak_bin as f32 * line.hz_per_bin;
        assert!((peak_hz - 1500.0).abs() < 3.0, "peak at {peak_hz} Hz");
        let peak_db = *peak as f32 - 200.0;
        assert!((peak_db - (-6.0)).abs() < 1.5, "peak {peak_db} dB"); // amplitude 0.5 = -6 dB
        assert!(line.bins[200] < peak - 60, "leakage too high far from the tone");

        let boosted = Spectrum::new(sr).line(&tone, 20.0).unwrap();
        assert_eq!(boosted.bins[peak_bin], peak + 20);
    }
}
