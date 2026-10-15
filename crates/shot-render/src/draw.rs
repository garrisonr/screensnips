//! Vector primitive rasterization in pixel space. Deliberately simple
//! exact-ish rasterizers (distance/implicit-equation based) — correctness
//! and predictability over cleverness at prototype stage.

use crate::image::RgbaImage;
use shot_model::Color;

fn blend_px(img: &mut RgbaImage, x: i32, y: i32, color: Color) {
    if img.in_bounds(x, y) {
        img.blend(x as u32, y as u32, color);
    }
}

fn set_px(img: &mut RgbaImage, x: i32, y: i32, color: Color) {
    if img.in_bounds(x, y) {
        img.set(x as u32, y as u32, color);
    }
}

/// Thick line segment with round caps, alpha-blended.
pub fn line(img: &mut RgbaImage, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Color) {
    let half = width / 2.0;
    let minx = (x0.min(x1) - half - 1.0).floor() as i32;
    let maxx = (x0.max(x1) + half + 1.0).ceil() as i32;
    let miny = (y0.min(y1) - half - 1.0).floor() as i32;
    let maxy = (y0.max(y1) + half + 1.0).ceil() as i32;
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len2 = dx * dx + dy * dy;
    for y in miny..=maxy {
        for x in minx..=maxx {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let t = if len2 == 0.0 {
                0.0
            } else {
                (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0)
            };
            let (cx, cy) = (x0 + t * dx, y0 + t * dy);
            let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            if d <= half {
                blend_px(img, x, y, color);
            } else if d <= half + 1.0 {
                // 1px anti-alias fringe
                let a = (color.a as f32 * (half + 1.0 - d)) as u8;
                blend_px(img, x, y, Color::rgba(color.r, color.g, color.b, a));
            }
        }
    }
}

fn point_in_triangle(px: f32, py: f32, a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let sign = |p: (f32, f32), p1: (f32, f32), p2: (f32, f32)| {
        (p.0 - p2.0) * (p1.1 - p2.1) - (p1.0 - p2.0) * (p.1 - p2.1)
    };
    let d1 = sign((px, py), a, b);
    let d2 = sign((px, py), b, c);
    let d3 = sign((px, py), c, a);
    let neg = (d1 < 0.0) || (d2 < 0.0) || (d3 < 0.0);
    let pos = (d1 > 0.0) || (d2 > 0.0) || (d3 > 0.0);
    !(neg && pos)
}

/// Arrow: shaft + filled triangular head at `to`.
/// Highlighter pen (Dickles' law: a pen, not a box): a wide,
/// translucent stroke along `pts` that multiply-tints what it
/// crosses — dark pixels (text) stay dark, light pixels take the
/// ink, exactly like a marker on paper. One pass over the stroke's
/// bbox: coverage is the pixel's distance to the path (full inside
/// the nib, feathered across its outer quarter), so a slow stroke
/// that overlaps itself never double-darkens.
pub fn highlight_stroke(img: &mut RgbaImage, pts: &[(f32, f32)], width: f32, color: Color) {
    if pts.is_empty() || width <= 0.0 {
        return;
    }
    let half = width / 2.0;
    let (mut minx, mut miny) = (f32::MAX, f32::MAX);
    let (mut maxx, mut maxy) = (f32::MIN, f32::MIN);
    for &(x, y) in pts {
        minx = minx.min(x);
        miny = miny.min(y);
        maxx = maxx.max(x);
        maxy = maxy.max(y);
    }
    let x0 = (minx - half - 1.0).floor() as i32;
    let x1 = (maxx + half + 1.0).ceil() as i32;
    let y0 = (miny - half - 1.0).floor() as i32;
    let y1 = (maxy + half + 1.0).ceil() as i32;
    // Ink strength: the pen lays down a bit more than half its color.
    let strength = (color.a as f32 / 255.0) * 0.55;
    if strength <= 0.0 {
        return;
    }
    for y in y0..=y1 {
        for x in x0..=x1 {
            if !img.in_bounds(x, y) {
                continue;
            }
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let mut dmin = f32::MAX;
            if pts.len() == 1 {
                dmin = ((px - pts[0].0).powi(2) + (py - pts[0].1).powi(2)).sqrt();
            }
            for pair in pts.windows(2) {
                let (ax, ay) = pair[0];
                let (bx, by) = pair[1];
                if px < ax.min(bx) - half || px > ax.max(bx) + half {
                    continue;
                }
                if py < ay.min(by) - half || py > ay.max(by) + half {
                    continue;
                }
                let (dx, dy) = (bx - ax, by - ay);
                let len2 = dx * dx + dy * dy;
                let t = if len2 == 0.0 {
                    0.0
                } else {
                    (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
                };
                let (cx, cy) = (ax + t * dx, ay + t * dy);
                let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                if d < dmin {
                    dmin = d;
                }
            }
            if dmin > half {
                continue;
            }
            let feather = half * 0.75;
            let cov = if dmin <= feather {
                1.0
            } else {
                (half - dmin) / (half - feather)
            };
            let a = strength * cov;
            let base = img.get(x as u32, y as u32);
            // Multiply blend: channel' = base × ink / 255, mixed by a.
            let mix = |b: u8, ink: u8| -> u8 {
                let tinted = b as f32 * ink as f32 / 255.0;
                (b as f32 * (1.0 - a) + tinted * a)
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
            img.set(
                x as u32,
                y as u32,
                Color::rgba(
                    mix(base.r, color.r),
                    mix(base.g, color.g),
                    mix(base.b, color.b),
                    base.a,
                ),
            );
        }
    }
}

pub fn arrow(img: &mut RgbaImage, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Color) {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let head_len = (width * 4.5).min(len * 0.35).max(width * 2.5);
    let head_half = head_len * 0.42;
    let (bx, by) = (x1 - ux * head_len, y1 - uy * head_len);
    let (pxn, pyn) = (-uy, ux);
    // shaft stops at the head base
    line(img, x0, y0, bx, by, width, color);
    let a = (x1, y1);
    let b = (bx + pxn * head_half, by + pyn * head_half);
    let c = (bx - pxn * head_half, by - pyn * head_half);
    let minx = (a.0.min(b.0).min(c.0) - 1.0).floor() as i32;
    let maxx = (a.0.max(b.0).max(c.0) + 1.0).ceil() as i32;
    let miny = (a.1.min(b.1).min(c.1) - 1.0).floor() as i32;
    let maxy = (a.1.max(b.1).max(c.1) + 1.0).ceil() as i32;
    for y in miny..=maxy {
        for x in minx..=maxx {
            if point_in_triangle(x as f32 + 0.5, y as f32 + 0.5, a, b, c) {
                blend_px(img, x, y, color);
            }
        }
    }
}

/// Curved arrow: a quadratic Bézier shaft from (x0,y0) through control
/// (cx,cy) to the tip (x1,y1), with a filled triangular head oriented
/// along the curve's end tangent (tip − control direction).
pub fn arrow_curved(
    img: &mut RgbaImage,
    x0: f32,
    y0: f32,
    cx: f32,
    cy: f32,
    x1: f32,
    y1: f32,
    width: f32,
    color: Color,
) {
    let (dx, dy) = (x1 - cx, y1 - cy);
    let tlen = (dx * dx + dy * dy).sqrt();
    if tlen < 1.0 {
        return;
    }
    let (ux, uy) = (dx / tlen, dy / tlen);
    let head_len = (width * 4.5).min(tlen * 0.6).max(width * 2.5);
    let head_half = head_len * 0.42;
    let (bx, by) = (x1 - ux * head_len, y1 - uy * head_len);
    // Shaft: sample the curve, stopping where the head begins
    // (approximately t where the point reaches the head base).
    let steps = 28;
    let mut prev: Option<(f32, f32)> = None;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let mt = 1.0 - t;
        let (px, py) = (
            mt * mt * x0 + 2.0 * mt * t * cx + t * t * x1,
            mt * mt * y0 + 2.0 * mt * t * cy + t * t * y1,
        );
        // stop the shaft at the head base: once within head_len of the
        // tip along the tangent, further points hide under the head.
        let along = (px - bx) * ux + (py - by) * uy;
        if let Some(q) = prev {
            if along <= 0.0 {
                line(img, q.0, q.1, px, py, width, color);
            }
        }
        prev = Some((px, py));
    }
    let (pxn, pyn) = (-uy, ux);
    let a = (x1, y1);
    let b = (bx + pxn * head_half, by + pyn * head_half);
    let c = (bx - pxn * head_half, by - pyn * head_half);
    let minx = (a.0.min(b.0).min(c.0) - 1.0).floor() as i32;
    let maxx = (a.0.max(b.0).max(c.0) + 1.0).ceil() as i32;
    let miny = (a.1.min(b.1).min(c.1) - 1.0).floor() as i32;
    let maxy = (a.1.max(b.1).max(c.1) + 1.0).ceil() as i32;
    for y in miny..=maxy {
        for x in minx..=maxx {
            if point_in_triangle(x as f32 + 0.5, y as f32 + 0.5, a, b, c) {
                blend_px(img, x, y, color);
            }
        }
    }
}

/// Fill a polygon given as pixel-space vertices. Filled as a triangle fan
/// around the centroid — correct for convex and star-shaped polygons
/// (stars, triangles, badges), which is all the stamp set needs.
pub fn polygon_fill(img: &mut RgbaImage, pts: &[(f32, f32)], color: Color) {
    if pts.len() < 3 {
        return;
    }
    let (mut sx, mut sy) = (0.0f32, 0.0f32);
    for p in pts {
        sx += p.0;
        sy += p.1;
    }
    let c = (sx / pts.len() as f32, sy / pts.len() as f32);
    let minx = (pts.iter().map(|p| p.0).fold(f32::MAX, f32::min) - 1.0).floor() as i32;
    let maxx = (pts.iter().map(|p| p.0).fold(f32::MIN, f32::max) + 1.0).ceil() as i32;
    let miny = (pts.iter().map(|p| p.1).fold(f32::MAX, f32::min) - 1.0).floor() as i32;
    let maxy = (pts.iter().map(|p| p.1).fold(f32::MIN, f32::max) + 1.0).ceil() as i32;
    for y in miny..=maxy {
        for x in minx..=maxx {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            for i in 0..pts.len() {
                let a = pts[i];
                let b = pts[(i + 1) % pts.len()];
                if point_in_triangle(fx, fy, c, a, b) {
                    blend_px(img, x, y, color);
                    break;
                }
            }
        }
    }
}

/// Rounded-rect inside test: returns signed distance-ish membership.
/// (px,py) inside rect [x,x+w]x[y,y+h] with corner radius r.
fn in_rounded_rect(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> bool {
    let r = r.min(w / 2.0).min(h / 2.0);
    let cx = (px - (x + r)).max((x + w - r) - px).max(0.0);
    // distance outside the inner rect, standard rounded-box SDF
    let qx = (px - (x + w / 2.0)).abs() - (w / 2.0 - r);
    let qy = (py - (y + h / 2.0)).abs() - (h / 2.0 - r);
    let sdf = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
    let _ = cx;
    sdf <= 0.0
}

/// Rectangle with optional fill and stroke (stroke drawn inside the edge).
pub fn rect(
    img: &mut RgbaImage,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    stroke_width: f32,
    stroke: Color,
    fill: Option<Color>,
) {
    let minx = (x - 1.0).floor() as i32;
    let maxx = (x + w + 1.0).ceil() as i32;
    let miny = (y - 1.0).floor() as i32;
    let maxy = (y + h + 1.0).ceil() as i32;
    for py in miny..=maxy {
        for px in minx..=maxx {
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            if in_rounded_rect(fx, fy, x, y, w, h, radius) {
                if let Some(f) = fill {
                    blend_px(img, px, py, f);
                }
                let inner = in_rounded_rect(
                    fx,
                    fy,
                    x + stroke_width,
                    y + stroke_width,
                    (w - 2.0 * stroke_width).max(0.0),
                    (h - 2.0 * stroke_width).max(0.0),
                    (radius - stroke_width).max(0.0),
                );
                if !inner && stroke_width > 0.0 {
                    blend_px(img, px, py, stroke);
                }
            }
        }
    }
}

/// Ellipse with optional fill and stroke.
pub fn ellipse(
    img: &mut RgbaImage,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    stroke_width: f32,
    stroke: Color,
    fill: Option<Color>,
) {
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let (rx, ry) = (w / 2.0, h / 2.0);
    if rx <= 0.0 || ry <= 0.0 {
        return;
    }
    let minx = (x - 1.0).floor() as i32;
    let maxx = (x + w + 1.0).ceil() as i32;
    let miny = (y - 1.0).floor() as i32;
    let maxy = (y + h + 1.0).ceil() as i32;
    let irx = (rx - stroke_width).max(0.0);
    let iry = (ry - stroke_width).max(0.0);
    for py in miny..=maxy {
        for px in minx..=maxx {
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            let outer = ((fx - cx) / rx).powi(2) + ((fy - cy) / ry).powi(2);
            if outer <= 1.0 {
                if let Some(f) = fill {
                    blend_px(img, px, py, f);
                }
                let inner = if irx > 0.0 && iry > 0.0 {
                    ((fx - cx) / irx).powi(2) + ((fy - cy) / iry).powi(2)
                } else {
                    2.0
                };
                if inner > 1.0 && stroke_width > 0.0 {
                    blend_px(img, px, py, stroke);
                }
            }
        }
    }
}

/// Filled circle (step badges, stamp anchors).
pub fn circle(img: &mut RgbaImage, cx: f32, cy: f32, r: f32, color: Color) {
    let minx = (cx - r - 1.0).floor() as i32;
    let maxx = (cx + r + 1.0).ceil() as i32;
    let miny = (cy - r - 1.0).floor() as i32;
    let maxy = (cy + r + 1.0).ceil() as i32;
    for y in miny..=maxy {
        for x in minx..=maxx {
            let d = (((x as f32 + 0.5) - cx).powi(2) + ((y as f32 + 0.5) - cy).powi(2)).sqrt();
            if d <= r - 0.5 {
                blend_px(img, x, y, color);
            } else if d <= r + 0.5 {
                let a = (color.a as f32 * (r + 0.5 - d)) as u8;
                blend_px(img, x, y, Color::rgba(color.r, color.g, color.b, a));
            }
        }
    }
}

// --- Seven-segment digits (step-badge numerals: crisp at any size) ---
// Segments: a=top, b=top-right, c=bottom-right, d=bottom, e=bottom-left,
// f=top-left, g=middle.
fn segments_for(d: u8) -> [bool; 7] {
    // [a, b, c, d, e, f, g]
    match d {
        0 => [true, true, true, true, true, true, false],
        1 => [false, true, true, false, false, false, false],
        2 => [true, true, false, true, true, false, true],
        3 => [true, true, true, true, false, false, true],
        4 => [false, true, true, false, false, true, true],
        5 => [true, false, true, true, false, true, true],
        6 => [true, false, true, true, true, true, true],
        7 => [true, true, true, false, false, false, false],
        8 => [true, true, true, true, true, true, true],
        9 => [true, true, true, true, false, true, true],
        _ => [false; 7],
    }
}

fn fill_rect(img: &mut RgbaImage, x: f32, y: f32, w: f32, h: f32, color: Color) {
    let minx = x.floor() as i32;
    let maxx = (x + w).ceil() as i32;
    let miny = y.floor() as i32;
    let maxy = (y + h).ceil() as i32;
    for py in miny..maxy {
        for px in minx..maxx {
            set_px(img, px, py, color);
        }
    }
}

fn digit(img: &mut RgbaImage, x: f32, y: f32, w: f32, h: f32, d: u8, color: Color) {
    let t = (w * 0.24).max(1.0); // segment thickness
    let segs = segments_for(d);
    let mid = y + h / 2.0;
    // a (top)
    if segs[0] {
        fill_rect(img, x + t * 0.5, y, w - t, t, color);
    }
    // b (top-right)
    if segs[1] {
        fill_rect(img, x + w - t, y + t * 0.5, t, h / 2.0 - t * 0.5, color);
    }
    // c (bottom-right)
    if segs[2] {
        fill_rect(img, x + w - t, mid, t, h / 2.0 - t * 0.5, color);
    }
    // d (bottom)
    if segs[3] {
        fill_rect(img, x + t * 0.5, y + h - t, w - t, t, color);
    }
    // e (bottom-left)
    if segs[4] {
        fill_rect(img, x, mid, t, h / 2.0 - t * 0.5, color);
    }
    // f (top-left)
    if segs[5] {
        fill_rect(img, x, y + t * 0.5, t, h / 2.0 - t * 0.5, color);
    }
    // g (middle)
    if segs[6] {
        fill_rect(img, x + t * 0.5, mid - t / 2.0, w - t, t, color);
    }
}

/// Draw a step number centered at (cx, cy); `height` is the digit height.
/// Supports 1–3 digits (badge numbers beyond 999 just draw 999).
pub fn step_number(img: &mut RgbaImage, cx: f32, cy: f32, height: f32, n: u32, color: Color) {
    let n = n.min(999);
    let digits: Vec<u8> = n
        .to_string()
        .chars()
        .map(|c| c.to_digit(10).unwrap() as u8)
        .collect();
    let dw = height * 0.58;
    let gap = height * 0.16;
    let total = dw * digits.len() as f32 + gap * (digits.len() as f32 - 1.0);
    let mut x = cx - total / 2.0;
    let y = cy - height / 2.0;
    for d in digits {
        digit(img, x, y, dw, height, d, color);
        x += dw + gap;
    }
}

/// Pixelate a pixel-space rectangle in place.
pub fn pixelate(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, block: u32) {
    let block = block.max(2);
    let x1 = (x + w).min(img.width);
    let y1 = (y + h).min(img.height);
    let mut by = y;
    while by < y1 {
        let mut bx = x;
        while bx < x1 {
            let bw = block.min(x1 - bx);
            let bh = block.min(y1 - by);
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for yy in by..by + bh {
                for xx in bx..bx + bw {
                    let c = img.get(xx, yy);
                    acc[0] += c.r as u32;
                    acc[1] += c.g as u32;
                    acc[2] += c.b as u32;
                    acc[3] += c.a as u32;
                    n += 1;
                }
            }
            let avg = Color::rgba(
                (acc[0] / n) as u8,
                (acc[1] / n) as u8,
                (acc[2] / n) as u8,
                (acc[3] / n) as u8,
            );
            for yy in by..by + bh {
                for xx in bx..bx + bw {
                    img.set(xx, yy, avg);
                }
            }
            bx += block;
        }
        by += block;
    }
}
