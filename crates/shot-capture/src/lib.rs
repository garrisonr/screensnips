//! Screensnips capture backends.
//!
//! Design (plan §4): concrete per-environment backends behind one narrow
//! trait — the portal is the portable Wayland path, wlroots gets a
//! screencopy fast path with our own selection surface, X11 gets a grab.
//! Each backend reports exactly what UX it can offer; nothing pretends.

pub mod detect;
pub mod portal;

use std::path::PathBuf;

pub use detect::{detect, BackendKind, CaptureUx, Detection};

pub type Result<T> = std::result::Result<T, CaptureError>;

#[derive(Debug)]
pub enum CaptureError {
    /// No session bus / not in a graphical session.
    Session(String),
    /// The portal refused or answered nonsense.
    Portal(String),
    /// The user dismissed the compositor's consent/picker UI.
    Cancelled,
    /// No response within the deadline.
    Timeout,
    /// This backend can't capture here — with the reason, in plain words.
    Unsupported(String),
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::Session(m) => write!(f, "no graphical session: {}", m),
            CaptureError::Portal(m) => write!(f, "portal error: {}", m),
            CaptureError::Cancelled => write!(f, "capture cancelled by user"),
            CaptureError::Timeout => write!(f, "capture timed out"),
            CaptureError::Unsupported(m) => write!(f, "unsupported here: {}", m),
        }
    }
}
impl std::error::Error for CaptureError {}

/// A capture backend. `capture_full` returns the path of the captured PNG
/// (physical pixels). Region capture on OwnSurface backends arrives with
/// the editor surface; the portal path takes its region from the
/// compositor's picker.
pub trait CaptureBackend {
    fn kind(&self) -> BackendKind;
    fn ux(&self) -> CaptureUx;
    fn capture_full(&self) -> Result<PathBuf>;
}

pub struct PortalBackend;

impl CaptureBackend for PortalBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Portal
    }
    fn ux(&self) -> CaptureUx {
        CaptureUx::CompositorPicker
    }
    fn capture_full(&self) -> Result<PathBuf> {
        portal::capture_full()
    }
}

/// Placeholder backend for environments whose fast path lands later
/// (wlroots screencopy, X11 grab). Honest Unsupported, never a fake success.
pub struct DeferredBackend {
    kind: BackendKind,
    reason: String,
}

impl DeferredBackend {
    pub fn new(kind: BackendKind, reason: impl Into<String>) -> Self {
        DeferredBackend { kind, reason: reason.into() }
    }
}

impl CaptureBackend for DeferredBackend {
    fn kind(&self) -> BackendKind {
        self.kind
    }
    fn ux(&self) -> CaptureUx {
        match self.kind {
            BackendKind::Wlroots | BackendKind::X11 => CaptureUx::OwnSurface,
            _ => CaptureUx::Unknown,
        }
    }
    fn capture_full(&self) -> Result<PathBuf> {
        Err(CaptureError::Unsupported(self.reason.clone()))
    }
}

/// Choose the backend for this machine from detection.
pub fn backend_for(d: &Detection) -> Box<dyn CaptureBackend> {
    match d.kind {
        BackendKind::Portal => Box::new(PortalBackend),
        BackendKind::Wlroots => Box::new(DeferredBackend::new(
            BackendKind::Wlroots,
            "wlroots screencopy backend lands with the capture-surface milestone",
        )),
        BackendKind::X11 => Box::new(DeferredBackend::new(
            BackendKind::X11,
            "X11 grab backend lands with the capture-surface milestone",
        )),
        BackendKind::None => Box::new(DeferredBackend::new(
            BackendKind::None,
            "no capture path detected on this machine (no portal interface, no wlroots, no X11)",
        )),
    }
}

pub fn auto_backend() -> Box<dyn CaptureBackend> {
    backend_for(&detect())
}
