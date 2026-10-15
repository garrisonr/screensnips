//! Screensnips renderer (prototype): turns a shot-model `Document` plus its
//! source pixels into a finished, beautified image. Pure Rust software
//! rasterization; GPU/canvas presentation lives in the app layer under the
//! Step 0 dirty-region rules.

pub mod blur;
pub mod detect;
pub mod draw;
pub mod font;
pub mod image;
pub mod render;
pub mod text;
pub mod warp;

pub use image::RgbaImage;
pub use render::{render, RenderError, RenderOutcome};
pub use blur::resample_nearest;

/// Path of the system font used for text rasterization (diagnostics);
/// None means the bitmap fallback is active.
pub fn text_font_path() -> Option<&'static str> {
    font::font_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shot_model::*;

    #[test]
    fn callout_advance_constant_tracks_the_real_font() {
        // The model's callout estimate is calibrated to this crate's
        // font; if the font stack changes and the constant drifts,
        // this fails and the constant gets re-measured — the editor,
        // model, and exporter must not draw three different boxes.
        // (Skipped when no system font is available: the bitmap
        // fallback has its own advance and the estimate only drives
        // handles there, never drawn pixels.)
        if let Some(measured) = font::avg_advance_em() {
            assert!(
                (measured - shot_model::CALLOUT_ADVANCE_EM).abs() < 0.10,
                "measured advance {measured:.3} vs model constant {}",
                shot_model::CALLOUT_ADVANCE_EM
            );
        }
    }

    #[test]
    fn aspect_growth_centers_the_shot_and_shadow() {
        // Regression (audit fix): aspect growth used to grow pad_l
        // only. The shot — and the shadow mass around it — must land
        // symmetric on the grown canvas, verified in pixels.
        let mut doc = doc_for(200, 100);
        doc.beautify.padding = Padding::uniform(0.1);
        doc.beautify.background = Background::Solid {
            color: Color::rgba(200, 200, 200, 255),
        };
        doc.beautify.shadow.enabled = true;
        doc.beautify.shadow.opacity = 0.9;
        doc.beautify.shadow.blur = 0.02;
        doc.beautify.shadow.offset_y = 0.0;
        doc.beautify.aspect = Some(3.0); // forces wide growth
        let src = solid_source(200, 100, Color::WHITE);
        let out = render(&doc, &src).unwrap();
        let img = &out.image;
        assert!(img.width > 300, "canvas should have grown, w={}", img.width);
        // Locate the white shot on the middle row.
        let y = img.height / 2;
        let row: Vec<Color> = (0..img.width).map(|x| img.get(x, y)).collect();
        let first = row.iter().position(|c| c.r > 250).unwrap();
        let last = row.iter().rposition(|c| c.r > 250).unwrap();
        let left_margin = first as i32;
        let right_margin = img.width as i32 - 1 - last as i32;
        assert!(
            (left_margin - right_margin).abs() <= 1,
            "shot not centered: left {left_margin} vs right {right_margin}"
        );
        // Shadow mass: darkened (non-background, non-shot) pixels in
        // the left pad zone vs the right pad zone on the middle rows.
        let count_zone = |x0: u32, x1: u32| -> usize {
            let mut n = 0;
            for yy in (img.height / 3)..(2 * img.height / 3) {
                for x in x0..x1 {
                    let c = img.get(x, yy);
                    if c.r < 185 {
                        n += 1;
                    }
                }
            }
            n
        };
        let left_shadow = count_zone(0, first as u32);
        let right_shadow = count_zone(last as u32 + 1, img.width);
        assert!(left_shadow > 0 && right_shadow > 0, "shadow missing on a side");
        let (hi, lo) = if left_shadow > right_shadow {
            (left_shadow, right_shadow)
        } else {
            (right_shadow, left_shadow)
        };
        assert!(
            ((hi - lo) as f32 / hi as f32) < 0.15,
            "shadow mass asymmetric: left {left_shadow} vs right {right_shadow}"
        );
    }

    fn doc_for(w: u32, h: u32) -> Document {
        Document::new(ImageRef { path: "x.png".into(), width: w, height: h, sha256: None })
    }

    fn solid_source(w: u32, h: u32, c: Color) -> RgbaImage {
        RgbaImage::filled(w, h, c)
    }

    #[test]
    fn canvas_dimensions_include_uniform_padding() {
        let mut doc = doc_for(200, 100);
        doc.beautify.padding = Padding::uniform(0.1); // 20px per side (of width 200)
        doc.beautify.shadow.enabled = false;
        let src = solid_source(200, 100, Color::WHITE);
        let out = render(&doc, &src).unwrap();
        assert_eq!((out.image.width, out.image.height), (240, 140));
        // corner shows the solid background color
        assert_eq!(out.image.get(0, 0), Color::rgba(88, 101, 220, 255));
        // center shows the white shot
        assert_eq!(out.image.get(120, 70), Color::WHITE);
    }

    #[test]
    fn shadow_darkens_background_below_shot() {
        let mut doc = doc_for(200, 100);
        doc.beautify.background = Background::Solid { color: Color::WHITE };
        doc.beautify.padding = Padding::uniform(0.25);
        doc.beautify.shadow = Shadow { enabled: true, blur: 0.05, offset_y: 0.1, opacity: 0.5 };
        let src = solid_source(200, 100, Color::rgba(200, 200, 200, 255));
        let out = render(&doc, &src).unwrap();
        // sample far below-left of the shot (no shadow) vs just below the shot
        let plain = out.image.get(5, out.image.height - 5);
        let shaded = out.image.get(100, 100 + 50 + 12);
        assert_eq!(plain, Color::WHITE);
        assert!(shaded.r < 250, "shadow should darken: got {:?}", shaded);
    }

    #[test]
    fn arrow_paints_stroke_color_on_its_path() {
        let mut doc = doc_for(200, 100);
        doc.beautify.padding = Padding::uniform(0.0);
        doc.beautify.shadow.enabled = false;
        doc.add(
            PrimitiveKind::Arrow {
                from: Point::new(0.1, 0.5),
                to: Point::new(0.9, 0.5),
                bend: 0.0,
            },
            Style::default(),
        );
        let src = solid_source(200, 100, Color::WHITE);
        let out = render(&doc, &src).unwrap();
        assert_eq!(out.image.get(60, 50), Color::ANNOTATION_RED);
    }

    #[test]
    fn rotated_stamp_paints_beyond_its_upright_box() {
        // An upright arrow_up stamp paints nothing at its top-left
        // corner; rotated 45°, ink lands there. Proves the rotation
        // path in the renderer actually turns the artwork.
        let render_with = |deg: f32| {
            let mut doc = doc_for(200, 200);
            doc.beautify.padding = Padding::uniform(0.0);
            doc.beautify.shadow.enabled = false;
            doc.add(
                PrimitiveKind::Stamp {
                    at: Point::new(0.3, 0.3),
                    stamp_id: "cross".into(),
                    scale: 0.4,
                    rotation_deg: deg,
                },
                Style::default(),
            );
            let src = solid_source(200, 200, Color::WHITE);
            render(&doc, &src).unwrap().image
        };
        // stamp spans px 60..140 in both axes; its center is (100, 100).
        // A point at 45° up-left of center sits beyond the cross's
        // upright arms (which run corner to corner through the center —
        // sample instead where ONLY the rotated arm reaches: midway
        // along the up direction from center at r=34).
        let upright = render_with(0.0);
        let rotated = render_with(45.0);
        let probe = |img: &RgbaImage| img.get(100, 100 - 25);
        // (100, 75): upright cross has no ink there (its arms are the
        // diagonals); rotated 45°, an arm points straight up through it.
        assert_eq!(probe(&upright), Color::WHITE);
        assert_eq!(probe(&rotated), Color::ANNOTATION_RED);
    }

    #[test]
    fn curved_arrow_bows_off_the_straight_line() {
        // A rightward arrow bent +0.45 bows DOWN-screen (the (−dy, dx)
        // normal of rightward travel points down in y-down space): the
        // curve's apex region carries ink where the straight arrow
        // leaves white.
        let render_bend = |bend: f32| {
            let mut doc = doc_for(200, 200);
            doc.beautify.padding = Padding::uniform(0.0);
            doc.beautify.shadow.enabled = false;
            doc.add(
                PrimitiveKind::Arrow {
                    from: Point::new(0.1, 0.5),
                    to: Point::new(0.9, 0.5),
                    bend,
                },
                Style::default(),
            );
            let src = solid_source(200, 200, Color::WHITE);
            render(&doc, &src).unwrap().image
        };
        let straight = render_bend(0.0);
        let curved = render_bend(0.45);
        // apex of the curve is (100, 136): probe right on it.
        assert_eq!(straight.get(100, 136), Color::WHITE);
        assert_eq!(curved.get(100, 136), Color::ANNOTATION_RED);
    }

    #[test]
    fn frame_effects_paint() {
        use shot_model::FrameStyle;
        let render_frame = |frame: FrameStyle| {
            let mut doc = doc_for(240, 160);
            doc.beautify.padding = Padding::uniform(0.05);
            doc.beautify.frame = frame;
            let src = solid_source(240, 160, Color::WHITE);
            render(&doc, &src).unwrap().image
        };
        let none = render_frame(FrameStyle::None);
        let instant = render_frame(FrameStyle::Instant);
        // Instant's chin makes the canvas taller than the plain render.
        assert!(instant.height > none.height + 20, "instant {} vs none {}", instant.height, none.height);
        // The chin (below the shot, inside the frame) is frame-white,
        // where the plain render shows the indigo background.
        let (iw, ih) = (instant.width, instant.height);
        let chin = instant.get(iw / 2, ih - 16);
        assert_eq!(chin, Color::rgba(252, 252, 253, 255), "chin pixel: {:?}", chin);
        // Postcard: the border ring is frame-white (the hairline's
        // anti-aliasing grazes it, so: near-white, not exact).
        let post = render_frame(FrameStyle::Postcard);
        let border = post.get(12, post.height / 2);
        assert!(border.r >= 240 && border.g >= 240 && border.b >= 240, "postcard border: {:?}", border);
        // Peeled: the extreme bottom-right corner of the shot reveals
        // the background instead of the source's white.
        let peeled = render_frame(FrameStyle::Peeled);
        let corner = peeled.get(peeled.width - 13 - 1, peeled.height - 13 - 1);
        assert_ne!(corner, Color::WHITE, "peeled corner still shows the shot");
    }

    #[test]
    fn second_row_stamps_all_paint() {
        // heart, question, exclaim, pin, smiley, ring — every new stamp
        // must render ink (a stamp that returns false lands in
        // `unrendered`, which the pipeline treats as a bug).
        for id in ["heart", "question", "exclaim", "pin", "smiley", "ring"] {
            let mut doc = doc_for(200, 200);
            doc.beautify.padding = Padding::uniform(0.0);
            doc.beautify.shadow.enabled = false;
            doc.add(
                PrimitiveKind::Stamp {
                    at: Point::new(0.5, 0.5),
                    stamp_id: id.into(),
                    scale: 0.5,
                    rotation_deg: 0.0,
                },
                Style::default(),
            );
            let src = solid_source(200, 200, Color::WHITE);
            let out = render(&doc, &src).unwrap();
            assert!(out.unrendered.is_empty(), "{} did not render", id);
            let red_pixels = (0..200)
                .flat_map(|y| (0..200).map(move |x| (x, y)))
                .filter(|&(x, y)| out.image.get(x, y) == Color::ANNOTATION_RED)
                .count();
            assert!(red_pixels > 40, "{} painted only {} red px", id, red_pixels);
        }
        // the pin's hole is a real hole: its center stays transparent-ish
        // (white source shows through where the pin head's hole is).
        let mut doc = doc_for(200, 200);
        doc.beautify.padding = Padding::uniform(0.0);
        doc.beautify.shadow.enabled = false;
        doc.add(
            PrimitiveKind::Stamp {
                at: Point::new(0.5, 0.5),
                stamp_id: "pin".into(),
                scale: 0.5,
                rotation_deg: 0.0,
            },
            Style::default(),
        );
        let src = solid_source(200, 200, Color::WHITE);
        let out = render(&doc, &src).unwrap();
        assert_eq!(out.image.get(100, 86), Color::WHITE, "pin hole should show the source");
    }

    #[test]
    fn step_badge_is_filled_circle_with_white_numeral() {
        let mut doc = doc_for(200, 100);
        doc.beautify.padding = Padding::uniform(0.0);
        doc.beautify.shadow.enabled = false;
        doc.add(PrimitiveKind::StepBadge { at: Point::new(0.5, 0.5) }, Style::default());
        let src = solid_source(200, 100, Color::BLACK);
        let out = render(&doc, &src).unwrap();
        // badge center area should be badge red (circle fill)
        let center = out.image.get(100, 50);
        assert_eq!(center, Color::ANNOTATION_RED);
        // and some white numeral pixels should exist inside the badge
        let mut white = 0;
        for y in 38..62 {
            for x in 88..112 {
                if out.image.get(x, y) == Color::WHITE {
                    white += 1;
                }
            }
        }
        assert!(white > 4, "expected numeral pixels, found {}", white);
    }

    #[test]
    fn pixelate_flattens_a_checkerboard() {
        let mut doc = doc_for(64, 64);
        doc.beautify.padding = Padding::uniform(0.0);
        doc.beautify.shadow.enabled = false;
        let mut src = RgbaImage::new(64, 64);
        for y in 0..64 {
            for x in 0..64 {
                let c = if (x + y) % 2 == 0 { Color::BLACK } else { Color::WHITE };
                src.set(x, y, c);
            }
        }
        doc.add(
            PrimitiveKind::Pixelate {
                rect: RectN { x: 0.0, y: 0.0, w: 1.0, h: 1.0 },
            },
            Style::default(),
        );
        let out = render(&doc, &src).unwrap();
        // after pixelation with block >= 4, immediate neighbours inside a
        // block must agree — the 1px checkerboard must be gone
        assert_eq!(out.image.get(10, 10), out.image.get(11, 10));
        assert_eq!(out.image.get(10, 10), out.image.get(10, 11));
    }

    #[test]
    fn size_mismatch_is_an_error_not_a_guess() {
        let doc = doc_for(200, 100);
        let src = solid_source(100, 100, Color::WHITE);
        assert!(matches!(render(&doc, &src), Err(RenderError::SizeMismatch { .. })));
    }

    #[test]
    fn text_renders_and_builtin_stamps_paint_unknown_stamps_report() {
        let mut doc = doc_for(200, 100);
        doc.beautify.padding = Padding::uniform(0.0);
        doc.beautify.shadow.enabled = false;
        doc.add(
            PrimitiveKind::Text { at: Point::new(0.05, 0.1), text: "HI".into(), script: false },
            Style { font_size: Some(0.14), ..Style::default() },
        );
        doc.add(
            PrimitiveKind::Stamp {
                at: Point::new(0.7, 0.7),
                stamp_id: "check".into(),
                scale: 0.1, rotation_deg: 0.0,
            },
            Style::default(),
        );
        let bogus_id = doc.add(
            PrimitiveKind::Stamp {
                at: Point::new(0.85, 0.1),
                stamp_id: "does-not-exist".into(),
                scale: 0.1,
                rotation_deg: 0.0,
            },
            Style::default(),
        );
        let src = solid_source(200, 100, Color::WHITE);
        let out = render(&doc, &src).unwrap();
        // the built-in check stamp paints; only the unknown id reports
        assert_eq!(out.unrendered, vec![bogus_id]);
        // and text pixels actually landed (annotation-red on white field)
        let mut red = 0;
        for y in 5..45 {
            for x in 5..80 {
                if out.image.get(x, y) == Color::ANNOTATION_RED {
                    red += 1;
                }
            }
        }
        assert!(red > 20, "expected text glyph pixels, found {}", red);
    }

    #[test]
    fn word_stamps_paint_on_a_wide_screenshot() {
        // RECEIVED / REVIEWED / APPROVED — the HR rubber stamps.
        // Non-square fixture on purpose (the §19 lesson: aspect-blind
        // stamp geometry only shows its bugs off the square).
        for id in ["received", "reviewed", "approved"] {
            let mut doc = doc_for(1000, 400);
            doc.beautify.padding = Padding::uniform(0.0);
            doc.beautify.shadow.enabled = false;
            doc.add(
                PrimitiveKind::Stamp {
                    at: Point::new(0.3, 0.3),
                    stamp_id: id.into(),
                    scale: 0.3,
                    rotation_deg: 0.0,
                },
                Style::default(),
            );
            let src = solid_source(1000, 400, Color::WHITE);
            let out = render(&doc, &src).unwrap();
            assert!(out.unrendered.is_empty(), "{id} reported unrendered");
            // Stamp tile: 300px at (300, 120). Count annotation-red.
            let mut red = 0;
            for y in 120..420.min(out.image.height) {
                for x in 300..600 {
                    if out.image.get(x, y) == Color::ANNOTATION_RED {
                        red += 1;
                    }
                }
            }
            assert!(red > 400, "{id} should paint border + word, found {red} px");
        }
    }

    #[test]
    fn stamp_check_paints_its_stroke() {
        let mut doc = doc_for(200, 100);
        doc.beautify.padding = Padding::uniform(0.0);
        doc.beautify.shadow.enabled = false;
        // stamp box: at (0.7, 0.7) scale 0.1 of width 200 -> 20px box at (140,70)
        doc.add(
            PrimitiveKind::Stamp {
                at: Point::new(0.7, 0.7),
                stamp_id: "star".into(),
                scale: 0.1, rotation_deg: 0.0,
            },
            Style::default(),
        );
        let src = solid_source(200, 100, Color::WHITE);
        let out = render(&doc, &src).unwrap();
        assert!(out.unrendered.is_empty());
        let mut red = 0;
        for y in 68..92 {
            for x in 138..162 {
                if out.image.get(x, y) == Color::ANNOTATION_RED {
                    red += 1;
                }
            }
        }
        assert!(red > 25, "star should paint a filled glyph, found {} px", red);
    }

    #[test]
    fn spotlight_dims_outside_and_spares_inside() {
        let mut doc = doc_for(200, 100);
        doc.beautify.padding = Padding::uniform(0.0);
        doc.beautify.shadow.enabled = false;
        doc.add(
            PrimitiveKind::Spotlight {
                rect: RectN { x: 0.4, y: 0.4, w: 0.2, h: 0.2 },
            },
            Style::default(),
        );
        let src = solid_source(200, 100, Color::WHITE);
        let out = render(&doc, &src).unwrap();
        assert_eq!(out.image.get(100, 50), Color::WHITE, "inside stays lit");
        let outside = out.image.get(5, 5);
        assert!(outside.r < 130 && outside.a == 255, "outside dims: {:?}", outside);
    }

    #[test]
    fn callout_paints_box_tail_and_text() {
        let mut doc = doc_for(400, 200);
        doc.beautify.padding = Padding::uniform(0.0);
        doc.beautify.shadow.enabled = false;
        doc.add(
            PrimitiveKind::Callout {
                at: Point::new(0.5, 0.55),
                tail: Point::new(0.2, 0.2),
                text: "LOOK".into(),
            },
            Style { font_size: Some(0.05), ..Style::default() },
        );
        let src = solid_source(400, 200, Color::WHITE);
        let out = render(&doc, &src).unwrap();
        assert!(out.unrendered.is_empty());
        // box fill is the dark surface color in the left pad column
        // (text never reaches there, whatever font measured it)
        let inside = out.image.get(204, 144);
        assert!(inside.r < 60 && inside.a == 255, "callout box fill: {:?}", inside);
        // the tail endpoint dot is annotation red (tail at 0.2,0.2 -> 80,40)
        let dot = out.image.get(80, 40);
        assert_eq!(dot, Color::ANNOTATION_RED, "tail dot: {:?}", dot);
    }

    #[test]
    fn aspect_grows_canvas_to_target() {
        let mut doc = doc_for(200, 100);
        doc.beautify.padding = Padding::uniform(0.0);
        doc.beautify.shadow.enabled = false;
        doc.beautify.aspect = Some(1.0); // square from a 2:1 shot
        let src = solid_source(200, 100, Color::WHITE);
        let out = render(&doc, &src).unwrap();
        assert_eq!(out.image.width, 200);
        assert!((out.image.height as i32 - 200).abs() <= 1, "height {}", out.image.height);
        // grown area is background; shot is vertically centered
        assert_eq!(out.image.get(100, 100), Color::WHITE);
        assert_eq!(out.image.get(100, 2), Color::rgba(88, 101, 220, 255));
    }
}
