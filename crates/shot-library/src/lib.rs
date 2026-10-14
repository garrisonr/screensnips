//! The local library: a folder of PNGs with `.annotations.toml` sidecars
//! (exactly what the app exports). Scanning is read-only and total — a
//! corrupt sidecar degrades its entry to bare file facts, never an error
//! that hides the rest of the library. Search is plain substring over file
//! names for the prototype; tags + OCR text join in Phase 2.

use shot_model::Document;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LibraryEntry {
    pub shot_path: PathBuf,
    pub sidecar_path: Option<PathBuf>,
    /// From the sidecar's document when present; 0 when unknown (the app
    /// decodes the PNG on demand in that case).
    pub width: u32,
    pub height: u32,
    pub modified_unix: u64,
    pub primitive_count: usize,
    /// False when a sidecar exists but failed to parse — surfaced, not hidden.
    pub sidecar_ok: bool,
}

impl LibraryEntry {
    pub fn file_name(&self) -> String {
        self.shot_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Sidecar path convention: `<shot>.annotations.toml` next to the shot,
/// where `<shot>` includes the `.png` extension (matches the exporter).
pub fn sidecar_for(shot: &Path) -> PathBuf {
    let mut s = shot.as_os_str().to_owned();
    s.push(".annotations.toml");
    PathBuf::from(s)
}

fn entry_for(shot: PathBuf) -> LibraryEntry {
    let modified_unix = std::fs::metadata(&shot)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let sidecar = sidecar_for(&shot);
    if sidecar.exists() {
        match std::fs::read_to_string(&sidecar) {
            Ok(text) => match Document::from_toml(&text) {
                Ok(doc) => LibraryEntry {
                    shot_path: shot,
                    sidecar_path: Some(sidecar),
                    width: doc.image.width,
                    height: doc.image.height,
                    modified_unix,
                    primitive_count: doc.primitives.len(),
                    sidecar_ok: true,
                },
                Err(_) => LibraryEntry {
                    shot_path: shot,
                    sidecar_path: Some(sidecar),
                    width: 0,
                    height: 0,
                    modified_unix,
                    primitive_count: 0,
                    sidecar_ok: false,
                },
            },
            Err(_) => LibraryEntry {
                shot_path: shot,
                sidecar_path: Some(sidecar),
                width: 0,
                height: 0,
                modified_unix,
                primitive_count: 0,
                sidecar_ok: false,
            },
        }
    } else {
        LibraryEntry {
            shot_path: shot,
            sidecar_path: None,
            width: 0,
            height: 0,
            modified_unix,
            primitive_count: 0,
            sidecar_ok: true,
        }
    }
}

/// Scan a directory (non-recursive) for shots. Newest first.
pub fn scan(dir: &Path) -> Vec<LibraryEntry> {
    let mut entries: Vec<LibraryEntry> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let is_png = p
                .extension()
                .map(|x| x.eq_ignore_ascii_case("png"))
                .unwrap_or(false);
            if is_png {
                entries.push(entry_for(p));
            }
        }
    }
    entries.sort_by(|a, b| b.modified_unix.cmp(&a.modified_unix).then(a.shot_path.cmp(&b.shot_path)));
    entries
}

/// Case-insensitive substring search over file names.
pub fn search<'a>(entries: &'a [LibraryEntry], query: &str) -> Vec<&'a LibraryEntry> {
    let q = query.to_lowercase();
    entries
        .iter()
        .filter(|e| e.file_name().to_lowercase().contains(&q))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shot_model::{ImageRef, Point, PrimitiveKind, Style};

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "screensnips-lib-test-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn scan_reads_sidecars_and_degrades_gracefully() {
        let dir = temp_dir("scan");
        // one annotated shot
        let shot = dir.join("screensnip-1.png");
        std::fs::write(&shot, b"\x89PNG fake").unwrap();
        let mut doc = Document::new(ImageRef {
            path: "screensnip-1.png".into(),
            width: 2560,
            height: 1080,
            sha256: None,
        });
        doc.add(PrimitiveKind::StepBadge { at: Point::new(0.5, 0.5) }, Style::default());
        std::fs::write(sidecar_for(&shot), doc.to_toml().unwrap()).unwrap();
        // one bare shot
        std::fs::write(dir.join("plain.png"), b"\x89PNG fake").unwrap();
        // one shot with a corrupt sidecar
        let bad = dir.join("broken.png");
        std::fs::write(&bad, b"\x89PNG fake").unwrap();
        std::fs::write(sidecar_for(&bad), "this is not toml =[").unwrap();
        // noise that must be ignored
        std::fs::write(dir.join("notes.txt"), b"hi").unwrap();

        let entries = scan(&dir);
        assert_eq!(entries.len(), 3);
        let annotated = entries.iter().find(|e| e.file_name() == "screensnip-1.png").unwrap();
        assert_eq!((annotated.width, annotated.height), (2560, 1080));
        assert_eq!(annotated.primitive_count, 1);
        assert!(annotated.sidecar_ok);
        let bare = entries.iter().find(|e| e.file_name() == "plain.png").unwrap();
        assert!(bare.sidecar_path.is_none());
        let broken = entries.iter().find(|e| e.file_name() == "broken.png").unwrap();
        assert!(!broken.sidecar_ok, "corrupt sidecar must be flagged, not hidden");

        assert_eq!(search(&entries, "SCREEN").len(), 1);
        assert_eq!(search(&entries, "png").len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
