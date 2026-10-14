//! Screensnips Step 0 — Tauri frame-pipeline bench (WebKit leg).
//!
//! Serves full-resolution RGBA frames to the webview two ways:
//! 1. Custom protocol `frame://localhost/<w>x<h>` streaming raw bytes
//!    (the architecture the plan mandates), and
//! 2. `get_frame_base64` Tauri command (frames serialized over IPC — the
//!    forbidden path, measured only to prove why it's forbidden).
//! The page times delivery + canvas presentation for both, then calls
//! `save_results`, which writes JSON atomically to $TB_OUT
//! (default /tmp/tauri-framebench-results.json).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::sync::Mutex;

static FRAMES: Mutex<Option<HashMap<(u32, u32), Vec<u8>>>> = Mutex::new(None);

fn make_frame(w: u32, h: u32) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            buf[i] = (x % 251) as u8;
            buf[i + 1] = (y % 241) as u8;
            buf[i + 2] = ((x ^ y) % 233) as u8;
            buf[i + 3] = 255;
        }
    }
    buf
}

fn frame(w: u32, h: u32) -> Vec<u8> {
    let mut guard = FRAMES.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    map.entry((w, h)).or_insert_with(|| make_frame(w, h)).clone()
}

fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// The forbidden path: a whole frame, base64'd, through command IPC.
#[tauri::command]
fn get_frame_base64(size: String) -> Result<String, String> {
    use base64::Engine;
    let (w, h) = parse_size(&size).ok_or("bad size")?;
    Ok(base64::engine::general_purpose::STANDARD.encode(frame(w, h)))
}

/// Progress/diagnostic channel: the page appends one line per step so a
/// stalled bench can be diagnosed from the log instead of dying silently.
#[tauri::command]
fn bench_log(line: String) {
    let path = std::env::var("TB_PROGRESS")
        .unwrap_or_else(|_| "/tmp/tauri-framebench-progress.log".to_string());
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{}", line);
    }
}

#[tauri::command]
fn save_results(json: String) -> Result<(), String> {
    let path = std::env::var("TB_OUT")
        .unwrap_or_else(|_| "/tmp/tauri-framebench-results.json".to_string());
    let tmp = format!("{}.tmp", path);
    std::fs::write(&tmp, &json).map_err(|e| e.to_string())?;
    // validate before rename: must parse as JSON
    let back = std::fs::read_to_string(&tmp).map_err(|e| e.to_string())?;
    serde_json::from_str::<serde_json::Value>(&back).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    println!("results saved to {}", path);
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .register_uri_scheme_protocol("frame", |_ctx, request| {
            let uri = request.uri().to_string();
            eprintln!("frame request: {}", uri);
            let size_part = uri.rsplit('/').next().unwrap_or("").to_string();
            match parse_size(&size_part) {
                Some((w, h)) => {
                    let body = frame(w, h);
                    tauri::http::Response::builder()
                        .header("Content-Type", "application/octet-stream")
                        // WebKit refuses fetch() reads of custom-scheme
                        // responses without CORS headers — the original
                        // version hung silently on exactly this.
                        .header("Access-Control-Allow-Origin", "*")
                        .header("X-Frame-Width", w.to_string())
                        .header("X-Frame-Height", h.to_string())
                        .body(body)
                        .unwrap()
                }
                None => tauri::http::Response::builder()
                    .status(400)
                    .body(b"bad size".to_vec())
                    .unwrap(),
            }
        })
        .invoke_handler(tauri::generate_handler![get_frame_base64, save_results, bench_log])
        .run(tauri::generate_context!())
        .expect("error while running tauri-framebench");
}
