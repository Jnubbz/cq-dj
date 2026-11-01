//! End-to-end tests: build transmit audio, run it through a simulated channel, decode it.

use cqdj_engine::*;

const SR: u32 = SAMPLE_RATE;

/// Small deterministic RNG (xorshift64*) so tests need no extra dependencies.
struct Rng(u64);
impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Standard normal sample (Box-Muller).
    fn gauss(&mut self) -> f32 {
        let u1 = self.next_f64().max(1e-12);
        let u2 = self.next_f64();
        ((-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()) as f32
    }
}

/// A 15-second slot with `tx` placed at the normal +0.5 s start (plus `extra_dt`).
fn slot_with(tx: &[f32], extra_dt: f32) -> Vec<f32> {
    let mut slot = vec![0.0f32; (FT8_SLOT_SECONDS * SR as f32) as usize];
    let start = ((FT8_TX_START_SECONDS + extra_dt) * SR as f32) as usize;
    for (i, s) in tx.iter().enumerate() {
        if start + i < slot.len() {
            slot[start + i] += s;
        }
    }
    slot
}

/// Adds white Gaussian noise so that a full-scale single tone of amplitude `amp`
/// has the given SNR in a 2500 Hz bandwidth (the WSJT-X convention).
fn add_noise(slot: &mut [f32], amp: f32, snr_db: f32, seed: u64) {
    // Signal power = amp^2 / 2. Noise power in 2500 Hz = sigma^2 * 2500 / (SR / 2).
    let sig_power = amp * amp / 2.0;
    let noise_2500 = sig_power / 10f32.powf(snr_db / 10.0);
    let sigma = (noise_2500 * (SR as f32 / 2.0) / 2500.0).sqrt();
    let mut rng = Rng(seed);
    for s in slot.iter_mut() {
        *s += sigma * rng.gauss();
    }
}

fn texts(decodes: &[Decode]) -> Vec<String> {
    decodes.iter().map(|d| d.text.clone()).collect()
}

#[test]
fn rejects_bad_messages() {
    assert!(encode_tones("CQ KN0BLE EL98").is_ok());
    assert!(encode_tones("this is way too long for ft8 free text").is_err());
    assert!(matches!(build_tx_audio(&[], SR, 1.0), Err(EngineError::NothingToSend)));
}

#[test]
fn single_signal_round_trip() {
    let audio = build_tx_audio(&[TxSignal::new("CQ KN0BLE EL98", 1500.0)], SR, 1.0).unwrap();
    assert_eq!(audio.len(), (79.0 * FT8_SYMBOL_PERIOD * SR as f32).round() as usize);
    let decodes = decode(&slot_with(&audio, 0.0), SR, DecodeOptions::default()).unwrap();
    assert_eq!(texts(&decodes), vec!["CQ KN0BLE EL98"]);
    let d = &decodes[0];
    assert!((d.freq_hz - 1500.0).abs() < 4.0, "freq {}", d.freq_hz);
    assert!(d.dt_sec.abs() < 0.1, "dt {}", d.dt_sec);
}

#[test]
fn every_message_type_in_a_qso_round_trips() {
    let qso = [
        "CQ KN0BLE EL98",
        "KN0BLE JA1QRX PM95",
        "JA1QRX KN0BLE -14",
        "KN0BLE JA1QRX R-09",
        "JA1QRX KN0BLE RR73",
        "KN0BLE JA1QRX 73",
        "CQ POTA KN0BLE EL98",
    ];
    for msg in qso {
        let audio = build_tx_audio(&[TxSignal::new(msg, 1200.0)], SR, 1.0).unwrap();
        let decodes = decode(&slot_with(&audio, 0.0), SR, DecodeOptions::default()).unwrap();
        assert_eq!(texts(&decodes), vec![msg.to_string()], "message {msg}");
    }
}

#[test]
fn five_simultaneous_qsos_all_decode() {
    let signals = vec![
        TxSignal::new("JA1QRX KN0BLE -14", 600.0),
        TxSignal::new("EA8DJX KN0BLE RR73", 1000.0),
        TxSignal::new("W7KZT KN0BLE EL98", 1400.0),
        TxSignal::new("ZL2BNH KN0BLE EL98", 1800.0),
        TxSignal::new("OH6KTW KN0BLE R-12", 2200.0),
    ];
    let audio = build_tx_audio(&signals, SR, 1.0).unwrap();
    let peak = audio.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak <= 1.0 + 1e-4, "mixed audio clips: peak {peak}");

    let decodes = decode(&slot_with(&audio, 0.0), SR, DecodeOptions::default()).unwrap();
    let got = texts(&decodes);
    for s in &signals {
        assert!(got.contains(&s.message), "missing {} in {:?}", s.message, got);
        let d = decodes.iter().find(|d| d.text == s.message).unwrap();
        assert!((d.freq_hz - s.freq_hz).abs() < 4.0, "{} at {} Hz", s.message, d.freq_hz);
    }
    assert_eq!(got.len(), signals.len());
}

#[test]
fn three_slots_decode_through_noise() {
    // Three streams, each 9.5 dB down from full power, into noise: each lands around -10 dB.
    let signals = vec![
        TxSignal::new("JA1QRX KN0BLE -14", 800.0),
        TxSignal::new("EA8DJX KN0BLE RR73", 1300.0),
        TxSignal::new("W7KZT KN0BLE EL98", 1900.0),
    ];
    let audio = build_tx_audio(&signals, SR, 1.0).unwrap();
    let mut slot = slot_with(&audio, 0.15);
    add_noise(&mut slot, 1.0 / 3.0, -10.0, 42);
    let decodes = decode(&slot, SR, DecodeOptions::default()).unwrap();
    let got = texts(&decodes);
    for s in &signals {
        assert!(got.contains(&s.message), "missing {} in {:?}", s.message, got);
    }
    for d in &decodes {
        assert!((d.dt_sec - 0.15).abs() < 0.1, "dt {}", d.dt_sec);
    }
}

#[test]
fn snr_estimate_is_close() {
    for (i, snr) in [-18.0f32, -12.0, -6.0, 0.0].iter().enumerate() {
        let audio = build_tx_audio(&[TxSignal::new("CQ KN0BLE EL98", 1500.0)], SR, 1.0).unwrap();
        let mut slot = slot_with(&audio, 0.0);
        add_noise(&mut slot, 1.0, *snr, 7 + i as u64);
        let decodes = decode(&slot, SR, DecodeOptions::default()).unwrap();
        assert_eq!(decodes.len(), 1, "at {snr} dB");
        let est = decodes[0].snr_db as f32;
        println!("true SNR {snr:+.0} dB -> estimated {est:+.0} dB");
        assert!((est - snr).abs() <= 3.0, "true {snr} dB, estimated {est} dB");
    }
}

#[test]
fn overlapping_or_out_of_band_signals_are_refused() {
    let overlap = build_tx_audio(&[TxSignal::new("CQ KN0BLE EL98", 1000.0), TxSignal::new("CQ KN0BLE EL98", 1030.0)], SR, 1.0);
    assert!(matches!(overlap, Err(EngineError::Overlap { .. })));
    let oob = build_tx_audio(&[TxSignal::new("CQ KN0BLE EL98", 3200.0)], SR, 1.0);
    assert!(matches!(oob, Err(EngineError::OutOfBand { .. })));
}

#[test]
fn power_split_matches_wsjtx_numbers() {
    // K1JT & K9AN: 2, 3, 4, 5 streams attenuate each by 6, 9.5, 12, 14 dB.
    let expected = [(1, 0.0), (2, 6.0), (3, 9.5), (4, 12.0), (5, 14.0)];
    for (n, db) in expected {
        assert!((per_signal_loss_db(n) - db).abs() < 0.1, "n={n}: {}", per_signal_loss_db(n));
    }
}

#[test]
fn tones_match_ft8_lib_reference_generator() {
    // Output of ft8_lib's demo/gen_ft8 "CQ KN0BLE EL98" (same tones WSJT-X sends).
    let expected = "3140652000000001115043266605042520743140652620111374134055532333244157273140652";
    let tones: String = encode_tones("CQ KN0BLE EL98").unwrap().iter().map(|t| t.to_string()).collect();
    assert_eq!(tones, expected);
}
