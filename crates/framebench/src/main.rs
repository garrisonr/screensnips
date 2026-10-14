//! Screensnips Step 0 — frame-pipeline benchmark (Rust side).
//!
//! Measures the costs the editor's design depends on, at real screenshot sizes:
//! - full-frame copy (memory bandwidth floor for any pipeline),
//! - thumbnail downscale, 1 thread vs all threads (the "Rust worker thumbnails" claim),
//! - separable box blur (drop-shadow proxy),
//! - beautify composite: padded canvas + blurred shadow + frame (the Phase 1 render),
//! - chunked stream throughput (custom-protocol delivery simulation: 256 KiB chunks,
//!   checksum sink so nothing is optimized away).
//!
//! This does NOT measure WebKit/canvas presentation — that leg is the Tauri harness.
//! Pure std. Usage: screensnips-framebench [--save <path>]

use std::env;
use std::fs;
use std::time::Instant;

const SIZES: [(u32, u32, &str); 3] = [
    (2560, 1080, "2560x1080 (G9 ultrawide, actual)"),
    (3840, 2160, "3840x2160 (4K)"),
    (5120, 2880, "5120x2880 (5K)"),
];

fn make_frame(w: u32, h: u32) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            buf[i] = (x % 251) as u8;
            buf[i + 1] = (y % 241) as u8;
            buf[i + 2] = ((x ^ y) % 233) as u8;
            buf[i + 3] = 255;
        }
    }
    buf
}

fn median_ms(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    samples[samples.len() / 2]
}

fn bench<F: FnMut()>(reps: usize, mut f: F) -> f64 {
    f(); // warmup
    let mut samples = Vec::new();
    for _ in 0..reps {
        let t = Instant::now();
        f();
        samples.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    median_ms(samples)
}

/// Separable box blur over an RGBA buffer (sliding window), radius r.
fn box_blur(src: &[u8], w: u32, h: u32, r: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut tmp = vec![0u8; src.len()];
    let mut dst = vec![0u8; src.len()];
    let r = r as i64;
    // horizontal
    for y in 0..h {
        let row = y * w * 4;
        for c in 0..4 {
            let mut sum: u32 = 0;
            // prime window
            for k in -r..=r {
                let x = k.clamp(0, w as i64 - 1) as usize;
                sum += src[row + x * 4 + c] as u32;
            }
            let count = (2 * r + 1) as u32;
            for x in 0..w {
                tmp[row + x * 4 + c] = (sum / count) as u8;
                let x_out = (x as i64 - r).clamp(0, w as i64 - 1) as usize;
                let x_in = (x as i64 + r + 1).clamp(0, w as i64 - 1) as usize;
                sum = sum - src[row + x_out * 4 + c] as u32 + src[row + x_in * 4 + c] as u32;
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
            let count = (2 * r + 1) as u32;
            for y in 0..h {
                dst[(y * w + x) * 4 + c] = (sum / count) as u8;
                let y_out = (y as i64 - r).clamp(0, h as i64 - 1) as usize;
                let y_in = (y as i64 + r + 1).clamp(0, h as i64 - 1) as usize;
                sum = sum - tmp[(y_out * w + x) * 4 + c] as u32 + tmp[(y_in * w + x) * 4 + c] as u32;
            }
        }
    }
    dst
}

/// Box-average downscale to target width; rows split across `threads` scoped threads.
fn thumbnail(src: &[u8], w: u32, h: u32, target_w: u32, threads: usize) -> Vec<u8> {
    let tw = target_w;
    let th = ((h as u64 * target_w as u64) / w as u64).max(1) as u32;
    let mut dst = vec![0u8; (tw * th * 4) as usize];
    let rows_per = th.div_ceil(threads as u32);
    let dst_base = dst.as_mut_ptr() as usize;
    std::thread::scope(|s| {
        for t in 0..threads {
            let y0 = t as u32 * rows_per;
            let y1 = ((t as u32 + 1) * rows_per).min(th);
            if y0 >= y1 {
                continue;
            }
            let src_ref = src;
            s.spawn(move || {
                for dy in y0..y1 {
                    let sy0 = (dy * h) / th;
                    let sy1 = (((dy + 1) * h) / th).max(sy0 + 1);
                    for dx in 0..tw {
                        let sx0 = (dx * w) / tw;
                        let sx1 = (((dx + 1) * w) / tw).max(sx0 + 1);
                        let mut acc = [0u32; 4];
                        let mut n = 0u32;
                        for sy in sy0..sy1 {
                            for sx in sx0..sx1 {
                                let i = ((sy * w + sx) * 4) as usize;
                                for c in 0..4 {
                                    acc[c] += src_ref[i + c] as u32;
                                }
                                n += 1;
                            }
                        }
                        let di = dst_base + ((dy * tw + dx) * 4) as usize;
                        unsafe {
                            let p = di as *mut u8;
                            for c in 0..4 {
                                *p.add(c) = (acc[c] / n) as u8;
                            }
                        }
                    }
                }
            });
        }
    });
    dst
}

/// Beautify composite proxy: padded canvas, bg fill, blurred offset shadow, frame copy-in.
fn beautify(frame: &[u8], w: u32, h: u32) -> Vec<u8> {
    let pad_x = w / 12;
    let pad_y = h / 12;
    let cw = w + pad_x * 2;
    let ch = h + pad_y * 2;
    // shadow layer: dark rect at frame position + offset, blurred
    let mut shadow = vec![0u8; (cw * ch * 4) as usize];
    let dy_off = pad_y / 3;
    for y in 0..h {
        let row = ((y + pad_y + dy_off) * cw + pad_x) as usize * 4;
        for x in 0..w as usize {
            let i = row + x * 4;
            shadow[i] = 20;
            shadow[i + 1] = 20;
            shadow[i + 2] = 24;
            shadow[i + 3] = 190;
        }
    }
    let shadow = box_blur(&shadow, cw, ch, 24);
    // canvas = gradient background, then shadow, then frame (opaque copy)
    let mut canvas = vec![0u8; (cw * ch * 4) as usize];
    for i in (0..canvas.len()).step_by(4) {
        canvas[i] = 88;
        canvas[i + 1] = 101;
        canvas[i + 2] = 220;
        canvas[i + 3] = 255;
    }
    // naive alpha-over of shadow (dark, mostly matters where frame is absent)
    for i in (0..canvas.len()).step_by(4) {
        let a = shadow[i + 3] as u32;
        if a > 0 {
            for c in 0..3 {
                canvas[i + c] = ((shadow[i + c] as u32 * a + canvas[i + c] as u32 * (255 - a)) / 255) as u8;
            }
        }
    }
    for y in 0..h {
        let dst = (((y + pad_y) * cw + pad_x) * 4) as usize;
        let src_i = (y * w * 4) as usize;
        canvas[dst..dst + (w * 4) as usize].copy_from_slice(&frame[src_i..src_i + (w * 4) as usize]);
    }
    canvas
}

/// Stream simulation: deliver the frame in 256 KiB chunks to a checksum sink.
/// Returns the checksum so the caller can black-box it (otherwise the whole
/// loop is dead code and the compiler deletes it — measured 0.00 ms once).
fn stream_once(frame: &[u8]) -> u64 {
    let chunk = 256 * 1024;
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut sink = vec![0u8; chunk];
    for part in frame.chunks(chunk) {
        sink[..part.len()].copy_from_slice(part);
        for &b in &sink[..part.len()] {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    std::hint::black_box(&sink);
    hash
}

/// Shadow-blur the cheap way production uses: blur a 1/8-scale copy, upscale back.
/// (A drop shadow is a low-frequency signal; full-res blur is wasted work.)
fn blur_downscaled(frame: &[u8], w: u32, h: u32, threads: usize) -> Vec<u8> {
    let sw = (w / 8).max(1);
    let small = thumbnail(frame, w, h, sw, threads);
    let sh = ((h as u64 * sw as u64) / w as u64).max(1) as u32;
    let blurred = box_blur(&small, sw, sh, 2);
    // nearest-neighbour upscale back to full size
    let mut dst = vec![0u8; frame.len()];
    for y in 0..h {
        let sy = (y * sh) / h;
        for x in 0..w {
            let sx = (x * sw) / w;
            let si = ((sy * sw + sx) * 4) as usize;
            let di = ((y * w + x) * 4) as usize;
            dst[di..di + 4].copy_from_slice(&blurred[si..si + 4]);
        }
    }
    dst
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let save_path: Option<String> = args.windows(2).find(|w| w[0] == "--save").map(|w| w[1].clone());
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let mut out = String::new();
    out.push_str("SCREENSNIPS — STEP 0 FRAME BENCH (Rust side)\n");
    out.push_str(&format!("threads available: {}\n", threads));
    out.push_str("(WebKit/canvas presentation leg is measured by the Tauri harness, not here.)\n");

    for (w, h, label) in SIZES {
        let frame = make_frame(w, h);
        let bytes = frame.len() as f64;
        let reps = if w >= 5120 { 3 } else { 5 };
        out.push_str(&format!("\n== {} ==\n", label));
        let mut copy_dst = vec![0u8; frame.len()];
        let copy_ms = bench(reps, || {
            copy_dst.copy_from_slice(&frame);
            std::hint::black_box(&copy_dst);
        });
        out.push_str(&format!("  frame copy:        {:8.2} ms  ({:.2} GB/s)\n", copy_ms, bytes / (copy_ms / 1000.0) / 1e9));
        let th1 = bench(reps, || {
            let t = thumbnail(&frame, w, h, 512, 1);
            std::hint::black_box(&t);
        });
        let thn = bench(reps, || {
            let t = thumbnail(&frame, w, h, 512, threads);
            std::hint::black_box(&t);
        });
        out.push_str(&format!("  thumbnail 512w 1T: {:8.2} ms\n", th1));
        out.push_str(&format!("  thumbnail 512w {}T: {:8.2} ms  ({:.1}x)\n", threads, thn, th1 / thn));
        let blur_ms = bench(reps, || {
            let b = box_blur(&frame, w, h, 16);
            std::hint::black_box(&b);
        });
        out.push_str(&format!("  box blur r16 full: {:8.2} ms\n", blur_ms));
        let blur_ds_ms = bench(reps, || {
            let b = blur_downscaled(&frame, w, h, threads);
            std::hint::black_box(&b);
        });
        out.push_str(&format!("  blur via 1/8 scale:{:8.2} ms  ({:.1}x faster)\n", blur_ds_ms, blur_ms / blur_ds_ms));
        let comp_ms = bench(reps, || {
            let c = beautify(&frame, w, h);
            std::hint::black_box(&c);
        });
        out.push_str(&format!("  beautify composite:{:8.2} ms  ({:.0} fps equiv)\n", comp_ms, 1000.0 / comp_ms));
        let stream_ms = bench(reps, || {
            let hh = stream_once(&frame);
            std::hint::black_box(hh);
        });
        out.push_str(&format!("  stream 256K chunks:{:8.2} ms  ({:.2} GB/s)\n", stream_ms, bytes / (stream_ms / 1000.0) / 1e9));
    }

    print!("{}", out);
    if let Some(p) = save_path {
        let tmp = format!("{}.tmp", p);
        if fs::write(&tmp, &out).is_ok()
            && fs::read_to_string(&tmp).map(|s| s == out).unwrap_or(false)
            && fs::rename(&tmp, &p).is_ok()
        {
            eprintln!("Report saved to {}", p);
        } else {
            eprintln!("WARNING: could not save report to {}", p);
            let _ = fs::remove_file(&tmp);
        }
    }
}
