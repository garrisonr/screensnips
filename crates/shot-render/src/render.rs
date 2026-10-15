//! The render pipeline: document + source image -> finished pixels.
//!
//! Order of operations (deliberate, matches the model):
//! 1. crop the source (beautify.crop)
//! 2. rasterize annotations onto the cropped shot, in z-order — privacy
//!    regions included, so later annotations can point at redacted areas
//! 3. round the shot's corners (alpha mask)
//! 4. build the canvas: background, drop shadow (downscaled-blur path),
//!    then the shot at its padded position
//!
//! Text renders through the crate's built-in bitmap font (see `text.rs`).
//! Primitives that still need machinery this crate doesn't have (stamp
//! artwork) are NOT silently skipped: their ids come back in
//! `RenderOutcome::unrendered` so the caller can say so out loud.

use crate::blur;
use crate::draw;
use crate::image::RgbaImage;
use crate::text;
use shot_model::{Background, Color, Document, ModelError, PrimitiveKind};

#[derive(Debug)]
pub enum RenderError {
    Model(ModelError),
    SizeMismatch { expected: (u32, u32), got: (u32, u32) },
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderError::Model(e) => write!(f, "model error: {}", e),
            RenderError::SizeMismatch { expected, got } => write!(
                f,
                "source size {:?} does not match document image {:?}",
                got, expected
            ),
        }
    }
}
impl std::error::Error for RenderError {}

#[derive(Debug)]
pub struct RenderOutcome {
    pub image: RgbaImage,
    /// Primitive ids that were not rasterized (text, stamps — pending the
    /// font/asset milestone). Never silently dropped.
    pub unrendered: Vec<u64>,
    /// Non-fatal notes (e.g. background fallback).
    pub warnings: Vec<String>,
}

fn to_px(nx: f32, size: u32) -> f32 {
    nx * size as f32
}

/// Apply the rounded-corner alpha mask to the shot itself.
fn apply_corner_radius(img: &mut RgbaImage, radius_px: f32) {
    if radius_px <= 0.5 {
        return;
    }
    let (w, h) = (img.width as f32, img.height as f32);
    let r = radius_px.min(w / 2.0).min(h / 2.0);
    let corners = [(r, r), (w - r, r), (r, h - r), (w - r, h - r)];
    for y in 0..img.height {
        for x in 0..img.width {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Only pixels inside a corner square can be affected.
            let in_corner_x = fx < r || fx > w - r;
            let in_corner_y = fy < r || fy > h - r;
            if !(in_corner_x && in_corner_y) {
                continue;
            }
            let (cx, cy) = corners[((fy > h / 2.0) as usize) * 2 + (fx > w / 2.0) as usize];
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let px = img.get(x, y);
            if d > r {
                let falloff = (d - r).clamp(0.0, 1.0);
                let a = (px.a as f32 * (1.0 - falloff)) as u8;
                img.set(x, y, Color::rgba(px.r, px.g, px.b, a));
            }
        }
    }
}

fn draw_annotations(doc: &Document, img: &mut RgbaImage, unrendered: &mut Vec<u64>) {
    let (iw, ih) = (img.width as f32, img.height as f32);
    let step_numbers: std::collections::HashMap<u64, u32> =
        doc.step_numbers().into_iter().collect();
    for prim in &doc.primitives {
        let s = &prim.style;
        let sw = (s.stroke_width * iw).max(1.0);
        match &prim.kind {
            PrimitiveKind::Arrow { from, to, bend } => {
                let (fx, fy) = (to_px(from.x, img.width), to_px(from.y, img.height));
                let (tx, ty) = (to_px(to.x, img.width), to_px(to.y, img.height));
                if bend.abs() < 0.001 {
                    draw::arrow(img, fx, fy, tx, ty, sw, s.stroke);
                } else {
                    // Control point in PIXEL space (matches the model
                    // formula's geometry, without aspect distortion).
                    let (dx, dy) = (tx - fx, ty - fy);
                    let len = (dx * dx + dy * dy).sqrt().max(1.0);
                    let (nx, ny) = (-dy / len, dx / len);
                    let (cx, cy) = (
                        (fx + tx) / 2.0 + nx * bend * len,
                        (fy + ty) / 2.0 + ny * bend * len,
                    );
                    draw::arrow_curved(img, fx, fy, cx, cy, tx, ty, sw, s.stroke);
                }
            }
            PrimitiveKind::Line { from, to } => {
                draw::line(
                    img,
                    to_px(from.x, img.width),
                    to_px(from.y, img.height),
                    to_px(to.x, img.width),
                    to_px(to.y, img.height),
                    sw,
                    s.stroke,
                );
            }
            PrimitiveKind::Freehand { points } => {
                for pair in points.windows(2) {
                    draw::line(
                        img,
                        to_px(pair[0].x, img.width),
                        to_px(pair[0].y, img.height),
                        to_px(pair[1].x, img.width),
                        to_px(pair[1].y, img.height),
                        sw,
                        s.stroke,
                    );
                }
            }
            PrimitiveKind::Rect { rect, corner_radius } => {
                draw::rect(
                    img,
                    to_px(rect.x, img.width),
                    to_px(rect.y, img.height),
                    rect.w * iw,
                    rect.h * ih,
                    corner_radius * iw,
                    sw,
                    s.stroke,
                    s.fill,
                );
            }
            PrimitiveKind::Ellipse { rect } => {
                draw::ellipse(
                    img,
                    to_px(rect.x, img.width),
                    to_px(rect.y, img.height),
                    rect.w * iw,
                    rect.h * ih,
                    sw,
                    s.stroke,
                    s.fill,
                );
            }
            PrimitiveKind::Highlight { points } => {
                // A highlighter PEN (Dickles' law — this used to fill
                // a translucent rectangle, which is a box, not a
                // highlight). The nib width follows the pen-size
                // setting, scaled up to a marker's chunkiness.
                let pts: Vec<(f32, f32)> = points
                    .iter()
                    .map(|p| (to_px(p.x, img.width), to_px(p.y, img.height)))
                    .collect();
                let width = (s.stroke_width * iw * 4.0).clamp(10.0, 160.0);
                draw::highlight_stroke(img, &pts, width, s.stroke);
            }
            PrimitiveKind::StepBadge { at } => {
                let r = (0.02 * iw).max(12.0);
                let (cx, cy) = (to_px(at.x, img.width), to_px(at.y, img.height));
                draw::circle(img, cx, cy, r, s.stroke);
                let n = step_numbers.get(&prim.id).copied().unwrap_or(0);
                draw::step_number(img, cx, cy, r * 1.05, n, Color::WHITE);
            }
            PrimitiveKind::Blur { rect } => {
                let x = to_px(rect.x, img.width) as u32;
                let y = to_px(rect.y, img.height) as u32;
                let w = (rect.w * iw) as u32;
                let h = (rect.h * ih) as u32;
                if w > 0 && h > 0 {
                    let region = img.crop(x.min(img.width), y.min(img.height), w, h);
                    // Privacy-grade strength (Dickles' law: blur must
                    // BLUR — text under it illegible, not softened).
                    // The old formula chained the radius straight to
                    // the 0.004 default line weight (~10px on a wide
                    // shot), which left text readable. The weight
                    // slider still drives it, but scaled ×2.5 with a
                    // 12px floor: the default lands ~26px on a 2560
                    // shot — nothing reads through that.
                    let radius = (s.stroke_width * iw * 2.5).clamp(12.0, 64.0);
                    let blurred = blur::blur_smart(&region, radius);
                    img.blit(&blurred, x as i32, y as i32);
                }
            }
            PrimitiveKind::Pixelate { rect } => {
                let block = ((s.stroke_width * iw * 3.4) as u32).clamp(4, 72);
                draw::pixelate(
                    img,
                    to_px(rect.x, img.width) as u32,
                    to_px(rect.y, img.height) as u32,
                    (rect.w * iw) as u32,
                    (rect.h * ih) as u32,
                    block,
                );
            }
            PrimitiveKind::Spotlight { rect } => {
                let dim = Color::rgba(0, 0, 0, 150);
                let (x0, y0) = (to_px(rect.x, img.width), to_px(rect.y, img.height));
                let (x1, y1) = (x0 + rect.w * iw, y0 + rect.h * ih);
                for y in 0..img.height {
                    for x in 0..img.width {
                        let (fx, fy) = (x as f32, y as f32);
                        if fx >= x0 && fx < x1 && fy >= y0 && fy < y1 {
                            continue;
                        }
                        img.blend(x, y, dim);
                    }
                }
            }
            PrimitiveKind::Callout { at, tail, text: content } => {
                // Box math lives in text::callout_box_px (measured, one
                // source of truth shared with the editor and the model
                // estimate) — audit fix for editor/export box drift.
                let (bw, bh, height, pad) = text::callout_box_px(content, s.font_size, img.width);
                let bx = to_px(at.x, img.width);
                let by = to_px(at.y, img.height);
                let (tx, ty) = (to_px(tail.x, img.width), to_px(tail.y, img.height));
                // leader tail under the box: from the box edge nearest
                // the target point out to the point itself.
                let nx = tx.clamp(bx, bx + bw);
                let ny = ty.clamp(by, by + bh);
                draw::line(img, nx, ny, tx, ty, (sw * 0.8).max(1.5), s.stroke);
                draw::circle(img, tx, ty, (sw * 0.9).max(2.0), s.stroke);
                let fill = s.fill.unwrap_or(Color::rgba(17, 20, 27, 238));
                draw::rect(img, bx, by, bw, bh, height * 0.28, sw.max(1.5), s.stroke, Some(fill));
                text::draw_text(img, bx + pad, by + pad * 0.65, height, content, Color::WHITE);
            }
            PrimitiveKind::Text { at, text: content } => {
                let height = (s.font_size.unwrap_or(0.035) * iw).max(7.0);
                text::draw_text(
                    img,
                    to_px(at.x, img.width),
                    to_px(at.y, img.height),
                    height,
                    content,
                    s.stroke,
                );
            }
            PrimitiveKind::Stamp { at, stamp_id, scale, rotation_deg } => {
                let size = scale * iw;
                let side = size.ceil().max(1.0) as u32;
                // Draw the stamp upright onto its own tile, then rotate
                // the tile into place — one code path for the artwork,
                // any angle the editor asks for.
                let mut tile = RgbaImage::new(side, side);
                let painted = draw_stamp(&mut tile, 0.0, 0.0, side as f32, stamp_id, s.stroke);
                if !painted {
                    // Unknown stamp id: say so, never silently drop.
                    unrendered.push(prim.id);
                } else {
                    let cx = to_px(at.x, img.width) + size / 2.0;
                    let cy = to_px(at.y, img.height) + size / 2.0;
                    blit_rotated(img, &tile, cx, cy, *rotation_deg);
                }
            }
        }
    }
}

/// The built-in stamp set — original procedural artwork drawn from the
/// vector helpers, no image assets (license-clean by construction).
/// Geometry is relative to a square of `size` px at (x0, y0). Returns
/// false for unknown ids so the caller can report them as unrendered.
fn draw_stamp(img: &mut RgbaImage, x0: f32, y0: f32, size: f32, stamp_id: &str, color: Color) -> bool {
    let p = |fx: f32, fy: f32| (x0 + fx * size, y0 + fy * size);
    match stamp_id {
        "check" => {
            let w = size * 0.15;
            let (ax, ay) = p(0.14, 0.56);
            let (bx, by) = p(0.40, 0.82);
            let (cx, cy) = p(0.88, 0.20);
            draw::line(img, ax, ay, bx, by, w, color);
            draw::line(img, bx, by, cx, cy, w, color);
        }
        "cross" => {
            let w = size * 0.14;
            let (ax, ay) = p(0.24, 0.24);
            let (bx, by) = p(0.76, 0.76);
            let (cx, cy) = p(0.76, 0.24);
            let (dx, dy) = p(0.24, 0.76);
            draw::line(img, ax, ay, bx, by, w, color);
            draw::line(img, cx, cy, dx, dy, w, color);
        }
        "star" => {
            let (cx, cy) = p(0.5, 0.52);
            let (ro, ri) = (size * 0.47, size * 0.20);
            let mut pts: Vec<(f32, f32)> = Vec::with_capacity(10);
            for i in 0..10 {
                let ang = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
                let r = if i % 2 == 0 { ro } else { ri };
                pts.push((cx + r * ang.cos(), cy + r * ang.sin()));
            }
            draw::polygon_fill(img, &pts, color);
        }
        "warning" => {
            let w = size * 0.07;
            let a = p(0.5, 0.05);
            let b = p(0.97, 0.92);
            let c = p(0.03, 0.92);
            draw::line(img, a.0, a.1, b.0, b.1, w, color);
            draw::line(img, b.0, b.1, c.0, c.1, w, color);
            draw::line(img, c.0, c.1, a.0, a.1, w, color);
            let (ex, ey) = p(0.5, 0.36);
            let (fx, fy) = p(0.5, 0.62);
            draw::line(img, ex, ey, fx, fy, size * 0.085, color);
            let (dx, dy) = p(0.5, 0.76);
            draw::circle(img, dx, dy, size * 0.05, color);
        }
        "arrow_up" => {
            let (ax, ay) = p(0.5, 0.9);
            let (bx, by) = p(0.5, 0.1);
            draw::arrow(img, ax, ay, bx, by, size * 0.17, color);
        }
        "heart" => {
            // The classic parametric heart, sampled into a polygon.
            let mut pts: Vec<(f32, f32)> = Vec::with_capacity(48);
            for i in 0..48 {
                let t = i as f32 / 48.0 * std::f32::consts::TAU;
                let x = 16.0 * t.sin().powi(3);
                let y = 13.0 * t.cos()
                    - 5.0 * (2.0 * t).cos()
                    - 2.0 * (3.0 * t).cos()
                    - (4.0 * t).cos();
                pts.push(p(0.5 + x / 36.0, 0.54 - y / 36.0));
            }
            draw::polygon_fill(img, &pts, color);
        }
        "question" => {
            // Hook arc + tail + dot, all vector.
            let (cx, cy) = p(0.5, 0.36);
            let r = size * 0.165;
            let w = size * 0.105;
            let mut prev: Option<(f32, f32)> = None;
            for i in 0..=20 {
                let a = -std::f32::consts::FRAC_PI_2 - 0.5 + (i as f32 / 20.0) * 4.2;
                let pt = (cx + r * a.cos(), cy + r * a.sin());
                if let Some(q) = prev {
                    draw::line(img, q.0, q.1, pt.0, pt.1, w, color);
                }
                prev = Some(pt);
            }
            let (tx, ty) = p(0.5, 0.55);
            draw::line(img, tx, ty, tx, ty + size * 0.07, w, color);
            let (dx, dy) = p(0.5, 0.82);
            draw::circle(img, dx, dy, size * 0.062, color);
        }
        "exclaim" => {
            let (ax, ay) = p(0.5, 0.14);
            let (bx, by) = p(0.5, 0.60);
            draw::line(img, ax, ay, bx, by, size * 0.13, color);
            let (dx, dy) = p(0.5, 0.82);
            draw::circle(img, dx, dy, size * 0.068, color);
        }
        "pin" => {
            let (cx, cy) = p(0.5, 0.36);
            draw::circle(img, cx, cy, size * 0.27, color);
            let a = p(0.265, 0.52);
            let b = p(0.735, 0.52);
            let c = p(0.5, 0.98);
            draw::polygon_fill(img, &[a, b, c], color);
            // the hole: transparent pixels straight through the tile
            let hole = Color::rgba(0, 0, 0, 0);
            let (hx, hy) = p(0.5, 0.36);
            let hr = (size * 0.115) as i32;
            for dy in -hr..=hr {
                for dx in -hr..=hr {
                    if dx * dx + dy * dy <= hr * hr {
                        let (x, y) = ((hx as i32 + dx).max(0) as u32, (hy as i32 + dy).max(0) as u32);
                        if x < img.width && y < img.height {
                            img.set(x, y, hole);
                        }
                    }
                }
            }
        }
        "smiley" => {
            let (cx, cy) = p(0.5, 0.5);
            let r = size * 0.42;
            let w = size * 0.055;
            let mut prev: Option<(f32, f32)> = None;
            for i in 0..=40 {
                let a = i as f32 / 40.0 * std::f32::consts::TAU;
                let pt = (cx + r * a.cos(), cy + r * a.sin());
                if let Some(q) = prev {
                    draw::line(img, q.0, q.1, pt.0, pt.1, w, color);
                }
                prev = Some(pt);
            }
            let (e1x, e1y) = p(0.37, 0.40);
            let (e2x, e2y) = p(0.63, 0.40);
            draw::circle(img, e1x, e1y, size * 0.048, color);
            draw::circle(img, e2x, e2y, size * 0.048, color);
            let sr = size * 0.21;
            let (scx, scy) = p(0.5, 0.52);
            let mut prev: Option<(f32, f32)> = None;
            for i in 0..=16 {
                let a = 0.35 + (i as f32 / 16.0) * (std::f32::consts::PI - 0.7);
                let pt = (scx + sr * a.cos(), scy + sr * a.sin());
                if let Some(q) = prev {
                    draw::line(img, q.0, q.1, pt.0, pt.1, w, color);
                }
                prev = Some(pt);
            }
        }
        "ring" => {
            let (cx, cy) = p(0.5, 0.5);
            let r = size * 0.40;
            let w = size * 0.10;
            let mut prev: Option<(f32, f32)> = None;
            for i in 0..=44 {
                let a = i as f32 / 44.0 * std::f32::consts::TAU;
                let pt = (cx + r * a.cos(), cy + r * a.sin());
                if let Some(q) = prev {
                    draw::line(img, q.0, q.1, pt.0, pt.1, w, color);
                }
                prev = Some(pt);
            }
        }
        _ => return false,
    }
    true
}

/// Bilinear sample with edge clamping; None only for a degenerate tile.
fn sample_bilinear(img: &RgbaImage, x: f32, y: f32) -> Option<Color> {
    if img.width == 0 || img.height == 0 {
        return None;
    }
    let fx = x.clamp(0.0, (img.width - 1) as f32);
    let fy = y.clamp(0.0, (img.height - 1) as f32);
    let x0 = fx.floor() as u32;
    let y0 = fy.floor() as u32;
    let x1 = (x0 + 1).min(img.width - 1);
    let y1 = (y0 + 1).min(img.height - 1);
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let (c00, c10, c01, c11) = (img.get(x0, y0), img.get(x1, y0), img.get(x0, y1), img.get(x1, y1));
    let chan = |f: fn(&Color) -> u8| {
        let top = f(&c00) as f32 + (f(&c10) as f32 - f(&c00) as f32) * tx;
        let bot = f(&c01) as f32 + (f(&c11) as f32 - f(&c01) as f32) * tx;
        top + (bot - top) * ty
    };
    Some(Color::rgba(
        chan(|c| c.r) as u8,
        chan(|c| c.g) as u8,
        chan(|c| c.b) as u8,
        chan(|c| c.a) as u8,
    ))
}

/// Composite `tile` onto `dst`, rotated `deg` clockwise around (cx, cy).
/// Angle 0 takes the plain blit fast path.
fn blit_rotated(dst: &mut RgbaImage, tile: &RgbaImage, cx: f32, cy: f32, deg: f32) {
    let deg = ((deg % 360.0) + 360.0) % 360.0;
    if deg == 0.0 {
        dst.blit(
            tile,
            (cx - tile.width as f32 / 2.0).round() as i32,
            (cy - tile.height as f32 / 2.0).round() as i32,
        );
        return;
    }
    let rad = deg.to_radians();
    let (sin, cos) = rad.sin_cos();
    let half_diag =
        (((tile.width * tile.width + tile.height * tile.height) as f32).sqrt() / 2.0).ceil();
    let (tw, th) = (tile.width as f32, tile.height as f32);
    let x_lo = ((cx - half_diag).floor() as i32).max(0);
    let x_hi = ((cx + half_diag).ceil() as i32).min(dst.width as i32 - 1);
    let y_lo = ((cy - half_diag).floor() as i32).max(0);
    let y_hi = ((cy + half_diag).ceil() as i32).min(dst.height as i32 - 1);
    for py in y_lo..=y_hi {
        for px in x_lo..=x_hi {
            let dx = px as f32 + 0.5 - cx;
            let dy = py as f32 + 0.5 - cy;
            // inverse map: rotate the destination offset back into tile space
            let sx = dx * cos + dy * sin + tw / 2.0 - 0.5;
            let sy = -dx * sin + dy * cos + th / 2.0 - 0.5;
            if sx < -0.5 || sy < -0.5 || sx > tw - 0.5 || sy > th - 0.5 {
                continue;
            }
            if let Some(c) = sample_bilinear(tile, sx, sy) {
                if c.a > 0 {
                    dst.blend(px as u32, py as u32, c);
                }
            }
        }
    }
}

fn build_background(
    doc: &Document,
    shot: &RgbaImage,
    cw: u32,
    ch: u32,
    warnings: &mut Vec<String>,
) -> RgbaImage {
    match &doc.beautify.background {
        Background::Solid { color } => RgbaImage::filled(cw, ch, *color),
        Background::Gradient { from, to, angle_deg } => {
            let mut img = RgbaImage::new(cw, ch);
            let ang = angle_deg.to_radians();
            let (dx, dy) = (ang.cos(), ang.sin());
            let diag = (cw as f32 * dx.abs()) + (ch as f32 * dy.abs());
            for y in 0..ch {
                for x in 0..cw {
                    let t = if diag > 0.0 {
                        (((x as f32 - cw as f32 / 2.0) * dx + (y as f32 - ch as f32 / 2.0) * dy)
                            / diag
                            + 0.5)
                            .clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
                    img.set(
                        x,
                        y,
                        Color::rgba(
                            mix(from.r, to.r),
                            mix(from.g, to.g),
                            mix(from.b, to.b),
                            mix(from.a, to.a),
                        ),
                    );
                }
            }
            img
        }
        Background::BlurredScreenshot => {
            // Cover-fit: scale the shot so it covers the canvas, blur hard.
            let scale = (cw as f32 / shot.width as f32).max(ch as f32 / shot.height as f32);
            let sw = (shot.width as f32 * scale).ceil() as u32;
            let sh = (shot.height as f32 * scale).ceil() as u32;
            let scaled = blur::resample_nearest(shot, sw.max(1), sh.max(1));
            let blurred = blur::blur_smart(&scaled, 42.0);
            let ox = (sw.saturating_sub(cw)) / 2;
            let oy = (sh.saturating_sub(ch)) / 2;
            blurred.crop(ox.min(blurred.width), oy.min(blurred.height), cw, ch)
        }
        Background::Image { path } => {
            warnings.push(format!(
                "background image '{}' not loaded by the prototype renderer; used solid fallback",
                path
            ));
            RgbaImage::filled(cw, ch, Color::rgba(24, 24, 28, 255))
        }
    }
}

/// Render a document against its source image.
pub fn render(doc: &Document, source: &RgbaImage) -> Result<RenderOutcome, RenderError> {
    doc.validate().map_err(RenderError::Model)?;
    if source.width != doc.image.width || source.height != doc.image.height {
        return Err(RenderError::SizeMismatch {
            expected: (doc.image.width, doc.image.height),
            got: (source.width, source.height),
        });
    }
    let mut warnings = Vec::new();
    let mut unrendered = Vec::new();

    // 1. crop
    let mut shot = match &doc.beautify.crop {
        Some(c) => source.crop(
            (c.x * source.width as f32) as u32,
            (c.y * source.height as f32) as u32,
            ((c.w * source.width as f32) as u32).max(1),
            ((c.h * source.height as f32) as u32).max(1),
        ),
        None => source.clone(),
    };

    // 2. annotations. Coordinates are interpreted in the cropped image's
    // space: when the editor commits a crop it rewrites the document
    // (primitives remapped, image ref replaced), so the renderer never
    // juggles two coordinate spaces.
    draw_annotations(doc, &mut shot, &mut unrendered);

    // 3. rounded corners
    let radius_px = doc.beautify.corner_radius * shot.width as f32;
    apply_corner_radius(&mut shot, radius_px);

    // 4. canvas + shadow + shot
    let (pt, pr, pb, pl) = doc.beautify.padding.effective();
    let unit = shot.width as f32;
    let (mut pad_t, mut pad_r, mut pad_b, mut pad_l) = (
        (pt * unit) as u32,
        (pr * unit) as u32,
        (pb * unit) as u32,
        (pl * unit) as u32,
    );
    let mut cw = shot.width + pad_l + pad_r;
    let mut ch = shot.height + pad_t + pad_b;
    // Frame insets (fractions of shot width, converted below): the
    // frame lives between the padding and the shot. Instant gets the
    // classic deep chin; Postcard a slim uniform border; Peeled and
    // None take no space (Peeled draws over the shot's corner later).
    let frame_frac = match doc.beautify.frame {
        shot_model::FrameStyle::Instant => (0.035f32, 0.035f32, 0.17f32, 0.035f32),
        shot_model::FrameStyle::Postcard => (0.022, 0.022, 0.022, 0.022),
        _ => (0.0, 0.0, 0.0, 0.0),
    };
    let (f_t, f_r, f_b, f_l) = (
        (frame_frac.0 * unit) as u32,
        (frame_frac.1 * unit) as u32,
        (frame_frac.2 * unit) as u32,
        (frame_frac.3 * unit) as u32,
    );
    cw += f_l + f_r;
    ch += f_t + f_b;
    // Aspect fit (Xnapper-style): GROW the canvas with background until it
    // matches the target aspect — the shot is never cropped or scaled.
    // Extra space lands symmetric; the shadow rides along because it is
    // drawn from the shot's position below.
    if let Some(a) = doc.beautify.aspect {
        if a > 0.0 {
            let cur = cw as f32 / ch as f32;
            if cur < a {
                let new_cw = (ch as f32 * a).ceil() as u32;
                // Grow BOTH sides (audit fix): pad_l alone grew while
                // pad_r stayed stale — the pair must sum to the delta
                // so the pad bookkeeping (and everything derived from
                // it: shot placement, frame, shadow silhouette) stays
                // exactly symmetric instead of relying on the canvas
                // width to absorb the missing half implicitly.
                let grow = new_cw - cw;
                pad_l += grow / 2;
                pad_r += grow - grow / 2;
                cw = new_cw;
            } else if cur > a {
                let new_ch = (cw as f32 / a).ceil() as u32;
                let grow = new_ch - ch;
                pad_t += grow / 2;
                pad_b += grow - grow / 2;
                ch = new_ch;
            }
        }
    }
    let _ = (pad_r, pad_b); // consumed implicitly via cw/ch; kept explicit for symmetry
    let mut canvas = build_background(doc, &shot, cw, ch, &mut warnings);
    // Shot position inside the canvas: outer padding + frame inset.
    let (shot_x, shot_y) = (pad_l + f_l, pad_t + f_t);
    let framed = f_l + f_r + f_t + f_b > 0;

    if doc.beautify.shadow.enabled {
        let mut sil = RgbaImage::new(cw, ch);
        let off_y = (doc.beautify.shadow.offset_y * unit) as i32;
        if framed {
            // The shadow hugs the FRAME, not the shot inside it.
            let (fx, fy) = (pad_l as i32, pad_t as i32 + off_y);
            let (fw, fh) = (
                (f_l + shot.width + f_r) as i32,
                (f_t + shot.height + f_b) as i32,
            );
            let r = radius_px + f_l.min(f_t) as f32;
            for y in fy.max(0)..(fy + fh).min(ch as i32) {
                for x in fx.max(0)..(fx + fw).min(cw as i32) {
                    if rounded_rect_contains(x - fx, y - fy, fw, fh, r) {
                        let alpha = (255.0 * doc.beautify.shadow.opacity) as u8;
                        sil.set(x as u32, y as u32, Color::rgba(0, 0, 0, alpha));
                    }
                }
            }
        } else {
            for y in 0..shot.height {
                for x in 0..shot.width {
                    let a = shot.get(x, y).a;
                    if a > 0 {
                        let alpha = (a as f32 * doc.beautify.shadow.opacity) as u8;
                        let tx = x as i32 + shot_x as i32;
                        let ty = y as i32 + shot_y as i32 + off_y;
                        if sil.in_bounds(tx, ty) {
                            sil.set(tx as u32, ty as u32, Color::rgba(0, 0, 0, alpha));
                        }
                    }
                }
            }
        }
        let blur_px = (doc.beautify.shadow.blur * unit).max(1.0);
        let soft = blur::blur_smart(&sil, blur_px);
        canvas.blit(&soft, 0, 0);
    }
    if framed {
        // The frame itself: a clean near-white card under the shot.
        let frame_color = Color::rgba(252, 252, 253, 255);
        let (fx, fy) = (pad_l as i32, pad_t as i32);
        let (fw, fh) = (
            (f_l + shot.width + f_r) as i32,
            (f_t + shot.height + f_b) as i32,
        );
        let r = radius_px + f_l.min(f_t) as f32;
        for y in fy.max(0)..(fy + fh).min(ch as i32) {
            for x in fx.max(0)..(fx + fw).min(cw as i32) {
                if rounded_rect_contains(x - fx, y - fy, fw, fh, r) {
                    canvas.set(x as u32, y as u32, frame_color);
                }
            }
        }
    }
    // Peeled corner reveals what's UNDER the shot — snapshot the
    // background before the shot lands.
    let under = if doc.beautify.frame == shot_model::FrameStyle::Peeled {
        Some(canvas.clone())
    } else {
        None
    };
    canvas.blit(&shot, shot_x as i32, shot_y as i32);
    if doc.beautify.frame == shot_model::FrameStyle::Postcard {
        // Hairline just inside the border — the postcard tell.
        let inset_x = pad_l + f_l / 2;
        let inset_y = pad_t + f_t / 2;
        let (w2, h2) = (
            (shot.width + f_l) as f32,
            (shot.height + f_t) as f32,
        );
        let hair = Color::rgba(60, 60, 70, 46);
        draw::line(&mut canvas, inset_x as f32, inset_y as f32, inset_x as f32 + w2, inset_y as f32, 1.4, hair);
        draw::line(&mut canvas, inset_x as f32, inset_y as f32 + h2, inset_x as f32 + w2, inset_y as f32 + h2, 1.4, hair);
        draw::line(&mut canvas, inset_x as f32, inset_y as f32, inset_x as f32, inset_y as f32 + h2, 1.4, hair);
        draw::line(&mut canvas, inset_x as f32 + w2, inset_y as f32, inset_x as f32 + w2, inset_y as f32 + h2, 1.4, hair);
    }
    if let (Some(under), shot_model::FrameStyle::Peeled) = (&under, doc.beautify.frame) {
        draw_peel(&mut canvas, under, shot_x, shot_y, shot.width, shot.height);
    }

    Ok(RenderOutcome { image: canvas, unrendered, warnings })
}

/// Point-in-rounded-rect test (local coords), for frame + shadow shapes.
fn rounded_rect_contains(x: i32, y: i32, w: i32, h: i32, r: f32) -> bool {
    if x < 0 || y < 0 || x >= w || y >= h {
        return false;
    }
    let (fx, fy) = (x as f32, y as f32);
    let cx = if fx < r {
        r
    } else if fx > w as f32 - r {
        w as f32 - r
    } else {
        return true;
    };
    let cy = if fy < r {
        r
    } else if fy > h as f32 - r {
        h as f32 - r
    } else {
        return true;
    };
    (fx - cx) * (fx - cx) + (fy - cy) * (fy - cy) <= r * r
}

/// The dog-ear: the shot's bottom-right corner folds back along a
/// diagonal. The corner triangle reveals `under` (background), the
/// reflected triangle is the paper's back with a soft gradient, and a
/// shadow + highlight sell the fold.
fn draw_peel(canvas: &mut RgbaImage, under: &RgbaImage, sx: u32, sy: u32, sw: u32, sh: u32) {
    let s = (sw as f32 * 0.085) as i32;
    if s < 10 || sw < 40 || sh < 40 {
        return;
    }
    let x1 = (sx + sw) as i32;
    let y1 = (sy + sh) as i32;
    for y in (y1 - s)..y1 {
        for x in (x1 - s)..x1 {
            if !canvas.in_bounds(x, y) {
                continue;
            }
            let u = (x - (x1 - s)) as f32 / s as f32;
            let v = (y1 - y) as f32 / s as f32;
            if v <= u {
                // revealed: background pixel, darkened near the fold
                // (the flap's shadow on the page beneath).
                let mut c = under.get(x as u32, y as u32);
                let d = (u - v) / 1.4142;
                if d < 0.16 {
                    let k = 1.0 - (0.16 - d) / 0.16 * 0.35;
                    c = Color::rgba(
                        (c.r as f32 * k) as u8,
                        (c.g as f32 * k) as u8,
                        (c.b as f32 * k) as u8,
                        c.a,
                    );
                }
                canvas.set(x as u32, y as u32, c);
            } else {
                // the flap: paper back, lighter at the fold, grayer at
                // the folded-over tip.
                let t = ((v - u) / 1.4142).clamp(0.0, 1.0);
                let lerp = |a: f32, b: f32| (a + (b - a) * t) as u8;
                canvas.set(
                    x as u32,
                    y as u32,
                    Color::rgba(lerp(253.0, 203.0), lerp(253.0, 209.0), lerp(254.0, 218.0), 255),
                );
            }
            // fold highlight
            if (v - u).abs() < 0.035 {
                canvas.blend(x as u32, y as u32, Color::rgba(255, 255, 255, 110));
            }
        }
    }
}
