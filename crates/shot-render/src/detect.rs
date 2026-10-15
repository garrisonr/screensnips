//! Smart document detection — the eyes of the straightender.
//!
//! Classical pipeline, no ML, no dependencies: downscale, luminance,
//! Sobel edges, adaptive threshold, dilate, largest connected component,
//! convex hull, four diagonal-extreme corners, snap-to-strongest-edge
//! refinement, geometric validation. Returns normalized [TL, TR, BR, BL]
//! corners in [0,1] space, or None when nothing plausible is found —
//! the caller then offers manual corner placement instead of guessing.

use crate::blur;
use crate::image::RgbaImage;
use shot_model::Point;

const WORK_MAX_W: u32 = 800;

pub fn detect_document(src: &RgbaImage) -> Option<[Point; 4]> {
    if src.width < 40 || src.height < 40 {
        return None;
    }
    let scale = (WORK_MAX_W as f32 / src.width as f32).min(1.0);
    let (ww, wh) = if scale < 1.0 {
        (
            (src.width as f32 * scale) as u32,
            (src.height as f32 * scale) as u32,
        )
    } else {
        (src.width, src.height)
    };
    let work = if (ww, wh) != (src.width, src.height) {
        blur::resample_nearest(src, ww, wh)
    } else {
        src.clone()
    };
    let (w, h) = (ww as usize, wh as usize);
    let n = w * h;

    // luminance
    let mut gray = vec![0f32; n];
    for y in 0..h {
        for x in 0..w {
            let c = work.get(x as u32, y as u32);
            gray[y * w + x] = 0.299 * c.r as f32 + 0.587 * c.g as f32 + 0.114 * c.b as f32;
        }
    }
    // 3x3 box blur to tame photo noise
    let mut blurred = vec![0f32; n];
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0f32;
            let mut cnt = 0f32;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                        sum += gray[ny as usize * w + nx as usize];
                        cnt += 1.0;
                    }
                }
            }
            blurred[y * w + x] = sum / cnt;
        }
    }
    // Sobel magnitude
    let mut mag = vec![0f32; n];
    let mut hist = [0u32; 256];
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let i = y * w + x;
            let gx = -blurred[i - w - 1] - 2.0 * blurred[i - 1] - blurred[i + w - 1]
                + blurred[i - w + 1]
                + 2.0 * blurred[i + 1]
                + blurred[i + w + 1];
            let gy = -blurred[i - w - 1] - 2.0 * blurred[i - w] - blurred[i - w + 1]
                + blurred[i + w - 1]
                + 2.0 * blurred[i + w]
                + blurred[i + w + 1];
            let m = (gx * gx + gy * gy).sqrt();
            mag[i] = m;
            hist[(m / 6.0).min(255.0) as usize] += 1;
        }
    }
    // adaptive threshold: 88th percentile of magnitudes, floored
    let target = (n as f32 * 0.88) as u32;
    let mut acc = 0u32;
    let mut thresh = 30.0f32;
    for (bin, &count) in hist.iter().enumerate() {
        acc += count;
        if acc >= target {
            thresh = (bin as f32 * 6.0).max(30.0);
            break;
        }
    }
    let mut mask = vec![false; n];
    for i in 0..n {
        mask[i] = mag[i] >= thresh;
    }
    // dilate twice so the outline becomes a solid band
    for _ in 0..2 {
        let prev = mask.clone();
        for y in 0..h {
            for x in 0..w {
                if prev[y * w + x] {
                    continue;
                }
                let mut on = false;
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                        if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h
                            && prev[ny as usize * w + nx as usize]
                        {
                            on = true;
                        }
                    }
                }
                mask[y * w + x] = on;
            }
        }
    }
    // largest connected component (iterative flood fill)
    let mut visited = vec![false; n];
    let mut best: Vec<(i32, i32)> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..n {
        if !mask[start] || visited[start] {
            continue;
        }
        let mut comp: Vec<(i32, i32)> = Vec::new();
        stack.clear();
        stack.push(start);
        visited[start] = true;
        while let Some(i) = stack.pop() {
            let (x, y) = ((i % w) as i32, (i / w) as i32);
            comp.push((x, y));
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx as usize >= w || ny as usize >= h {
                    continue;
                }
                let ni = ny as usize * w + nx as usize;
                if mask[ni] && !visited[ni] {
                    visited[ni] = true;
                    stack.push(ni);
                }
            }
        }
        if comp.len() > best.len() {
            best = comp;
        }
    }
    if (best.len() as f32) < n as f32 * 0.012 {
        return None; // no substantial outline found
    }
    // convex hull (monotone chain)
    let mut pts = best.clone();
    pts.sort();
    pts.dedup();
    if pts.len() < 4 {
        return None;
    }
    let cross = |o: (i32, i32), a: (i32, i32), b: (i32, i32)| -> i64 {
        (a.0 - o.0) as i64 * (b.1 - o.1) as i64 - (a.1 - o.1) as i64 * (b.0 - o.0) as i64
    };
    let mut lower: Vec<(i32, i32)> = Vec::new();
    for &p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0 {
            lower.pop();
        }
        lower.push(p);
    }
    let mut upper: Vec<(i32, i32)> = Vec::new();
    for &p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0 {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    let hull: Vec<(i32, i32)> = lower.into_iter().chain(upper).collect();
    if hull.len() < 4 {
        return None;
    }
    // four diagonal extremes of the hull
    let pick = |score: &dyn Fn((i32, i32)) -> i32, want_max: bool| -> (i32, i32) {
        let mut best_p = hull[0];
        let mut best_s = score(best_p);
        for &p in &hull[1..] {
            let s = score(p);
            if (want_max && s > best_s) || (!want_max && s < best_s) {
                best_s = s;
                best_p = p;
            }
        }
        best_p
    };
    let mut corners = [
        pick(&|(x, y)| x + y, false), // TL
        pick(&|(x, y)| x - y, true),  // TR
        pick(&|(x, y)| x + y, true),  // BR
        pick(&|(x, y)| x - y, false), // BL
    ];
    // snap each corner to the strongest edge pixel nearby
    for c in corners.iter_mut() {
        let mut best_m = -1f32;
        let mut best_p = *c;
        for dy in -10i32..=10 {
            for dx in -10i32..=10 {
                let (nx, ny) = (c.0 + dx, c.1 + dy);
                if nx < 0 || ny < 0 || nx as usize >= w || ny as usize >= h {
                    continue;
                }
                let m = mag[ny as usize * w + nx as usize];
                if m > best_m {
                    best_m = m;
                    best_p = (nx, ny);
                }
            }
        }
        *c = best_p;
    }
    // validate: convex, area, angles
    let area2 = {
        let mut a = 0i64;
        for i in 0..4 {
            let (x1, y1) = corners[i];
            let (x2, y2) = corners[(i + 1) % 4];
            a += x1 as i64 * y2 as i64 - x2 as i64 * y1 as i64;
        }
        a.abs()
    };
    if (area2 as f32) < 0.20 * (n as f32) {
        return None; // quad covers < 10% of the photo — not a document shot
    }
    for i in 0..4 {
        let p = corners[i];
        let q = corners[(i + 1) % 4];
        let r = corners[(i + 2) % 4];
        let (e1x, e1y) = ((q.0 - p.0) as f32, (q.1 - p.1) as f32);
        let (e2x, e2y) = ((r.0 - q.0) as f32, (r.1 - q.1) as f32);
        let cr = e1x * e2y - e1y * e2x;
        if cr <= 0.0 {
            return None; // not convex in TL,TR,BR,BL winding (y-down => positive turns)
        }
        let dot = e1x * e2x + e1y * e2y;
        let cos = dot / ((e1x * e1x + e1y * e1y).sqrt() * (e2x * e2x + e2y * e2y).sqrt() + 1e-6);
        if cos.abs() > 0.92 {
            return None; // corner too sharp or too flat to be a document
        }
    }
    Some([
        Point::new(corners[0].0 as f32 / w as f32, corners[0].1 as f32 / h as f32),
        Point::new(corners[1].0 as f32 / w as f32, corners[1].1 as f32 / h as f32),
        Point::new(corners[2].0 as f32 / w as f32, corners[2].1 as f32 / h as f32),
        Point::new(corners[3].0 as f32 / w as f32, corners[3].1 as f32 / h as f32),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw;
    use shot_model::Color;

    #[test]
    fn finds_a_skewed_white_document_on_a_dark_desk() {
        // 400x300 photo: dark desk, bright document quad, some text-ish
        // scribbles inside it for realism.
        let mut img = RgbaImage::filled(400, 300, Color::rgba(38, 34, 30, 255));
        let quad = [(70.0, 45.0), (345.0, 70.0), (325.0, 255.0), (55.0, 225.0)];
        draw::polygon_fill(
            &mut img,
            &quad,
            Color::rgba(238, 236, 230, 255),
        );
        // fake text lines inside
        for row in 0..5 {
            let y = 95.0 + row as f32 * 26.0;
            draw::line(&mut img, 95.0, y, 290.0, y + 4.0, 3.0, Color::rgba(60, 60, 60, 255));
        }
        let found = detect_document(&img).expect("document should be found");
        let truth = [
            (70.0 / 400.0, 45.0 / 300.0),
            (345.0 / 400.0, 70.0 / 300.0),
            (325.0 / 400.0, 255.0 / 300.0),
            (55.0 / 400.0, 225.0 / 300.0),
        ];
        for (i, (tx, ty)) in truth.iter().enumerate() {
            let (fx, fy) = (found[i].x, found[i].y);
            assert!(
                (fx - tx).abs() < 0.045 && (fy - ty).abs() < 0.06,
                "corner {} off: got ({:.3},{:.3}) want ({:.3},{:.3})",
                i, fx, fy, tx, ty
            );
        }
    }

    #[test]
    fn blank_photo_finds_nothing() {
        let img = RgbaImage::filled(300, 200, Color::rgba(120, 120, 120, 255));
        assert!(detect_document(&img).is_none());
    }
}
