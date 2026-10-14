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
            PrimitiveKind::Arrow { from, to } => {
                draw::arrow(
                    img,
                    to_px(from.x, img.width),
                    to_px(from.y, img.height),
                    to_px(to.x, img.width),
                    to_px(to.y, img.height),
                    sw,
                    s.stroke,
                );
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
            PrimitiveKind::Highlight { rect } => {
                let mut c = s.stroke;
                c.a = c.a / 3; // highlighter translucency
                draw::rect(
                    img,
                    to_px(rect.x, img.width),
                    to_px(rect.y, img.height),
                    rect.w * iw,
                    rect.h * ih,
                    0.0,
                    0.0,
                    c,
                    Some(c),
                );
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
                    let blurred = blur::blur_smart(&region, 12.0);
                    img.blit(&blurred, x as i32, y as i32);
                }
            }
            PrimitiveKind::Pixelate { rect } => {
                let block = ((0.012 * iw) as u32).max(4);
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
                let height = (s.font_size.unwrap_or(0.04) * iw).max(8.0);
                let text_w = text::measure(content, height);
                let lines = content.split('\n').count() as f32;
                let pad = height * 0.45;
                let bw = text_w + pad * 2.0;
                let bh = lines * height * 1.3 + pad * 1.4;
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
            PrimitiveKind::Stamp { at, stamp_id, scale } => {
                let size = scale * iw;
                let painted = draw_stamp(
                    img,
                    to_px(at.x, img.width),
                    to_px(at.y, img.height),
                    size,
                    stamp_id,
                    s.stroke,
                );
                if !painted {
                    // Unknown stamp id: say so, never silently drop.
                    unrendered.push(prim.id);
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
        _ => return false,
    }
    true
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
    let (mut pad_t, pad_r, pad_b, mut pad_l) = (
        (pt * unit) as u32,
        (pr * unit) as u32,
        (pb * unit) as u32,
        (pl * unit) as u32,
    );
    let mut cw = shot.width + pad_l + pad_r;
    let mut ch = shot.height + pad_t + pad_b;
    // Aspect fit (Xnapper-style): GROW the canvas with background until it
    // matches the target aspect — the shot is never cropped or scaled.
    // Extra space lands symmetric; the shadow rides along because it is
    // drawn from the shot's position below.
    if let Some(a) = doc.beautify.aspect {
        if a > 0.0 {
            let cur = cw as f32 / ch as f32;
            if cur < a {
                let new_cw = (ch as f32 * a).ceil() as u32;
                pad_l += (new_cw - cw) / 2;
                cw = new_cw;
            } else if cur > a {
                let new_ch = (cw as f32 / a).ceil() as u32;
                pad_t += (new_ch - ch) / 2;
                ch = new_ch;
            }
        }
    }
    let _ = (pad_r, pad_b);
    let mut canvas = build_background(doc, &shot, cw, ch, &mut warnings);

    if doc.beautify.shadow.enabled {
        // Silhouette of the (rounded) shot, blurred via the smart path.
        let mut sil = RgbaImage::new(cw, ch);
        let off_y = (doc.beautify.shadow.offset_y * unit) as i32;
        for y in 0..shot.height {
            for x in 0..shot.width {
                let a = shot.get(x, y).a;
                if a > 0 {
                    let alpha = (a as f32 * doc.beautify.shadow.opacity) as u8;
                    let tx = x as i32 + pad_l as i32;
                    let ty = y as i32 + pad_t as i32 + off_y;
                    if sil.in_bounds(tx, ty) {
                        sil.set(tx as u32, ty as u32, Color::rgba(0, 0, 0, alpha));
                    }
                }
            }
        }
        let blur_px = (doc.beautify.shadow.blur * unit).max(1.0);
        let soft = blur::blur_smart(&sil, blur_px);
        canvas.blit(&soft, 0, 0);
    }
    canvas.blit(&shot, pad_l as i32, pad_t as i32);

    Ok(RenderOutcome { image: canvas, unrendered, warnings })
}
