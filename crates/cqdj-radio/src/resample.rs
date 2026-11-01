//! Streaming sample-rate conversion for receive audio (device rate -> 12 kHz).
//!
//! FT8 lives below 3 kHz, so a modest windowed-sinc low-pass at ~4.8 kHz followed by
//! linear interpolation is plenty: aliasing and interpolation error sit far below the noise.

pub struct Resampler {
    ratio: f64, // input samples per output sample
    taps: Vec<f32>,
    history: Vec<f32>, // last taps.len()-1 input samples (filter memory)
    filtered: Vec<f32>, // filtered input not yet consumed
    pos: f64,          // position of the next output sample within `filtered`
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let ratio = in_rate as f64 / out_rate as f64;
        let taps = if ratio > 1.0 { lowpass_taps(in_rate as f64, 0.4 * out_rate as f64, 63) } else { vec![1.0] };
        let history = vec![0.0; taps.len() - 1];
        Resampler { ratio, taps, history, filtered: Vec::new(), pos: 0.0 }
    }

    /// Feeds input samples; appends resampled output to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let n = self.taps.len();
        self.history.extend_from_slice(input);
        let usable = self.history.len().saturating_sub(n - 1);
        for i in 0..usable {
            let window = &self.history[i..i + n];
            let acc: f32 = window.iter().zip(&self.taps).map(|(x, t)| x * t).sum();
            self.filtered.push(acc);
        }
        self.history.drain(..usable);

        while self.pos + 1.0 < self.filtered.len() as f64 {
            let i = self.pos.floor() as usize;
            let frac = (self.pos - i as f64) as f32;
            out.push(self.filtered[i] * (1.0 - frac) + self.filtered[i + 1] * frac);
            self.pos += self.ratio;
        }
        let consumed = (self.pos.floor() as usize).min(self.filtered.len());
        self.filtered.drain(..consumed);
        self.pos -= consumed as f64;
    }
}

fn lowpass_taps(rate: f64, cutoff_hz: f64, n: usize) -> Vec<f32> {
    let fc = cutoff_hz / rate;
    let m = (n - 1) as f64;
    let mut taps: Vec<f64> = (0..n)
        .map(|i| {
            let x = i as f64 - m / 2.0;
            let sinc = if x == 0.0 { 2.0 * fc } else { (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x) };
            let blackman = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / m).cos() + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / m).cos();
            sinc * blackman
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    for t in &mut taps {
        *t /= sum;
    }
    taps.into_iter().map(|t| t as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hz: f32, secs: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize).map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin()).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn keeps_ft8_tones_and_removes_aliases() {
        for in_rate in [48_000u32, 44_100, 96_000] {
            let mut r = Resampler::new(in_rate, 12_000);
            let mut out = Vec::new();
            // Feed in odd-sized chunks like a sound card would.
            let input = tone(in_rate, 1500.0, 2.0);
            for chunk in input.chunks(517) {
                r.process(chunk, &mut out);
            }
            let expected = 2.0 * 12_000.0;
            assert!((out.len() as f32 - expected).abs() < 100.0, "{in_rate}: {} samples", out.len());
            let level = rms(&out[1000..]);
            assert!((level - 0.7071).abs() < 0.02, "{in_rate}: passband rms {level}");

            // A 9 kHz tone would alias to 3 kHz at 12 kHz; it must be strongly attenuated.
            let mut r = Resampler::new(in_rate, 12_000);
            let mut out = Vec::new();
            r.process(&tone(in_rate, 9000.0, 1.0), &mut out);
            let alias = rms(&out[500..]);
            assert!(alias < 0.01, "{in_rate}: alias rms {alias}");
        }
    }

    #[test]
    fn decodes_ft8_after_resampling_from_48k() {
        use cqdj_engine::*;
        let tx = build_tx_audio(&[TxSignal::new("CQ KN0BLE EL98", 1234.0)], 48_000, 0.5).unwrap();
        let mut slot48 = vec![0.0f32; 15 * 48_000];
        let start = 24_000; // +0.5 s
        slot48[start..start + tx.len()].copy_from_slice(&tx);
        let mut r = Resampler::new(48_000, 12_000);
        let mut slot12 = Vec::new();
        r.process(&slot48, &mut slot12);
        let d = decode(&slot12, 12_000, DecodeOptions::default()).unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].text, "CQ KN0BLE EL98");
        assert!((d[0].freq_hz - 1234.0).abs() < 4.0);
    }
}
