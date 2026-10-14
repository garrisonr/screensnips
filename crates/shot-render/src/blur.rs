//! Blur + resampling. Production rule from the Step 0 benchmark: never blur
//! at full resolution when a downscaled pass gives the same visual result
//! (measured 14–32x faster on the target hardware). `blur_smart` enforces it.

use crate::image::RgbaImage;
use shot_model::Color;

/// Box-average downscale to `target_w` (aspect preserved).
pub fn downscale(src: &RgbaImage, target_w: u32) -> RgbaImage {
    if target_w == 0 || src.width == 0 {
        return src.clone();
    }
    let tw = target_w.min(src.width);
    let th = ((src.height as u64 * tw as u64) / src.width as u64).max(1) as u32;
    let mut out = RgbaImage::new(tw, th);
    for dy in 0..th {
        let sy0 = (dy * src.height) / th;
        let sy1 = (((dy + 1) * src.height) / th).max(sy0 + 1);
        for dx in 0..tw {
            let sx0 = (dx * src.width) / tw;
            let sx1 = (((dx + 1) * src.width) / tw).max(sx0 + 1);
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let c = src.get(sx, sy);
                    acc[0] += c.r as u32;
                    acc[1] += c.g as u32;
                    acc[2] += c.b as u32;
                    acc[3] += c.a as u32;
                    n += 1;
                }
            }
            out.set(
                dx,
                dy,
                Color::rgba(
                    (acc[0] / n) as u8,
                    (acc[1] / n) as u8,
                    (acc[2] / n) as u8,
                    (acc[3] / n) as u8,
                ),
            );
        }
    }
    out
}

/// Nearest-neighbour resample to an exact size.
pub fn resample_nearest(src: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    if w == 0 || h == 0 || src.width == 0 {
        return src.clone();
    }
    let mut out = RgbaImage::new(w, h);
    for y in 0..h {
        let sy = (y * src.height) / h;
        for x in 0..w {
            let sx = (x * src.width) / w;
            out.set(x, y, src.get(sx.min(src.width - 1), sy.min(src.height - 1)));
        }
    }
    out
}

/// Separable sliding-window box blur, radius in pixels.
pub fn box_blur(src: &RgbaImage, radius: u32) -> RgbaImage {
    if radius == 0 || src.width == 0 || src.height == 0 {
        return src.clone();
    }
    let (w, h) = (src.width as usize, src.height as usize);
    let r = radius as i64;
    let count = (2 * r + 1) as u32;
    let mut tmp = vec![0u8; src.data.len()];
    let mut dst = vec![0u8; src.data.len()];
    // horizontal
    for y in 0..h {
        let row = y * w * 4;
        for c in 0..4 {
            let mut sum: u32 = 0;
            for k in -r..=r {
                let x = k.clamp(0, w as i64 - 1) as usize;
                sum += src.data[row + x * 4 + c] as u32;
            }
            for x in 0..w {
                tmp[row + x * 4 + c] = (sum / count) as u8;
                let xo = (x as i64 - r).clamp(0, w as i64 - 1) as usize;
                let xi = (x as i64 + r + 1).clamp(0, w as i64 - 1) as usize;
                sum = sum - src.data[row + xo * 4 + c] as u32 + src.data[row + xi * 4 + c] as u32;
            }
        }
    }
    // vertical
    for x in 0..w {
        for c in 0..4 {
            let mut sum: u32 = 0;
            for k in -r..=r {
                let y = k.clamp(0, h as i64 - 1) as usize;
                sum += tmp[(y * w + x) * 4 + c] as u32;
            }
            for y in 0..h {
                dst[(y * w + x) * 4 + c] = (sum / count) as u8;
                let yo = (y as i64 - r).clamp(0, h as i64 - 1) as usize;
                let yi = (y as i64 + r + 1).clamp(0, h as i64 - 1) as usize;
                sum = sum - tmp[(yo * w + x) * 4 + c] as u32 + tmp[(yi * w + x) * 4 + c] as u32;
            }
        }
    }
    RgbaImage { width: src.width, height: src.height, data: dst }
}

/// Blur with an effective radius, using the downscale path whenever the
/// radius is large enough for it to win (the Step 0 mandate).
pub fn blur_smart(src: &RgbaImage, radius_px: f32) -> RgbaImage {
    if radius_px <= 1.5 {
        return box_blur(src, radius_px.round().max(1.0) as u32);
    }
    // Scale so the reduced-space radius lands around 2px.
    let factor = (radius_px / 2.0).ceil().max(1.0);
    let small_w = ((src.width as f32 / factor) as u32).max(1);
    let small = downscale(src, small_w);
    let blurred = box_blur(&small, 2);
    resample_nearest(&blurred, src.width, src.height)
}
