//! Screensnips renderer (prototype): turns a shot-model `Document` plus its
//! source pixels into a finished, beautified image. Pure Rust software
//! rasterization; GPU/canvas presentation lives in the app layer under the
//! Step 0 dirty-region rules.

pub mod blur;
pub mod draw;
pub mod font;
pub mod image;
pub mod render;
pub mod text;

pub use image::RgbaImage;
pub use render::{render, RenderError, RenderOutcome};

/// Path of the system font used for text rasterization (diagnostics);
/// None means the bitmap fallback is active.
pub fn text_font_path() -> Option<&'static str> {
    font::font_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shot_model::*;

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
            },
            Style::default(),
        );
        let src = solid_source(200, 100, Color::WHITE);
        let out = render(&doc, &src).unwrap();
        assert_eq!(out.image.get(60, 50), Color::ANNOTATION_RED);
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
            PrimitiveKind::Text { at: Point::new(0.05, 0.1), text: "HI".into() },
            Style { font_size: Some(0.14), ..Style::default() },
        );
        doc.add(
            PrimitiveKind::Stamp {
                at: Point::new(0.7, 0.7),
                stamp_id: "check".into(),
                scale: 0.1,
            },
            Style::default(),
        );
        let bogus_id = doc.add(
            PrimitiveKind::Stamp {
                at: Point::new(0.85, 0.1),
                stamp_id: "does-not-exist".into(),
                scale: 0.1,
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
    fn stamp_check_paints_its_stroke() {
        let mut doc = doc_for(200, 100);
        doc.beautify.padding = Padding::uniform(0.0);
        doc.beautify.shadow.enabled = false;
        // stamp box: at (0.7, 0.7) scale 0.1 of width 200 -> 20px box at (140,70)
        doc.add(
            PrimitiveKind::Stamp {
                at: Point::new(0.7, 0.7),
                stamp_id: "star".into(),
                scale: 0.1,
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
