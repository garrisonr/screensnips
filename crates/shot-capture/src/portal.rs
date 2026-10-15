//! xdg-desktop-portal Screenshot backend (zbus; the async core is
//! driven by `futures_lite::future::block_on` so callers stay sync).
//!
//! Flow: subscribe to the Request.Response signal for our predicted request
//! path, call org.freedesktop.portal.Screenshot.Screenshot (non-interactive),
//! wait for the response, return the captured file's path. The predicted
//! path scheme (unique-name mangling + handle token) was verified live
//! against GNOME's portal on the target machine during Step 0.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::{CaptureError, Result};

const PORTAL_DEST: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PATH: &str = "/org/freedesktop/portal/desktop";
const TIMEOUT: Duration = Duration::from_secs(120);

/// Percent-decode + strip a file:// URI into a local path.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = if let Some(p) = rest.strip_prefix("localhost") {
        p
    } else if rest.starts_with('/') {
        rest
    } else {
        // file://hostname/path — only empty host or localhost are local
        let slash = rest.find('/')?;
        &rest[slash..]
    };
    let mut bytes: Vec<u8> = Vec::with_capacity(path.len());
    let raw = path.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            let hex = std::str::from_utf8(&raw[i + 1..i + 3]).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            bytes.push(raw[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8_lossy(&bytes).into_owned()))
}

fn request_path(unique_name: &str, token: &str) -> String {
    let mangled = unique_name.trim_start_matches(':').replace('.', "_");
    format!("/org/freedesktop/portal/desktop/request/{}/{}", mangled, token)
}

/// Capture via the portal, non-interactive (instant full-screen grab).
pub fn capture_full() -> Result<PathBuf> {
    capture_full_opts(false)
}

/// Capture via the portal. With `interactive: true` the compositor shows
/// its own screenshot UI first — the user frames/selects the shot (area,
/// window, or screen) and sees what they're getting before it lands,
/// instead of capturing blind. Dickles' call, 2026-10-14: interactive is
/// the app's default; instant is the secondary fast path.
pub fn capture_full_opts(interactive: bool) -> Result<PathBuf> {
    futures_lite::future::block_on(capture_async(interactive))
}

/// The capture itself, on the async zbus API (audit fix). The previous
/// implementation spawned a thread that blocked forever on the blocking
/// signal iterator: when the portal never answered, the caller timed
/// out but the watcher thread stayed parked on the bus for the life of
/// the process, leaking one thread + connection per failed capture.
/// Here the signal wait is a future raced against the timeout on the
/// CALLING thread — when the timer wins, everything (stream,
/// connection) drops with the future and nothing outlives the call.
async fn capture_async(interactive: bool) -> Result<PathBuf> {
    use futures_lite::StreamExt;

    let conn = zbus::Connection::session()
        .await
        .map_err(|e| CaptureError::Session(e.to_string()))?;
    let unique = conn
        .unique_name()
        .map(|n| n.to_string())
        .unwrap_or_else(|| ":0.0".to_string());
    let token = format!("screensnips_{}", std::process::id());
    let req_path = request_path(&unique, &token);

    // Subscribe BEFORE the call (the ashpd pattern) so the response
    // can't race past us.
    let request_proxy = zbus::Proxy::new(
        &conn,
        PORTAL_DEST,
        req_path.as_str(),
        "org.freedesktop.portal.Request",
    )
    .await
    .map_err(|e| CaptureError::Portal(e.to_string()))?;
    let mut signals = request_proxy
        .receive_signal("Response")
        .await
        .map_err(|e| CaptureError::Portal(e.to_string()))?;

    let proxy = zbus::Proxy::new(
        &conn,
        PORTAL_DEST,
        DESKTOP_PATH,
        "org.freedesktop.portal.Screenshot",
    )
    .await
    .map_err(|e| CaptureError::Portal(e.to_string()))?;

    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("handle_token", Value::from(token));
    options.insert("modal", Value::from(false));
    options.insert("interactive", Value::from(interactive));

    let _handle: OwnedObjectPath = proxy
        .call("Screenshot", &("", options))
        .await
        .map_err(|e| CaptureError::Portal(e.to_string()))?;

    let msg = futures_lite::future::or(
        async { signals.next().await },
        async {
            async_io::Timer::after(TIMEOUT).await;
            None
        },
    )
    .await;

    let msg = match msg {
        Some(m) => m,
        None => return Err(CaptureError::Timeout),
    };
    let (code, uri) = msg
        .body()
        .deserialize::<(u32, HashMap<String, OwnedValue>)>()
        .map(|(code, results)| {
            let uri = results.get("uri").and_then(|v| match &**v {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            });
            (code, uri)
        })
        .map_err(|e| CaptureError::Portal(format!("bad Response body: {}", e)))?;
    match (code, uri) {
        (0, Some(uri)) => uri_to_path(&uri)
            .ok_or_else(|| CaptureError::Portal(format!("unparsable uri {}", uri))),
        (0, None) => Err(CaptureError::Portal("response carried no uri".into())),
        (1, _) => Err(CaptureError::Cancelled),
        (code, _) => Err(CaptureError::Portal(format!("portal response code {}", code))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_decoding() {
        assert_eq!(
            uri_to_path("file:///home/richard/Pictures/Screenshot%20One.png"),
            Some(PathBuf::from("/home/richard/Pictures/Screenshot One.png"))
        );
        assert_eq!(
            uri_to_path("file://localhost/tmp/a.png"),
            Some(PathBuf::from("/tmp/a.png"))
        );
        assert_eq!(uri_to_path("https://example.com/x.png"), None);
    }

    #[test]
    fn request_path_mangles_unique_name() {
        assert_eq!(
            request_path(":1.370", "screensnips_42"),
            "/org/freedesktop/portal/desktop/request/1_370/screensnips_42"
        );
    }
}
