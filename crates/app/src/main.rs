//! Screensnips — Tauri app shell.
//!
//! Wiring: shot-capture (portal) -> shot-model (document) -> shot-render
//! (pixels) -> PNG export + TOML sidecar. Frames reach the webview ONLY via
//! the `shot://` custom protocol (Step 0 law); commands carry small JSON
//! (document ops), never frame bytes.
//!
//! Headless verification: SCREENSNIPS_SELFTEST=1 runs capture-detect +
//! model + render + PNG encode/decode + sidecar round-trip on a synthetic
//! image, prints SELFTEST_OK, and exits before the event loop.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use shot_model::{Beautify, Document, ImageRef, PrimitiveKind, RectN, Style};
use shot_render::{render, RgbaImage};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{Manager, State};

// ---------------------------------------------------------------- state

struct Session {
    source: RgbaImage,
    doc: Document,
    rev: u64,
    rendered: Option<(u64, RgbaImage)>,
    history: Vec<HistEntry>,
    future: Vec<HistEntry>,
}

/// One undoable state. `source` is Some only for transitions that changed
/// the pixels (crop) — restoring it is what makes crop undoable without
/// keeping a full image copy per annotation.
struct HistEntry {
    doc: Document,
    source: Option<RgbaImage>,
}

#[derive(Clone, Default)]
struct AppState(
    Arc<Mutex<Option<Session>>>,
    Arc<Mutex<std::collections::HashMap<std::path::PathBuf, RgbaImage>>>,
);

// ---------------------------------------------------------------- config

/// Config dir: SCREENSNIPS_CONFIG_DIR override (tests), else
/// $XDG_CONFIG_HOME/screensnips, else ~/.config/screensnips.
fn config_dir() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("SCREENSNIPS_CONFIG_DIR") {
        return std::path::PathBuf::from(d);
    }
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
        });
    base.join("screensnips")
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Settings {
    /// "auto" | "portal" | "wlroots" | "x11" — gate 7: forced backend
    /// selection ships, and the backend says why when it can't.
    backend: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { backend: "auto".into() }
    }
}

fn load_settings() -> Settings {
    let path = config_dir().join("settings.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_settings_file(s: &Settings) -> Result<(), String> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    write_atomic(&dir.join("settings.json"), text.as_bytes())
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Preset {
    name: String,
    beautify: Beautify,
}

fn starter_presets() -> Vec<Preset> {
    use shot_model::{Background, Color, Padding, Shadow};
    vec![
        Preset {
            name: "Bondi".into(),
            beautify: Beautify {
                background: Background::Gradient {
                    from: Color::rgba(29, 127, 166, 255),
                    to: Color::rgba(58, 173, 210, 255),
                    angle_deg: 45.0,
                },
                padding: Padding::uniform(0.07),
                corner_radius: 0.014,
                shadow: Shadow::default(),
                aspect: None,
                crop: None,
            },
        },
        Preset {
            name: "Newspaper".into(),
            beautify: Beautify {
                background: Background::Solid { color: Color::rgba(245, 241, 232, 255) },
                padding: Padding::uniform(0.045),
                corner_radius: 0.004,
                shadow: Shadow { enabled: true, blur: 0.018, offset_y: 0.008, opacity: 0.22 },
                aspect: None,
                crop: None,
            },
        },
        Preset {
            name: "Grizzette".into(),
            beautify: Beautify {
                background: Background::Gradient {
                    from: Color::rgba(123, 45, 91, 255),
                    to: Color::rgba(229, 107, 159, 255),
                    angle_deg: 45.0,
                },
                padding: Padding::uniform(0.08),
                corner_radius: 0.018,
                shadow: Shadow::default(),
                aspect: None,
                crop: None,
            },
        },
    ]
}

fn load_presets() -> Vec<Preset> {
    let path = config_dir().join("presets.json");
    match std::fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str(&t).unwrap_or_else(|_| starter_presets()),
        Err(_) => {
            let seeds = starter_presets();
            let _ = save_presets_file(&seeds);
            seeds
        }
    }
}

fn save_presets_file(p: &[Preset]) -> Result<(), String> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(p).map_err(|e| e.to_string())?;
    write_atomic(&dir.join("presets.json"), text.as_bytes())
}

/// Is `bin` an executable on PATH?
fn which(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(bin);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

// ------------------------------------------------------- startup action

/// CLI entry points for the GlobalShortcuts-absent fallback (Dickles'
/// rig): a desktop shortcut binding runs `screensnips --capture=picker|
/// full [--delay=N]`; the frontend asks once at boot what to do.
#[derive(Clone)]
struct StartupAction {
    mode: String,
    delay: u64,
}

static STARTUP: OnceLock<Mutex<Option<StartupAction>>> = OnceLock::new();

fn parse_startup_args() {
    let mut action: Option<StartupAction> = None;
    let mut delay = 0u64;
    for arg in std::env::args().skip(1) {
        if let Some(v) = arg.strip_prefix("--capture=") {
            action = Some(StartupAction { mode: v.to_string(), delay: 0 });
        } else if let Some(v) = arg.strip_prefix("--delay=") {
            delay = v.parse().unwrap_or(0);
        }
    }
    if let Some(mut a) = action {
        a.delay = delay;
        let _ = STARTUP.set(Mutex::new(Some(a)));
    } else {
        let _ = STARTUP.set(Mutex::new(None));
    }
}

impl Session {
    fn rendered(&mut self) -> Result<&RgbaImage, String> {
        if self.rendered.as_ref().map(|(r, _)| *r) != Some(self.rev) {
            let out = render(&self.doc, &self.source).map_err(|e| e.to_string())?;
            if !out.unrendered.is_empty() {
                eprintln!("render: unrendered primitives {:?}", out.unrendered);
            }
            self.rendered = Some((self.rev, out.image));
        }
        Ok(&self.rendered.as_ref().unwrap().1)
    }
    fn bump(&mut self) {
        self.rev += 1;
    }

    /// Record the pre-mutation state. Call BEFORE changing doc/source.
    /// `with_source` for pixel-changing ops (crop) only.
    fn snapshot(&mut self, with_source: bool) {
        self.history.push(HistEntry {
            doc: self.doc.clone(),
            source: if with_source { Some(self.source.clone()) } else { None },
        });
        if self.history.len() > 50 {
            self.history.remove(0);
        }
        self.future.clear();
    }

    fn undo(&mut self) -> Result<bool, String> {
        let entry = self.history.pop().ok_or("nothing to undo")?;
        let source_changed = entry.source.is_some();
        self.future.push(HistEntry {
            doc: std::mem::replace(&mut self.doc, entry.doc),
            source: if source_changed { Some(self.source.clone()) } else { None },
        });
        if let Some(src) = entry.source {
            self.source = src;
        }
        self.bump();
        Ok(source_changed)
    }

    fn redo(&mut self) -> Result<bool, String> {
        let entry = self.future.pop().ok_or("nothing to redo")?;
        let source_changed = entry.source.is_some();
        self.history.push(HistEntry {
            doc: std::mem::replace(&mut self.doc, entry.doc),
            source: if source_changed { Some(self.source.clone()) } else { None },
        });
        if let Some(src) = entry.source {
            self.source = src;
        }
        self.bump();
        Ok(source_changed)
    }
}

// ---------------------------------------------------------------- helpers

fn decode_png(bytes: &[u8]) -> Result<RgbaImage, String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let data = &buf[..frame.buffer_size()];
    let (w, h) = (frame.width, frame.height);
    let mut img = RgbaImage::new(w, h);
    match frame.color_type {
        png::ColorType::Rgba => img.data.copy_from_slice(data),
        png::ColorType::Rgb => {
            for (i, px) in data.chunks_exact(3).enumerate() {
                img.data[i * 4] = px[0];
                img.data[i * 4 + 1] = px[1];
                img.data[i * 4 + 2] = px[2];
                img.data[i * 4 + 3] = 255;
            }
        }
        png::ColorType::Grayscale => {
            for (i, g) in data.iter().enumerate() {
                img.data[i * 4] = *g;
                img.data[i * 4 + 1] = *g;
                img.data[i * 4 + 2] = *g;
                img.data[i * 4 + 3] = 255;
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for (i, px) in data.chunks_exact(2).enumerate() {
                img.data[i * 4] = px[0];
                img.data[i * 4 + 1] = px[0];
                img.data[i * 4 + 2] = px[0];
                img.data[i * 4 + 3] = px[1];
            }
        }
        other => return Err(format!("unsupported PNG color type {:?}", other)),
    }
    Ok(img)
}

fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    {
        let mut enc = png::Encoder::new(std::io::Cursor::new(&mut out), img.width, img.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&img.data).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Encode a rendered frame as PNG/JPG/WebP, optionally upscaled
/// (presentation export at 2x). PNG at 1x keeps the fast native encoder.
fn encode_image(img: &RgbaImage, format: &str, scale: u32) -> Result<Vec<u8>, String> {
    if format == "png" && scale <= 1 {
        return encode_png(img);
    }
    let mut dyn_img = image::DynamicImage::ImageRgba8(
        image::RgbaImage::from_raw(img.width, img.height, img.data.clone())
            .ok_or_else(|| "frame buffer size mismatch".to_string())?,
    );
    if scale > 1 {
        dyn_img = dyn_img.resize_exact(
            img.width * scale,
            img.height * scale,
            image::imageops::FilterType::Triangle,
        );
    }
    let mut out: Vec<u8> = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut out);
    let written = match format {
        "jpg" => image::DynamicImage::ImageRgb8(dyn_img.to_rgb8())
            .write_to(&mut cursor, image::ImageFormat::Jpeg),
        "webp" => dyn_img.write_to(&mut cursor, image::ImageFormat::WebP),
        _ => dyn_img.write_to(&mut cursor, image::ImageFormat::Png),
    };
    written.map_err(|e| e.to_string())?;
    Ok(out)
}

/// Atomic file write: temp + validate-by-readback + rename.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    let back = std::fs::read(&tmp).map_err(|e| e.to_string())?;
    if back != bytes {
        let _ = std::fs::remove_file(&tmp);
        return Err("write verification failed".into());
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- commands

#[derive(serde::Serialize)]
struct EnvInfo {
    backend: String,
    ux: String,
    session_type: String,
    desktop: String,
    portal_screenshot: bool,
    portal_screencast: bool,
    portal_global_shortcuts: bool,
}

#[tauri::command]
fn env_info() -> EnvInfo {
    let d = shot_capture::detect();
    EnvInfo {
        backend: format!("{:?}", d.kind),
        ux: format!("{:?}", d.ux),
        session_type: d.session_type,
        desktop: d.desktop,
        portal_screenshot: d.portal_screenshot,
        portal_screencast: d.portal_screencast,
        portal_global_shortcuts: d.portal_global_shortcuts,
    }
}

#[tauri::command]
async fn capture(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    interactive: Option<bool>,
) -> Result<serde_json::Value, String> {
    let interactive = interactive.unwrap_or(true);
    let forced = load_settings().backend;
    let arc = state.0.clone();
    // An instant grab must not photograph our own window: hide it, let
    // the compositor settle, grab, come back. The picker path is driven
    // by the compositor, whose UI covers us anyway.
    let window = app.get_webview_window("main");
    if !interactive {
        if let Some(w) = &window {
            let _ = w.hide();
        }
    }
    let result = tauri::async_runtime::spawn_blocking(move || {
        if !interactive {
            std::thread::sleep(std::time::Duration::from_millis(220));
        }
        // Backend selection: a forced backend (gate 7) wins; the forced
        // backends that aren't implemented yet answer with their honest
        // Unsupported reason, straight from shot-capture.
        let detection = shot_capture::detect();
        let path = match forced.as_str() {
            "portal" => {
                shot_capture::portal::capture_full_opts(interactive).map_err(|e| e.to_string())?
            }
            "wlroots" | "x11" => {
                let mut det = detection.clone();
                det.kind = if forced == "wlroots" {
                    shot_capture::BackendKind::Wlroots
                } else {
                    shot_capture::BackendKind::X11
                };
                shot_capture::backend_for(&det).capture_full().map_err(|e| e.to_string())?
            }
            _ if detection.kind == shot_capture::BackendKind::Portal => {
                shot_capture::portal::capture_full_opts(interactive).map_err(|e| e.to_string())?
            }
            _ => shot_capture::auto_backend().capture_full().map_err(|e| e.to_string())?,
        };
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let img = decode_png(&bytes)?;
        let doc = Document::new(ImageRef {
            path: path.to_string_lossy().into_owned(),
            width: img.width,
            height: img.height,
            sha256: None,
        });
        let (w, h) = (img.width, img.height);
        let mut guard = arc.lock().unwrap();
        *guard = Some(Session { source: img, doc, rev: 1, rendered: None, history: Vec::new(), future: Vec::new() });
        Ok::<_, String>(serde_json::json!({
            "path": path.to_string_lossy(),
            "width": w,
            "height": h,
            "rev": 1,
        }))
    })
    .await
    .map_err(|e| e.to_string());
    if !interactive {
        if let Some(w) = &window {
            let _ = w.show();
            let _ = w.set_focus();
        }
    }
    result?
}

#[tauri::command]
fn get_doc(state: State<'_, AppState>) -> Option<Document> {
    state.0.lock().unwrap().as_ref().map(|s| s.doc.clone())
}

fn with_session<T>(
    state: &State<'_, AppState>,
    f: impl FnOnce(&mut Session) -> Result<T, String>,
) -> Result<T, String> {
    let mut guard = state.0.lock().unwrap();
    let session = guard.as_mut().ok_or("no capture yet")?;
    f(session)
}

#[tauri::command]
fn add_primitive(
    state: State<'_, AppState>,
    kind: PrimitiveKind,
    style: Option<Style>,
) -> Result<serde_json::Value, String> {
    with_session(&state, |s| {
        s.snapshot(false);
        let id = s.doc.add(kind, style.unwrap_or_default());
        s.bump();
        Ok(serde_json::json!({ "id": id, "rev": s.rev }))
    })
}

#[tauri::command]
fn move_primitive(
    state: State<'_, AppState>,
    id: u64,
    dx: f32,
    dy: f32,
) -> Result<serde_json::Value, String> {
    with_session(&state, |s| {
        s.snapshot(false);
        s.doc.translate_primitive(id, dx, dy).map_err(|e| e.to_string())?;
        s.bump();
        Ok(serde_json::json!({ "rev": s.rev }))
    })
}

#[tauri::command]
fn resize_primitive(
    state: State<'_, AppState>,
    id: u64,
    bbox: RectN,
) -> Result<serde_json::Value, String> {
    with_session(&state, |s| {
        s.snapshot(false);
        s.doc.resize_primitive(id, bbox).map_err(|e| e.to_string())?;
        s.bump();
        Ok(serde_json::json!({ "rev": s.rev }))
    })
}

#[tauri::command]
fn commit_crop(state: State<'_, AppState>, rect: RectN) -> Result<serde_json::Value, String> {
    with_session(&state, |s| {
        s.snapshot(true);
        let (old_w, old_h) = (s.doc.image.width, s.doc.image.height);
        let (applied, dropped) = s.doc.commit_crop(rect);
        let x = (applied.x * old_w as f32).round() as u32;
        let y = (applied.y * old_h as f32).round() as u32;
        let (nw, nh) = (s.doc.image.width, s.doc.image.height);
        let cropped = s.source.crop(x.min(old_w), y.min(old_h), nw, nh);
        // Rounding at the far edge can shave a pixel; normalize to the
        // document's exact dims so render's size guard always passes.
        s.source = if cropped.width != nw || cropped.height != nh {
            shot_render::blur::resample_nearest(&cropped, nw, nh)
        } else {
            cropped
        };
        s.bump();
        Ok(serde_json::json!({
            "rev": s.rev,
            "dropped": dropped,
            "width": nw,
            "height": nh,
        }))
    })
}

#[tauri::command]
fn remove_primitive(state: State<'_, AppState>, id: u64) -> Result<serde_json::Value, String> {
    with_session(&state, |s| {
        s.snapshot(false);
        s.doc.remove(id).map_err(|e| e.to_string())?;
        s.bump();
        Ok(serde_json::json!({ "rev": s.rev }))
    })
}

#[tauri::command]
fn set_beautify(state: State<'_, AppState>, beautify: Beautify) -> Result<serde_json::Value, String> {
    with_session(&state, |s| {
        s.snapshot(false);
        s.doc.beautify = beautify;
        s.bump();
        Ok(serde_json::json!({ "rev": s.rev }))
    })
}

#[tauri::command]
fn undo(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    with_session(&state, |s| {
        let source_changed = s.undo()?;
        Ok(serde_json::json!({
            "rev": s.rev,
            "source_changed": source_changed,
            "width": s.doc.image.width,
            "height": s.doc.image.height,
            "can_undo": !s.history.is_empty(),
            "can_redo": !s.future.is_empty(),
        }))
    })
}

#[tauri::command]
fn redo(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    with_session(&state, |s| {
        let source_changed = s.redo()?;
        Ok(serde_json::json!({
            "rev": s.rev,
            "source_changed": source_changed,
            "width": s.doc.image.width,
            "height": s.doc.image.height,
            "can_undo": !s.history.is_empty(),
            "can_redo": !s.future.is_empty(),
        }))
    })
}

#[tauri::command]
fn history_state(state: State<'_, AppState>) -> serde_json::Value {
    match state.0.lock().unwrap().as_ref() {
        Some(s) => serde_json::json!({
            "can_undo": !s.history.is_empty(),
            "can_redo": !s.future.is_empty(),
        }),
        None => serde_json::json!({ "can_undo": false, "can_redo": false }),
    }
}

fn library_dir() -> Result<std::path::PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "no HOME".to_string())?;
    Ok(std::path::PathBuf::from(home).join("Pictures").join("Screensnips"))
}

/// Export writes a trio into the library folder: the rendered image (the
/// shareable, in the chosen format/scale), the source shot PNG, and the
/// document sidecar that pairs with the source — so the library can
/// reopen the shot WITH its annotations editable, not just admire the
/// flattened export.
fn export_impl(state: &State<'_, AppState>, format: &str, scale: u32) -> Result<String, String> {
    let dir = library_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let shot_name = format!("screensnip-{}.shot.png", stamp);
    let (img_bytes, source_bytes, toml_text) = with_session(state, |s| {
        let rendered = s.rendered()?.clone();
        let img = encode_image(&rendered, format, scale)?;
        let source = encode_png(&s.source)?;
        let mut doc = s.doc.clone();
        doc.image.path = shot_name.clone();
        let toml = doc.to_toml().map_err(|e| e.to_string())?;
        Ok((img, source, toml))
    })?;
    let ext = match format {
        "jpg" => "jpg",
        "webp" => "webp",
        _ => "png",
    };
    let out_path = dir.join(format!("screensnip-{}.{}", stamp, ext));
    write_atomic(&out_path, &img_bytes)?;
    let shot_path = dir.join(&shot_name);
    write_atomic(&shot_path, &source_bytes)?;
    let sidecar_path = shot_library::sidecar_for(&shot_path);
    write_atomic(&sidecar_path, toml_text.as_bytes())?;
    Ok(out_path.to_string_lossy().into_owned())
}

#[tauri::command]
fn export_png(state: State<'_, AppState>) -> Result<String, String> {
    export_impl(&state, "png", 1)
}

#[tauri::command]
fn export_image(
    state: State<'_, AppState>,
    format: String,
    scale: Option<u32>,
) -> Result<String, String> {
    export_impl(&state, &format, scale.unwrap_or(1).clamp(1, 2))
}

#[tauri::command]
fn library_list() -> Vec<shot_library::LibraryEntry> {
    match library_dir() {
        Ok(dir) => shot_library::scan(&dir),
        Err(_) => Vec::new(),
    }
}

/// Open a library shot into the editor. With a sidecar, the full document
/// (annotations + beautify) comes back; without one, the image opens bare
/// and can be annotated fresh.
#[tauri::command]
fn library_open(
    state: State<'_, AppState>,
    shot_path: String,
) -> Result<serde_json::Value, String> {
    let path = std::path::PathBuf::from(&shot_path);
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let img = decode_png(&bytes)?;
    let sidecar = shot_library::sidecar_for(&path);
    let (doc, editable) = if sidecar.exists() {
        let text = std::fs::read_to_string(&sidecar).map_err(|e| e.to_string())?;
        let mut doc = Document::from_toml(&text).map_err(|e| e.to_string())?;
        if doc.image.width != img.width || doc.image.height != img.height {
            return Err(format!(
                "sidecar says {}x{} but the image is {}x{} — refusing to guess",
                doc.image.width, doc.image.height, img.width, img.height
            ));
        }
        doc.image.path = shot_path.clone();
        (doc, true)
    } else {
        (
            Document::new(ImageRef {
                path: shot_path.clone(),
                width: img.width,
                height: img.height,
                sha256: None,
            }),
            false,
        )
    };
    let (w, h) = (img.width, img.height);
    let mut guard = state.0.lock().unwrap();
    *guard = Some(Session { source: img, doc, rev: 1, rendered: None, history: Vec::new(), future: Vec::new() });
    Ok(serde_json::json!({ "width": w, "height": h, "rev": 1, "editable": editable }))
}

// ------------------------------------------------- settings & presets

#[tauri::command]
fn get_settings() -> Settings {
    load_settings()
}

#[tauri::command]
fn save_settings(settings: Settings) -> Result<(), String> {
    save_settings_file(&settings)
}

#[tauri::command]
fn get_presets() -> Vec<Preset> {
    load_presets()
}

#[tauri::command]
fn save_preset(name: String, beautify: Beautify) -> Result<Vec<Preset>, String> {
    let mut presets = load_presets();
    presets.retain(|p| p.name != name);
    presets.push(Preset { name, beautify });
    save_presets_file(&presets)?;
    Ok(presets)
}

#[tauri::command]
fn delete_preset(name: String) -> Result<Vec<Preset>, String> {
    let mut presets = load_presets();
    presets.retain(|p| p.name != name);
    save_presets_file(&presets)?;
    Ok(presets)
}

// ------------------------------------------------------- diagnostics

fn os_pretty_name() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "Linux".into())
}

/// The facts the Diagnostics panel shows and a bug report attaches.
fn diag_facts() -> Vec<(String, String)> {
    let d = shot_capture::detect();
    vec![
        ("Version".into(), env!("CARGO_PKG_VERSION").to_string()),
        ("OS".into(), os_pretty_name()),
        ("Backend".into(), format!("{:?}", d.kind)),
        ("Capture UX".into(), format!("{:?}", d.ux)),
        ("Session".into(), d.session_type.clone()),
        ("Desktop".into(), d.desktop.clone()),
        (
            "Portal Screenshot".into(),
            if d.portal_screenshot { "present".into() } else { "absent".into() },
        ),
        (
            "Portal GlobalShortcuts".into(),
            if d.portal_global_shortcuts {
                "present".into()
            } else {
                "absent — bind the CLI commands in your desktop's keyboard settings".into()
            },
        ),
        (
            "Text font".into(),
            shot_render::text_font_path()
                .map(|p| p.to_string())
                .unwrap_or_else(|| "bitmap fallback (no system font found)".into()),
        ),
        (
            "wl-copy (clipboard)".into(),
            if which("wl-copy").is_some() { "present".into() } else { "will be set up on first Copy".into() },
        ),
        ("Library".into(), library_dir().map(|p| p.display().to_string()).unwrap_or_default()),
        ("Config".into(), config_dir().display().to_string()),
    ]
}

#[tauri::command]
fn diag_info() -> serde_json::Value {
    let facts: serde_json::Map<String, serde_json::Value> = diag_facts()
        .into_iter()
        .map(|(k, v)| (k, serde_json::Value::String(v)))
        .collect();
    serde_json::Value::Object(facts)
}

// ---------------------------------------------------------- clipboard
//
// The no-dependency-errors law: if the session's clipboard tool is
// missing, Screensnips installs it itself (OS privilege prompt via
// pkexec — a mechanism, not an error). Failures come back as plain
// words about what the app did, never as raw tool output.

fn ensure_tool(bin: &str, package: &str) -> Result<(), String> {
    if which(bin).is_some() {
        return Ok(());
    }
    if which("apt-get").is_none() {
        return Err(format!(
            "Screensnips couldn't set up {} automatically on this system (no apt). \
             Saving still works — only Copy is affected.",
            package
        ));
    }
    let attempt = Command::new("pkexec")
        .args(["/usr/bin/apt-get", "install", "-y", package])
        .output();
    match attempt {
        Ok(_) if which(bin).is_some() => Ok(()),
        _ => Err(format!(
            "Screensnips tried to set up {} but the install didn't complete. \
             Copy will retry next time — nothing else is affected.",
            package
        )),
    }
}

fn pipe_to_clipboard(tool: &str, args: &[&str], png: &[u8]) -> Result<(), String> {
    let mut child = Command::new(tool)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "the clipboard tool wouldn't start".to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(png)
            .map_err(|_| "the clipboard tool stopped listening".to_string())?;
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("the clipboard tool didn't accept the image".into())
    }
}

#[tauri::command]
fn copy_rendered(state: State<'_, AppState>) -> Result<String, String> {
    let png = with_session(&state, |s| {
        let img = s.rendered()?.clone();
        encode_png(&img)
    })?;
    // wl-copy / xclip both fork a holder that keeps serving the image
    // after Screensnips closes — "copy, close, paste" is structural,
    // not a promise (ship gate 4).
    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        ensure_tool("wl-copy", "wl-clipboard")?;
        pipe_to_clipboard("wl-copy", &["--type", "image/png"], &png)?;
    } else {
        ensure_tool("xclip", "xclip")?;
        pipe_to_clipboard("xclip", &["-selection", "clipboard", "-t", "image/png", "-i"], &png)?;
    }
    Ok("Copied — the image stays on the clipboard even if you close Screensnips.".into())
}

// ------------------------------------------------------------ session

#[tauri::command]
fn clear_session(state: State<'_, AppState>) -> Result<(), String> {
    *state.0.lock().unwrap() = None;
    Ok(())
}

#[tauri::command]
fn startup_action() -> Option<serde_json::Value> {
    let slot = STARTUP.get()?;
    let mut guard = slot.lock().unwrap();
    guard
        .take()
        .map(|a| serde_json::json!({ "mode": a.mode, "delay": a.delay }))
}

// ------------------------------------------------- bug / feature doors

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn open_issue(kind: &str) -> Result<(), String> {
    let facts = diag_facts()
        .iter()
        .map(|(k, v)| format!("- {}: {}", k, v))
        .collect::<Vec<_>>()
        .join("\n");
    let (title, body) = if kind == "bug" {
        (
            "Bug report".to_string(),
            format!(
                "What happened:\n\n\nWhat I expected:\n\n\n---\nDiagnostics (gathered by the app):\n{}",
                facts
            ),
        )
    } else {
        (
            "Feature request".to_string(),
            "What I'd love Screensnips to do:\n\n\nWhy it matters to me:\n".to_string(),
        )
    };
    let url = format!(
        "https://github.com/garrisonr/screensnips/issues/new?title={}&body={}",
        percent_encode(&title),
        percent_encode(&body)
    );
    Command::new("xdg-open")
        .arg(&url)
        .spawn()
        .map_err(|_| "couldn't open a browser for the issue form".to_string())?;
    Ok(())
}

#[tauri::command]
fn report_bug() -> Result<(), String> {
    open_issue("bug")
}

#[tauri::command]
fn request_feature() -> Result<(), String> {
    open_issue("feature")
}

// ---------------------------------------------------------------- selftest

/// MVP feature phase: stamps + callout + spotlight render with nothing
/// left unrendered, aspect grows the canvas, and the JPG/WebP/2x export
/// encoders produce valid files.
fn selftest_features() -> Result<(), String> {
    let mut src = RgbaImage::new(400, 200);
    for px in src.data.chunks_exact_mut(4) {
        px[0] = 30;
        px[1] = 30;
        px[2] = 34;
        px[3] = 255;
    }
    use shot_model::{Point, PrimitiveKind as PK, RectN, Style};
    let mut doc = Document::new(ImageRef {
        path: "synthetic2.png".into(),
        width: 400,
        height: 200,
        sha256: None,
    });
    doc.add(
        PK::Stamp { at: Point::new(0.05, 0.1), stamp_id: "check".into(), scale: 0.12 },
        Style::default(),
    );
    doc.add(
        PK::Stamp { at: Point::new(0.25, 0.1), stamp_id: "star".into(), scale: 0.12 },
        Style::default(),
    );
    doc.add(
        PK::Callout {
            at: Point::new(0.55, 0.55),
            tail: Point::new(0.3, 0.3),
            text: "LOOK".into(),
        },
        Style { font_size: Some(0.05), ..Style::default() },
    );
    doc.add(
        PK::Spotlight { rect: RectN { x: 0.7, y: 0.05, w: 0.2, h: 0.3 } },
        Style::default(),
    );
    let out = render(&doc, &src).map_err(|e| e.to_string())?;
    if !out.unrendered.is_empty() {
        return Err(format!("features render left unrendered {:?}", out.unrendered));
    }
    // aspect: square canvas from a 2:1 shot, padding zeroed for exactness
    let mut doc3 = Document::new(ImageRef {
        path: "s3.png".into(),
        width: 400,
        height: 200,
        sha256: None,
    });
    doc3.beautify.padding = shot_model::Padding::uniform(0.0);
    doc3.beautify.shadow.enabled = false;
    doc3.beautify.aspect = Some(1.0);
    let out3 = render(&doc3, &src).map_err(|e| e.to_string())?;
    if out3.image.width != 400 || (out3.image.height as i32 - 400).abs() > 1 {
        return Err(format!("aspect canvas {}x{}", out3.image.width, out3.image.height));
    }
    // encoders: jpg + webp magic bytes, 2x png round trip at double size
    let jpg = encode_image(&out.image, "jpg", 1)?;
    if jpg.len() < 3 || jpg[0] != 0xFF || jpg[1] != 0xD8 {
        return Err("jpg magic bytes wrong".into());
    }
    let webp = encode_image(&out.image, "webp", 1)?;
    if webp.len() < 12 || &webp[0..4] != b"RIFF" || &webp[8..12] != b"WEBP" {
        return Err("webp magic bytes wrong".into());
    }
    let png2 = encode_image(&out.image, "png", 2)?;
    let back = decode_png(&png2)?;
    if back.width != out.image.width * 2 || back.height != out.image.height * 2 {
        return Err("2x export scale wrong".into());
    }
    Ok(())
}

fn selftest() -> i32 {
    let d = shot_capture::detect();
    eprintln!("selftest detect: backend={:?} ux={:?} session={}", d.kind, d.ux, d.session_type);
    if let Err(e) = selftest_features() {
        eprintln!("SELFTEST_FAIL features: {}", e);
        return 1;
    }
    // synthetic gradient source 800x450
    let mut src = RgbaImage::new(800, 450);
    for y in 0..450u32 {
        for x in 0..800u32 {
            src.set(
                x,
                y,
                shot_model::Color::rgba((x % 256) as u8, (y % 256) as u8, 128, 255),
            );
        }
    }
    let mut doc = Document::new(ImageRef {
        path: "synthetic.png".into(),
        width: 800,
        height: 450,
        sha256: None,
    });
    use shot_model::{Point, PrimitiveKind as PK, RectN, Style};
    doc.add(PK::Arrow { from: Point::new(0.1, 0.2), to: Point::new(0.6, 0.55) }, Style::default());
    doc.add(PK::StepBadge { at: Point::new(0.3, 0.3) }, Style::default());
    doc.add(PK::StepBadge { at: Point::new(0.7, 0.7) }, Style::default());
    doc.add(
        PK::Pixelate { rect: RectN { x: 0.75, y: 0.05, w: 0.2, h: 0.15 } },
        Style::default(),
    );
    match render(&doc, &src) {
        Ok(out) => {
            let expect_w = 800 + 2 * (0.06 * 800.0) as u32;
            if out.image.width != expect_w {
                eprintln!("SELFTEST_FAIL canvas width {} != {}", out.image.width, expect_w);
                return 1;
            }
            match encode_png(&out.image) {
                Ok(bytes) => {
                    let path = std::path::Path::new("/tmp/screensnips-selftest.png");
                    if write_atomic(path, &bytes).is_err() {
                        eprintln!("SELFTEST_FAIL could not write {}", path.display());
                        return 1;
                    }
                    match decode_png(&bytes) {
                        Ok(back) if back.width == out.image.width && back.height == out.image.height => {}
                        _ => {
                            eprintln!("SELFTEST_FAIL png round trip mismatch");
                            return 1;
                        }
                    }
                    match doc.to_toml().map_err(|e| e.to_string()).and_then(|t| {
                        Document::from_toml(&t).map_err(|e| e.to_string())
                    }) {
                        Ok(back) if back == doc => {}
                        _ => {
                            eprintln!("SELFTEST_FAIL sidecar round trip mismatch");
                            return 1;
                        }
                    }
                    // library round trip: sidecar next to the png, scan finds it
                    let sidecar = shot_library::sidecar_for(path);
                    let toml = match doc.to_toml() {
                        Ok(t) => t,
                        Err(e) => {
                            eprintln!("SELFTEST_FAIL toml: {}", e);
                            return 1;
                        }
                    };
                    if std::fs::write(&sidecar, toml).is_err() {
                        eprintln!("SELFTEST_FAIL could not write sidecar");
                        return 1;
                    }
                    let entries = shot_library::scan(std::path::Path::new("/tmp"));
                    let found = entries.iter().find(|e| e.file_name() == "screensnips-selftest.png");
                    match found {
                        Some(e) if e.width == 800 && e.height == 450 && e.primitive_count == 4 && e.sidecar_ok => {}
                        _ => {
                            eprintln!("SELFTEST_FAIL library scan did not find the selftest entry intact");
                            return 1;
                        }
                    }
                    println!(
                        "SELFTEST_OK canvas={}x{} png_bytes={} unrendered={} library=ok",
                        out.image.width,
                        out.image.height,
                        bytes.len(),
                        out.unrendered.len()
                    );
                    0
                }
                Err(e) => {
                    eprintln!("SELFTEST_FAIL encode: {}", e);
                    1
                }
            }
        }
        Err(e) => {
            eprintln!("SELFTEST_FAIL render: {}", e);
            1
        }
    }
}

// ---------------------------------------------------------------- main

fn percent_decode(s: &str) -> String {
    let raw = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            if let Some(hex) = std::str::from_utf8(&raw[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(hex);
                i += 3;
                continue;
            }
        }
        out.push(if raw[i] == b'+' { b' ' } else { raw[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn main() {
    if std::env::var("SCREENSNIPS_SELFTEST").is_ok() {
        std::process::exit(selftest());
    }
    parse_startup_args();
    tauri::Builder::default()
        .manage(AppState::default())
        .register_uri_scheme_protocol("shot", |ctx, request| {
            let handle = ctx.app_handle();
            let uri = request.uri().to_string();
            let state = handle.state::<AppState>();
            // Library thumbnails: shot://localhost/thumb?path=<urlencoded>
            if let Some(q) = uri.split("/thumb?").nth(1) {
                let raw_path = q
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("path="))
                    .unwrap_or("");
                let path = std::path::PathBuf::from(percent_decode(raw_path));
                let cached = state.1.lock().unwrap().get(&path).cloned();
                let thumb = match cached {
                    Some(t) => Some(t),
                    None => std::fs::read(&path)
                        .ok()
                        .and_then(|bytes| decode_png(&bytes).ok())
                        .map(|img| {
                            let t = shot_render::blur::downscale(&img, 256);
                            state.1.lock().unwrap().insert(path.clone(), t.clone());
                            t
                        }),
                };
                return match thumb {
                    Some(t) => tauri::http::Response::builder()
                        .header("Content-Type", "application/octet-stream")
                        .header("Access-Control-Allow-Origin", "*")
                        .header("Access-Control-Expose-Headers", "*")
                        .header("X-Width", t.width.to_string())
                        .header("X-Height", t.height.to_string())
                        .body(t.data)
                        .unwrap(),
                    None => tauri::http::Response::builder()
                        .status(404)
                        .header("Access-Control-Allow-Origin", "*")
                        .header("Access-Control-Expose-Headers", "*")
                        .body(b"no such shot".to_vec())
                        .unwrap(),
                };
            }
            let route = uri.rsplit('/').next().unwrap_or("").to_string();
            let mut guard = state.0.lock().unwrap();
            let response = match guard.as_mut() {
                None => tauri::http::Response::builder()
                    .status(404)
                    .header("Access-Control-Allow-Origin", "*")
                    .header("Access-Control-Expose-Headers", "*")
                    .body(b"no capture yet".to_vec())
                    .unwrap(),
                Some(session) => {
                    let img = if route == "source" {
                        session.source.clone()
                    } else {
                        match session.rendered() {
                            Ok(i) => i.clone(),
                            Err(e) => {
                                return tauri::http::Response::builder()
                                    .status(500)
                                    .header("Access-Control-Allow-Origin", "*")
                                    .header("Access-Control-Expose-Headers", "*")
                                    .body(e.into_bytes())
                                    .unwrap();
                            }
                        }
                    };
                    tauri::http::Response::builder()
                        .header("Content-Type", "application/octet-stream")
                        .header("Access-Control-Allow-Origin", "*")
                        .header("Access-Control-Expose-Headers", "*")
                        .header("X-Width", img.width.to_string())
                        .header("X-Height", img.height.to_string())
                        .header("X-Rev", session.rev.to_string())
                        .body(img.data)
                        .unwrap()
                }
            };
            response
        })
        .invoke_handler(tauri::generate_handler![
            env_info,
            capture,
            get_doc,
            add_primitive,
            move_primitive,
            resize_primitive,
            commit_crop,
            remove_primitive,
            undo,
            redo,
            history_state,
            library_list,
            library_open,
            set_beautify,
            export_png,
            export_image,
            copy_rendered,
            clear_session,
            startup_action,
            get_settings,
            save_settings,
            get_presets,
            save_preset,
            delete_preset,
            diag_info,
            report_bug,
            request_feature
        ])
        .run(tauri::generate_context!())
        .expect("error while running Screensnips");
}
