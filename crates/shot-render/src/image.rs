//! Minimal RGBA raster: the renderer's canvas. Pixels are straight
//! (non-premultiplied) RGBA8, row-major.

use shot_model::Color;

#[derive(Debug, Clone, PartialEq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>, // RGBA8, len = width * height * 4
}

impl RgbaImage {
    pub fn new(width: u32, height: u32) -> Self {
        RgbaImage { width, height, data: vec![0; (width * height * 4) as usize] }
    }

    pub fn filled(width: u32, height: u32, color: Color) -> Self {
        let mut img = RgbaImage::new(width, height);
        for px in img.data.chunks_exact_mut(4) {
            px[0] = color.r;
            px[1] = color.g;
            px[2] = color.b;
            px[3] = color.a;
        }
        img
    }

    #[inline]
    fn idx(&self, x: u32, y: u32) -> usize {
        ((y * self.width + x) * 4) as usize
    }

    pub fn in_bounds(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height
    }

    pub fn get(&self, x: u32, y: u32) -> Color {
        let i = self.idx(x, y);
        Color::rgba(self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3])
    }

    /// Bilinear sample at fractional coords (edge-clamped). None only
    /// for a degenerate image. Shared by the rotation and warp paths.
    pub fn sample_bilinear(&self, x: f32, y: f32) -> Option<Color> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        let fx = x.clamp(0.0, (self.width - 1) as f32);
        let fy = y.clamp(0.0, (self.height - 1) as f32);
        let x0 = fx.floor() as u32;
        let y0 = fy.floor() as u32;
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);
        let tx = fx - x0 as f32;
        let ty = fy - y0 as f32;
        let (c00, c10, c01, c11) =
            (self.get(x0, y0), self.get(x1, y0), self.get(x0, y1), self.get(x1, y1));
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

    pub fn set(&mut self, x: u32, y: u32, color: Color) {
        let i = self.idx(x, y);
        self.data[i] = color.r;
        self.data[i + 1] = color.g;
        self.data[i + 2] = color.b;
        self.data[i + 3] = color.a;
    }

    /// Alpha-over blend of `src` color onto the pixel at (x, y).
    pub fn blend(&mut self, x: u32, y: u32, src: Color) {
        if src.a == 0 {
            return;
        }
        if src.a == 255 {
            self.set(x, y, src);
            return;
        }
        let i = self.idx(x, y);
        let sa = src.a as u32;
        let da = self.data[i + 3] as u32;
        let out_a = sa + da * (255 - sa) / 255;
        if out_a == 0 {
            self.data[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
            return;
        }
        for c in 0..3 {
            let s = src_channel(src, c) as u32;
            let d = self.data[i + c] as u32;
            self.data[i + c] = ((s * sa + d * da * (255 - sa) / 255) / out_a) as u8;
        }
        self.data[i + 3] = out_a as u8;
    }

    /// Copy `src` image over this one with its top-left at (dx, dy),
    /// alpha-blending per pixel. Negative offsets clip.
    pub fn blit(&mut self, src: &RgbaImage, dx: i32, dy: i32) {
        for sy in 0..src.height as i32 {
            let ty = dy + sy;
            if ty < 0 || ty as u32 >= self.height {
                continue;
            }
            for sx in 0..src.width as i32 {
                let tx = dx + sx;
                if tx < 0 || tx as u32 >= self.width {
                    continue;
                }
                let c = src.get(sx as u32, sy as u32);
                self.blend(tx as u32, ty as u32, c);
            }
        }
    }

    /// Crop a rectangle (clamped to bounds).
    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> RgbaImage {
        let mut out = RgbaImage::new(w, h);
        for oy in 0..h {
            for ox in 0..w {
                let sx = x + ox;
                let sy = y + oy;
                if sx < self.width && sy < self.height {
                    out.set(ox, oy, self.get(sx, sy));
                }
            }
        }
        out
    }

    /// Write a binary PPM (P6, alpha dropped) for quick visual debugging.
    pub fn to_ppm(&self) -> Vec<u8> {
        let mut out = format!("P6\n{} {}\n255\n", self.width, self.height).into_bytes();
        for px in self.data.chunks_exact(4) {
            out.extend_from_slice(&px[..3]);
        }
        out
    }
}

fn src_channel(c: Color, ch: usize) -> u8 {
    match ch {
        0 => c.r,
        1 => c.g,
        _ => c.b,
    }
}
