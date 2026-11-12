// Hide the console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::Mutex;

use base64::Engine as _;
use serde::Serialize;
use stepcast_core::encode::{encode_gif, EncodeOptions, RgbFrame};
use tauri::{Emitter, Manager, State, WebviewWindow};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Shortcut, ShortcutState};

/// A recording in progress.
#[cfg(windows)]
type Active = stepcast_core::capture_win::Session;

/// Off Windows there is no capture; a synthetic recording keeps the UI testable.
#[cfg(not(windows))]
struct Active {
    started: std::time::Instant,
    fps: u32,
}

#[derive(Default)]
struct AppState {
    active: Mutex<Option<Active>>,
}

#[derive(Serialize)]
struct RecordResult {
    path: String,
    preview: Option<String>,
    width: u32,
    height: u32,
    seconds: f64,
    input_frames: usize,
    written_frames: usize,
    encode_secs: f64,
    file_bytes: u64,
}

/// Portable default: a `recordings` folder next to the exe; falls back to Pictures\StepCast.
#[tauri::command]
fn default_output_dir(app: tauri::AppHandle) -> String {
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("recordings")));
    if let Some(dir) = beside_exe {
        if std::fs::create_dir_all(&dir).is_ok() {
            let probe = dir.join(".write-test");
            if std::fs::write(&probe, b"").is_ok() {
                let _ = std::fs::remove_file(probe);
                return dir.to_string_lossy().into_owned();
            }
        }
    }
    app.path()
        .picture_dir()
        .map(|d| d.join("StepCast"))
        .unwrap_or_else(|_| PathBuf::from("."))
        .to_string_lossy()
        .into_owned()
}

#[tauri::command]
fn start_recording(window: WebviewWindow, state: State<'_, AppState>, fps: u32, width: u32) -> Result<(), String> {
    let mut active = state.active.lock().map_err(|_| "內部狀態錯誤")?;
    if active.is_some() {
        return Err("已經在錄製中".into());
    }
    let fps = fps.clamp(1, 60);
    // Get the app out of the way so it is not in the recording.
    let _ = window.minimize();

    #[cfg(windows)]
    {
        match stepcast_core::capture_win::Session::start(fps, width) {
            Ok(session) => *active = Some(session),
            Err(e) => {
                let _ = window.unminimize();
                return Err(e);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = width;
        *active = Some(Active { started: std::time::Instant::now(), fps });
    }
    Ok(())
}

#[tauri::command]
async fn stop_recording(
    window: WebviewWindow,
    state: State<'_, AppState>,
    dither: bool,
    out_dir: String,
    file_name: String,
) -> Result<RecordResult, String> {
    let active = state
        .active
        .lock()
        .map_err(|_| "內部狀態錯誤")?
        .take()
        .ok_or("目前沒有在錄製")?;
    let _ = window.unminimize();
    let _ = window.set_focus();

    tauri::async_runtime::spawn_blocking(move || finish(active, dither, out_dir, file_name))
        .await
        .map_err(|e| format!("編碼工作失敗：{e}"))?
}

fn finish(active: Active, dither: bool, out_dir: String, file_name: String) -> Result<RecordResult, String> {
    #[cfg(windows)]
    let (frames, end_ms): (Vec<RgbFrame>, u64) = active.stop()?;
    #[cfg(not(windows))]
    let (frames, end_ms): (Vec<RgbFrame>, u64) = {
        let secs = active.started.elapsed().as_secs().clamp(1, 30) as u32;
        let frames = stepcast_core::selftest::synthetic_frames(640, 400, active.fps, secs);
        (frames, secs as u64 * 1000)
    };

    let first = frames
        .first()
        .ok_or("沒有收到任何影格（畫面可能完全沒變，錄製時請移動滑鼠或操作視窗）")?;
    let (width, height) = (first.width, first.height);

    let dir = PathBuf::from(&out_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("無法建立資料夾 {out_dir}：{e}"))?;
    let safe_name: String = file_name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let path = dir.join(format!("{safe_name}.gif"));

    let stats = encode_gif(&frames, &path, &EncodeOptions { dither, end_ms })?;

    // Small files are previewed inline; huge ones are only saved.
    let preview = if stats.file_bytes <= 30 * 1024 * 1024 {
        std::fs::read(&path)
            .ok()
            .map(|b| format!("data:image/gif;base64,{}", base64::engine::general_purpose::STANDARD.encode(b)))
    } else {
        None
    };

    Ok(RecordResult {
        path: path.to_string_lossy().into_owned(),
        preview,
        width,
        height,
        seconds: end_ms as f64 / 1000.0,
        input_frames: stats.input_frames,
        written_frames: stats.written_frames,
        encode_secs: stats.encode_secs,
        file_bytes: stats.file_bytes,
    })
}

/// Opens Explorer with the file selected.
#[tauri::command]
fn reveal_file(path: String) -> Result<(), String> {
    #[cfg(windows)]
    {
        std::process::Command::new("explorer")
            .arg(format!("/select,{path}"))
            .spawn()
            .map_err(|e| format!("無法開啟檔案總管：{e}"))?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        Err(format!("只在 Windows 支援；檔案位置：{path}"))
    }
}

fn main() {
    let toggle = Shortcut::new(None, Code::F9);
    tauri::Builder::default()
        .manage(AppState::default())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    if shortcut == &toggle && event.state() == ShortcutState::Pressed {
                        let _ = app.emit("hotkey-toggle", ());
                    }
                })
                .build(),
        )
        .setup(move |app| {
            if let Err(e) = app.global_shortcut().register(toggle) {
                eprintln!("無法註冊 F9 快捷鍵：{e}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![default_output_dir, start_recording, stop_recording, reveal_file])
        .run(tauri::generate_context!())
        .expect("StepCast 啟動失敗");
}
