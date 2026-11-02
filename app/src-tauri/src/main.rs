// CQ DJ desktop app: a thin Tauri shell around the cqdj-radio station loop.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use cqdj_radio::{Command, Parity, Station, StationConfig};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};
use tauri_plugin_updater::UpdaterExt;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Settings {
    #[serde(default)]
    station: StationConfig,
    /// True once the user has finished first-time setup.
    #[serde(default)]
    setup_done: bool,
    #[serde(default)]
    log: LogSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct LogSettings {
    /// QRZ logbook API key (QRZ: My Logbook > Settings).
    qrz_api_key: String,
    qrz_upload: bool,
    /// Log finished QSOs without asking.
    auto_log: bool,
    tx_power_w: Option<u32>,
}

struct AppState {
    station: Mutex<Option<Station>>,
    settings_path: PathBuf,
    /// Where ALL.TXT and the ADIF log live.
    data_dir: PathBuf,
    /// The update found by the last check. Installing uses exactly this one, because a second check
    /// right after a release can hit a stale GitHub cache and report "up to date".
    pending_update: Mutex<Option<tauri_plugin_updater::Update>>,
}

fn load(path: &PathBuf) -> Settings {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Settings {
    load(&state.settings_path)
}

#[tauri::command]
fn save_settings(state: State<AppState>, settings: Settings) -> Result<(), String> {
    if let Some(dir) = state.settings_path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&state.settings_path, json).map_err(|e| e.to_string())
}

#[tauri::command]
fn list_audio() -> cqdj_radio::AudioDeviceList {
    cqdj_radio::list_devices()
}

#[tauri::command]
fn list_ports() -> Vec<String> {
    cqdj_radio::list_serial_ports()
}

#[derive(Serialize)]
struct FoundRadio {
    port: String,
    baud: u32,
}

/// Looks for a Yaesu radio on every serial port. Slow-ish (a few seconds), so it runs off the UI thread.
#[tauri::command]
async fn find_radio(state: State<'_, AppState>) -> Result<Option<FoundRadio>, String> {
    if state.station.lock().unwrap().is_some() {
        return Err("Stop the radio first.".into());
    }
    let found = tauri::async_runtime::spawn_blocking(cqdj_radio::find_yaesu).await.map_err(|e| e.to_string())?;
    Ok(found.map(|(port, baud)| FoundRadio { port, baud }))
}

#[tauri::command]
async fn start_station(app: tauri::AppHandle, state: State<'_, AppState>, config: StationConfig) -> Result<(), String> {
    let mut config = config;
    config.all_txt_path = Some(state.data_dir.join("ALL.TXT"));
    let mut slot = state.station.lock().unwrap();
    if slot.is_some() {
        return Ok(());
    }
    let handle = app.clone();
    let station = Station::start(config, move |event| {
        let _ = handle.emit("station", &event);
    })?;
    *slot = Some(station);
    Ok(())
}

#[tauri::command]
fn stop_station(state: State<AppState>) {
    // Dropping the station unkeys the radio and closes the sound card.
    let station = state.station.lock().unwrap().take();
    drop(station);
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum UiCommand {
    Tx { enabled: bool, message: String },
    Tune { on: bool },
    Dial { hz: u64 },
    TxHz { hz: f32 },
    TxLevel { level: f32 },
    Parity { parity: Parity },
    RxGain { db: f32 },
}

#[tauri::command]
fn station_command(state: State<AppState>, command: UiCommand) -> Result<(), String> {
    let guard = state.station.lock().unwrap();
    let station = guard.as_ref().ok_or("The radio isn't running.")?;
    station.send(match command {
        UiCommand::Tx { enabled, message } => Command::SetTx { enabled, message },
        UiCommand::Tune { on } => Command::Tune(on),
        UiCommand::Dial { hz } => Command::SetDial(hz),
        UiCommand::TxHz { hz } => Command::SetTxAudioHz(hz),
        UiCommand::TxLevel { level } => Command::SetTxLevel(level),
        UiCommand::Parity { parity } => Command::SetParity(parity),
        UiCommand::RxGain { db } => Command::SetRxGain(db),
    });
    Ok(())
}

/// Checks that a message is something FT8 can send; returns a friendly error if not.
#[tauri::command]
fn check_message(message: String) -> Result<(), String> {
    cqdj_engine::encode_tones(&message).map(|_| ()).map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct UpdateInfo {
    version: String,
    current: String,
    notes: Option<String>,
}

#[tauri::command]
fn app_version(app: tauri::AppHandle) -> String {
    app.package_info().version.to_string()
}

/// Asks GitHub whether a newer release exists.
#[tauri::command]
async fn check_update(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Option<UpdateInfo>, String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater.check().await.map_err(|e| format!("Couldn't check for updates: {e}"))?;
    let mut pending = state.pending_update.lock().unwrap();
    match update {
        Some(u) => {
            let info = UpdateInfo { version: u.version.clone(), current: u.current_version.clone(), notes: u.body.clone() };
            *pending = Some(u);
            Ok(Some(info))
        }
        // A stale "no update" answer never cancels an update we already found.
        None => Ok(pending.as_ref().map(|u| UpdateInfo { version: u.version.clone(), current: u.current_version.clone(), notes: u.body.clone() })),
    }
}

#[derive(Clone, Serialize)]
struct UpdateProgress {
    downloaded: usize,
    total: Option<u64>,
}

/// Downloads the update, checks its signature, installs it and restarts CQ DJ.
/// The radio is stopped (and unkeyed) first.
#[tauri::command]
async fn install_update(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let station = state.station.lock().unwrap().take();
    drop(station);
    let found = state.pending_update.lock().unwrap().clone();
    let update = match found {
        Some(u) => u,
        None => {
            let updater = app.updater().map_err(|e| e.to_string())?;
            updater.check().await.map_err(|e| e.to_string())?.ok_or("You're already up to date.")?
        }
    };
    let mut downloaded = 0usize;
    let handle = app.clone();
    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk;
                let _ = handle.emit("update_progress", UpdateProgress { downloaded, total });
            },
            || {},
        )
        .await
        .map_err(|e| format!("Update failed: {e}"))?;
    app.restart();
}

// ---------- logging ----------

const ADIF_FILE: &str = "cqdj_log.adi";

#[derive(Serialize)]
struct LogResult {
    file: String,
    /// QRZ upload outcome, if an upload was attempted.
    qrz: Option<Result<cqdj_radio::logbook::QrzReply, String>>,
}

async fn qrz_call(app: &tauri::AppHandle, key: &str, action: &str, adif: Option<String>, my_call: &str) -> Result<cqdj_radio::logbook::QrzReply, String> {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    // QRZ asks for an identifiable User-Agent and rejects unknown parameters.
    let agent = format!("CQ-DJ/{} ({})", app.package_info().version, my_call);
    let client = reqwest::Client::builder()
        .user_agent(agent)
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let mut form = vec![("KEY", key.trim().to_string()), ("ACTION", action.to_string())];
    if let Some(a) = adif {
        form.push(("ADIF", a));
    }
    let body = client
        .post("https://logbook.qrz.com/api")
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("Couldn't reach QRZ: {e}"))?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    Ok(cqdj_radio::logbook::parse_qrz_reply(&body))
}

/// Saves a QSO to the ADIF log and, if turned on, uploads it to QRZ.
#[tauri::command]
async fn log_qso(app: tauri::AppHandle, state: State<'_, AppState>, qso: cqdj_radio::logbook::Qso) -> Result<LogResult, String> {
    let path = state.data_dir.join(ADIF_FILE);
    let version = app.package_info().version.to_string();
    cqdj_radio::logbook::append_adif(&path, &qso, &version).map_err(|e| format!("Couldn't save the QSO: {e}"))?;
    let settings = load(&state.settings_path);
    let qrz = if settings.log.qrz_upload && !settings.log.qrz_api_key.trim().is_empty() {
        Some(qrz_call(&app, &settings.log.qrz_api_key, "INSERT", Some(qso.to_adif()), &qso.my_call).await)
    } else {
        None
    };
    Ok(LogResult { file: path.display().to_string(), qrz })
}

/// Checks a QRZ API key with the STATUS action.
#[tauri::command]
async fn test_qrz(app: tauri::AppHandle, key: String, my_call: String) -> Result<String, String> {
    let r = qrz_call(&app, &key, "STATUS", None, &my_call).await?;
    if r.ok {
        let count = r.fields.iter().find(|(k, _)| k == "COUNT").map(|(_, v)| v.clone());
        let callsign = r.fields.iter().find(|(k, _)| k == "CALLSIGN").map(|(_, v)| v.clone());
        Ok(match (callsign, count) {
            (Some(c), Some(n)) => format!("Key works: logbook {c}, {n} QSOs."),
            _ => "Key works.".into(),
        })
    } else {
        Err(format!("QRZ said: {}", if r.reason.is_empty() { "not accepted" } else { &r.reason }))
    }
}

/// Calls already in the ADIF log, with band, for "worked before".
#[tauri::command]
fn worked_list(state: State<AppState>) -> Vec<(String, String)> {
    std::fs::read_to_string(state.data_dir.join(ADIF_FILE)).map(|t| cqdj_radio::logbook::worked_from_adif(&t)).unwrap_or_default()
}

#[tauri::command]
fn open_log_folder(state: State<AppState>) -> Result<String, String> {
    let dir = &state.data_dir;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(target_os = "windows")]
    let opener = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let opener = "xdg-open";
    std::process::Command::new(opener).arg(dir).spawn().map_err(|e| e.to_string())?;
    Ok(dir.display().to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let dir = app.path().app_config_dir().unwrap_or_else(|_| PathBuf::from("."));
            let data_dir = app.path().app_data_dir().unwrap_or_else(|_| dir.clone());
            app.manage(AppState {
                station: Mutex::new(None),
                settings_path: dir.join("settings.json"),
                data_dir,
                pending_update: Mutex::new(None),
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                // Make sure the radio is unkeyed when the window closes.
                if let Some(state) = window.try_state::<AppState>() {
                    let station = state.station.lock().unwrap().take();
                    drop(station);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            list_audio,
            list_ports,
            find_radio,
            start_station,
            stop_station,
            station_command,
            check_message,
            app_version,
            check_update,
            install_update,
            log_qso,
            test_qrz,
            worked_list,
            open_log_folder
        ])
        .run(tauri::generate_context!())
        .expect("error while running CQ DJ");
}
