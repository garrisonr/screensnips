//! Environment detection: which backend can capture on this machine, and
//! which capture UX the user gets. Same heuristics as the Step 0 probe
//! (`screensnips-probe`), which validated them on the target hardware —
//! Portal backends are concrete per environment, never a leaky abstraction.

use std::env;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Portal,
    Wlroots,
    X11,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureUx {
    /// We draw the selection surface (loupe, dimensions, countdown, and the
    /// DESIGN.md handle spec).
    OwnSurface,
    /// The compositor's picker draws the selection (portal path).
    CompositorPicker,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Detection {
    pub kind: BackendKind,
    pub ux: CaptureUx,
    pub session_type: String,
    pub desktop: String,
    pub portal_screenshot: bool,
    pub portal_screencast: bool,
    pub portal_global_shortcuts: bool,
}

fn portal_interfaces() -> Vec<String> {
    let mut dirs = vec![
        PathBuf::from("/usr/share/xdg-desktop-portal/portals"),
        PathBuf::from("/usr/local/share/xdg-desktop-portal/portals"),
    ];
    if let Some(home) = env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/xdg-desktop-portal/portals"));
    }
    let mut ifaces: Vec<String> = Vec::new();
    for dir in &dirs {
        if let Ok(rd) = fs::read_dir(dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().map(|e| e == "portal").unwrap_or(false) {
                    if let Ok(text) = fs::read_to_string(&p) {
                        for line in text.lines() {
                            if let Some(rest) = line.trim().strip_prefix("Interfaces=") {
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
    ifaces
}

fn has(ifaces: &[String], suffix: &str) -> bool {
    ifaces.iter().any(|i| i.ends_with(suffix))
}

pub fn detect() -> Detection {
    let session_type = env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let desktop = env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let wayland = env::var("WAYLAND_DISPLAY").is_ok();
    let x11 = env::var("DISPLAY").is_ok();
    let wlroots = env::var("HYPRLAND_INSTANCE_SIGNATURE").is_ok() || env::var("SWAYSOCK").is_ok();

    let ifaces = portal_interfaces();
    let p_shot = has(&ifaces, ".Screenshot");
    let p_cast = has(&ifaces, ".ScreenCast");
    let p_shortcuts = has(&ifaces, ".GlobalShortcuts");

    let (kind, ux) = if wlroots {
        (BackendKind::Wlroots, CaptureUx::OwnSurface)
    } else if session_type == "x11" || (x11 && !wayland) {
        (BackendKind::X11, CaptureUx::OwnSurface)
    } else if p_shot || p_cast {
        (BackendKind::Portal, CaptureUx::CompositorPicker)
    } else if wayland {
        // Wayland with no usable portal interface and no wlroots protocol:
        // detection succeeds, capture cannot. Report honestly.
        (BackendKind::None, CaptureUx::Unknown)
    } else {
        (BackendKind::None, CaptureUx::Unknown)
    };

    Detection {
        kind,
        ux,
        session_type,
        desktop,
        portal_screenshot: p_shot,
        portal_screencast: p_cast,
        portal_global_shortcuts: p_shortcuts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_never_panics() {
        let d = detect();
        // In CI/sandbox there is usually no session: None is the honest answer.
        if d.session_type.is_empty() {
            assert_eq!(d.kind, BackendKind::None);
        }
    }
}
