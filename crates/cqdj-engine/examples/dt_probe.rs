use cqdj_engine::*;
fn main() {
    let audio = build_tx_audio(&[TxSignal::new("CQ KN0BLE EL98", 1500.0)], SAMPLE_RATE, 1.0).unwrap();
    for dt_ms in [-400i32, -200, -100, -50, 0, 30, 60, 100, 200, 500, 1000, 2000] {
        let mut slot = vec![0.0f32; 15 * SAMPLE_RATE as usize];
        let start = (500 + dt_ms) as usize * SAMPLE_RATE as usize / 1000;
        for (i, s) in audio.iter().enumerate() { if start + i < slot.len() { slot[start + i] = *s; } }
        let d = decode(&slot, SAMPLE_RATE, DecodeOptions::default()).unwrap();
        println!("{dt_ms:5} ms -> {:?}", d.iter().map(|d| (d.dt_sec, d.freq_hz)).collect::<Vec<_>>());
    }
}
