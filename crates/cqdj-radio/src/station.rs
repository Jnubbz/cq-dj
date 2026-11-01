//! The station loop: keeps time with FT8 slots, decodes each receive slot, and keys the radio
//! and plays audio for each transmit slot. Runs on its own thread; talk to it with [`Command`]s
//! and hear back through [`Event`]s.

use crate::audio::{Audio, AudioIo, RX_RATE};
use crate::cat::{self, NoRig, Rig};
use crate::clock::{self, Parity};
use cqdj_engine::{build_tx_audio, decode, DecodeOptions, TxSignal};
use serde::{Deserialize, Serialize};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

/// Decode this far into the slot: FT8 transmissions end at 13.14 s; leave room for late stations.
const DECODE_AT: f64 = 13.7;
/// Key the radio this far into the slot, before audio starts at 0.5 s.
const PTT_AT: f64 = 0.2;
/// Never stay keyed longer than this (a stuck transmitter is the worst failure).
const MAX_KEYED_SECONDS: f64 = 14.5;
/// The tune carrier shuts off by itself after this long.
const TUNE_SECONDS: f64 = 10.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StationConfig {
    pub callsign: String,
    pub grid: String,
    pub input_device: String,
    pub output_device: String,
    /// Serial port for CAT, or None to key with VOX and tune by hand.
    pub cat_port: Option<String>,
    pub cat_baud: u32,
    /// Dial frequency in Hz (USB), e.g. 14_074_000.
    pub dial_hz: u64,
    /// Audio frequency for our transmit signal, in Hz.
    pub tx_audio_hz: f32,
    /// Transmit audio level, 0.0-1.0. Turn it down until ALC reads zero.
    pub tx_level: f32,
    /// Which slots we transmit in.
    pub tx_parity: Parity,
    /// Digital gain applied to receive audio before decoding, in dB (0-60).
    pub rx_gain_db: f32,
}

impl Default for StationConfig {
    fn default() -> Self {
        StationConfig {
            callsign: String::new(),
            grid: String::new(),
            input_device: String::new(),
            output_device: String::new(),
            cat_port: None,
            cat_baud: 38_400,
            dial_hz: 14_074_000,
            tx_audio_hz: 1500.0,
            tx_level: 0.3,
            tx_parity: Parity::Even,
            rx_gain_db: 0.0,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Command {
    /// Arm or disarm transmitting `message` every transmit slot.
    SetTx { enabled: bool, message: String },
    /// Start or stop a steady tune carrier (for setting audio level / checking ALC).
    Tune(bool),
    SetDial(u64),
    SetTxAudioHz(f32),
    SetTxLevel(f32),
    SetParity(Parity),
    SetRxGain(f32),
    Stop,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecodeLine {
    pub utc: String,
    pub snr: i32,
    pub dt: f32,
    pub freq: f32,
    pub text: String,
    /// Addressed to our callsign.
    pub to_me: bool,
    pub is_cq: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Started { input: String, output: String, rig: String, output_rate: u32 },
    Clock { offset_secs: Option<f64>, corrected: bool, message: String },
    Status {
        utc: String,
        seconds_into_slot: f64,
        slot_parity: Parity,
        /// Receive level after gain, dBFS.
        rx_dbfs: f32,
        /// Receive level straight from the sound card, dBFS.
        rx_raw_dbfs: f32,
        rx_gain_db: f32,
        transmitting: bool,
        tuning: bool,
        tx_armed: bool,
        tx_message: String,
        dial_hz: u64,
    },
    Decodes { utc: String, parity: Parity, lines: Vec<DecodeLine> },
    /// A waterfall line, about 5 per second.
    Spectrum { line: crate::spectrum::SpectrumLine, transmitting: bool, slot_start: bool },
    Transmit { utc: String, message: String, freq: f32 },
    Log { level: String, message: String },
    Stopped { reason: String },
}

pub struct Station {
    commands: Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl Station {
    /// Opens the sound card and radio and starts the loop. Returns once both are open.
    pub fn start<F>(config: StationConfig, on_event: F) -> Result<Station, String>
    where
        F: Fn(Event) + Send + Sync + 'static,
    {
        let opener_cfg = config.clone();
        let station = Station::start_with(
            config,
            move || {
                let audio = Audio::open(&opener_cfg.input_device, &opener_cfg.output_device).map_err(|e| e.to_string())?;
                let rig = open_rig(&opener_cfg)?;
                Ok((audio, rig))
            },
            on_event,
            true,
        )?;
        Ok(station)
    }

    /// Like [`Station::start`] but with any sound card and radio (tests use fakes).
    /// `open` runs on the station thread, so the audio device never has to cross threads.
    pub fn start_with<A, O, F>(config: StationConfig, open: O, on_event: F, check_clock: bool) -> Result<Station, String>
    where
        A: AudioIo + 'static,
        O: FnOnce() -> Result<(A, Box<dyn Rig>), String> + Send + 'static,
        F: Fn(Event) + Send + Sync + 'static,
    {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let on_event = std::sync::Arc::new(on_event);
        let ev = on_event.clone();
        let thread = std::thread::Builder::new()
            .name("cqdj-station".into())
            .spawn(move || match open() {
                Ok((audio, rig)) => {
                    let _ = ready_tx.send(Ok(()));
                    run(config, audio, rig, cmd_rx, move |e| ev(e))
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => return Err("timed out opening the radio".into()),
        }
        if check_clock {
            check_clock_in_background(on_event);
        }
        Ok(Station { commands: cmd_tx, thread: Some(thread) })
    }

    pub fn send(&self, cmd: Command) {
        let _ = self.commands.send(cmd);
    }
}

impl Drop for Station {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn check_clock_in_background(on_event: std::sync::Arc<dyn Fn(Event) + Send + Sync>) {
    std::thread::spawn(move || {
        let servers = ["time.apple.com", "pool.ntp.org", "time.windows.com", "time.google.com"];
        for server in servers {
            if let Ok(offset) = clock::measure_clock_offset(server, Duration::from_secs(2)) {
                let ms = offset * 1000.0;
                let (corrected, message) = if offset.abs() < 0.05 {
                    clock::set_correction(offset);
                    (true, format!("Clock is good ({ms:+.0} ms vs {server})."))
                } else if offset.abs() < 60.0 {
                    clock::set_correction(offset);
                    (true, format!("Your computer clock is {ms:+.0} ms off ({server}). CQ DJ is correcting for it."))
                } else {
                    (false, format!("Your computer clock is {offset:+.0} s off. Fix it in System Settings > Date & Time; FT8 won't work until you do."))
                };
                on_event(Event::Clock { offset_secs: Some(offset), corrected, message });
                return;
            }
        }
        on_event(Event::Clock {
            offset_secs: None,
            corrected: false,
            message: "Couldn't reach a time server; make sure your clock is set automatically.".into(),
        });
    });
}

fn log(on_event: &impl Fn(Event), level: &str, message: impl Into<String>) {
    on_event(Event::Log { level: level.into(), message: message.into() });
}

fn open_rig(config: &StationConfig) -> Result<Box<dyn Rig>, String> {
    match &config.cat_port {
        None => Ok(Box::new(NoRig)),
        Some(port) => {
            let mut rig = cat::open_yaesu(port, config.cat_baud).map_err(|e| e.to_string())?;
            rig.set_ptt(false).map_err(|e| e.to_string())?;
            Ok(Box::new(rig))
        }
    }
}

struct TxState {
    armed: bool,
    message: String,
    keyed_at: Option<f64>,
    tx_slot: Option<f64>,
    tune_until: Option<f64>,
}

fn run<A: AudioIo>(config: StationConfig, audio: A, mut rig: Box<dyn Rig>, commands: Receiver<Command>, on_event: impl Fn(Event)) {
    let mut config = config;
    let (input, output) = audio.device_names();
    let out_rate = audio.output_rate();
    on_event(Event::Started { input, output, rig: rig.name().to_string(), output_rate: out_rate });

    // Mode first, then frequency (an FT-710 shifts frequency when the mode changes).
    let set_dial = |rig: &mut Box<dyn Rig>, hz: u64| -> Result<(), String> {
        rig.set_data_mode().map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(100));
        rig.set_frequency(hz).map_err(|e| e.to_string())
    };
    if let Err(e) = set_dial(&mut rig, config.dial_hz) {
        log(&on_event, "error", format!("Couldn't set the radio to {:.3} MHz DATA-U: {e}", config.dial_hz as f64 / 1e6));
    }

    let mut tx = TxState { armed: false, message: String::new(), keyed_at: None, tx_slot: None, tune_until: None };
    let mut decoded_slot = -1.0f64;
    let mut handled_tx_slot = -1.0f64;
    let mut last_status = 0.0f64;
    let mut last_spectrum = 0.0f64;
    let mut spectrum_slot = -1.0f64;
    let spectrum = crate::spectrum::Spectrum::new(RX_RATE);
    let (decode_done_tx, decode_done_rx) = mpsc::channel::<Event>();

    let unkey = |rig: &mut Box<dyn Rig>, tx: &mut TxState, audio: &A| {
        audio.stop();
        if tx.keyed_at.is_some() {
            if let Err(e) = rig.set_ptt(false) {
                eprintln!("PTT off failed: {e}");
            }
        }
        tx.keyed_at = None;
        tx.tune_until = None;
    };

    let stop_reason = loop {
        // --- commands ---
        let mut stop = None;
        while let Ok(cmd) = commands.try_recv() {
            match cmd {
                Command::Stop => stop = Some("stopped".to_string()),
                Command::SetTx { enabled, message } => {
                    if enabled {
                        match cqdj_engine::encode_tones(&message) {
                            Ok(_) => {
                                tx.armed = true;
                                tx.message = message.trim().to_ascii_uppercase();
                            }
                            Err(e) => log(&on_event, "error", e.to_string()),
                        }
                    } else {
                        tx.armed = false;
                        // Stop a transmission in progress right away, like WSJT-X's Halt Tx.
                        if tx.tx_slot.is_some() {
                            unkey(&mut rig, &mut tx, &audio);
                            tx.tx_slot = None;
                            log(&on_event, "info", "Transmit stopped.");
                        }
                    }
                }
                Command::Tune(on) => {
                    if on && tx.keyed_at.is_none() {
                        let n = (TUNE_SECONDS * out_rate as f64) as usize;
                        let w = 2.0 * std::f32::consts::PI * config.tx_audio_hz / out_rate as f32;
                        let level = config.tx_level.clamp(0.0, 1.0);
                        let tone: Vec<f32> = (0..n).map(|i| level * (w * i as f32).sin()).collect();
                        match rig.set_ptt(true) {
                            Ok(()) => {
                                let now = clock::now_secs();
                                tx.keyed_at = Some(now);
                                tx.tune_until = Some(now + TUNE_SECONDS);
                                audio.play(tone);
                                log(&on_event, "info", format!("Tune: steady tone at {:.0} Hz. Watch ALC and power.", config.tx_audio_hz));
                            }
                            Err(e) => log(&on_event, "error", format!("Couldn't key the radio: {e}")),
                        }
                    } else if !on && tx.tune_until.is_some() {
                        unkey(&mut rig, &mut tx, &audio);
                    }
                }
                Command::SetDial(hz) => {
                    if tx.keyed_at.is_some() {
                        log(&on_event, "warn", "Can't change frequency while transmitting.");
                    } else {
                        config.dial_hz = hz;
                        match set_dial(&mut rig, hz) {
                            Ok(()) => log(&on_event, "info", format!("Radio set to {:.3} MHz DATA-U.", hz as f64 / 1e6)),
                            Err(e) => log(&on_event, "error", format!("Couldn't change frequency: {e}")),
                        }
                    }
                }
                Command::SetTxAudioHz(hz) => config.tx_audio_hz = hz.clamp(cqdj_engine::MIN_AUDIO_HZ, cqdj_engine::MAX_AUDIO_HZ),
                Command::SetTxLevel(l) => config.tx_level = l.clamp(0.0, 1.0),
                Command::SetParity(p) => config.tx_parity = p,
                Command::SetRxGain(db) => config.rx_gain_db = db.clamp(0.0, 60.0),
            }
        }
        if let Some(reason) = stop {
            break reason;
        }

        let now = clock::now_secs();
        let slot = clock::slot_start(now);
        let into = now - slot;
        let parity = Parity::of_slot(slot);

        // --- safety: tune timeout and stuck-transmitter watchdog ---
        if let Some(until) = tx.tune_until {
            if now >= until {
                unkey(&mut rig, &mut tx, &audio);
                log(&on_event, "info", "Tune finished.");
            }
        }
        if let Some(at) = tx.keyed_at {
            if now - at > MAX_KEYED_SECONDS && tx.tune_until.is_none() {
                unkey(&mut rig, &mut tx, &audio);
                tx.tx_slot = None;
                log(&on_event, "warn", "Transmit watchdog: unkeyed the radio.");
            }
        }

        // --- transmit ---
        let tuning = tx.tune_until.is_some();
        if tx.armed && !tuning && parity == config.tx_parity && slot != handled_tx_slot && into >= PTT_AT && into < 1.5 {
            handled_tx_slot = slot;
            let signal = TxSignal::new(tx.message.clone(), config.tx_audio_hz);
            match build_tx_audio(&[signal], out_rate, config.tx_level) {
                Ok(mut samples) => {
                    // Starting a little late? Skip the part of the message we already missed.
                    let late = (into - cqdj_engine::FT8_TX_START_SECONDS as f64).max(0.0);
                    let skip = (late * out_rate as f64) as usize;
                    if skip > 0 {
                        samples.drain(..skip.min(samples.len()));
                    }
                    match rig.set_ptt(true) {
                        Ok(()) => {
                            tx.keyed_at = Some(now);
                            tx.tx_slot = Some(slot);
                            let start = slot + cqdj_engine::FT8_TX_START_SECONDS as f64;
                            let wait = start - clock::now_secs();
                            if wait > 0.0 {
                                std::thread::sleep(Duration::from_secs_f64(wait));
                            }
                            audio.play(samples);
                            on_event(Event::Transmit { utc: clock::utc_label(slot), message: tx.message.clone(), freq: config.tx_audio_hz });
                        }
                        Err(e) => log(&on_event, "error", format!("Couldn't key the radio: {e}")),
                    }
                }
                Err(e) => log(&on_event, "error", e.to_string()),
            }
        }
        // Transmission finished: unkey.
        if tx.tx_slot.is_some() && tx.keyed_at.is_some() && !audio.is_playing() && clock::now_secs() - slot > 1.0 {
            std::thread::sleep(Duration::from_millis(50)); // let the last audio buffer drain
            unkey(&mut rig, &mut tx, &audio);
            tx.tx_slot = None;
        }

        // --- receive ---
        if into >= DECODE_AT && slot != decoded_slot {
            decoded_slot = slot;
            if handled_tx_slot != slot {
                let mut samples = audio.rx_since(slot, 15.0);
                let gain = 10f32.powf(config.rx_gain_db / 20.0);
                if gain != 1.0 {
                    for s in &mut samples {
                        *s *= gain;
                    }
                }
                let mycall = config.callsign.trim().to_ascii_uppercase();
                let done = decode_done_tx.clone();
                std::thread::spawn(move || {
                    if samples.len() < (10 * RX_RATE) as usize {
                        return; // not enough audio yet (just started)
                    }
                    let utc = clock::utc_label(slot);
                    let lines = match decode(&samples, RX_RATE, DecodeOptions::default()) {
                        Ok(ds) => ds
                            .into_iter()
                            .map(|d| {
                                let first = d.text.split_whitespace().next().unwrap_or("");
                                DecodeLine {
                                    utc: utc.clone(),
                                    snr: d.snr_db,
                                    dt: d.dt_sec,
                                    freq: d.freq_hz,
                                    to_me: !mycall.is_empty() && first == mycall,
                                    is_cq: first == "CQ",
                                    text: d.text,
                                }
                            })
                            .collect(),
                        Err(_) => Vec::new(),
                    };
                    let _ = done.send(Event::Decodes { utc, parity: Parity::of_slot(slot), lines });
                });
            }
        }
        while let Ok(e) = decode_done_rx.try_recv() {
            on_event(e);
        }

        // --- waterfall, 5 lines a second ---
        if now - last_spectrum >= 0.2 {
            last_spectrum = now;
            let samples = audio.rx_latest(crate::spectrum::FFT_LEN);
            if let Some(line) = spectrum.line(&samples, config.rx_gain_db) {
                let slot_start = spectrum_slot != slot;
                spectrum_slot = slot;
                on_event(Event::Spectrum { line, transmitting: tx.keyed_at.is_some(), slot_start });
            }
        }

        // --- status for the UI, 4 times a second ---
        if now - last_status >= 0.25 {
            last_status = now;
            let rx_raw_dbfs = audio.rx_dbfs();
            let rx_dbfs = if rx_raw_dbfs <= -119.0 { rx_raw_dbfs } else { rx_raw_dbfs + config.rx_gain_db };
            on_event(Event::Status {
                utc: clock::utc_label(now),
                seconds_into_slot: into,
                slot_parity: parity,
                rx_dbfs,
                rx_raw_dbfs,
                rx_gain_db: config.rx_gain_db,
                transmitting: tx.keyed_at.is_some(),
                tuning: tx.tune_until.is_some(),
                tx_armed: tx.armed,
                tx_message: tx.message.clone(),
                dial_hz: config.dial_hz,
            });
        }

        std::thread::sleep(Duration::from_millis(20));
    };

    unkey(&mut rig, &mut tx, &audio);
    on_event(Event::Stopped { reason: stop_reason });
}
