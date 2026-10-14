//! Screensnips Step 0 capability probe.
//!
//! Answers, per machine, before any product promise is made:
//! - What session/compositor is this?
//! - Is the xdg-desktop-portal present, and which interfaces do its backends expose
//!   (Screenshot / ScreenCast / GlobalShortcuts / Clipboard)?
//! - Is this a wlroots compositor where wlr-screencopy / ext-image-copy-capture
//!   could give us our own capture surface?
//! - Which capture UX will the user actually get: OUR surface (loupe, dimensions,
//!   countdown) or the COMPOSITOR's native picker?
//! - Are the runtime helpers present (PipeWire, Tesseract, wl-copy, grim-class tools)?
//! - How strong is the hardware (drives the Fast Render suggestion)?
//!
//! Pure std: env vars, PATH lookup, /proc, and portal definition files. No network.
//! Usage: screensnips-probe [--json] [--save <path>]

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Report {
    lines: Vec<String>,
    json: Vec<(String, String)>, // key -> raw json value (already encoded)
}

impl Report {
    fn new() -> Self {
        Report { lines: Vec::new(), json: Vec::new() }
    }
    fn line(&mut self, s: impl Into<String>) {
        self.lines.push(s.into());
    }
    fn kv(&mut self, key: &str, val: &str) {
        self.json.push((key.to_string(), json_str(val)));
    }
    fn kb(&mut self, key: &str, val: bool) {
        self.json.push((key.to_string(), val.to_string()));
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn which(name: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

fn env_or(key: &str) -> String {
    env::var(key).unwrap_or_else(|_| String::new())
}

/// Read portal definition files and collect the interfaces any installed backend exposes.
fn portal_interfaces() -> (bool, Vec<String>) {
    let mut dirs = vec![
        PathBuf::from("/usr/share/xdg-desktop-portal/portals"),
        PathBuf::from("/usr/local/share/xdg-desktop-portal/portals"),
    ];
    if let Some(home) = env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/xdg-desktop-portal/portals"));
    }
    let mut found_file = false;
    let mut ifaces: Vec<String> = Vec::new();
    for dir in &dirs {
        if let Ok(rd) = fs::read_dir(dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().map(|e| e == "portal").unwrap_or(false) {
                    found_file = true;
                    if let Ok(text) = fs::read_to_string(&p) {
                        for line in text.lines() {
                            let line = line.trim();
                            if let Some(rest) = line.strip_prefix("Interfaces=") {
                                for iface in rest.trim_end_matches(';').split(';') {
                                    let i = iface.trim().to_string();
                                    if !i.is_empty() && !ifaces.contains(&i) {
                                        ifaces.push(i);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    ifaces.sort();
    (found_file, ifaces)
}

fn has_iface(ifaces: &[String], suffix: &str) -> bool {
    ifaces.iter().any(|i| i.ends_with(suffix))
}

fn cpu_info() -> (String, usize) {
    let mut model = String::from("unknown");
    let mut cores = 0usize;
    if let Ok(text) = fs::read_to_string("/proc/cpuinfo") {
        for line in text.lines() {
            if line.starts_with("processor") {
                cores += 1;
            }
            if model == "unknown" && line.starts_with("model name") {
                if let Some(v) = line.split(':').nth(1) {
                    model = v.trim().to_string();
                }
            }
        }
    }
    if cores == 0 {
        cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    }
    (model, cores)
}

fn mem_total_gb() -> f64 {
    if let Ok(text) = fs::read_to_string("/proc/meminfo") {
        for line in text.lines() {
            if line.starts_with("MemTotal:") {
                if let Some(kb) = line.split_whitespace().nth(1) {
                    if let Ok(kb) = kb.parse::<f64>() {
                        return kb / 1_048_576.0;
                    }
                }
            }
        }
    }
    0.0
}

/// Ask wayland-info (if present and a Wayland display exists) which protocols are advertised.
fn wayland_protocols() -> Vec<String> {
    if env::var("WAYLAND_DISPLAY").is_err() {
        return Vec::new();
    }
    let tool = which("wayland-info").or_else(|| which("weston-info"));
    let tool = match tool {
        Some(t) => t,
        None => return Vec::new(),
    };
    let out = Command::new(tool).output();
    let mut found = Vec::new();
    if let Ok(out) = out {
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        for needle in [
            "ext_image_copy_capture_v1",
            "zwlr_screencopy_manager_v1",
            "zwlr_layer_shell_v1",
            "xdg_wm_base",
        ] {
            if text.contains(needle) {
                found.push(needle.to_string());
            }
        }
    }
    found
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let want_json = args.iter().any(|a| a == "--json");
    let save_path: Option<String> = args
        .windows(2)
        .find(|w| w[0] == "--save")
        .map(|w| w[1].clone());

    let mut r = Report::new();
    r.line("SCREENSNIPS — STEP 0 CAPABILITY PROBE");
    r.line("=====================================");

    // --- Session / compositor ---
    let session_type = env_or("XDG_SESSION_TYPE");
    let desktop = env_or("XDG_CURRENT_DESKTOP");
    let wayland_display = env::var("WAYLAND_DISPLAY").is_ok();
    let x11_display = env::var("DISPLAY").is_ok();
    let hyprland = env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok();
    let sway = env::var("SWAYSOCK").is_ok();
    let wlroots_env = hyprland || sway;
    r.line(format!("Session type:      {}", if session_type.is_empty() { "(unset)" } else { &session_type }));
    r.line(format!("Desktop:           {}", if desktop.is_empty() { "(unset)" } else { &desktop }));
    r.line(format!("Wayland display:   {}", if wayland_display { "present" } else { "absent" }));
    r.line(format!("X11 display:       {}", if x11_display { "present" } else { "absent" }));
    r.line(format!("wlroots signals:   Hyprland={} Sway={}", hyprland, sway));
    r.kv("session_type", &session_type);
    r.kv("desktop", &desktop);
    r.kb("wayland_display", wayland_display);
    r.kb("x11_display", x11_display);
    r.kb("wlroots_env", wlroots_env);

    // --- Portal ---
    let (portal_files, ifaces) = portal_interfaces();
    let p_screenshot = has_iface(&ifaces, ".Screenshot");
    let p_screencast = has_iface(&ifaces, ".ScreenCast");
    let p_shortcuts = has_iface(&ifaces, ".GlobalShortcuts");
    let p_clipboard = has_iface(&ifaces, ".Clipboard");
    let portal_bin = which("xdg-desktop-portal").is_some();
    r.line(String::new());
    r.line("PORTAL");
    r.line(format!("  portal binary:        {}", yesno(portal_bin)));
    r.line(format!("  backend definitions:  {}", yesno(portal_files)));
    r.line(format!("  Screenshot interface: {}", yesno(p_screenshot)));
    r.line(format!("  ScreenCast interface: {}", yesno(p_screencast)));
    r.line(format!("  GlobalShortcuts:      {}", yesno(p_shortcuts)));
    r.line(format!("  Clipboard interface:  {}", yesno(p_clipboard)));
    r.kb("portal_binary", portal_bin);
    r.kb("portal_screenshot", p_screenshot);
    r.kb("portal_screencast", p_screencast);
    r.kb("portal_global_shortcuts", p_shortcuts);
    r.kb("portal_clipboard", p_clipboard);

    // --- Wayland protocols (best effort) ---
    let protos = wayland_protocols();
    let ext_copy = protos.iter().any(|p| p.contains("ext_image_copy_capture"));
    let wlr_copy = protos.iter().any(|p| p.contains("zwlr_screencopy"));
    let layer_shell = protos.iter().any(|p| p.contains("zwlr_layer_shell"));
    r.line(String::new());
    r.line("WAYLAND PROTOCOLS (via wayland-info, if available)");
    if protos.is_empty() {
        r.line("  (could not query — no WAYLAND_DISPLAY or no wayland-info/weston-info)");
    }
    r.line(format!("  ext-image-copy-capture: {}", yesno(ext_copy)));
    r.line(format!("  wlr-screencopy:         {}", yesno(wlr_copy)));
    r.line(format!("  wlr-layer-shell:        {}", yesno(layer_shell)));
    r.kb("proto_ext_image_copy_capture", ext_copy);
    r.kb("proto_wlr_screencopy", wlr_copy);
    r.kb("proto_layer_shell", layer_shell);

    // --- Helper tools ---
    let tools = [
        "grim", "slurp", "wf-recorder", "spectacle", "flameshot",
        "tesseract", "pipewire", "pw-cli", "wireplumber",
        "wl-copy", "wl-paste", "hyprctl", "busctl", "gdbus",
    ];
    r.line(String::new());
    r.line("HELPER TOOLS ON PATH");
    let mut tool_map: BTreeMap<String, bool> = BTreeMap::new();
    for t in tools {
        let present = which(t).is_some();
        tool_map.insert(t.to_string(), present);
        r.line(format!("  {:<12} {}", t, yesno(present)));
    }
    let tesseract = tool_map["tesseract"];
    let pipewire = tool_map["pipewire"] || tool_map["pw-cli"];
    r.kb("tool_tesseract", tesseract);
    r.kb("tool_pipewire", pipewire);

    // --- Hardware ---
    let (cpu_model, cores) = cpu_info();
    let mem = mem_total_gb();
    let low_spec = cores <= 4 || (mem > 0.0 && mem < 8.0);
    r.line(String::new());
    r.line("HARDWARE");
    r.line(format!("  CPU: {} ({} threads)", cpu_model, cores));
    r.line(format!("  RAM: {:.1} GB", mem));
    r.line(format!("  Low-spec (Fast Render suggested): {}", yesno(low_spec)));
    r.kv("cpu_model", &cpu_model);
    r.kv("cpu_threads", &cores.to_string());
    r.kb("low_spec", low_spec);

    // --- Verdict: which capture UX does this machine get? ---
    let wlroots_surface = wlroots_env || wlr_copy || layer_shell;
    let (backend, ux) = if wlroots_surface {
        ("wlroots screencopy / ext-image-copy-capture",
         "OUR capture surface (frozen screen, loupe, dimensions, countdown)")
    } else if session_type == "x11" || (x11_display && !wayland_display) {
        ("X11 grab", "OUR capture surface (frozen screen, loupe, dimensions, countdown)")
    } else if p_screenshot || p_screencast {
        ("xdg-desktop-portal",
         "COMPOSITOR native picker (no custom loupe inside the portal picker)")
    } else if wayland_display {
        ("none detected",
         "UNKNOWN — Wayland session but no portal interface and no wlroots protocol found")
    } else {
        ("none detected",
         "UNKNOWN — no graphical session detected (headless/SSH/sandbox?)")
    };
    r.line(String::new());
    r.line("VERDICT");
    r.line(format!("  Capture backend: {}", backend));
    r.line(format!("  Capture UX:      {}", ux));
    r.line(format!("  Global hotkeys:  {}", if p_shortcuts {
        "GlobalShortcuts portal available"
    } else {
        "portal missing — fallback: map CLI capture commands in DE shortcut settings"
    }));
    r.line(format!("  OCR (Phase 2):   {}", if tesseract { "Tesseract present" } else { "Tesseract missing — app must offer install" }));
    r.kv("capture_backend", backend);
    r.kv("capture_ux", ux);

    // --- Output ---
    let text = r.lines.join("\n") + "\n";
    if want_json {
        let mut j = String::from("{\n");
        let n = r.json.len();
        for (i, (k, v)) in r.json.iter().enumerate() {
            j.push_str(&format!("  {}: {}{}\n", json_str(k), v, if i + 1 < n { "," } else { "" }));
        }
        j.push_str("}\n");
        print!("{}", j);
    } else {
        print!("{}", text);
    }
    if let Some(p) = save_path {
        // Atomic save: write temp, validate by re-reading, rename.
        let tmp = format!("{}.tmp", p);
        if fs::write(&tmp, &text).is_ok()
            && fs::read_to_string(&tmp).map(|s| s == text).unwrap_or(false)
            && fs::rename(&tmp, &p).is_ok()
        {
            eprintln!("Report saved to {}", p);
        } else {
            eprintln!("WARNING: could not save report to {}", p);
            let _ = fs::remove_file(&tmp);
        }
    }
    let _ = Path::new(""); // keep Path import used on all platforms
}

fn yesno(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}
