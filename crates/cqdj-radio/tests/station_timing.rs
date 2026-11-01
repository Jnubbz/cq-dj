//! Runs the real station loop for about 45 seconds against a fake sound card and radio,
//! and checks FT8 timing: when PTT goes on and off, when audio starts, and that the
//! receive slot is decoded. Real time, so it's ignored by default:
//!     cargo test --release -p cqdj-radio -- --include-ignored

use cqdj_engine::{build_tx_audio, TxSignal};
use cqdj_radio::audio::AudioIo;
use cqdj_radio::cat::{CatError, Rig};
use cqdj_radio::clock::{self, Parity};
use cqdj_radio::{Command, Event, Station, StationConfig};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Log = Arc<Mutex<Vec<(f64, String)>>>;

fn note(log: &Log, what: impl Into<String>) {
    log.lock().unwrap().push((clock::now_secs(), what.into()));
}

struct FakeRig(Log);
impl Rig for FakeRig {
    fn name(&self) -> &str {
        "fake rig"
    }
    fn set_data_mode(&mut self) -> Result<(), CatError> {
        note(&self.0, "mode");
        Ok(())
    }
    fn set_frequency(&mut self, hz: u64) -> Result<(), CatError> {
        note(&self.0, format!("freq {hz}"));
        Ok(())
    }
    fn frequency(&mut self) -> Result<u64, CatError> {
        Ok(14_074_000)
    }
    fn set_ptt(&mut self, on: bool) -> Result<(), CatError> {
        note(&self.0, if on { "ptt on" } else { "ptt off" });
        Ok(())
    }
}

/// Plays into a log; "receives" a station calling us in every slot.
struct FakeAudio {
    log: Log,
    playing_until: Mutex<f64>,
    incoming: Vec<f32>,
}
impl AudioIo for FakeAudio {
    fn output_rate(&self) -> u32 {
        12_000
    }
    fn play(&self, samples: Vec<f32>) {
        note(&self.log, format!("play {}", samples.len()));
        *self.playing_until.lock().unwrap() = clock::now_secs() + samples.len() as f64 / 12_000.0;
    }
    fn stop(&self) {
        *self.playing_until.lock().unwrap() = 0.0;
    }
    fn is_playing(&self) -> bool {
        clock::now_secs() < *self.playing_until.lock().unwrap()
    }
    fn rx_since(&self, start: f64, max_secs: f64) -> Vec<f32> {
        let n = (((clock::now_secs() - start).min(max_secs)) * 12_000.0) as usize;
        self.incoming[..n.min(self.incoming.len())].to_vec()
    }
    fn rx_dbfs(&self) -> f32 {
        -30.0
    }
    fn rx_latest(&self, n: usize) -> Vec<f32> {
        self.incoming[..n.min(self.incoming.len())].to_vec()
    }
    fn device_names(&self) -> (String, String) {
        ("fake in".into(), "fake out".into())
    }
}

#[test]
#[ignore = "real time, about 45 seconds"]
fn transmits_on_time_and_decodes_the_other_slot() {
    // A slot's worth of audio with JA1QRX answering our CQ at 1200 Hz.
    let call = build_tx_audio(&[TxSignal::new("KN0BLE JA1QRX PM95", 1200.0)], 12_000, 0.5).unwrap();
    let mut incoming = vec![0.0f32; 15 * 12_000];
    incoming[6_000..6_000 + call.len()].copy_from_slice(&call);

    let rig_log: Log = Default::default();
    let audio_log: Log = Default::default();
    let events: Arc<Mutex<Vec<Event>>> = Default::default();

    // Transmit in the next slot.
    let now = clock::now_secs();
    let tx_slot = clock::slot_start(now) + 15.0;
    let all_txt = std::env::temp_dir().join(format!("cqdj-all-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&all_txt);
    let config = StationConfig {
        all_txt_path: Some(all_txt.clone()),
        callsign: "KN0BLE".into(),
        grid: "EL98".into(),
        tx_parity: Parity::of_slot(tx_slot),
        ..Default::default()
    };

    let (rl, al, ev) = (rig_log.clone(), audio_log.clone(), events.clone());
    let station = Station::start_with(
        config,
        move || {
            let audio = FakeAudio { log: al, playing_until: Mutex::new(0.0), incoming };
            Ok((audio, Box::new(FakeRig(rl)) as Box<dyn Rig>))
        },
        move |e| ev.lock().unwrap().push(e),
        false,
    )
    .unwrap();
    station.send(Command::SetTx { enabled: true, message: "CQ KN0BLE EL98".into() });

    // Wait through the transmit slot and the following receive slot's decode.
    let rx_slot = tx_slot + 15.0;
    while clock::now_secs() < rx_slot + 14.5 {
        std::thread::sleep(Duration::from_millis(200));
    }
    station.send(Command::SetTx { enabled: false, message: String::new() });
    drop(station);

    let rig = rig_log.lock().unwrap().clone();
    let audio = audio_log.lock().unwrap().clone();
    let at = |log: &[(f64, String)], what: &str, after: f64| log.iter().find(|(t, w)| w.starts_with(what) && *t >= after).map(|(t, w)| (*t, w.clone()));

    assert!(rig.iter().any(|(_, w)| w == "mode") && rig.iter().any(|(_, w)| w == "freq 14074000"), "radio not set up: {rig:?}");

    let (ptt_on, _) = at(&rig, "ptt on", tx_slot).expect("never keyed");
    let (play, what) = at(&audio, "play", tx_slot).expect("never played");
    let (ptt_off, _) = at(&rig, "ptt off", ptt_on).expect("never unkeyed");
    println!("PTT on +{:.3} s, audio +{:.3} s ({what}), PTT off +{:.3} s", ptt_on - tx_slot, play - tx_slot, ptt_off - tx_slot);

    assert!((0.15..0.45).contains(&(ptt_on - tx_slot)), "PTT on at +{:.3}", ptt_on - tx_slot);
    assert!((0.45..0.60).contains(&(play - tx_slot)), "audio at +{:.3}", play - tx_slot);
    assert_eq!(what, format!("play {}", (79.0f64 * 0.16 * 12_000.0).round() as usize));
    assert!((13.1..13.8).contains(&(ptt_off - tx_slot)), "PTT off at +{:.3}", ptt_off - tx_slot);
    // Keyed exactly once in the transmit slot, never in the receive slot.
    assert_eq!(rig.iter().filter(|(t, w)| w == "ptt on" && *t >= tx_slot && *t < rx_slot + 15.0).count(), 1);

    let events = events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e, Event::Transmit { message, .. } if message == "CQ KN0BLE EL98")));
    let tx_label = clock::utc_label(tx_slot);
    let rx_label = clock::utc_label(rx_slot);
    let decodes_for = |label: &str| events.iter().filter_map(|e| match e {
        Event::Decodes { utc, lines, .. } if utc == label => Some(lines.clone()),
        _ => None,
    }).next();
    assert!(decodes_for(&tx_label).is_none(), "decoded our own transmit slot");
    let lines = decodes_for(&rx_label).expect("receive slot was not decoded");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0].text, "KN0BLE JA1QRX PM95");
    assert!(lines[0].to_me && !lines[0].is_cq);
    assert!(lines[0].dt.abs() < 0.2, "dt {}", lines[0].dt);
    let all = std::fs::read_to_string(&all_txt).expect("ALL.TXT not written");
    let all_lines: Vec<&str> = all.lines().collect();
    println!("{all}");
    let stamp = |t: f64| {
        let (y, mo, d, h, mi, s) = cqdj_radio::logbook::utc_parts(t as i64);
        format!("{:02}{:02}{:02}_{:02}{:02}{:02}", y % 100, mo, d, h, mi, s)
    };
    let tx_line = format!("{}    14.074 Tx FT8      0  0.0 1500 CQ KN0BLE EL98", stamp(tx_slot));
    let rx_line = format!("{}    14.074 Rx FT8 ", stamp(rx_slot));
    assert!(all_lines.contains(&tx_line.as_str()), "missing {tx_line}");
    assert!(all_lines.iter().any(|l| l.starts_with(&rx_line) && l.ends_with(" 1200 KN0BLE JA1QRX PM95")), "missing rx line");
    assert!(!all_lines.iter().any(|l| l.starts_with(&format!("{} ", stamp(tx_slot))) && l.contains(" Rx ")), "decoded own TX slot");
    let _ = std::fs::remove_file(&all_txt);
    let lines_per_sec = events.iter().filter(|e| matches!(e, Event::Spectrum { .. })).count() as f64 / 40.0;
    assert!(lines_per_sec > 3.0, "waterfall only {lines_per_sec:.1} lines/s");
}
