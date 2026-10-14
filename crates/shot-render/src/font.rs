//! Real-text rasterization from SYSTEM fonts (never bundled — no font
//! license baggage, per the plan). Uses the first available face from a
//! preference list (the design face Inter first, then the near-universal
//! free faces). When no font file exists, callers fall back to the 5x7
//! bitmap font in `text.rs` — text always renders, never silently drops.

use crate::image::RgbaImage;
use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use shot_model::Color;
use std::sync::OnceLock;

const CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/inter-zorin-os/Inter-Bold.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
    "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
    "/usr/share/fonts/truetype/inter-zorin-os/Inter-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans.ttf",
];

static FONT: OnceLock<Option<(FontRef<'static>, &'static str)>> = OnceLock::new();

fn font() -> Option<&'static FontRef<'static>> {
    FONT.get_or_init(|| {
        for path in CANDIDATES {
            if let Ok(bytes) = std::fs::read(path) {
                let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
                if let Ok(f) = FontRef::try_from_slice(leaked) {
                    return Some((f, path));
                }
            }
        }
        None
    })
    .as_ref()
    .map(|(f, _)| f)
}

/// Path of the font in use (diagnostics); None = bitmap fallback active.
pub fn font_path() -> Option<&'static str> {
    let _ = font();
    FONT.get().and_then(|o| o.as_ref().map(|(_, p)| *p))
}

/// The em scale that makes `height` mean "glyph box height" like the
/// bitmap font's contract: PxScale.y covers ascent+descent (~1.31 em for
/// the candidate faces), so scale down a touch.
fn scale_for(height: f32) -> PxScale {
    PxScale::from((height * 1.25).max(1.0))
}

/// Draw `text` with its top-left at (x, y); `height` is the glyph height in
/// pixels. Supports '\n'. Returns the drawn width, or None when no system
/// font is available (caller falls back to the bitmap font).
pub fn draw_text(img: &mut RgbaImage, x: f32, y: f32, height: f32, text: &str, color: Color) -> Option<f32> {
    let font = font()?;
    let scale = scale_for(height);
    let scaled = font.as_scaled(scale);
    let mut max_width = 0.0f32;
    for (line_idx, line) in text.split('\n').enumerate() {
        let baseline = y + scaled.ascent() + line_idx as f32 * height * 1.3;
        let mut cx = x;
        let mut prev: Option<ab_glyph::GlyphId> = None;
        for ch in line.chars() {
            let id = font.glyph_id(ch);
            if let Some(p) = prev {
                cx += scaled.kern(p, id);
            }
            prev = Some(id);
            let glyph = ab_glyph::Glyph { id, scale, position: point(cx, baseline) };
            if let Some(outlined) = font.outline_glyph(glyph) {
                let bb = outlined.px_bounds();
                outlined.draw(|gx, gy, coverage| {
                    let px = bb.min.x as i32 + gx as i32;
                    let py = bb.min.y as i32 + gy as i32;
                    if coverage > 0.003 && img.in_bounds(px, py) {
                        let a = (coverage * color.a as f32).clamp(0.0, 255.0) as u8;
                        if a > 0 {
                            img.blend(
                                px as u32,
                                py as u32,
                                Color::rgba(color.r, color.g, color.b, a),
                            );
                        }
                    }
                });
            }
            cx += scaled.h_advance(id);
        }
        max_width = max_width.max(cx - x);
    }
    Some(max_width)
}

/// Pixel width of the widest line at the given glyph height, if a system
/// font is available.
pub fn measure(text: &str, height: f32) -> Option<f32> {
    let font = font()?;
    let scale = scale_for(height);
    let scaled = font.as_scaled(scale);
    let mut max_width = 0.0f32;
    for line in text.split('\n') {
        let mut w = 0.0f32;
        let mut prev: Option<ab_glyph::GlyphId> = None;
        for ch in line.chars() {
            let id = font.glyph_id(ch);
            if let Some(p) = prev {
                w += scaled.kern(p, id);
            }
            prev = Some(id);
            w += scaled.h_advance(id);
        }
        max_width = max_width.max(w);
    }
    Some(max_width)
}
