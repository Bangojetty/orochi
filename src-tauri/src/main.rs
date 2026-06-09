// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Mutex;

use base64::Engine as _;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

// ---------- State ----------

#[derive(Clone, Copy)]
struct Region {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

struct CapturedFrame {
    data: Vec<u8>, // RGBA
    w: u32,
    h: u32,
}

#[derive(Default)]
struct AppState {
    frames: Mutex<Vec<CapturedFrame>>,
    region: Mutex<Option<Region>>,
    hotkey: Mutex<String>,
}

#[derive(Serialize, Clone, Copy)]
struct RegionDto {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

#[derive(Serialize)]
struct StateSnapshot {
    frame_count: usize,
    hotkey: String,
    region: Option<RegionDto>,
}

// ---------- Screen capture ----------

/// Grab the primary monitor as a raw RGBA buffer.
fn capture_primary() -> Result<(Vec<u8>, u32, u32), String> {
    let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
    let mut iter = monitors.into_iter();
    let mut chosen = iter.next().ok_or_else(|| "No monitor found".to_string())?;
    for m in iter {
        if m.is_primary().unwrap_or(false) {
            chosen = m;
            break;
        }
    }
    let img = chosen.capture_image().map_err(|e| e.to_string())?;
    let (w, h) = (img.width(), img.height());
    Ok((img.into_raw(), w, h))
}

/// Crop a raw RGBA buffer to the given region (clamped to bounds).
fn crop(raw: &[u8], fw: u32, fh: u32, r: Region) -> (Vec<u8>, u32, u32) {
    let x0 = r.x.min(fw.saturating_sub(1));
    let y0 = r.y.min(fh.saturating_sub(1));
    let w = r.w.min(fw - x0);
    let h = r.h.min(fh - y0);
    if w == 0 || h == 0 {
        return (raw.to_vec(), fw, fh);
    }
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in 0..h {
        let src_y = y0 + row;
        let start = (((src_y * fw) + x0) * 4) as usize;
        let end = start + (w * 4) as usize;
        out.extend_from_slice(&raw[start..end]);
    }
    (out, w, h)
}

/// Encode a downscaled PNG thumbnail as a data URL.
fn make_thumb(data: &[u8], w: u32, h: u32) -> String {
    use image::{imageops::FilterType, ImageEncoder, RgbaImage};
    let img = match RgbaImage::from_raw(w, h, data.to_vec()) {
        Some(i) => i,
        None => return String::new(),
    };
    let tw = 160u32;
    let th = (((h as f32) * (tw as f32 / w as f32)).round() as u32).max(1);
    let small = image::imageops::resize(&img, tw, th, FilterType::Triangle);
    let mut png = Vec::new();
    if image::codecs::png::PngEncoder::new(&mut png)
        .write_image(small.as_raw(), tw, th, image::ExtendedColorType::Rgba8)
        .is_err()
    {
        return String::new();
    }
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png)
    )
}

/// Capture the current region (or full screen) and store it as a frame.
fn do_capture(app: &AppHandle) -> Result<usize, String> {
    let state = app.state::<AppState>();
    let region = *state.region.lock().unwrap();
    let (raw, fw, fh) = capture_primary()?;
    let (data, w, h) = match region {
        Some(r) => crop(&raw, fw, fh, r),
        None => (raw, fw, fh),
    };
    let thumb = make_thumb(&data, w, h);
    let count = {
        let mut frames = state.frames.lock().unwrap();
        frames.push(CapturedFrame { data, w, h });
        frames.len()
    };
    let _ = app.emit(
        "frame-captured",
        serde_json::json!({ "count": count, "thumb": thumb }),
    );
    Ok(count)
}

// ---------- Commands ----------

#[tauri::command]
async fn select_region(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("overlay") {
        let _ = w.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(&app, "overlay", WebviewUrl::App("overlay.html".into()))
        .title("Select Region")
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .fullscreen(true)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn submit_region(
    app: AppHandle,
    state: State<'_, AppState>,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Result<(), String> {
    *state.region.lock().unwrap() = Some(Region { x, y, w, h });
    if let Some(win) = app.get_webview_window("overlay") {
        let _ = win.close();
    }
    let _ = app.emit("region-updated", RegionDto { x, y, w, h });
    Ok(())
}

#[tauri::command]
fn cancel_region(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("overlay") {
        let _ = win.close();
    }
    Ok(())
}

#[tauri::command]
fn capture_frame(app: AppHandle) -> Result<usize, String> {
    do_capture(&app)
}

#[tauri::command]
fn clear_frames(app: AppHandle, state: State<'_, AppState>) -> Result<usize, String> {
    state.frames.lock().unwrap().clear();
    let _ = app.emit("frames-cleared", serde_json::json!({ "count": 0 }));
    Ok(0)
}

#[tauri::command]
async fn pick_output_dir(app: AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let folder = app.dialog().file().blocking_pick_folder();
    Ok(folder
        .and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().to_string()))
}

#[tauri::command]
fn generate_gif(
    state: State<'_, AppState>,
    output_dir: String,
    delay_ms: u32,
    max_width: u32,
    filename: String,
) -> Result<String, String> {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{imageops::FilterType, Delay, Frame, RgbaImage};

    let frames = state.frames.lock().unwrap();
    if frames.is_empty() {
        return Err("No frames captured yet.".into());
    }

    // Target size derived from the first frame, capped by max_width.
    let first = &frames[0];
    let (mut tw, mut th) = (first.w, first.h);
    if max_width > 0 && tw > max_width {
        th = (((th as f32) * (max_width as f32 / tw as f32)).round() as u32).max(1);
        tw = max_width;
    }

    let dir = std::path::Path::new(&output_dir);
    if !dir.is_dir() {
        return Err("Output folder does not exist.".into());
    }

    let mut name = filename.trim().to_string();
    if name.is_empty() {
        name = "orochi.gif".into();
    }
    if !name.to_lowercase().ends_with(".gif") {
        name.push_str(".gif");
    }
    let mut path = dir.join(&name);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("orochi")
        .to_string();
    let mut i = 1;
    while path.exists() {
        path = dir.join(format!("{stem}_{i}.gif"));
        i += 1;
    }

    let file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut encoder = GifEncoder::new_with_speed(file, 10);
    encoder
        .set_repeat(Repeat::Infinite)
        .map_err(|e| e.to_string())?;

    for f in frames.iter() {
        let img = RgbaImage::from_raw(f.w, f.h, f.data.clone())
            .ok_or_else(|| "Corrupt frame buffer".to_string())?;
        let img = if f.w != tw || f.h != th {
            image::imageops::resize(&img, tw, th, FilterType::Triangle)
        } else {
            img
        };
        let delay = Delay::from_numer_denom_ms(delay_ms.max(1), 1);
        encoder
            .encode_frame(Frame::from_parts(img, 0, 0, delay))
            .map_err(|e| e.to_string())?;
    }
    drop(encoder); // flush to disk

    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
fn set_hotkey(app: AppHandle, state: State<'_, AppState>, accelerator: String) -> Result<(), String> {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    gs.register(accelerator.as_str()).map_err(|e| e.to_string())?;
    *state.hotkey.lock().unwrap() = accelerator;
    Ok(())
}

#[tauri::command]
fn get_state(state: State<'_, AppState>) -> StateSnapshot {
    let frame_count = state.frames.lock().unwrap().len();
    let hotkey = state.hotkey.lock().unwrap().clone();
    let region = state
        .region
        .lock()
        .unwrap()
        .map(|r| RegionDto {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        });
    StateSnapshot {
        frame_count,
        hotkey,
        region,
    }
}

// ---------- Entry point ----------

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        let _ = do_capture(app);
                    }
                })
                .build(),
        )
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            select_region,
            submit_region,
            cancel_region,
            capture_frame,
            clear_frames,
            generate_gif,
            pick_output_dir,
            set_hotkey,
            get_state
        ])
        .setup(|app| {
            let default_hotkey = "F8".to_string();
            *app.state::<AppState>().hotkey.lock().unwrap() = default_hotkey.clone();
            if let Err(e) = app.handle().global_shortcut().register(default_hotkey.as_str()) {
                eprintln!("failed to register default hotkey: {e}");
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Orochi");
}
