// CQ DJ desktop app: a thin Tauri shell around the cqdj-radio station loop.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use cqdj_radio::{Command, Parity, Station, StationConfig};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{Emitter, Manager, State};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Settings {
    #[serde(default)]
    station: StationConfig,
    /// True once the user has finished first-time setup.
    #[serde(default)]
    setup_done: bool,
}

struct AppState {
    station: Mutex<Option<Station>>,
    settings_path: PathBuf,
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
    });
    Ok(())
}

/// Checks that a message is something FT8 can send; returns a friendly error if not.
#[tauri::command]
fn check_message(message: String) -> Result<(), String> {
    cqdj_engine::encode_tones(&message).map(|_| ()).map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_config_dir().unwrap_or_else(|_| PathBuf::from("."));
            app.manage(AppState { station: Mutex::new(None), settings_path: dir.join("settings.json") });
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
            check_message
        ])
        .run(tauri::generate_context!())
        .expect("error while running CQ DJ");
}
