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
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Where pictures are saved. Empty = the default
    /// (~/Pictures/Screensnips); "~/…" expands against $HOME.
    #[serde(default)]
    save_dir: String,
    /// Library auto-delete: shots older than this many days are swept
    /// (shot + sidecar + export together). 0 = never — the default,
    /// because deleting a man's pictures unasked is a firing offense.
    #[serde(default)]
    retention_days: u32,
    /// The user's visual signature, free-form JSON owned by the UI
    /// ({mode:"ink", strokes:[[[x,y],…]]} or {mode:"script", name}).
    /// Kept in this file, on this machine, nowhere else — it is a
    /// picture of a signature, not a credential.
    #[serde(default)]
    signature: Option<serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { backend: "auto".into(), save_dir: String::new(), retention_days: 0, signature: None }
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
    use shot_model::{Background, Color, FrameStyle, Padding, Shadow};
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
                frame: FrameStyle::None,
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
                frame: FrameStyle::None,
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
                frame: FrameStyle::None,
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
    /// Set when launched with an image path (Open With, `screensnips
    /// photo.jpg`): the frontend imports it instead of capturing.
    path: Option<String>,
}

static STARTUP: OnceLock<Mutex<Option<StartupAction>>> = OnceLock::new();

/// Set by the frontend (Esc) to abort a capture that is sitting in its
/// backend delay wait; `capture` clears it on entry and polls it while
/// waiting. The delay lives in Rust on purpose: the countdown used to be
/// a JS timer chain in the WebView, and a renderer's timers are
/// throttled or suspended whenever its window is minimized or covered —
/// exactly what a delayed menu grab sends the user off to do — so the
/// grab silently never fired (Dickles, 2026-10-16). A deadline the
/// backend keeps cannot be eaten by the compositor.
static CAPTURE_CANCEL: AtomicBool = AtomicBool::new(false);

fn parse_startup_args() {
    let mut action: Option<StartupAction> = None;
    let mut delay = 0u64;
    for arg in std::env::args().skip(1) {
        if let Some(v) = arg.strip_prefix("--capture=") {
            action = Some(StartupAction {
                mode: v.to_string(),
                delay: 0,
                path: None,
            });
        } else if let Some(v) = arg.strip_prefix("--delay=") {
            delay = v.parse().unwrap_or(0);
        } else if !arg.starts_with('-') && action.is_none() {
            let p = std::path::PathBuf::from(&arg);
            if p.is_file() && is_importable(&p) {
                action = Some(StartupAction {
                    mode: "open".to_string(),
                    delay: 0,
                    path: Some(arg.clone()),
                });
            }
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

// ------------------------------------------------------- OCR (offline)

/// Tesseract present? The app never shows a "missing dependency" dead
/// end — it offers a one-click install (polkit prompt) instead.
fn tesseract_present() -> bool {
    std::process::Command::new("tesseract")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run Tesseract over an image. Small images are upscaled 2× first —
/// Tesseract reads noticeably better with more pixels per glyph.
fn ocr_image(img: &RgbaImage) -> Result<String, String> {
    if !tesseract_present() {
        return Err("tesseract missing".into());
    }
    let work;
    let src = if img.width < 1400 {
        work = shot_render::resample_nearest(img, (img.width * 2).min(6000), (img.height * 2).min(6000));
        &work
    } else {
        img
    };
    let tmp = std::env::temp_dir().join(format!("screensnips-ocr-{}.png", std::process::id()));
    std::fs::write(&tmp, encode_png(src)?).map_err(|e| e.to_string())?;
    let out = std::process::Command::new("tesseract")
        .arg(&tmp)
        .arg("stdout")
        .arg("--psm")
        .arg("6")
        .output()
        .map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&tmp);
    let out = out?;
    if !out.status.success() {
        return Err(format!(
            "tesseract failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[tauri::command]
fn ocr_status() -> serde_json::Value {
    serde_json::json!({ "tesseract": tesseract_present() })
}

/// One-click install of the OCR engine. The polkit prompt (pkexec) is
/// the OS doing its job — this is the dependency law in action: the
/// app detects what's missing and installs it, the user just approves
/// the system prompt.
#[tauri::command]
async fn ensure_ocr() -> Result<String, String> {
    if tesseract_present() {
        return Ok("already installed".into());
    }
    // Same cross-distro installer as every other helper (audit fix —
    // this was a second apt-only hardcode, and it leaked the package
    // manager's raw stderr to the user when the install failed).
    let installed = tauri::async_runtime::spawn_blocking(|| {
        ensure_tool("tesseract", "tesseract-ocr").is_ok() && tesseract_present()
    })
    .await
    .map_err(|_| "the text-recognition setup was interrupted".to_string())?;
    if installed {
        Ok("installed".into())
    } else {
        Err("Screensnips couldn't set up text recognition on this system. \
             Saving and everything else still works — only Grab Text is affected."
            .into())
    }
}

/// Script faces the signature feature can use, best first. Dancing
/// Script (OFL) arrives via ensure_script_font; Comic Relief ships
/// with Zorin. Mirrors shot-render's SCRIPT_CANDIDATES.
const SCRIPT_FONT_FILES: &[&str] = &[
    "/usr/share/fonts/truetype/dancingscript/DancingScript-Regular.ttf",
    "/usr/share/fonts/truetype/dancingscript/DancingScript[wght].ttf",
    "/usr/share/fonts/truetype/comic-relief-zorin-os/ComicRelief-Regular.ttf",
];

fn script_font_present() -> bool {
    SCRIPT_FONT_FILES.iter().any(|p| std::path::Path::new(p).exists())
}

#[tauri::command]
fn script_font_status() -> serde_json::Value {
    serde_json::json!({ "script_font": script_font_present() })
}

/// One-click install of a real script font for signatures — same
/// dependency law as the OCR engine: detect, install, the polkit
/// prompt is the only thing the user ever sees.
#[tauri::command]
async fn ensure_script_font() -> Result<String, String> {
    if script_font_present() {
        return Ok("already installed".into());
    }
    let installed = tauri::async_runtime::spawn_blocking(|| {
        install_packages(&["fonts-dancingscript"]).is_ok() && script_font_present()
    })
    .await
    .map_err(|_| "the signature-font setup was interrupted".to_string())?;
    if installed {
        Ok("installed".into())
    } else {
        Err("Screensnips couldn't set up the script font on this system. \
             Signatures still work in the built-in fallback style."
            .into())
    }
}

/// OCR the image currently in the editor; the text lands in the
/// document (and therefore the sidecar on export) + returns for display.
#[tauri::command]
fn grab_text(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let src = {
        let guard = state.0.lock().map_err(|_| "session lock poisoned".to_string())?;
        let s = guard.as_ref().ok_or("no screenshot yet")?;
        s.source.clone()
    };
    let text = ocr_image(&src)?;
    with_session(&state, |s| {
        s.doc.ocr_text = Some(text.clone());
        Ok(serde_json::json!({ "text": text }))
    })
}

/// OCR a library shot in place: reads the file, recognizes, and stores
/// the text into its sidecar (creating a minimal sidecar if none).
#[tauri::command]
fn ocr_library_shot(shot_path: String) -> Result<serde_json::Value, String> {
    let path = checked_library_path(&shot_path)?;
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    if !path.extension().map(|e| e == "png").unwrap_or(false) {
        return Err("OCR reads the PNG source of a shot".into());
    }
    let img = decode_png(&bytes)?;
    let text = ocr_image(&img)?;
    let sidecar = shot_library::sidecar_for(&path);
    let mut doc = if sidecar.exists() {
        let t = std::fs::read_to_string(&sidecar).map_err(|e| e.to_string())?;
        Document::from_toml(&t).map_err(|e| e.to_string())?
    } else {
        Document::new(ImageRef {
            path: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            width: img.width,
            height: img.height,
            sha256: None,
        })
    };
    doc.ocr_text = Some(text.clone());
    write_atomic(&sidecar, doc.to_toml().map_err(|e| e.to_string())?.as_bytes())?;
    Ok(serde_json::json!({ "text": text }))
}

// ------------------------------------------------- library zip in/out

/// CRC-32 (IEEE) for zip entries — table-driven, no dependency.
fn crc32(data: &[u8]) -> u32 {
    static mut TABLE: [u32; 256] = [0; 256];
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        for i in 0..256u32 {
            let mut c = i;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB88320 ^ (c >> 1) } else { c >> 1 };
            }
            TABLE[i as usize] = c;
        }
    });
    let table = unsafe { &*(&raw const TABLE) };
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

/// Write a STORE-method zip (PNGs don't compress further anyway).
fn zip_write(path: &std::path::Path, entries: &[(String, Vec<u8>)]) -> Result<(), String> {
    use std::io::Write;
    let mut buf: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for (name, data) in entries {
        let offset = buf.len() as u32;
        let crc = crc32(data);
        let nb = name.as_bytes();
        let mut h = Vec::with_capacity(30);
        h.extend_from_slice(&0x04034b50u32.to_le_bytes());
        h.extend_from_slice(&20u16.to_le_bytes()); // version
        h.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 flag
        h.extend_from_slice(&0u16.to_le_bytes()); // method: store
        h.extend_from_slice(&0u16.to_le_bytes()); // time
        h.extend_from_slice(&0x21u16.to_le_bytes()); // date 1980-01-01
        h.extend_from_slice(&crc.to_le_bytes());
        h.extend_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend_from_slice(&(data.len() as u32).to_le_bytes());
        h.extend_from_slice(&(nb.len() as u16).to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // extra len
        buf.extend_from_slice(&h);
        buf.extend_from_slice(nb);
        buf.extend_from_slice(data);
        let mut c = Vec::with_capacity(46);
        c.extend_from_slice(&0x02014b50u32.to_le_bytes());
        c.extend_from_slice(&20u16.to_le_bytes());
        c.extend_from_slice(&20u16.to_le_bytes());
        c.extend_from_slice(&0x0800u16.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&0x21u16.to_le_bytes());
        c.extend_from_slice(&crc.to_le_bytes());
        c.extend_from_slice(&(data.len() as u32).to_le_bytes());
        c.extend_from_slice(&(data.len() as u32).to_le_bytes());
        c.extend_from_slice(&(nb.len() as u16).to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes()); // extra
        c.extend_from_slice(&0u16.to_le_bytes()); // comment
        c.extend_from_slice(&0u16.to_le_bytes()); // disk
        c.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        c.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        c.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(&c);
        central.extend_from_slice(nb);
    }
    let cd_offset = buf.len() as u32;
    buf.extend_from_slice(&central);
    let mut e = Vec::with_capacity(22);
    e.extend_from_slice(&0x06054b50u32.to_le_bytes());
    e.extend_from_slice(&0u16.to_le_bytes());
    e.extend_from_slice(&0u16.to_le_bytes());
    e.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    e.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    e.extend_from_slice(&(central.len() as u32).to_le_bytes());
    e.extend_from_slice(&cd_offset.to_le_bytes());
    e.extend_from_slice(&0u16.to_le_bytes());
    buf.extend_from_slice(&e);
    let mut f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    f.write_all(&buf).map_err(|e| e.to_string())
}

/// Read a zip via its central directory. Supports STORE and DEFLATE
/// (our exports are stored; Finder's are deflated — both work).
fn zip_read(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let rd_u16 = |b: &[u8], o: usize| -> u16 {
        u16::from_le_bytes([b[o], b[o + 1]])
    };
    let rd_u32 = |b: &[u8], o: usize| -> u32 {
        u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
    };
    // find end-of-central-directory (scan the tail)
    let mut eocd = None;
    let start = bytes.len().saturating_sub(66000);
    for i in (start..bytes.len().saturating_sub(3)).rev() {
        if rd_u32(bytes, i) == 0x06054b50 {
            eocd = Some(i);
            break;
        }
    }
    let eocd = eocd.ok_or("not a zip file (no end record)")?;
    let count = rd_u16(bytes, eocd + 10) as usize;
    let mut off = rd_u32(bytes, eocd + 16) as usize;
    let mut out = Vec::new();
    for _ in 0..count {
        if off + 46 > bytes.len() || rd_u32(bytes, off) != 0x02014b50 {
            return Err("corrupt zip central directory".into());
        }
        let method = rd_u16(bytes, off + 10);
        let comp_size = rd_u32(bytes, off + 20) as usize;
        let name_len = rd_u16(bytes, off + 28) as usize;
        let extra_len = rd_u16(bytes, off + 30) as usize;
        let comment_len = rd_u16(bytes, off + 32) as usize;
        let local_off = rd_u32(bytes, off + 42) as usize;
        let name = String::from_utf8_lossy(&bytes[off + 46..off + 46 + name_len]).into_owned();
        // jump to the local header to find the data
        if local_off + 30 > bytes.len() || rd_u32(bytes, local_off) != 0x04034b50 {
            return Err("corrupt zip local header".into());
        }
        let ln = rd_u16(bytes, local_off + 26) as usize;
        let le = rd_u16(bytes, local_off + 28) as usize;
        let data_start = local_off + 30 + ln + le;
        let data_end = data_start + comp_size;
        if data_end > bytes.len() {
            return Err("zip entry overruns the file".into());
        }
        let raw = &bytes[data_start..data_end];
        let data = match method {
            0 => raw.to_vec(),
            8 => {
                use std::io::Read;
                let mut dec = flate2::read::DeflateDecoder::new(raw);
                let mut v = Vec::new();
                dec.read_to_end(&mut v).map_err(|e| e.to_string())?;
                v
            }
            m => return Err(format!("unsupported zip method {}", m)),
        };
        out.push((name, data));
        off += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

#[tauri::command]
fn export_library_zip() -> Result<serde_json::Value, String> {
    let dir = library_dir()?;
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut read = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
    while let Some(ent) = read.next() {
        let ent = ent.map_err(|e| e.to_string())?;
        let p = ent.path();
        if !p.is_file() {
            continue;
        }
        let name = ent.file_name().to_string_lossy().into_owned();
        match std::fs::read(&p) {
            Ok(data) => entries.push((name, data)),
            Err(_) => continue,
        }
    }
    if entries.is_empty() {
        return Err("library is empty — nothing to export".into());
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let out_path = std::path::PathBuf::from(home)
        .join("Downloads")
        .join(format!("screensnips-library-{}.zip", stamp));
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    zip_write(&out_path, &entries)?;
    Ok(serde_json::json!({
        "path": out_path.to_string_lossy(),
        "files": entries.len(),
    }))
}

#[tauri::command]
fn import_library_zip(zip_path: String) -> Result<serde_json::Value, String> {
    let dir = library_dir()?;
    let bytes = std::fs::read(&zip_path).map_err(|e| format!("can't read that file: {}", e))?;
    let entries = zip_read(&bytes)?;
    let mut imported = 0usize;
    let mut skipped = 0usize;
    for (name, data) in &entries {
        // flat import only: basename, allowlisted extensions
        let base = std::path::Path::new(name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let lower = base.to_lowercase();
        let ok_ext = [".png", ".jpg", ".jpeg", ".webp", ".toml"]
            .iter()
            .any(|e| lower.ends_with(e));
        if base.is_empty() || !ok_ext {
            skipped += 1;
            continue;
        }
        let target = dir.join(&base);
        if target.exists() {
            skipped += 1;
            continue;
        }
        std::fs::write(&target, data).map_err(|e| e.to_string())?;
        imported += 1;
    }
    Ok(serde_json::json!({ "imported": imported, "skipped": skipped }))
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
fn cancel_capture() {
    CAPTURE_CANCEL.store(true, Ordering::SeqCst);
}

#[tauri::command]
async fn capture(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    interactive: Option<bool>,
    delay_secs: Option<u64>,
) -> Result<serde_json::Value, String> {
    let interactive = interactive.unwrap_or(true);
    // The delayed-grab wait (see CAPTURE_CANCEL above). The picker
    // never waits: a delay always implies a full-screen menu grab, and
    // Instant passes no delay at all.
    let wait_secs = if interactive { 0 } else { delay_secs.unwrap_or(0) };
    CAPTURE_CANCEL.store(false, Ordering::SeqCst);
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
        if wait_secs > 0 {
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
            while std::time::Instant::now() < deadline {
                if CAPTURE_CANCEL.load(Ordering::SeqCst) {
                    return Err("capture cancelled".to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
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
fn rotate_primitive(
    state: State<'_, AppState>,
    id: u64,
    deg: f32,
) -> Result<serde_json::Value, String> {
    with_session(&state, |s| {
        // Peek first so a non-rotatable primitive doesn't burn a
        // history entry on a failed attempt.
        let is_stamp = matches!(
            s.doc.get(id).map(|p| &p.kind),
            Some(PrimitiveKind::Stamp { .. })
        );
        if !is_stamp {
            return Err("only stamps can rotate (for now)".into());
        }
        s.snapshot(false);
        s.doc.set_rotation(id, deg).map_err(|e| e.to_string())?;
        s.bump();
        Ok(serde_json::json!({ "rev": s.rev }))
    })
}

#[tauri::command]
fn set_bend(
    state: State<'_, AppState>,
    id: u64,
    bend: f32,
) -> Result<serde_json::Value, String> {
    with_session(&state, |s| {
        let is_arrow = matches!(
            s.doc.get(id).map(|p| &p.kind),
            Some(PrimitiveKind::Arrow { .. })
        );
        if !is_arrow {
            return Err("only arrows bend".into());
        }
        s.snapshot(false);
        s.doc.set_bend(id, bend).map_err(|e| e.to_string())?;
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
    let custom = load_settings().save_dir.trim().to_string();
    if !custom.is_empty() {
        if let Some(rest) = custom.strip_prefix("~/") {
            let home = std::env::var("HOME").map_err(|_| "no HOME".to_string())?;
            return Ok(std::path::PathBuf::from(home).join(rest));
        }
        return Ok(std::path::PathBuf::from(custom));
    }
    let home = std::env::var("HOME").map_err(|_| "no HOME".to_string())?;
    Ok(std::path::PathBuf::from(home).join("Pictures").join("Screensnips"))
}

/// Export writes a trio into the library folder: the rendered image (the
/// shareable, in the chosen format/scale), the source shot PNG, and the
/// document sidecar that pairs with the source — so the library can
/// reopen the shot WITH its annotations editable, not just admire the
/// flattened export.
/// Turn a user-typed shot name into a safe filename stem: letters,
/// numbers, spaces, dashes, underscores, parentheses survive; every
/// path hazard becomes a dash. None when nothing usable remains.
fn sanitize_stem(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '(' | ')' | '\'') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('-').trim().to_string();
    let cleaned: String = cleaned.chars().take(80).collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn export_impl(
    state: &State<'_, AppState>,
    format: &str,
    scale: u32,
    name: Option<&str>,
) -> Result<String, String> {
    let dir = library_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let ext = match format {
        "jpg" => "jpg",
        "webp" => "webp",
        _ => "png",
    };
    // Named shots are searchable shots: a user name wins over the
    // timestamp stem, and collisions get " 2", " 3", … rather than
    // ever overwriting somebody's picture.
    let named = name.and_then(sanitize_stem);
    let base = named.unwrap_or_else(|| format!("screensnip-{}", stamp));
    let mut stem = base.clone();
    let mut n = 2;
    loop {
        let shot_taken = dir.join(format!("{}.shot.png", stem)).exists();
        let out_taken = dir.join(format!("{}.{}", stem, ext)).exists();
        if !shot_taken && !out_taken {
            break;
        }
        stem = format!("{} {}", base, n);
        n += 1;
        if n > 999 {
            return Err("too many shots with that name".into());
        }
    }
    let shot_name = format!("{}.shot.png", stem);
    let (img_bytes, source_bytes, toml_text) = with_session(state, |s| {
        let rendered = s.rendered()?.clone();
        let img = encode_image(&rendered, format, scale)?;
        let source = encode_png(&s.source)?;
        // Quietly read the shot's text on the way out (when the OCR
        // engine is installed) so the library is searchable by
        // content from day one. Best-effort: export never fails for OCR.
        if s.doc.ocr_text.is_none() && tesseract_present() {
            if let Ok(text) = ocr_image(&s.source) {
                if !text.is_empty() {
                    s.doc.ocr_text = Some(text);
                }
            }
        }
        let mut doc = s.doc.clone();
        doc.image.path = shot_name.clone();
        let toml = doc.to_toml().map_err(|e| e.to_string())?;
        Ok((img, source, toml))
    })?;
    let out_path = dir.join(format!("{}.{}", stem, ext));
    write_atomic(&out_path, &img_bytes)?;
    let shot_path = dir.join(&shot_name);
    write_atomic(&shot_path, &source_bytes)?;
    let sidecar_path = shot_library::sidecar_for(&shot_path);
    write_atomic(&sidecar_path, toml_text.as_bytes())?;
    Ok(out_path.to_string_lossy().into_owned())
}

#[tauri::command]
fn export_png(state: State<'_, AppState>, name: Option<String>) -> Result<String, String> {
    export_impl(&state, "png", 1, name.as_deref())
}

#[tauri::command]
fn export_image(
    state: State<'_, AppState>,
    format: String,
    scale: Option<u32>,
    name: Option<String>,
) -> Result<String, String> {
    export_impl(&state, &format, scale.unwrap_or(1).clamp(1, 2), name.as_deref())
}

/// Rename a library shot: the shot file, its sidecar, and its flattened
/// export twin move together, and the sidecar's stored image path is
/// rewritten to match. Refuses to overwrite an existing shot.
#[tauri::command]
fn rename_shot(shot_path: String, new_name: String) -> Result<String, String> {
    let stem = sanitize_stem(&new_name)
        .ok_or_else(|| "that name doesn't leave anything usable — try letters and numbers".to_string())?;
    let src = std::path::PathBuf::from(&shot_path);
    let dir = src
        .parent()
        .ok_or_else(|| "shot has no folder".to_string())?
        .to_path_buf();
    let src_name = src
        .file_name()
        .ok_or_else(|| "shot has no name".to_string())?
        .to_string_lossy()
        .into_owned();
    let is_doc = src_name.to_lowercase().ends_with(".shot.png");
    let new_shot_name = if is_doc {
        format!("{}.shot.png", stem)
    } else {
        let ext = src.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "png".into());
        format!("{}.{}", stem, ext)
    };
    let new_shot = dir.join(&new_shot_name);
    if new_shot.exists() && new_shot != src {
        return Err("a shot with that name already exists".into());
    }
    let mut ops: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
    if new_shot != src {
        ops.push((src.clone(), new_shot.clone()));
    }
    let old_sidecar = shot_library::sidecar_for(&src);
    if old_sidecar.exists() {
        ops.push((old_sidecar, shot_library::sidecar_for(&new_shot)));
    }
    if is_doc {
        let old_stem = src_name.trim_end_matches(".shot.png").to_string();
        let old_stem = if old_stem == src_name {
            src_name.trim_end_matches(".SHOT.PNG").to_string()
        } else {
            old_stem
        };
        for ext in ["png", "jpg", "webp"] {
            let twin = dir.join(format!("{}.{}", old_stem, ext));
            let new_twin = dir.join(format!("{}.{}", stem, ext));
            if twin.exists() && twin != src && new_twin != twin {
                if new_twin.exists() {
                    return Err("an export with the new name already exists".into());
                }
                ops.push((twin, new_twin));
            }
        }
    }
    for (a, b) in &ops {
        std::fs::rename(a, b).map_err(|e| e.to_string())?;
    }
    // Keep the sidecar's stored image path honest.
    let new_sidecar = shot_library::sidecar_for(&new_shot);
    if new_sidecar.exists() {
        if let Ok(text) = std::fs::read_to_string(&new_sidecar) {
            if let Ok(mut doc) = Document::from_toml(&text) {
                doc.image.path = new_shot_name.clone();
                if let Ok(t) = doc.to_toml() {
                    let _ = write_atomic(&new_sidecar, t.as_bytes());
                }
            }
        }
    }
    Ok(new_shot.to_string_lossy().into_owned())
}

/// Every file that belongs to a library card: the shot, its sidecar,
/// and any flattened export twin. Deleting a shot means all of them —
/// orphans are how libraries rot.
fn shot_files(shot_path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![shot_path.to_path_buf()];
    let sidecar = shot_library::sidecar_for(shot_path);
    if sidecar.exists() {
        out.push(sidecar);
    }
    let name = shot_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.to_lowercase().ends_with(".shot.png") {
        let stem = name[..name.len() - ".shot.png".len()].to_string();
        if let Some(dir) = shot_path.parent() {
            for ext in ["png", "jpg", "webp"] {
                let twin = dir.join(format!("{}.{}", stem, ext));
                if twin.exists() && !out.contains(&twin) {
                    out.push(twin);
                }
            }
        }
    }
    out
}

fn remove_shot_files(shot_path: &std::path::Path) -> (usize, u64) {
    let mut count = 0usize;
    let mut bytes = 0u64;
    for f in shot_files(shot_path) {
        if let Ok(m) = std::fs::metadata(&f) {
            bytes += m.len();
        }
        if std::fs::remove_file(&f).is_ok() {
            count += 1;
        }
    }
    (count, bytes)
}

/// Canonicalize `candidate` and keep it only if it lives inside the
/// already-canonicalized `canon_dir`. Symlinks and `..` escapes resolve
/// to their real targets before the check, so neither can smuggle a
/// path out of the fence. None when the file doesn't exist.
fn fenced_within(
    canon_dir: &std::path::Path,
    candidate: &std::path::Path,
) -> Option<std::path::PathBuf> {
    let canon = std::fs::canonicalize(candidate).ok()?;
    if canon.starts_with(canon_dir) {
        Some(canon)
    } else {
        None
    }
}

/// The shot:// thumbnail route's fence (audit fix): only files inside
/// the library folder may be served — the webview must never be able
/// to turn the protocol into a read-any-file oracle.
fn fenced_library_path(candidate: &std::path::Path) -> Option<std::path::PathBuf> {
    let dir = library_dir().ok()?;
    let canon_dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    fenced_within(&canon_dir, candidate)
}

/// Library paths only — a delete command that honors arbitrary paths
/// is a loaded gun pointed at the whole disk.
fn checked_library_path(raw: &str) -> Result<std::path::PathBuf, String> {
    let dir = library_dir()?;
    let p = std::path::PathBuf::from(raw);
    let parent = p.parent().ok_or_else(|| "no folder".to_string())?;
    if parent != dir {
        return Err("that shot is not in the library folder".into());
    }
    Ok(p)
}

#[tauri::command]
fn delete_shot(shot_path: String) -> Result<serde_json::Value, String> {
    let p = checked_library_path(&shot_path)?;
    let (files, bytes) = remove_shot_files(&p);
    Ok(serde_json::json!({ "files_removed": files, "bytes_freed": bytes }))
}

#[tauri::command]
fn delete_shots(paths: Vec<String>) -> Result<serde_json::Value, String> {
    let mut shots = 0usize;
    let mut files = 0usize;
    let mut bytes = 0u64;
    for raw in &paths {
        let p = checked_library_path(raw)?;
        let (f, b) = remove_shot_files(&p);
        if f > 0 {
            shots += 1;
        }
        files += f;
        bytes += b;
    }
    Ok(serde_json::json!({ "shots": shots, "files_removed": files, "bytes_freed": bytes }))
}

/// The retention sweep: with retention set, shots older than it are
/// deleted whole (shot + sidecar + export). Runs at startup and on
/// demand — never silently mid-session.
#[tauri::command]
fn library_sweep() -> Result<serde_json::Value, String> {
    let days = load_settings().retention_days;
    if days == 0 {
        return Ok(serde_json::json!({ "retention_days": 0, "deleted": 0, "bytes_freed": 0 }));
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let cutoff = now.saturating_sub(days as u64 * 86400);
    let mut deleted = 0usize;
    let mut bytes = 0u64;
    for card in library_list() {
        if card.modified_unix > 0 && card.modified_unix < cutoff {
            let (_, b) = remove_shot_files(std::path::Path::new(&card.shot_path));
            deleted += 1;
            bytes += b;
        }
    }
    Ok(serde_json::json!({ "retention_days": days, "deleted": deleted, "bytes_freed": bytes }))
}

fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// 64-bit difference hash over a 9x8 luminance grid — near-duplicate
/// photos land within a few bits of each other.
fn dhash64(img: &RgbaImage) -> u64 {
    let small = shot_render::blur::resample_nearest(img, 9, 8);
    let mut lum = [0f32; 72];
    for y in 0..8 {
        for x in 0..9 {
            let c = small.get(x, y);
            lum[(y * 9 + x) as usize] =
                0.299 * c.r as f32 + 0.587 * c.g as f32 + 0.114 * c.b as f32;
        }
    }
    let mut h = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            let bit = if lum[y * 9 + x] > lum[y * 9 + x + 1] { 1 } else { 0 };
            h = (h << 1) | bit;
        }
    }
    h
}

/// The cleanup wizard's eyes: find exact duplicates (identical pixels),
/// near-duplicates (perceptual hash within 5 bits), and dud shots
/// (tiny, nearly blank, black, blown out, transparent). Analysis only —
/// NOTHING is deleted here; the user confirms in the wizard.
#[tauri::command]
async fn library_analyze() -> Result<serde_json::Value, String> {
    let cards = library_list();
    struct Analyzed {
        path: String,
        name: String,
        exact: u64,
        dhash: u64,
        reasons: Vec<String>,
    }
    let mut items: Vec<Analyzed> = Vec::new();
    for card in &cards {
        let bytes = match std::fs::read(&card.shot_path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let img = match decode_png(&bytes) {
            Ok(i) => i,
            Err(_) => continue,
        };
        let mut reasons: Vec<String> = Vec::new();
        if img.width < 200 || img.height < 150 {
            reasons.push(format!("tiny ({}×{})", img.width, img.height));
        }
        // sample every 7th pixel for the dud heuristics
        let mut lum_sum = 0f64;
        let mut alpha_sum = 0f64;
        let mut buckets: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        let mut samples = 0u32;
        let mut top_bucket = 0u32;
        let mut y = 0;
        while y < img.height {
            let mut x = 0;
            while x < img.width {
                let c = img.get(x, y);
                lum_sum += 0.299 * c.r as f64 + 0.587 * c.g as f64 + 0.114 * c.b as f64;
                alpha_sum += c.a as f64;
                let key = ((c.r as u32 >> 5) << 6) | ((c.g as u32 >> 5) << 3) | (c.b as u32 >> 5);
                let e = buckets.entry(key).or_insert(0);
                *e += 1;
                if *e > top_bucket {
                    top_bucket = *e;
                }
                samples += 1;
                x += 7;
            }
            y += 7;
        }
        if samples > 0 {
            let mean_lum = lum_sum / samples as f64;
            let mean_alpha = alpha_sum / samples as f64;
            if top_bucket as f64 / samples as f64 > 0.97 {
                reasons.push("nearly one solid color — probably a blank frame".into());
            }
            if mean_lum < 8.0 {
                reasons.push("almost completely black".into());
            } else if mean_lum > 247.0 {
                reasons.push("blown out — almost completely white".into());
            }
            if mean_alpha < 40.0 {
                reasons.push("mostly transparent".into());
            }
        }
        items.push(Analyzed {
            path: card.shot_path.clone(),
            name: card.file_name.clone(),
            exact: fnv1a(&img.data),
            dhash: dhash64(&img),
            reasons,
        });
    }
    // exact duplicate groups
    let mut by_exact: std::collections::HashMap<u64, Vec<usize>> = std::collections::HashMap::new();
    for (i, it) in items.iter().enumerate() {
        by_exact.entry(it.exact).or_default().push(i);
    }
    let mut in_exact: Vec<bool> = vec![false; items.len()];
    let mut dup_groups: Vec<Vec<String>> = Vec::new();
    for group in by_exact.values() {
        if group.len() > 1 {
            for &i in group {
                in_exact[i] = true;
            }
            dup_groups.push(group.iter().map(|&i| items[i].path.clone()).collect());
        }
    }
    // near-duplicate clusters among the rest (anchor-greedy)
    let mut assigned: Vec<bool> = in_exact.clone();
    let mut near_groups: Vec<Vec<String>> = Vec::new();
    for i in 0..items.len() {
        if assigned[i] {
            continue;
        }
        let mut group = vec![i];
        for j in i + 1..items.len() {
            if !assigned[j] && (items[i].dhash ^ items[j].dhash).count_ones() <= 5 {
                group.push(j);
            }
        }
        if group.len() > 1 {
            for &g in &group {
                assigned[g] = true;
            }
            near_groups.push(group.iter().map(|&i| items[i].path.clone()).collect());
        }
    }
    let duds: Vec<serde_json::Value> = items
        .iter()
        .filter(|it| !it.reasons.is_empty())
        .map(|it| serde_json::json!({ "path": it.path, "name": it.name, "reasons": it.reasons }))
        .collect();
    Ok(serde_json::json!({
        "total": items.len(),
        "duplicates": dup_groups,
        "near_duplicates": near_groups,
        "duds": duds,
    }))
}

/// One library CARD per shot — never one per file. An export writes
/// three files (render, source shot, sidecar); the rail used to list the
/// render and the source as two confusing entries. Now the flattened
/// export folds into its shot's card as an attribute, bare images (no
/// document twin) stand alone, and the UI groups cards by day so months
/// of shots stay navigable.
#[derive(serde::Serialize)]
struct LibraryCard {
    shot_path: String,
    export_path: Option<String>,
    file_name: String,
    width: u32,
    height: u32,
    modified_unix: u64,
    primitive_count: usize,
    has_sidecar: bool,
    sidecar_ok: bool,
    ocr_text: Option<String>,
}

#[tauri::command]
fn library_list() -> Vec<LibraryCard> {
    let dir = match library_dir() {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let entries = shot_library::scan(&dir);
    let is_shot_doc = |name: &str| name.to_lowercase().ends_with(".shot.png");
    let export_twin_name = |shot_name: &str| -> String {
        let stem = shot_name.strip_suffix(".shot.png").unwrap_or(shot_name);
        // strip_suffix is case-sensitive; handle the common lowercase form
        let stem = if stem == shot_name {
            shot_name.trim_end_matches(".shot.png")
        } else {
            stem
        };
        format!("{}.png", stem)
    };
    let mut export_paths: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for e in &entries {
        export_paths.insert(e.file_name(), e.shot_path.to_string_lossy().into_owned());
    }
    let mut cards = Vec::new();
    let mut folded: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &entries {
        let name = e.file_name();
        if is_shot_doc(&name) {
            let twin = export_twin_name(&name);
            let export_path = if twin != name {
                export_paths.get(&twin).cloned()
            } else {
                None
            };
            if let Some(x) = &export_path {
                folded.insert(x.clone());
            }
            cards.push(LibraryCard {
                shot_path: e.shot_path.to_string_lossy().into_owned(),
                export_path,
                file_name: name,
                width: e.width,
                height: e.height,
                modified_unix: e.modified_unix,
                primitive_count: e.primitive_count,
                has_sidecar: e.sidecar_path.is_some(),
                sidecar_ok: e.sidecar_ok,
                ocr_text: e.ocr_text.clone(),
            });
        }
    }
    for e in &entries {
        let name = e.file_name();
        if is_shot_doc(&name) {
            continue;
        }
        let path = e.shot_path.to_string_lossy().into_owned();
        if folded.contains(&path) {
            continue; // already represented as its shot's export
        }
        cards.push(LibraryCard {
            shot_path: path,
            export_path: None,
            file_name: name,
            width: e.width,
            height: e.height,
            modified_unix: e.modified_unix,
            primitive_count: e.primitive_count,
            has_sidecar: e.sidecar_path.is_some(),
            sidecar_ok: e.sidecar_ok,
            ocr_text: e.ocr_text.clone(),
        });
    }
    cards.sort_by(|a, b| b.modified_unix.cmp(&a.modified_unix));
    cards
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

// ------------------------------------------------------------ open/import
//
// Any image on disk can become a Screensnips project (Dickles,
// 2026-10-14): a phone photo, a copier scan. The file is COPIED into
// the library as a shot source (the original is never modified), a
// fresh sidecar is written, and the editor session opens on it — so
// crop, straighten, annotate, beautify and export all work exactly
// like on a native capture. Entry points: the Open button (native
// picker), drag-and-drop onto the window, a CLI path argument, and
// the desktop file's Open With registration.

const IMPORT_EXTS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

fn is_importable(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMPORT_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Decode PNG (native path) or JPEG/WebP (image crate) into the
/// renderer's raster.
fn decode_image_any(bytes: &[u8]) -> Result<RgbaImage, String> {
    if bytes.starts_with(b"\x89PNG") {
        return decode_png(bytes);
    }
    let dyn_img = image::load_from_memory(bytes).map_err(|_| {
        "that file isn't an image Screensnips can read (PNG, JPEG, or WebP)".to_string()
    })?;
    let rgba = dyn_img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok(RgbaImage {
        width: w,
        height: h,
        data: rgba.into_raw(),
    })
}

fn import_image_at(
    state: &State<'_, AppState>,
    src_path: &std::path::Path,
) -> Result<serde_json::Value, String> {
    if !is_importable(src_path) {
        return Err("Screensnips opens PNG, JPEG, or WebP images.".into());
    }
    let bytes = std::fs::read(src_path)
        .map_err(|_| "couldn't read that file — it may have moved".to_string())?;
    let img = decode_image_any(&bytes)?;
    let (w, h) = (img.width, img.height);

    let dir = library_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let raw_stem = src_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem_base = sanitize_stem(&raw_stem).unwrap_or_else(|| "Imported shot".to_string());
    // First free name wins: photo.shot.png, photo 2.shot.png, …
    let mut stem = stem_base.clone();
    let mut n = 2;
    while dir.join(format!("{}.shot.png", stem)).exists() {
        stem = format!("{} {}", stem_base, n);
        n += 1;
    }
    let shot_path = dir.join(format!("{}.shot.png", stem));
    write_atomic(&shot_path, &encode_png(&img)?)?;
    let doc = Document::new(ImageRef {
        path: shot_path.to_string_lossy().into_owned(),
        width: w,
        height: h,
        sha256: None,
    });
    let toml = doc.to_toml().map_err(|e| e.to_string())?;
    write_atomic(&shot_library::sidecar_for(&shot_path), toml.as_bytes())?;

    let mut guard = state.0.lock().unwrap();
    *guard = Some(Session {
        source: img,
        doc,
        rev: 1,
        rendered: None,
        history: Vec::new(),
        future: Vec::new(),
    });
    Ok(serde_json::json!({
        "width": w, "height": h, "rev": 1, "editable": true,
        "name": stem,
        "library_path": shot_path.to_string_lossy(),
    }))
}

#[tauri::command]
fn open_image_file(
    state: State<'_, AppState>,
    path: String,
) -> Result<serde_json::Value, String> {
    import_image_at(&state, std::path::Path::new(&path))
}

/// Run the desktop's file picker and return the chosen path (None =
/// the user cancelled). zenity drives it on GNOME, kdialog on KDE;
/// if neither exists the dependency layer sets zenity up first —
/// the user never hunts for a tool.
fn pick_image_path() -> Result<Option<String>, String> {
    if which("zenity").is_none() && which("kdialog").is_none() {
        let _ = ensure_tool("zenity", "zenity");
    }
    if let Some(zenity) = which("zenity") {
        let out = Command::new(zenity)
            .args([
                "--file-selection",
                "--title=Open an image — Screensnips",
                "--file-filter=Images | *.png *.jpg *.jpeg *.webp *.PNG *.JPG *.JPEG *.WEBP",
            ])
            .output()
            .map_err(|_| "the file picker wouldn't start".to_string())?;
        if !out.status.success() {
            return Ok(None);
        }
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return Ok(if p.is_empty() { None } else { Some(p) });
    }
    if let Some(kdialog) = which("kdialog") {
        let out = Command::new(kdialog)
            .args(["--getopenfilename", ".", "image/png image/jpeg image/webp"])
            .output()
            .map_err(|_| "the file picker wouldn't start".to_string())?;
        if !out.status.success() {
            return Ok(None);
        }
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return Ok(if p.is_empty() { None } else { Some(p) });
    }
    Err("no file picker is available on this system — drag an image onto the window instead".into())
}

#[tauri::command]
async fn open_image_picker(
    state: State<'_, AppState>,
) -> Result<Option<serde_json::Value>, String> {
    let picked = tauri::async_runtime::spawn_blocking(pick_image_path)
        .await
        .map_err(|_| "the file picker was interrupted".to_string())??;
    match picked {
        None => Ok(None),
        Some(p) => Ok(Some(import_image_at(&state, std::path::Path::new(&p))?)),
    }
}

// ------------------------------------------------- settings & presets

#[tauri::command]
fn get_settings() -> Settings {
    load_settings()
}

#[tauri::command]
fn save_settings(settings: serde_json::Value) -> Result<(), String> {
    // Merge semantics: callers send only what changed (the backend menu
    // sends {backend}, Preferences sends {save_dir}) and must never wipe
    // each other's fields.
    let mut cur = load_settings();
    if let Some(b) = settings.get("backend").and_then(|v| v.as_str()) {
        cur.backend = b.to_string();
    }
    if let Some(d) = settings.get("save_dir").and_then(|v| v.as_str()) {
        cur.save_dir = d.to_string();
    }
    if let Some(r) = settings.get("retention_days").and_then(|v| v.as_u64()) {
        cur.retention_days = r.min(3650) as u32;
    }
    if let Some(sig) = settings.get("signature") {
        cur.signature = if sig.is_null() { None } else { Some(sig.clone()) };
    }
    save_settings_file(&cur)
}

#[tauri::command]
fn save_dir_info() -> serde_json::Value {
    let s = load_settings();
    serde_json::json!({
        "custom": s.save_dir,
        "effective": library_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
    })
}

/// Factory reset: the settings and preset files are moved aside (kept
/// as timestamped .bak files — a reset you can undo by hand beats a
/// reset that eats data), starter presets re-seed, and the caller gets
/// the defaults to repaint the UI with.
#[tauri::command]
fn reset_factory() -> Result<serde_json::Value, String> {
    let dir = config_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut cleared: Vec<String> = Vec::new();
    for name in ["settings.json", "presets.json"] {
        let p = dir.join(name);
        if p.exists() {
            let bak = dir.join(format!("{}.factory-reset-{}", name, stamp));
            std::fs::rename(&p, &bak).map_err(|e| e.to_string())?;
            cleared.push(name.to_string());
        }
    }
    let presets = load_presets(); // re-seeds starters now the file is gone
    Ok(serde_json::json!({
        "cleared": cleared,
        "settings": Settings::default(),
        "preset_count": presets.len(),
    }))
}

/// Smart document detection (the straightender's eyes): find the
/// document's four corners in the current shot.
#[tauri::command]
fn detect_document(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    with_session(&state, |s| {
        Ok(match shot_render::detect::detect_document(&s.source) {
            Some(c) => serde_json::json!({
                "found": true,
                "corners": [
                    [c[0].x, c[0].y],
                    [c[1].x, c[1].y],
                    [c[2].x, c[2].y],
                    [c[3].x, c[3].y],
                ],
            }),
            None => serde_json::json!({ "found": false }),
        })
    })
}

/// Flatten the quad [TL, TR, BR, BL] (normalized) into an upright
/// document. Output size comes from the quad's own edge lengths, or an
/// exact aspect (width/height — 1.586 for ID cards, 1.42 for passports)
/// when the caller asks. The pre-warp source + document are in the
/// undo history, so Ctrl+Z brings the photo back untouched.
#[tauri::command]
fn straighten_document(
    state: State<'_, AppState>,
    corners: [[f32; 2]; 4],
    aspect: Option<f32>,
) -> Result<serde_json::Value, String> {
    with_session(&state, |s| {
        let (iw, ih) = (s.source.width as f32, s.source.height as f32);
        let px = [
            (corners[0][0] * iw, corners[0][1] * ih),
            (corners[1][0] * iw, corners[1][1] * ih),
            (corners[2][0] * iw, corners[2][1] * ih),
            (corners[3][0] * iw, corners[3][1] * ih),
        ];
        let dist = |a: (f32, f32), b: (f32, f32)| {
            ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
        };
        let w = (((dist(px[0], px[1]) + dist(px[3], px[2])) / 2.0).round()).clamp(16.0, 8000.0);
        let mut h =
            (((dist(px[0], px[3]) + dist(px[1], px[2])) / 2.0).round()).clamp(16.0, 8000.0);
        if let Some(a) = aspect.filter(|a| *a > 0.05 && *a < 20.0) {
            h = (w / a).round().clamp(16.0, 8000.0);
        }
        let warped = shot_render::warp::warp_quad(&s.source, px, w as u32, h as u32)
            .ok_or_else(|| "could not flatten that quad".to_string())?;
        s.snapshot(true);
        s.source = warped;
        let path = s.doc.image.path.clone();
        s.doc = Document::new(ImageRef {
            path,
            width: w as u32,
            height: h as u32,
            sha256: None,
        });
        s.bump();
        Ok(serde_json::json!({ "width": w, "height": h, "rev": s.rev }))
    })
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

/// The distro families the dependency layer knows how to feed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PkgFamily {
    Debian,
    Fedora,
    Arch,
    Suse,
}

fn pkg_family() -> Option<PkgFamily> {
    if which("apt-get").is_some() {
        Some(PkgFamily::Debian)
    } else if which("dnf").is_some() {
        Some(PkgFamily::Fedora)
    } else if which("pacman").is_some() {
        Some(PkgFamily::Arch)
    } else if which("zypper").is_some() {
        Some(PkgFamily::Suse)
    } else {
        None
    }
}

/// Real package names for a logical tool package, per distro family.
/// Most names match Debian's; the exceptions are mapped here. OCR
/// needs its English language data too — the engine alone would
/// install "successfully" and then read nothing.
fn distro_packages<'a>(logical: &'a str, family: PkgFamily) -> Vec<&'a str> {
    match (logical, family) {
        ("tesseract-ocr", PkgFamily::Arch) => vec!["tesseract", "tesseract-data-eng"],
        ("tesseract-ocr", PkgFamily::Fedora) => vec!["tesseract", "tesseract-langpack-eng"],
        ("tesseract-ocr", PkgFamily::Suse) => {
            vec!["tesseract-ocr", "tesseract-ocr-traineddata-english"]
        }
        ("tesseract-ocr", PkgFamily::Debian) => vec!["tesseract-ocr"],
        ("wl-clipboard", _) => vec!["wl-clipboard"],
        ("xclip", _) => vec!["xclip"],
        (other, _) => vec![other],
    }
}

/// Install packages the distro's own way (audit fix — this used to be
/// apt-only, which broke the no-dependency-errors law everywhere
/// else). PackageKit (pkcon) is preferred when present: it fronts
/// every distro and drives the graphical auth itself. Otherwise the
/// native manager runs under pkexec. Ok only on a clean exit.
fn install_packages(pkgs: &[&str]) -> Result<(), ()> {
    let out = if let Some(pkcon) = which("pkcon") {
        Command::new(pkcon)
            .arg("install")
            .arg("-y")
            .args(pkgs)
            .output()
    } else {
        match pkg_family() {
            Some(PkgFamily::Debian) => Command::new("pkexec")
                .arg(which("apt-get").ok_or(())?)
                .args(["install", "-y"])
                .args(pkgs)
                .output(),
            Some(PkgFamily::Fedora) => Command::new("pkexec")
                .arg(which("dnf").ok_or(())?)
                .args(["install", "-y"])
                .args(pkgs)
                .output(),
            Some(PkgFamily::Arch) => Command::new("pkexec")
                .arg(which("pacman").ok_or(())?)
                .args(["-S", "--needed", "--noconfirm"])
                .args(pkgs)
                .output(),
            Some(PkgFamily::Suse) => Command::new("pkexec")
                .arg(which("zypper").ok_or(())?)
                .args(["--non-interactive", "install"])
                .args(pkgs)
                .output(),
            None => return Err(()),
        }
    };
    match out {
        Ok(o) if o.status.success() => Ok(()),
        _ => Err(()),
    }
}

fn ensure_tool(bin: &str, package: &str) -> Result<(), String> {
    if which(bin).is_some() {
        return Ok(());
    }
    let family = pkg_family();
    if family.is_none() && which("pkcon").is_none() {
        return Err(format!(
            "Screensnips couldn't set up {} automatically on this system — \
             no supported package manager was found. Saving still works; \
             only the feature that needs it is affected.",
            package
        ));
    }
    // Naming follows the distro family; Debian names are the fallback
    // guess when only PackageKit is visible.
    let pkgs = distro_packages(package, family.unwrap_or(PkgFamily::Debian));
    match install_packages(&pkgs) {
        Ok(()) if which(bin).is_some() => Ok(()),
        _ => Err(format!(
            "Screensnips tried to set up {} but the install didn't complete. \
             It will retry next time — nothing else is affected.",
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
        .map(|a| serde_json::json!({ "mode": a.mode, "delay": a.delay, "path": a.path }))
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
        PK::Stamp { at: Point::new(0.05, 0.1), stamp_id: "check".into(), scale: 0.12, rotation_deg: 30.0 },
        Style::default(),
    );
    doc.add(
        PK::Stamp { at: Point::new(0.25, 0.1), stamp_id: "star".into(), scale: 0.12, rotation_deg: 0.0 },
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

/// QA evidence (audit fix pass): render EVERY primitive onto a page
/// of real text so a human can eyeball primitive truth (the pen
/// highlight, privacy-grade blur...), then run the import path
/// end-to-end on a skewed "document photo": JPEG bytes → the import
/// decoder → document detection → the straighten warp → an upright
/// page. Artifacts land in /tmp/screensnips-qa/.
fn selftest_qa() -> Result<(), String> {
    use shot_model::{Point, PrimitiveKind as PK, RectN, Style};
    let dir = std::path::Path::new("/tmp/screensnips-qa");
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let save = |img: &RgbaImage, name: &str| -> Result<(), String> {
        let bytes = encode_png(img)?;
        write_atomic(&dir.join(name), &bytes)
            .map_err(|e| format!("write {}: {}", name, e))
    };

    // ---- a page of text (1100x780; line i baseline band starts at
    // y = 60 + i*64 px, glyphs 30 px tall)
    let (pw, ph) = (1100u32, 780u32);
    let mut page = RgbaImage::new(pw, ph);
    for px in page.data.chunks_exact_mut(4) {
        px[0] = 248;
        px[1] = 248;
        px[2] = 245;
        px[3] = 255;
    }
    let ink = shot_model::Color::rgba(25, 25, 30, 255);
    let lines = [
        "Screensnips QA - primitive truth page",
        "The quick brown fox jumps over the lazy dog.",
        "Pack my box with five dozen liquor jugs.",
        "SECRET LINE account 41-2207 blur must hide this",
        "PIXEL LINE token ZX-9917-QQ pixelate this",
        "Highlight this sentence with the pen, please.",
        "How vexingly quick daft zebras jump!",
    ];
    for (i, l) in lines.iter().enumerate() {
        shot_render::text::draw_text(&mut page, 60.0, 60.0 + i as f32 * 64.0, 30.0, l, ink);
    }
    save(&page, "text-page-source.png")?;

    // ---- every primitive on the page (padding zeroed, shadow off,
    // so output pixels line up with source pixels for the blur check)
    let mut doc = Document::new(ImageRef {
        path: "qa.png".into(),
        width: pw,
        height: ph,
        sha256: None,
    });
    doc.beautify.padding = shot_model::Padding::uniform(0.0);
    doc.beautify.shadow.enabled = false;
    let red = Style {
        stroke: shot_model::Color::rgba(230, 40, 30, 255),
        stroke_width: 0.005,
        ..Style::default()
    };
    let blue = Style {
        stroke: shot_model::Color::rgba(30, 90, 230, 255),
        stroke_width: 0.004,
        ..Style::default()
    };
    doc.add(PK::Arrow { from: Point::new(0.05, 0.74), to: Point::new(0.34, 0.62), bend: 0.2 }, red.clone());
    doc.add(PK::Line { from: Point::new(0.55, 0.74), to: Point::new(0.93, 0.74) }, blue.clone());
    doc.add(PK::Rect { rect: RectN { x: 0.045, y: 0.222, w: 0.63, h: 0.072 }, corner_radius: 0.01 }, blue.clone());
    doc.add(PK::Ellipse { rect: RectN { x: 0.60, y: 0.14, w: 0.32, h: 0.072 } }, red.clone());
    doc.add(
        PK::Freehand {
            points: (0..=20)
                .map(|i| {
                    let x = 0.05 + i as f32 * 0.03;
                    Point::new(x, 0.655 + 0.012 * (i as f32 * 1.1).sin())
                })
                .collect(),
        },
        red.clone(),
    );
    doc.add(
        PK::Highlight {
            points: vec![Point::new(0.05, 0.487), Point::new(0.35, 0.478), Point::new(0.63, 0.49)],
        },
        Style {
            stroke: shot_model::Color::rgba(255, 235, 0, 255),
            stroke_width: 0.004,
            ..Style::default()
        },
    );
    doc.add(
        PK::Text { at: Point::new(0.06, 0.84), text: "a text label".into(), script: false },
        Style { stroke: ink, font_size: Some(0.035), ..Style::default() },
    );
    // (Callout sits clear of the blur/pixelate bands on purpose: an
    // earlier layout overlapped the blur rect onto the bubble's first
    // letter and the smear ate it — correct z-order, confusing QA.)
    doc.add(
        PK::Callout { at: Point::new(0.70, 0.60), tail: Point::new(0.30, 0.50), text: "callout!".into() },
        Style { stroke: ink, font_size: Some(0.04), ..Style::default() },
    );
    doc.add(PK::StepBadge { at: Point::new(0.93, 0.13) }, Style::default());
    doc.add(PK::StepBadge { at: Point::new(0.93, 0.28) }, Style::default());
    doc.add(
        PK::Stamp { at: Point::new(0.85, 0.42), stamp_id: "check".into(), scale: 0.085, rotation_deg: 0.0 },
        Style::default(),
    );
    doc.add(PK::Blur { rect: RectN { x: 0.045, y: 0.30, w: 0.72, h: 0.07 } }, Style::default());
    doc.add(PK::Pixelate { rect: RectN { x: 0.045, y: 0.383, w: 0.66, h: 0.07 } }, Style::default());
    doc.add(PK::Spotlight { rect: RectN { x: 0.03, y: 0.055, w: 0.52, h: 0.11 } }, Style::default());
    let out = render(&doc, &page).map_err(|e| e.to_string())?;
    if !out.unrendered.is_empty() {
        return Err(format!("qa render left unrendered {:?}", out.unrendered));
    }
    // Privacy check with numbers: luminance stddev inside the SECRET
    // band must collapse after the blur (text = high-frequency ink).
    let band_std = |img: &RgbaImage| -> f32 {
        let (x0, y0, x1, y1) = (60u32, 240u32, 800u32, 285u32);
        let mut vals = Vec::new();
        for y in y0..y1 {
            for x in x0..x1 {
                let c = img.get(x, y);
                vals.push(0.299 * c.r as f32 + 0.587 * c.g as f32 + 0.114 * c.b as f32);
            }
        }
        let mean = vals.iter().sum::<f32>() / vals.len() as f32;
        (vals.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / vals.len() as f32).sqrt()
    };
    let (before, after) = (band_std(&page), band_std(&out.image));
    eprintln!("qa blur band stddev: before {:.1} after {:.1}", before, after);
    if after > before * 0.35 {
        return Err(format!(
            "blur too weak: band stddev {:.1} -> {:.1} (text would still read)",
            before, after
        ));
    }
    save(&out.image, "prims-on-text.png")?;

    // ---- skewed document photo: the page rotated -8 degrees on a
    // desk, saved as a JPEG exactly like a phone would deliver it
    let (cw, ch) = (1300u32, 950u32);
    let mut photo = RgbaImage::new(cw, ch);
    for px in photo.data.chunks_exact_mut(4) {
        px[0] = 108;
        px[1] = 102;
        px[2] = 96;
        px[3] = 255;
    }
    let ang = -8.0f32.to_radians();
    let (ca, sa) = (ang.cos(), ang.sin());
    let (ccx, ccy) = (cw as f32 / 2.0, ch as f32 / 2.0);
    let (pcx, pcy) = (pw as f32 / 2.0, ph as f32 / 2.0);
    for y in 0..ch {
        for x in 0..cw {
            let (dx, dy) = (x as f32 - ccx, y as f32 - ccy);
            // inverse-rotate the photo pixel back into page space
            let sx = pcx + (ca * dx + sa * dy) / 0.94;
            let sy = pcy + (-sa * dx + ca * dy) / 0.94;
            if sx >= 0.0 && sy >= 0.0 && sx < pw as f32 && sy < ph as f32 {
                let c = page.get(sx as u32, sy as u32);
                photo.set(x, y, c);
            }
        }
    }
    save(&photo, "doc-photo-before.png")?;
    let jpg = encode_image(&photo, "jpg", 1)?;
    std::fs::write(dir.join("doc-photo-before.jpg"), &jpg).map_err(|e| e.to_string())?;

    // ---- the import path itself: JPEG bytes through the SAME
    // decoder the Open/import commands use, then detection, then the
    // straighten command's own warp math
    let bytes = std::fs::read(dir.join("doc-photo-before.jpg")).map_err(|e| e.to_string())?;
    let imported = decode_image_any(&bytes)?;
    let detected = shot_render::detect::detect_document(&imported);
    let corners = match detected {
        Some(c) => {
            eprintln!("qa import: detect_document found the page quad");
            c
        }
        None => {
            // Manual-corner fallback — also a shipped UI path (the
            // user drags the four corners). True rotated corners:
            eprintln!("qa import: detect_document missed; using true corners (manual path)");
            let fwd = |px: f32, py: f32| {
                let (dx, dy) = ((px - pcx) * 0.94, (py - pcy) * 0.94);
                Point::new(
                    (ccx + ca * dx - sa * dy) / cw as f32,
                    (ccy + sa * dx + ca * dy) / ch as f32,
                )
            };
            [fwd(0.0, 0.0), fwd(pw as f32, 0.0), fwd(pw as f32, ph as f32), fwd(0.0, ph as f32)]
        }
    };
    let (iw, ih) = (imported.width as f32, imported.height as f32);
    let px = [
        (corners[0].x * iw, corners[0].y * ih),
        (corners[1].x * iw, corners[1].y * ih),
        (corners[2].x * iw, corners[2].y * ih),
        (corners[3].x * iw, corners[3].y * ih),
    ];
    let dist = |a: (f32, f32), b: (f32, f32)| ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt();
    let w = (((dist(px[0], px[1]) + dist(px[3], px[2])) / 2.0).round()).clamp(16.0, 8000.0);
    let h = (((dist(px[0], px[3]) + dist(px[1], px[2])) / 2.0).round()).clamp(16.0, 8000.0);
    let warped = shot_render::warp::warp_quad(&imported, px, w as u32, h as u32)
        .ok_or_else(|| "warp_quad failed on the imported photo".to_string())?;
    eprintln!("qa import: straightened to {}x{}", warped.width, warped.height);
    if warped.width < 800 || warped.height < 500 {
        return Err(format!(
            "straightened page implausibly small: {}x{}",
            warped.width, warped.height
        ));
    }
    save(&warped, "doc-photo-after.png")?;
    Ok(())
}

fn selftest() -> i32 {
    let d = shot_capture::detect();
    if let Err(e) = selftest_qa() {
        eprintln!("SELFTEST_FAIL qa: {}", e);
        return 1;
    }
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
    doc.add(PK::Arrow { from: Point::new(0.1, 0.2), to: Point::new(0.6, 0.55), bend: 0.25 }, Style::default());
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
                    // Zip round-trip: our stored export AND a deflated
                    // entry (Finder-style) must both read back intact —
                    // library import/export rides on this.
                    {
                        use std::io::Write as _;
                        let zpath = std::path::Path::new("/tmp/screensnips-selftest.zip");
                        let entries = vec![
                            ("a.shot.png".to_string(), bytes.clone()),
                            ("a.shot.png.annotations.toml".to_string(), b"hello = 1".to_vec()),
                        ];
                        if zip_write(zpath, &entries).is_err() {
                            eprintln!("SELFTEST_FAIL zip write");
                            return 1;
                        }
                        let zbytes = std::fs::read(zpath).unwrap_or_default();
                        match zip_read(&zbytes) {
                            Ok(back) if back.len() == 2 && back[0].1 == bytes => {}
                            _ => {
                                eprintln!("SELFTEST_FAIL zip stored round-trip");
                                return 1;
                            }
                        }
                        // hand-build a one-entry DEFLATE zip
                        let raw = b"deflate me deflate me deflate me".to_vec();
                        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
                        let _ = enc.write_all(&raw);
                        let comp = enc.finish().unwrap_or_default();
                        let crc = crc32(&raw);
                        let mut z: Vec<u8> = Vec::new();
                        let name = b"note.txt";
                        z.extend_from_slice(&0x04034b50u32.to_le_bytes());
                        z.extend_from_slice(&20u16.to_le_bytes());
                        z.extend_from_slice(&0u16.to_le_bytes());
                        z.extend_from_slice(&8u16.to_le_bytes());
                        z.extend_from_slice(&0u16.to_le_bytes());
                        z.extend_from_slice(&0x21u16.to_le_bytes());
                        z.extend_from_slice(&crc.to_le_bytes());
                        z.extend_from_slice(&(comp.len() as u32).to_le_bytes());
                        z.extend_from_slice(&(raw.len() as u32).to_le_bytes());
                        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
                        z.extend_from_slice(&0u16.to_le_bytes());
                        z.extend_from_slice(name);
                        z.extend_from_slice(&comp);
                        let cd_off = z.len() as u32;
                        z.extend_from_slice(&0x02014b50u32.to_le_bytes());
                        z.extend_from_slice(&20u16.to_le_bytes());
                        z.extend_from_slice(&20u16.to_le_bytes());
                        z.extend_from_slice(&0u16.to_le_bytes());
                        z.extend_from_slice(&8u16.to_le_bytes());
                        z.extend_from_slice(&0u16.to_le_bytes());
                        z.extend_from_slice(&0x21u16.to_le_bytes());
                        z.extend_from_slice(&crc.to_le_bytes());
                        z.extend_from_slice(&(comp.len() as u32).to_le_bytes());
                        z.extend_from_slice(&(raw.len() as u32).to_le_bytes());
                        z.extend_from_slice(&(name.len() as u16).to_le_bytes());
                        z.extend_from_slice(&0u16.to_le_bytes());
                        z.extend_from_slice(&0u16.to_le_bytes());
                        z.extend_from_slice(&0u16.to_le_bytes());
                        z.extend_from_slice(&0u16.to_le_bytes());
                        z.extend_from_slice(&0u32.to_le_bytes());
                        z.extend_from_slice(&0u32.to_le_bytes());
                        z.extend_from_slice(name);
                        let cd_len = z.len() as u32 - cd_off;
                        z.extend_from_slice(&0x06054b50u32.to_le_bytes());
                        z.extend_from_slice(&0u16.to_le_bytes());
                        z.extend_from_slice(&0u16.to_le_bytes());
                        z.extend_from_slice(&1u16.to_le_bytes());
                        z.extend_from_slice(&1u16.to_le_bytes());
                        z.extend_from_slice(&cd_len.to_le_bytes());
                        z.extend_from_slice(&cd_off.to_le_bytes());
                        z.extend_from_slice(&0u16.to_le_bytes());
                        match zip_read(&z) {
                            Ok(back) if back.len() == 1 && back[0].1 == raw => {}
                            _ => {
                                eprintln!("SELFTEST_FAIL zip deflate round-trip");
                                return 1;
                            }
                        }
                    }
                    println!(
                        "SELFTEST_OK canvas={}x{} png_bytes={} unrendered={} library=ok zip=ok",
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
                // Audit fix (path traversal): thumbnails are served ONLY
                // for files inside the library folder, canonicalized —
                // a crafted shot:// URL must not reach arbitrary files
                // elsewhere on disk. Anything outside resolves to no
                // path here and falls through to the 404 below.
                let path = fenced_library_path(&std::path::PathBuf::from(percent_decode(raw_path)));
                let cached = path
                    .as_ref()
                    .and_then(|p| state.1.lock().unwrap().get(p).cloned());
                let thumb = match (path, cached) {
                    (_, Some(t)) => Some(t),
                    (None, None) => None,
                    (Some(path), None) => std::fs::read(&path)
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
            cancel_capture,
            get_doc,
            add_primitive,
            move_primitive,
            resize_primitive,
            rotate_primitive,
            commit_crop,
            remove_primitive,
            undo,
            redo,
            history_state,
            library_list,
            library_open,
            open_image_file,
            open_image_picker,
            set_beautify,
            export_png,
            export_image,
            copy_rendered,
            clear_session,
            startup_action,
            get_settings,
            save_settings,
            save_dir_info,
            reset_factory,
            detect_document,
            straighten_document,
            rename_shot,
            delete_shot,
            delete_shots,
            library_sweep,
            library_analyze,
            set_bend,
            ocr_status,
            ensure_ocr,
            script_font_status,
            ensure_script_font,
            grab_text,
            ocr_library_shot,
            export_library_zip,
            import_library_zip,
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


#[cfg(test)]
mod audit_fix_tests {
    use super::*;

    #[test]
    fn thumb_fence_keeps_library_files_and_rejects_escapes() {
        let base = std::env::temp_dir().join(format!("ss-fence-test-{}", std::process::id()));
        let lib = base.join("library");
        let outside = base.join("outside");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let inside_file = lib.join("shot.png");
        std::fs::write(&inside_file, b"x").unwrap();
        let outside_file = outside.join("secret.png");
        std::fs::write(&outside_file, b"x").unwrap();
        let canon_lib = std::fs::canonicalize(&lib).unwrap();

        // A real library file passes.
        assert_eq!(
            fenced_within(&canon_lib, &inside_file),
            Some(std::fs::canonicalize(&inside_file).unwrap())
        );
        // A file outside the library is refused.
        assert_eq!(fenced_within(&canon_lib, &outside_file), None);
        // A .. traversal that lands outside is refused.
        assert_eq!(
            fenced_within(&canon_lib, &lib.join("../outside/secret.png")),
            None
        );
        // A missing file is refused (no existence oracle either).
        assert_eq!(fenced_within(&canon_lib, &lib.join("nope.png")), None);

        #[cfg(unix)]
        {
            // A symlink inside the library pointing OUT is refused —
            // canonicalization resolves it before the fence check.
            let link = lib.join("escape.png");
            std::os::unix::fs::symlink(&outside_file, &link).unwrap();
            assert_eq!(fenced_within(&canon_lib, &link), None);
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
