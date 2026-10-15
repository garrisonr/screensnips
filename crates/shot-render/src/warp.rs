//! Perspective warp ("the straightender"): given the four corners of a
//! photographed document, produce the flat, axis-aligned rectangle the
//! document would be if you'd scanned it. Pure math — homography solved
//! by Gaussian elimination, inverse-mapped bilinear resampling. No
//! dependencies, deterministic, unit-tested.

use crate::image::RgbaImage;

/// A 3x3 projective transform, row-major, h[8] normalized to 1.
pub type Homography = [f32; 9];

/// Solve for the homography mapping `src` points onto `dst` points
/// (both [TL, TR, BR, BL] or any 4 correspondences). None when the
/// system is degenerate (e.g. collinear points).
pub fn homography(src: [(f32, f32); 4], dst: [(f32, f32); 4]) -> Option<Homography> {
    // Build the 8x8 linear system A h = b for h = [h0..h7], h8 = 1.
    let mut a = [[0f32; 9]; 8]; // 8 cols + rhs
    for i in 0..4 {
        let (x, y) = src[i];
        let (u, v) = dst[i];
        let r = i * 2;
        // u = (h0 x + h1 y + h2) / (h6 x + h7 y + 1)
        a[r] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        // v = (h3 x + h4 y + h5) / (h6 x + h7 y + 1)
        a[r + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    // Gaussian elimination with partial pivoting.
    for col in 0..8 {
        let mut pivot = col;
        for r in col + 1..8 {
            if a[r][col].abs() > a[pivot][col].abs() {
                pivot = r;
            }
        }
        if a[pivot][col].abs() < 1e-9 {
            return None;
        }
        if pivot != col {
            a.swap(col, pivot);
        }
        let d = a[col][col];
        for j in col..9 {
            a[col][j] /= d;
        }
        for r in 0..8 {
            if r != col && a[r][col] != 0.0 {
                let f = a[r][col];
                for j in col..9 {
                    a[r][j] -= f * a[col][j];
                }
            }
        }
    }
    Some([a[0][8], a[1][8], a[2][8], a[3][8], a[4][8], a[5][8], a[6][8], a[7][8], 1.0])
}

/// Apply a homography to a point.
pub fn apply(h: &Homography, x: f32, y: f32) -> (f32, f32) {
    let w = h[6] * x + h[7] * y + h[8];
    (
        (h[0] * x + h[1] * y + h[2]) / w,
        (h[3] * x + h[4] * y + h[5]) / w,
    )
}

/// Warp the quadrilateral `corners_px` ([TL, TR, BR, BL], pixel coords in
/// `src`) into an `out_w` x `out_h` upright image.
pub fn warp_quad(
    src: &RgbaImage,
    corners_px: [(f32, f32); 4],
    out_w: u32,
    out_h: u32,
) -> Option<RgbaImage> {
    if out_w == 0 || out_h == 0 {
        return None;
    }
    let dst = [
        (0.0, 0.0),
        (out_w as f32, 0.0),
        (out_w as f32, out_h as f32),
        (0.0, out_h as f32),
    ];
    // We need dst -> src for inverse mapping.
    let inv = homography(dst, corners_px)?;
    let mut out = RgbaImage::new(out_w, out_h);
    for y in 0..out_h {
        for x in 0..out_w {
            let (sx, sy) = apply(&inv, x as f32 + 0.5, y as f32 + 0.5);
            if sx < -0.5 || sy < -0.5 || sx > src.width as f32 - 0.5 || sy > src.height as f32 - 0.5
            {
                continue; // outside the photo: leave transparent
            }
            if let Some(c) = src.sample_bilinear(sx - 0.5, sy - 0.5) {
                out.set(x, y, c);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shot_model::Color;

    fn gradient(w: u32, h: u32) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.set(x, y, Color::rgba((x * 255 / w.max(1)) as u8, (y * 255 / h.max(1)) as u8, 128, 255));
            }
        }
        img
    }

    #[test]
    fn identity_warp_recovers_the_image() {
        let src = gradient(60, 40);
        let corners = [(0.0, 0.0), (60.0, 0.0), (60.0, 40.0), (0.0, 40.0)];
        let out = warp_quad(&src, corners, 60, 40).unwrap();
        // center pixel survives the round trip ~exactly
        let (a, b) = (src.get(30, 20), out.get(30, 20));
        assert!((a.r as i32 - b.r as i32).abs() <= 2, "r drift: {} vs {}", a.r, b.r);
        assert!((a.g as i32 - b.g as i32).abs() <= 2, "g drift: {} vs {}", a.g, b.g);
    }

    #[test]
    fn warped_quad_flattens_a_skewed_patch() {
        // A red square painted in the source at a skewed quad; after
        // warping that quad to a rectangle, the center must be red and
        // the output corner must NOT be (it maps inside the source).
        let mut src = RgbaImage::filled(100, 100, Color::WHITE);
        let quad = [(30.0, 20.0), (85.0, 30.0), (75.0, 80.0), (20.0, 65.0)];
        crate::draw::polygon_fill(&mut src, &quad, Color::rgba(255, 0, 0, 255));
        let out = warp_quad(&src, quad, 55, 50).unwrap();
        let center = out.get(27, 25);
        assert_eq!(center, Color::rgba(255, 0, 0, 255));
    }

    #[test]
    fn degenerate_quad_is_none_not_a_panic() {
        let src = gradient(10, 10);
        let line = [(0.0, 0.0), (1.0, 1.0), (2.0, 2.0), (3.0, 3.0)];
        assert!(warp_quad(&src, line, 10, 10).is_none());
    }
}
