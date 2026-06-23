// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod winutil;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State, WebviewUrl,
    WebviewWindowBuilder,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

// Localhost quality bridge defaults. Both the Tampermonkey userscript and this
// app share them; override at runtime with the OROCHI_PORT / OROCHI_TOKEN env
// vars (keep the userscript in sync via its Tampermonkey menu).
const QUALITY_PORT: u16 = 47800;
const QUALITY_TOKEN: &str = "orochi-local-7Q2vXm";

// ---------- State ----------

/// A selected region stored as fractions (0.0..=1.0) of the captured frame.
/// Keeping it normalized makes it DPI-independent: the overlay reports where the
/// drag landed relative to its own viewport, and we resolve that to pixels against
/// the real capture buffer at grab time — so monitor scaling can't desync them.
#[derive(Clone, Copy)]
struct Region {
    fx: f64,
    fy: f64,
    fw: f64,
    fh: f64,
    // Physical top-left of the monitor the region was drawn on. On multi-monitor
    // setups the overlay can be on any screen, so we record which one and capture
    // *that* monitor at grab time rather than always grabbing the primary.
    mon_x: i32,
    mon_y: i32,
}

impl Region {
    /// Resolve to pixel coordinates against a frame of size `fw` x `fh`.
    fn to_pixels(&self, fw: u32, fh: u32) -> RegionDto {
        let c = |v: f64| v.clamp(0.0, 1.0);
        RegionDto {
            x: (c(self.fx) * fw as f64).round() as u32,
            y: (c(self.fy) * fh as f64).round() as u32,
            w: (c(self.fw) * fw as f64).round() as u32,
            h: (c(self.fh) * fh as f64).round() as u32,
        }
    }
}

struct CapturedFrame {
    data: Vec<u8>, // RGBA
    w: u32,
    h: u32,
}

/// What a registered global shortcut does when pressed. The plugin's single
/// handler looks the pressed `Shortcut` up in `AppState::actions` and runs the
/// matching action, so GIF capture and every text-paster binding can coexist.
#[derive(Clone)]
enum Action {
    GifCapture,
    Paste(String),
}

/// One text-paster binding: an accelerator string plus the text it pastes.
#[derive(Serialize, Deserialize, Clone, Default)]
struct PasteBinding {
    accelerator: String,
    text: String,
}

#[derive(Default)]
struct AppState {
    frames: Mutex<Vec<CapturedFrame>>,
    region: Mutex<Option<Region>>,
    hotkey: Mutex<String>,
    paste_bindings: Mutex<Vec<PasteBinding>>,
    actions: Mutex<HashMap<Shortcut, Action>>,
}

/// Live state for the floating cursor overlay, shared with its polling thread.
struct CursorState {
    /// Whether the polling thread should currently emit cursor positions.
    tracking: AtomicBool,
    /// Physical top-left of the overlay window (the virtual-desktop origin), so
    /// the thread can report cursor coords relative to it.
    origin: Mutex<(i32, i32)>,
    /// Last config received from the UI, re-served to the overlay on load.
    config: Mutex<serde_json::Value>,
    /// Whether we've blanked the system cursors (so we know to restore them).
    hiding: AtomicBool,
}

impl Default for CursorState {
    fn default() -> Self {
        CursorState {
            tracking: AtomicBool::new(false),
            origin: Mutex::new((0, 0)),
            config: Mutex::new(serde_json::Value::Null),
            hiding: AtomicBool::new(false),
        }
    }
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

// ---------- Quality bridge state ----------

/// One clip's raw reviewer comments (replace-per-clip; parsing happens in the
/// frontend so the taxonomy can evolve without touching the userscript).
#[derive(Serialize, Deserialize, Clone, Default)]
struct ClipEntry {
    ts: u64,
    comments: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct QualityStore {
    version: u32,
    clips: HashMap<String, ClipEntry>,
}

impl Default for QualityStore {
    fn default() -> Self {
        QualityStore {
            version: 1,
            clips: HashMap::new(),
        }
    }
}

struct QualityState {
    store: Mutex<QualityStore>,
    port: u16,
    token: String,
}

impl Default for QualityState {
    fn default() -> Self {
        let port = std::env::var("OROCHI_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(QUALITY_PORT);
        let token = std::env::var("OROCHI_TOKEN")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| QUALITY_TOKEN.to_string());
        QualityState {
            store: Mutex::new(QualityStore::default()),
            port,
            token,
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn quality_file(app: &AppHandle) -> Option<PathBuf> {
    let dir = app.path().app_config_dir().ok()?;
    Some(dir.join("quality.json"))
}

fn load_quality_from_disk(app: &AppHandle) -> QualityStore {
    if let Some(p) = quality_file(app) {
        if let Ok(txt) = std::fs::read_to_string(&p) {
            if let Ok(store) = serde_json::from_str::<QualityStore>(&txt) {
                return store;
            }
        }
    }
    QualityStore::default()
}

fn save_quality_to_disk(app: &AppHandle, store: &QualityStore) {
    if let Some(p) = quality_file(app) {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(txt) = serde_json::to_string_pretty(store) {
            let _ = std::fs::write(&p, txt);
        }
    }
}

/// Apply one /ingest payload from the userscript: replace this clip's comments,
/// persist, and notify the frontend. Returns the new clip count.
fn handle_ingest(app: &AppHandle, body: &str) -> Result<usize, String> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let clip_id = v
        .get("clipId")
        .and_then(|x| x.as_str())
        .ok_or("missing clipId")?
        .to_string();
    let comments: Vec<String> = v
        .get("comments")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let qs = app.state::<QualityState>();
    let count = {
        let mut store = qs.store.lock().unwrap();
        store.version = 1;
        store.clips.insert(
            clip_id.clone(),
            ClipEntry {
                ts: now_secs(),
                comments,
            },
        );
        store.clips.len()
    };
    {
        let store = qs.store.lock().unwrap();
        save_quality_to_disk(app, &store);
    }
    let _ = app.emit(
        "quality-updated",
        serde_json::json!({ "clipId": clip_id, "clipCount": count }),
    );
    Ok(count)
}

/// A JSON response carrying permissive CORS headers (belt-and-suspenders: the
/// userscript uses GM_xmlhttpRequest which ignores CORS, but this also allows a
/// plain fetch from any future client).
fn cors_resp(body: &str, code: u16) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let mut resp = tiny_http::Response::from_string(body).with_status_code(code);
    for (k, val) in [
        ("Access-Control-Allow-Origin", "*"),
        ("Access-Control-Allow-Methods", "POST, GET, OPTIONS"),
        ("Access-Control-Allow-Headers", "Content-Type, X-Orochi-Token"),
        ("Content-Type", "application/json"),
    ] {
        if let Ok(h) = tiny_http::Header::from_bytes(k.as_bytes(), val.as_bytes()) {
            resp = resp.with_header(h);
        }
    }
    resp
}

/// Spawn the loopback-only HTTP listener that receives comment payloads from the
/// userscript. Binds 127.0.0.1 so nothing ever touches the LAN.
fn start_quality_server(app: AppHandle) {
    let (port, token) = {
        let qs = app.state::<QualityState>();
        (qs.port, qs.token.clone())
    };
    std::thread::spawn(move || {
        use tiny_http::{Method, Server};
        let addr = format!("127.0.0.1:{port}");
        let server = match Server::http(&addr) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[orochi] quality bridge failed to bind {addr}: {e}");
                return;
            }
        };
        println!("[orochi] quality bridge listening on http://{addr}");

        for mut request in server.incoming_requests() {
            let method = request.method().clone();
            let url = request.url().to_string();

            if method == Method::Options {
                let _ = request.respond(cors_resp("", 204));
                continue;
            }
            if method == Method::Get && url.starts_with("/health") {
                let _ = request.respond(cors_resp("{\"ok\":true}", 200));
                continue;
            }
            if method == Method::Post && url.starts_with("/ingest") {
                let provided = request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("X-Orochi-Token"))
                    .map(|h| h.value.as_str().to_string())
                    .unwrap_or_default();
                if provided != token {
                    let _ = request.respond(cors_resp("{\"ok\":false,\"error\":\"forbidden\"}", 403));
                    continue;
                }
                let mut body = String::new();
                if request.as_reader().read_to_string(&mut body).is_err() {
                    let _ = request.respond(cors_resp("{\"ok\":false,\"error\":\"bad body\"}", 400));
                    continue;
                }
                match handle_ingest(&app, &body) {
                    Ok(n) => {
                        let _ = request.respond(cors_resp(&format!("{{\"ok\":true,\"clips\":{n}}}"), 200));
                    }
                    Err(e) => {
                        let msg = serde_json::json!({ "ok": false, "error": e }).to_string();
                        let _ = request.respond(cors_resp(&msg, 400));
                    }
                }
                continue;
            }
            let _ = request.respond(cors_resp("{\"ok\":false,\"error\":\"not found\"}", 404));
        }
    });
}

// ---------- Screen capture ----------

/// Pick the monitor whose physical origin matches `(mon_x, mon_y)`, falling back
/// to the primary monitor (then the first) when there's no exact match.
fn pick_monitor(monitors: &[xcap::Monitor], mon_x: i32, mon_y: i32) -> Option<&xcap::Monitor> {
    monitors
        .iter()
        .find(|m| {
            m.x().map(|x| x == mon_x).unwrap_or(false) && m.y().map(|y| y == mon_y).unwrap_or(false)
        })
        .or_else(|| monitors.iter().find(|m| m.is_primary().unwrap_or(false)))
        .or_else(|| monitors.first())
}

/// Grab the monitor at physical origin `(mon_x, mon_y)` as a raw RGBA buffer.
fn capture_monitor(mon_x: i32, mon_y: i32) -> Result<(Vec<u8>, u32, u32), String> {
    let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
    let chosen =
        pick_monitor(&monitors, mon_x, mon_y).ok_or_else(|| "No monitor found".to_string())?;
    let img = chosen.capture_image().map_err(|e| e.to_string())?;
    let (w, h) = (img.width(), img.height());
    Ok((img.into_raw(), w, h))
}

/// Grab the primary monitor as a raw RGBA buffer (used when no region is set).
fn capture_primary() -> Result<(Vec<u8>, u32, u32), String> {
    let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
    let chosen = monitors
        .iter()
        .find(|m| m.is_primary().unwrap_or(false))
        .or_else(|| monitors.first())
        .ok_or_else(|| "No monitor found".to_string())?;
    let img = chosen.capture_image().map_err(|e| e.to_string())?;
    let (w, h) = (img.width(), img.height());
    Ok((img.into_raw(), w, h))
}

/// Physical size of the monitor at origin `(mon_x, mon_y)` (used to label a region
/// in pixels for the UI without doing a full screen grab).
fn monitor_size(mon_x: i32, mon_y: i32) -> Result<(u32, u32), String> {
    let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
    let chosen =
        pick_monitor(&monitors, mon_x, mon_y).ok_or_else(|| "No monitor found".to_string())?;
    let w = chosen.width().map_err(|e| e.to_string())?;
    let h = chosen.height().map_err(|e| e.to_string())?;
    Ok((w, h))
}

/// Crop a raw RGBA buffer to the given pixel region (clamped to bounds).
fn crop(raw: &[u8], fw: u32, fh: u32, r: RegionDto) -> (Vec<u8>, u32, u32) {
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
    let (data, w, h) = match region {
        Some(r) => {
            let (raw, fw, fh) = capture_monitor(r.mon_x, r.mon_y)?;
            crop(&raw, fw, fh, r.to_pixels(fw, fh))
        }
        None => capture_primary()?,
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

// ---------- Global shortcut dispatch ----------

/// Rebuild the global-shortcut registration from the current GIF hotkey and the
/// text-paster bindings. Every accelerator is registered fresh and mapped to its
/// `Action`; the plugin handler then dispatches on the pressed shortcut.
fn reapply_shortcuts(app: &AppHandle) {
    let state = app.state::<AppState>();
    let gif = state.hotkey.lock().unwrap().clone();
    let bindings = state.paste_bindings.lock().unwrap().clone();

    let gs = app.global_shortcut();
    let _ = gs.unregister_all();

    let mut map: HashMap<Shortcut, Action> = HashMap::new();

    // GIF capture first so it wins any accidental collision with a paste binding.
    if let Ok(sc) = gif.parse::<Shortcut>() {
        if gs.register(gif.as_str()).is_ok() {
            map.insert(sc, Action::GifCapture);
        }
    }
    for b in &bindings {
        let acc = b.accelerator.trim();
        if acc.is_empty() {
            continue;
        }
        let sc = match acc.parse::<Shortcut>() {
            Ok(sc) => sc,
            Err(_) => continue,
        };
        if map.contains_key(&sc) {
            continue; // already taken (e.g. by the GIF hotkey)
        }
        if gs.register(acc).is_ok() {
            map.insert(sc, Action::Paste(b.text.clone()));
        }
    }

    *state.actions.lock().unwrap() = map;
}

/// Drop the text on the clipboard, paste it, then restore the old clipboard.
/// Runs on its own thread so the shortcut handler returns immediately.
fn run_paste(text: String) {
    std::thread::spawn(move || {
        let previous = winutil::clipboard_get_text();
        if winutil::clipboard_set_text(&text) {
            // Give the user a moment to release the hotkey before we inject Ctrl+V.
            std::thread::sleep(Duration::from_millis(40));
            winutil::send_paste();
            // Wait for the target app to consume the paste, then put the old
            // clipboard contents back.
            std::thread::sleep(Duration::from_millis(140));
            if let Some(prev) = previous {
                let _ = winutil::clipboard_set_text(&prev);
            }
        }
    });
}

// ---------- Cursor overlay ----------

const CURSOR_LABEL: &str = "cursor";

/// Bounding box of every monitor in physical pixels: (x, y, w, h). The overlay
/// window is sized to this so it can host a pointer graphic anywhere on the
/// virtual desktop.
fn virtual_desktop_bounds() -> (i32, i32, u32, u32) {
    let monitors = xcap::Monitor::all().unwrap_or_default();
    if monitors.is_empty() {
        return (0, 0, 1920, 1080);
    }
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    for m in &monitors {
        let x = m.x().unwrap_or(0);
        let y = m.y().unwrap_or(0);
        let w = m.width().unwrap_or(0) as i32;
        let h = m.height().unwrap_or(0) as i32;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x + w);
        max_y = max_y.max(y + h);
    }
    (
        min_x,
        min_y,
        (max_x - min_x).max(1) as u32,
        (max_y - min_y).max(1) as u32,
    )
}

/// Create the click-through, always-on-top overlay window that spans the whole
/// virtual desktop, if it doesn't already exist. Records its origin so the
/// polling thread can report cursor positions relative to it.
fn ensure_cursor_window(app: &AppHandle) -> Result<(), String> {
    if app.get_webview_window(CURSOR_LABEL).is_some() {
        return Ok(());
    }
    let (x, y, w, h) = virtual_desktop_bounds();
    let win = WebviewWindowBuilder::new(app, CURSOR_LABEL, WebviewUrl::App("cursor.html".into()))
        .title("Orochi Cursor")
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(false)
        .focused(false)
        .visible(true)
        .build()
        .map_err(|e| e.to_string())?;
    // Place and size it in physical pixels to cover all monitors exactly.
    let _ = win.set_position(PhysicalPosition::new(x, y));
    let _ = win.set_size(PhysicalSize::new(w, h));
    let _ = win.set_ignore_cursor_events(true);

    *app.state::<CursorState>().origin.lock().unwrap() = (x, y);
    Ok(())
}

/// Background loop that, while tracking is on, polls the OS cursor and emits its
/// position (relative to the overlay origin, in physical pixels) to the overlay.
fn spawn_cursor_thread(app: AppHandle) {
    std::thread::spawn(move || {
        let cs = app.state::<CursorState>();
        let mut last = (i32::MIN, i32::MIN);
        loop {
            if !cs.tracking.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(80));
                continue;
            }
            if let Some((gx, gy)) = winutil::cursor_pos() {
                if (gx, gy) != last {
                    last = (gx, gy);
                    let (ox, oy) = *cs.origin.lock().unwrap();
                    let _ = app.emit_to(
                        CURSOR_LABEL,
                        "cursor-pos",
                        serde_json::json!({ "x": gx - ox, "y": gy - oy }),
                    );
                }
            }
            std::thread::sleep(Duration::from_millis(8));
        }
    });
}

// ---------- Commands ----------

/// Close every region-selection overlay window (one per monitor).
fn close_overlays(app: &AppHandle) {
    for (label, win) in app.webview_windows() {
        if label.starts_with("overlay") {
            let _ = win.close();
        }
    }
}

#[tauri::command]
async fn select_region(app: AppHandle) -> Result<(), String> {
    // Already showing? Just refocus the first overlay.
    let existing: Vec<_> = app
        .webview_windows()
        .into_iter()
        .filter(|(label, _)| label.starts_with("overlay"))
        .collect();
    if let Some((_, w)) = existing.first() {
        let _ = w.set_focus();
        return Ok(());
    }

    // Open one borderless overlay covering each monitor so a region can be drawn
    // on any screen — not just the primary one. The overlay reports the selection
    // as fractions of its own viewport, and submit_region records which monitor it
    // landed on, so capture/DPI handling stays per-monitor correct.
    let monitors = app
        .get_webview_window("main")
        .ok_or("no main window")?
        .available_monitors()
        .map_err(|e| e.to_string())?;
    if monitors.is_empty() {
        return Err("No monitors found".into());
    }
    for (i, m) in monitors.iter().enumerate() {
        let pos = m.position();
        let size = m.size();
        let label = format!("overlay-{i}");
        let win = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App("overlay.html".into()))
            .title("Select Region")
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(i == 0)
            .build()
            .map_err(|e| e.to_string())?;
        // Place/size in physical pixels so it exactly covers this monitor.
        let _ = win.set_position(PhysicalPosition::new(pos.x, pos.y));
        let _ = win.set_size(PhysicalSize::new(size.width, size.height));
    }
    Ok(())
}

#[tauri::command]
fn submit_region(
    app: AppHandle,
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    fx: f64,
    fy: f64,
    fw: f64,
    fh: f64,
) -> Result<(), String> {
    // The selection came from a specific overlay window; record *that* window's
    // monitor (physical origin matching xcap's coords) so capture grabs the right
    // screen. Fall back to the window's own position if the monitor lookup fails.
    let (mon_x, mon_y) = window
        .current_monitor()
        .ok()
        .flatten()
        .map(|m| {
            let p = m.position();
            (p.x, p.y)
        })
        .or_else(|| window.outer_position().ok().map(|p| (p.x, p.y)))
        .unwrap_or((0, 0));
    let region = Region {
        fx,
        fy,
        fw,
        fh,
        mon_x,
        mon_y,
    };
    *state.region.lock().unwrap() = Some(region);
    close_overlays(&app);
    // Label the region in pixels for the UI. Resolve against that monitor's size
    // so the number shown matches what actually gets cropped.
    let (mw, mh) = monitor_size(mon_x, mon_y).unwrap_or((0, 0));
    let _ = app.emit("region-updated", region.to_pixels(mw, mh));
    Ok(())
}

#[tauri::command]
fn cancel_region(app: AppHandle) -> Result<(), String> {
    close_overlays(&app);
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
    // Validate before committing so a bad accelerator doesn't wipe the binding.
    accelerator.parse::<Shortcut>().map_err(|e| e.to_string())?;
    *state.hotkey.lock().unwrap() = accelerator;
    drop(state);
    reapply_shortcuts(&app);
    Ok(())
}

/// Replace the full set of text-paster bindings and re-register all shortcuts.
#[tauri::command]
fn set_paste_bindings(
    app: AppHandle,
    state: State<'_, AppState>,
    bindings: Vec<PasteBinding>,
) -> Result<(), String> {
    *state.paste_bindings.lock().unwrap() = bindings;
    drop(state);
    reapply_shortcuts(&app);
    Ok(())
}

/// Apply the cursor-overlay config from the UI: show/hide the overlay, toggle
/// position tracking, optionally blank the system cursor, and forward the visual
/// settings to the overlay window.
#[tauri::command]
fn cursor_set(app: AppHandle, config: serde_json::Value) -> Result<(), String> {
    let enabled = config.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
    let hide_real = config
        .get("hideReal")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let cs = app.state::<CursorState>();
    *cs.config.lock().unwrap() = config.clone();

    if enabled {
        ensure_cursor_window(&app)?;
        cs.tracking.store(true, Ordering::Relaxed);
        let _ = app.emit_to(CURSOR_LABEL, "cursor-config", config.clone());
    } else {
        cs.tracking.store(false, Ordering::Relaxed);
        if let Some(w) = app.get_webview_window(CURSOR_LABEL) {
            let _ = w.close();
        }
    }

    // Blank or restore the real system cursor as needed.
    let want_hidden = enabled && hide_real;
    if want_hidden != cs.hiding.load(Ordering::Relaxed) {
        winutil::set_system_cursor_hidden(want_hidden);
        cs.hiding.store(want_hidden, Ordering::Relaxed);
    }
    Ok(())
}

/// The overlay asks for the current config on load (avoids an emit/listen race).
#[tauri::command]
fn cursor_get_config(app: AppHandle) -> serde_json::Value {
    app.state::<CursorState>().config.lock().unwrap().clone()
}

#[tauri::command]
fn get_state(state: State<'_, AppState>) -> StateSnapshot {
    let frame_count = state.frames.lock().unwrap().len();
    let hotkey = state.hotkey.lock().unwrap().clone();
    let region = state.region.lock().unwrap().map(|r| {
        let (mw, mh) = monitor_size(r.mon_x, r.mon_y).unwrap_or((0, 0));
        r.to_pixels(mw, mh)
    });
    StateSnapshot {
        frame_count,
        hotkey,
        region,
    }
}

#[tauri::command]
fn quality_get_all(app: AppHandle) -> serde_json::Value {
    let qs = app.state::<QualityState>();
    let store = qs.store.lock().unwrap();
    serde_json::to_value(&*store).unwrap_or_else(|_| serde_json::json!({ "version": 1, "clips": {} }))
}

#[tauri::command]
fn quality_clear(app: AppHandle) -> Result<(), String> {
    let qs = app.state::<QualityState>();
    {
        let mut store = qs.store.lock().unwrap();
        store.clips.clear();
    }
    {
        let store = qs.store.lock().unwrap();
        save_quality_to_disk(&app, &store);
    }
    let _ = app.emit(
        "quality-updated",
        serde_json::json!({ "clipId": serde_json::Value::Null, "clipCount": 0 }),
    );
    Ok(())
}

#[tauri::command]
fn quality_conn(app: AppHandle) -> serde_json::Value {
    let qs = app.state::<QualityState>();
    serde_json::json!({ "port": qs.port, "token": qs.token })
}

// ---------- Entry point ----------

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state != ShortcutState::Pressed {
                        return;
                    }
                    let action = app
                        .state::<AppState>()
                        .actions
                        .lock()
                        .unwrap()
                        .get(shortcut)
                        .cloned();
                    match action {
                        Some(Action::GifCapture) => {
                            let _ = do_capture(app);
                        }
                        Some(Action::Paste(text)) => run_paste(text),
                        None => {}
                    }
                })
                .build(),
        )
        .manage(AppState::default())
        .manage(QualityState::default())
        .manage(CursorState::default())
        .invoke_handler(tauri::generate_handler![
            select_region,
            submit_region,
            cancel_region,
            capture_frame,
            clear_frames,
            generate_gif,
            pick_output_dir,
            set_hotkey,
            set_paste_bindings,
            cursor_set,
            cursor_get_config,
            get_state,
            quality_get_all,
            quality_clear,
            quality_conn
        ])
        .setup(|app| {
            // Register the default GIF-capture hotkey through the shared dispatcher
            // so it shares the registry with text-paster bindings.
            *app.state::<AppState>().hotkey.lock().unwrap() = "F8".to_string();
            reapply_shortcuts(app.handle());

            // Start the cursor-overlay polling loop (idle until the feature is on).
            spawn_cursor_thread(app.handle().clone());

            // Load any persisted quality data, then start the localhost bridge
            // that the Tampermonkey userscript POSTs reviewer comments to.
            let handle = app.handle().clone();
            let loaded = load_quality_from_disk(&handle);
            *app.state::<QualityState>().store.lock().unwrap() = loaded;
            start_quality_server(handle);

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while running Orochi")
        .run(|app, event| {
            // If we blanked the system cursor, make sure it's restored on exit so
            // the user is never left with an invisible pointer.
            if let tauri::RunEvent::Exit = event {
                let cs = app.state::<CursorState>();
                if cs.hiding.load(Ordering::Relaxed) {
                    winutil::set_system_cursor_hidden(false);
                }
            }
        });
}
