//! Screensnips document model.
//!
//! A document is a captured image (stored at physical pixels, referenced by
//! path) plus everything done to it, non-destructively:
//! - `beautify`: background, padding, corner radius, shadow (Phase 1 render
//!   settings; the renderer must use the downscaled-blur shadow path —
//!   Step 0 benchmark law),
//! - `primitives`: vector annotations in z-order. ALL geometry is normalized
//!   to [0,1] relative to the image, so annotations survive re-crop, resize
//!   and backend changes untouched. Rasterization happens at export size.
//!
//! Serialization is a TOML sidecar (`annotations.toml`) with an explicit
//! `format_version`. The renderer additionally uses `Primitive::bbox()` to
//! re-present only dirty regions during editing (Step 0 constraint: full
//! frames cross the protocol boundary only on capture/open).

use serde::{Deserialize, Serialize};

pub const FORMAT_VERSION: u32 = 1;

/// A point in normalized image space: (0,0) top-left, (1,1) bottom-right.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Point { x, y }
    }
    pub fn in_range(&self) -> bool {
        (0.0..=1.0).contains(&self.x) && (0.0..=1.0).contains(&self.y)
    }
}

/// An axis-aligned rectangle in normalized image space.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RectN {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectN {
    pub fn from_corners(a: Point, b: Point) -> Self {
        RectN {
            x: a.x.min(b.x),
            y: a.y.min(b.y),
            w: (a.x - b.x).abs(),
            h: (a.y - b.y).abs(),
        }
    }
    pub fn in_range(&self) -> bool {
        self.x >= 0.0
            && self.y >= 0.0
            && self.x + self.w <= 1.0 + f32::EPSILON
            && self.y + self.h <= 1.0 + f32::EPSILON
            && self.w > 0.0
            && self.h > 0.0
    }
    pub fn union(&self, other: &RectN) -> RectN {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let r = (self.x + self.w).max(other.x + other.w);
        let b = (self.y + self.h).max(other.y + other.h);
        RectN { x, y, w: r - x, h: b - y }
    }
}

/// RGBA color, 8 bits per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color { r, g, b, a }
    }
    /// Default annotation red (Snagit-family), fully opaque.
    pub const ANNOTATION_RED: Color = Color::rgba(255, 59, 48, 255);
    pub const WHITE: Color = Color::rgba(255, 255, 255, 255);
    pub const BLACK: Color = Color::rgba(0, 0, 0, 255);
}

/// Stroke/fill/text styling for a primitive. Widths and font sizes are
/// fractions of image WIDTH, so styles are resolution-independent.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Style {
    pub stroke: Color,
    pub stroke_width: f32,
    pub fill: Option<Color>,
    pub font_size: Option<f32>,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            stroke: Color::ANNOTATION_RED,
            stroke_width: 0.004,
            fill: None,
            font_size: None,
        }
    }
}

/// One vector annotation. Numbers on StepBadges are NOT stored — they are
/// derived from z-order among StepBadges (see `Document::step_numbers`), so
/// deleting or reordering steps renumbers automatically, Snagit-style.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PrimitiveKind {
    Arrow { from: Point, to: Point, #[serde(default)] bend: f32 },
    Line { from: Point, to: Point },
    Freehand { points: Vec<Point> },
    Rect { rect: RectN, corner_radius: f32 },
    Ellipse { rect: RectN },
    /// A highlighter PEN stroke (Dickles' law: a pen, not a box) —
    /// a wide translucent path that tints what it crosses. Stored
    /// as the pointer's polyline, exactly like Freehand; the nib
    /// width derives from the style's stroke_width at render time.
    /// (v1 sidecars stored a rect — from_toml migrates them.)
    Highlight { points: Vec<Point> },
    /// Dim everything OUTSIDE the rect (Snagit spotlight).
    Spotlight { rect: RectN },
    Text { at: Point, text: String },
    /// A text box (anchored at `at`, its top-left) with a leader tail
    /// pointing at `tail` — the thing being called out.
    Callout { at: Point, tail: Point, text: String },
    StepBadge { at: Point },
    Stamp {
        at: Point,
        stamp_id: String,
        scale: f32,
        /// Clockwise rotation in degrees (0 = upright). Default keeps
        /// sidecars written before rotation existed loading unchanged.
        #[serde(default)]
        rotation_deg: f32,
    },
    /// Privacy regions: rendered by blurring the underlying image pixels.
    Blur { rect: RectN },
    /// Privacy regions: rendered by pixelating the underlying image pixels.
    Pixelate { rect: RectN },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Primitive {
    pub id: u64,
    pub kind: PrimitiveKind,
    pub style: Style,
}

impl Primitive {
    /// Shift all geometry by (dx, dy) in normalized space, clamping so the
    /// primitive's bbox stays inside the image (editor drag-move).
    pub fn translate(&mut self, dx: f32, dy: f32) {
        let b = self.bbox();
        let dx = dx.clamp(-b.x, 1.0 - (b.x + b.w));
        let dy = dy.clamp(-b.y, 1.0 - (b.y + b.h));
        let mv = |p: &mut Point| {
            p.x = (p.x + dx).clamp(0.0, 1.0);
            p.y = (p.y + dy).clamp(0.0, 1.0);
        };
        let mvr = |r: &mut RectN| {
            r.x = (r.x + dx).clamp(0.0, 1.0 - r.w);
            r.y = (r.y + dy).clamp(0.0, 1.0 - r.h);
        };
        match &mut self.kind {
            PrimitiveKind::Arrow { from, to, .. } | PrimitiveKind::Line { from, to } => {
                mv(from);
                mv(to);
            }
            PrimitiveKind::Freehand { points } | PrimitiveKind::Highlight { points } => {
                points.iter_mut().for_each(mv)
            }
            PrimitiveKind::Rect { rect, .. }
            | PrimitiveKind::Ellipse { rect }
            | PrimitiveKind::Spotlight { rect }
            | PrimitiveKind::Blur { rect }
            | PrimitiveKind::Pixelate { rect } => mvr(rect),
            PrimitiveKind::Text { at, .. }
            | PrimitiveKind::StepBadge { at }
            | PrimitiveKind::Stamp { at, .. } => mv(at),
            PrimitiveKind::Callout { at, tail, .. } => {
                mv(at);
                mv(tail);
            }
        }
    }

    /// Resize/reposition by setting a new bounding box (editor handle
    /// drags). Point geometry is affine-mapped from the old bbox into the
    /// new one; rect-shaped primitives adopt the new box directly;
    /// point-anchored ones (badge/text/stamp) move their anchor, and stamps
    /// rescale. The target is clamped in-range with a minimum extent, so a
    /// handle drag can never push a primitive out of the document.
    pub fn set_bbox(&mut self, target: RectN) {
        let old = self.bbox();
        let nx = target.x.clamp(0.0, 0.995);
        let ny = target.y.clamp(0.0, 0.995);
        let nw = target.w.clamp(0.005, 1.0 - nx);
        let nh = target.h.clamp(0.005, 1.0 - ny);
        let new = RectN { x: nx, y: ny, w: nw, h: nh };
        let map_pt = |p: &mut Point| {
            p.x = if old.w > 0.0 { nx + (p.x - old.x) * (nw / old.w) } else { nx };
            p.y = if old.h > 0.0 { ny + (p.y - old.y) * (nh / old.h) } else { ny };
            p.x = p.x.clamp(0.0, 1.0);
            p.y = p.y.clamp(0.0, 1.0);
        };
        match &mut self.kind {
            PrimitiveKind::Arrow { from, to, .. } | PrimitiveKind::Line { from, to } => {
                map_pt(from);
                map_pt(to);
            }
            PrimitiveKind::Freehand { points } | PrimitiveKind::Highlight { points } => {
                points.iter_mut().for_each(map_pt)
            }
            PrimitiveKind::Rect { rect, .. }
            | PrimitiveKind::Ellipse { rect }
            | PrimitiveKind::Spotlight { rect }
            | PrimitiveKind::Blur { rect }
            | PrimitiveKind::Pixelate { rect } => *rect = new,
            PrimitiveKind::Callout { at, tail, .. } => {
                map_pt(at);
                map_pt(tail);
            }
            PrimitiveKind::Text { at, .. } => {
                *at = Point::new(nx, ny);
                if old.h > 0.0 {
                    if let Some(fs) = self.style.font_size {
                        self.style.font_size = Some((fs * (nh / old.h)).clamp(0.008, 0.5));
                    }
                }
            }
            PrimitiveKind::StepBadge { at } => *at = Point::new(nx, ny),
            PrimitiveKind::Stamp { at, scale, rotation_deg, .. } => {
                // The editor passes the ROTATED bbox as the target; undo
                // the |cos|+|sin| growth to recover the true scale, and
                // center the unrotated square on the target's center.
                let rad = rotation_deg.to_radians();
                let f = (rad.cos().abs() + rad.sin().abs()).max(1.0);
                let ns = (nw / f).clamp(0.005, 1.0);
                *scale = ns;
                at.x = (nx + nw / 2.0 - ns / 2.0).clamp(0.0, 1.0 - ns);
                at.y = (ny + nh / 2.0 - ns / 2.0).clamp(0.0, 1.0 - ns);
            }
        }
    }

    /// Affine-remap all geometry from the frame where (ox, oy, ow, oh) is
    /// the unit square into that sub-frame itself (crop commit). Sizes
    /// expressed as fractions of width (stamp scale, font size) rescale so
    /// the primitive keeps its pixel size in the new frame.
    pub fn remap_affine(&mut self, ox: f32, oy: f32, ow: f32, oh: f32) {
        // stroke width is a fraction of image width everywhere: rescale so
        // pixel width survives the frame change
        self.style.stroke_width /= ow;
        let mp = |p: &mut Point| {
            p.x = (p.x - ox) / ow;
            p.y = (p.y - oy) / oh;
        };
        let mr = |r: &mut RectN| {
            r.x = (r.x - ox) / ow;
            r.y = (r.y - oy) / oh;
            r.w /= ow;
            r.h /= oh;
        };
        match &mut self.kind {
            PrimitiveKind::Arrow { from, to, .. } | PrimitiveKind::Line { from, to } => {
                mp(from);
                mp(to);
            }
            PrimitiveKind::Freehand { points } | PrimitiveKind::Highlight { points } => {
                points.iter_mut().for_each(mp)
            }
            PrimitiveKind::Rect { rect, .. }
            | PrimitiveKind::Ellipse { rect }
            | PrimitiveKind::Spotlight { rect }
            | PrimitiveKind::Blur { rect }
            | PrimitiveKind::Pixelate { rect } => mr(rect),
            PrimitiveKind::Callout { at, tail, .. } => {
                mp(at);
                mp(tail);
                if let Some(fs) = self.style.font_size {
                    self.style.font_size = Some(fs / ow);
                }
            }
            PrimitiveKind::Text { at, .. } => {
                mp(at);
                if let Some(fs) = self.style.font_size {
                    self.style.font_size = Some(fs / ow);
                }
            }
            PrimitiveKind::StepBadge { at } => mp(at),
            PrimitiveKind::Stamp { at, scale, .. } => {
                mp(at);
                *scale /= ow;
            }
        }
    }

    /// Normalized bounding box — the dirty region a renderer must
    /// re-present when this primitive changes, and nothing more.
    pub fn bbox(&self) -> RectN {
        match &self.kind {
            PrimitiveKind::Arrow { from, to, bend } => {
                // Include the curve's apex so handles hug a bent arrow.
                let base = RectN::from_corners(*from, *to);
                if bend.abs() < 0.001 {
                    return base;
                }
                let ctrl = arrow_control(*from, *to, *bend);
                let apex = Point::new(
                    0.25 * from.x + 0.5 * ctrl.x + 0.25 * to.x,
                    0.25 * from.y + 0.5 * ctrl.y + 0.25 * to.y,
                );
                base.union(&RectN { x: apex.x, y: apex.y, w: f32::EPSILON, h: f32::EPSILON })
            }
            PrimitiveKind::Line { from, to } => RectN::from_corners(*from, *to),
            PrimitiveKind::Freehand { points } | PrimitiveKind::Highlight { points } => points
                .iter()
                .fold(None, |acc: Option<RectN>, p| {
                    let r = RectN { x: p.x, y: p.y, w: f32::EPSILON, h: f32::EPSILON };
                    Some(match acc {
                        None => r,
                        Some(a) => a.union(&r),
                    })
                })
                .unwrap_or(RectN { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }),
            PrimitiveKind::Rect { rect, .. }
            | PrimitiveKind::Ellipse { rect }
            | PrimitiveKind::Spotlight { rect }
            | PrimitiveKind::Blur { rect }
            | PrimitiveKind::Pixelate { rect } => *rect,
            PrimitiveKind::Callout { at, tail, text } => {
                let (bw, bh) = callout_box_estimate(text, self.style.font_size);
                let boxr = RectN { x: at.x, y: at.y, w: bw, h: bh };
                let tailr = RectN { x: tail.x, y: tail.y, w: f32::EPSILON, h: f32::EPSILON };
                boxr.union(&tailr)
            }
            PrimitiveKind::Text { at, .. } | PrimitiveKind::StepBadge { at } => {
                RectN { x: at.x, y: at.y, w: 0.05, h: 0.05 }
            }
            PrimitiveKind::Stamp { at, scale, rotation_deg, .. } => {
                // A rotated square's axis-aligned bounds grow by
                // |cos| + |sin|; keep the bbox honest so selection
                // handles hug what the user actually sees.
                let rad = rotation_deg.to_radians();
                let f = (rad.cos().abs() + rad.sin().abs()).max(1.0);
                let s2 = scale * f;
                RectN {
                    x: at.x + scale / 2.0 - s2 / 2.0,
                    y: at.y + scale / 2.0 - s2 / 2.0,
                    w: s2,
                    h: s2,
                }
            }
        }
    }

    /// Aspect-aware bounding box (audit fix). `bbox()` is aspect-blind:
    /// it treats width-normalized extents as if they were
    /// height-normalized too. For primitives whose size is defined in
    /// WIDTH units (stamps are pixel-squares sized by width; callout
    /// heights stack from a width-relative font size), the true
    /// height-normalized extent is the width-unit extent × (W/H). On a
    /// 21:9 screenshot the blind box understates a stamp's height by
    /// 2.37×, so handles, crop decisions, and dirty regions landed in
    /// another county. Callers that know the image aspect (Document
    /// ops) use this; `w2h` = image width / height.
    pub fn bbox_aspect(&self, w2h: f32) -> RectN {
        match &self.kind {
            PrimitiveKind::Stamp {
                at,
                scale,
                rotation_deg,
                ..
            } => {
                let rad = rotation_deg.to_radians();
                let f = (rad.cos().abs() + rad.sin().abs()).max(1.0);
                let s2 = scale * f;
                RectN {
                    x: at.x + scale / 2.0 - s2 / 2.0,
                    y: at.y + (scale * w2h) / 2.0 - (s2 * w2h) / 2.0,
                    w: s2,
                    h: s2 * w2h,
                }
            }
            PrimitiveKind::Callout { at, tail, text } => {
                let (bw, bh) = callout_box_estimate(text, self.style.font_size);
                let boxr = RectN {
                    x: at.x,
                    y: at.y,
                    w: bw,
                    h: bh * w2h,
                };
                let tailr = RectN {
                    x: tail.x,
                    y: tail.y,
                    w: f32::EPSILON,
                    h: f32::EPSILON,
                };
                boxr.union(&tailr)
            }
            _ => self.bbox(),
        }
    }
}

/// Quadratic control point for a bent arrow. `bend` is the
/// perpendicular offset as a fraction of the from→to distance,
/// measured along the left-hand normal (−dy, dx) of the direction of
/// travel — positive bend bows the arrow to its left on screen
/// (y-down). The renderer computes the same point in pixel space;
/// this normalized version serves bbox math.
pub fn arrow_control(from: Point, to: Point, bend: f32) -> Point {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-6 {
        return from;
    }
    let (nx, ny) = (-dy / len, dx / len);
    Point::new(
        (from.x + to.x) / 2.0 + nx * bend * len,
        (from.y + to.y) / 2.0 + ny * bend * len,
    )
}

/// Average glyph advance (fraction of the em) of the renderer's font
/// stack (bold system face). The renderer's own measurement of this
/// value lives in shot-render's `font::avg_advance_em`; a shot-render
/// test asserts the two stay in step, so this estimate is calibrated
/// against the real font instead of guessed (audit fix — the old
/// 0.60-em guess plus a mismatched formula let editor, model, and
/// exporter draw three different callout boxes).
pub const CALLOUT_ADVANCE_EM: f32 = 0.62;

/// Estimated callout box size, given the text and the style font size
/// (a fraction of image WIDTH). BOTH returned axes are in
/// width-normalized units: the width is directly usable, the height
/// must be multiplied by the image aspect (W/H) to become a
/// height-normalized extent — see `Primitive::bbox_aspect`. The
/// formula mirrors the renderer's measured box
/// (shot_render::text::callout_box_px): measured text width + padding
/// both sides; stacked line heights + vertical padding.
pub fn callout_box_estimate(text: &str, font_size: Option<f32>) -> (f32, f32) {
    let fs = font_size.unwrap_or(0.045);
    let longest = text.split('\n').map(|l| l.chars().count()).max().unwrap_or(0) as f32;
    let lines = text.split('\n').count().max(1) as f32;
    let pad = 0.45 * fs;
    let w = longest * CALLOUT_ADVANCE_EM * fs + pad * 2.0;
    let h = lines * 1.3 * fs + pad * 1.4;
    (w, h)
}

/// What sits behind the screenshot in the beautified canvas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Background {
    Solid { color: Color },
    Gradient { from: Color, to: Color, angle_deg: f32 },
    /// Another image file (wallpaper, brand backdrop).
    Image { path: String },
    /// The screenshot itself, heavily blurred (CleanShot-style backdrop).
    BlurredScreenshot,
}

impl Default for Background {
    fn default() -> Self {
        Background::Solid { color: Color::rgba(88, 101, 220, 255) }
    }
}

/// Padding around the screenshot, as fractions of image width.
/// When `linked`, all sides use `top` (the UI drives one value).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Padding {
    pub linked: bool,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Padding {
    pub fn uniform(v: f32) -> Self {
        Padding { linked: true, top: v, right: v, bottom: v, left: v }
    }
    /// (top, right, bottom, left) honoring the linked flag.
    pub fn effective(&self) -> (f32, f32, f32, f32) {
        if self.linked {
            (self.top, self.top, self.top, self.top)
        } else {
            (self.top, self.right, self.bottom, self.left)
        }
    }
}

impl Default for Padding {
    fn default() -> Self {
        Padding::uniform(0.06)
    }
}

/// Drop shadow for the screenshot on its canvas. `blur` is a fraction of
/// image width; the renderer MUST produce it via the downscaled-blur path
/// (Step 0: full-res blur measured 14–32x slower on the target hardware).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Shadow {
    pub enabled: bool,
    pub blur: f32,
    pub offset_y: f32,
    pub opacity: f32,
}

impl Default for Shadow {
    fn default() -> Self {
        Shadow { enabled: true, blur: 0.03, offset_y: 0.012, opacity: 0.35 }
    }
}

/// Beautify settings (Phase 1 render inputs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Beautify {
    pub background: Background,
    pub padding: Padding,
    /// Corner radius of the screenshot itself, fraction of image width.
    pub corner_radius: f32,
    pub shadow: Shadow,
    /// Target canvas aspect (width / height), Xnapper-style: the canvas
    /// GROWS padding (never crops the shot) until it matches. `None` =
    /// natural size. Serde-defaulted so v1 sidecars without it parse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aspect: Option<f32>,
    /// Straighten/crop applied to the source before compositing, if any.
    pub crop: Option<RectN>,
    /// Decorative frame effect around the shot (Instant-photo chin,
    /// postcard border, peeled corner). Serde-defaulted: v1 sidecars
    /// without it parse as None.
    #[serde(default)]
    pub frame: FrameStyle,
}

/// Decorative frames. "Instant" is the classic instant-photo look —
/// Polaroid is a trademark, the look is a layout: white frame, deep
/// chin. Postcard: slim white border + hairline. Peeled: a dog-eared
/// bottom-right corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FrameStyle {
    #[default]
    None,
    Instant,
    Postcard,
    Peeled,
}

impl Default for Beautify {
    fn default() -> Self {
        Beautify {
            background: Background::default(),
            padding: Padding::default(),
            corner_radius: 0.012,
            shadow: Shadow::default(),
            aspect: None,
            crop: None,
            frame: FrameStyle::None,
        }
    }
}

/// The captured image, referenced — never embedded in the sidecar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageRef {
    /// Path relative to the sidecar when possible (survives folder moves).
    pub path: String,
    /// Physical pixels.
    pub width: u32,
    pub height: u32,
    /// Content hash for library dedupe/integrity, when computed.
    pub sha256: Option<String>,
}

/// A Screensnips document: image + beautify + annotations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub format_version: u32,
    pub image: ImageRef,
    pub beautify: Beautify,
    /// Z-order: later = on top.
    pub primitives: Vec<Primitive>,
    #[serde(default = "default_next_id")]
    pub next_id: u64,
    /// Text recognized in the shot (offline OCR), stored with the shot
    /// so the library can search by content. Serde-defaulted: older
    /// sidecars parse as None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr_text: Option<String>,
}

fn default_next_id() -> u64 {
    1
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModelError {
    UnsupportedVersion(u32),
    DuplicateId(u64),
    UnknownId(u64),
    OutOfRange(String),
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::UnsupportedVersion(v) => write!(f, "unsupported format_version {}", v),
            ModelError::DuplicateId(id) => write!(f, "duplicate primitive id {}", id),
            ModelError::UnknownId(id) => write!(f, "unknown primitive id {}", id),
            ModelError::OutOfRange(what) => write!(f, "geometry out of range: {}", what),
        }
    }
}
impl std::error::Error for ModelError {}

impl Document {
    pub fn new(image: ImageRef) -> Self {
        Document {
            format_version: FORMAT_VERSION,
            image,
            beautify: Beautify::default(),
            primitives: Vec::new(),
            next_id: 1,
            ocr_text: None,
        }
    }

    /// Append a primitive on top; returns its new id.
    pub fn add(&mut self, kind: PrimitiveKind, style: Style) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.primitives.push(Primitive { id, kind, style });
        id
    }

    pub fn remove(&mut self, id: u64) -> Result<Primitive, ModelError> {
        let pos = self
            .primitives
            .iter()
            .position(|p| p.id == id)
            .ok_or(ModelError::UnknownId(id))?;
        Ok(self.primitives.remove(pos))
    }

    /// Move a primitive to a new z-index (0 = bottom).
    pub fn move_z(&mut self, id: u64, new_index: usize) -> Result<(), ModelError> {
        let pos = self
            .primitives
            .iter()
            .position(|p| p.id == id)
            .ok_or(ModelError::UnknownId(id))?;
        let prim = self.primitives.remove(pos);
        let idx = new_index.min(self.primitives.len());
        self.primitives.insert(idx, prim);
        Ok(())
    }

    pub fn get(&self, id: u64) -> Option<&Primitive> {
        self.primitives.iter().find(|p| p.id == id)
    }

    /// Commit a crop: rewrite the document into crop space. The image ref
    /// takes the cropped dimensions, every primitive is affine-remapped
    /// from the old frame into the crop frame, primitives entirely outside
    /// the crop are dropped (count returned), and partially-outside ones
    /// are clamped in via their visible intersection. Any pending
    /// beautify.crop is cleared — the crop is now baked into the document,
    /// and the caller crops the actual pixels with the returned rect.
    /// Returns (applied_rect, dropped_count).
    pub fn commit_crop(&mut self, rect: RectN) -> (RectN, usize) {
        let cx = rect.x.clamp(0.0, 0.995);
        let cy = rect.y.clamp(0.0, 0.995);
        let cw = rect.w.clamp(0.005, 1.0 - cx);
        let ch = rect.h.clamp(0.005, 1.0 - cy);
        let applied = RectN { x: cx, y: cy, w: cw, h: ch };

        let new_w = ((cw * self.image.width as f32).round() as u32).max(1);
        let new_h = ((ch * self.image.height as f32).round() as u32).max(1);

        let w2h = self.image.width as f32 / self.image.height.max(1) as f32;
        let mut dropped = 0usize;
        let mut kept: Vec<Primitive> = Vec::with_capacity(self.primitives.len());
        for mut prim in self.primitives.drain(..) {
            // Aspect-aware: crop intersection decisions must use the
            // box the user actually sees (stamps/callouts on wide
            // screenshots were misjudged by the blind box).
            let b = prim.bbox_aspect(w2h);
            // intersection with the crop rect, in old space
            let ix0 = b.x.max(cx);
            let iy0 = b.y.max(cy);
            let ix1 = (b.x + b.w).min(cx + cw);
            let iy1 = (b.y + b.h).min(cy + ch);
            if ix1 <= ix0 || iy1 <= iy0 {
                dropped += 1;
                continue;
            }
            // affine-remap the whole primitive into crop space
            prim.remap_affine(cx, cy, cw, ch);
            // clamp any overshoot to the visible intersection (crop space)
            let target = RectN {
                x: (ix0 - cx) / cw,
                y: (iy0 - cy) / ch,
                w: (ix1 - ix0) / cw,
                h: (iy1 - iy0) / ch,
            };
            // nb lives in CROP space — its aspect is the new frame's.
            let new_w2h = new_w as f32 / new_h.max(1) as f32;
            let nb = prim.bbox_aspect(new_w2h);
            if (nb.x - target.x).abs() > 1e-6
                || (nb.y - target.y).abs() > 1e-6
                || (nb.w - target.w).abs() > 1e-6
                || (nb.h - target.h).abs() > 1e-6
            {
                if let PrimitiveKind::Stamp {
                    at,
                    scale,
                    rotation_deg,
                    ..
                } = &mut prim.kind
                {
                    // Stamps: Primitive::set_bbox is aspect-blind and
                    // would rescale the stamp from a mixed frame —
                    // apply the same aspect-aware math as
                    // resize_primitive instead.
                    let rad = rotation_deg.to_radians();
                    let f = (rad.cos().abs() + rad.sin().abs()).max(1.0);
                    let ns = (target.w / f).clamp(0.005, 1.0);
                    let cx = target.x + target.w / 2.0;
                    let cy = target.y + target.h / 2.0;
                    *scale = ns;
                    at.x = (cx - ns / 2.0).clamp(0.0, (1.0 - ns).max(0.0));
                    at.y = (cy - ns * new_w2h / 2.0)
                        .clamp(0.0, (1.0 - ns * new_w2h).max(0.0));
                } else {
                    prim.set_bbox(target);
                }
            }
            kept.push(prim);
        }
        self.primitives = kept;
        self.image.width = new_w;
        self.image.height = new_h;
        self.beautify.crop = None;
        (applied, dropped)
    }

    /// Resize/reposition a primitive to a new bounding box (handle drags).
    pub fn resize_primitive(&mut self, id: u64, bbox: RectN) -> Result<(), ModelError> {
        // Stamps are squares in PIXEL space whose size is a fraction of
        // image WIDTH — in normalized coords their height is scale ×
        // (W/H). Primitive::set_bbox is aspect-blind and mangled stamp
        // resizes on every non-square screenshot (the stamp jumped
        // vertically and handles landed in another county). Route
        // stamps through aspect-aware math here, where the document
        // knows the image size.
        let w2h = self.image.width as f32 / self.image.height.max(1) as f32;
        let prim = self
            .primitives
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(ModelError::UnknownId(id))?;
        if let PrimitiveKind::Stamp { at, scale, rotation_deg, .. } = &mut prim.kind {
            let rad = rotation_deg.to_radians();
            let f = (rad.cos().abs() + rad.sin().abs()).max(1.0);
            let ns = (bbox.w / f).clamp(0.005, 1.0);
            let cx = bbox.x + bbox.w / 2.0;
            let cy = bbox.y + bbox.h / 2.0;
            *scale = ns;
            at.x = (cx - ns / 2.0).clamp(0.0, (1.0 - ns).max(0.0));
            at.y = (cy - ns * w2h / 2.0).clamp(0.0, (1.0 - ns * w2h).max(0.0));
            return Ok(());
        }
        prim.set_bbox(bbox);
        Ok(())
    }

    /// Set an arrow's bend (see arrow_control). Ok(false) for
    /// non-arrows, mirroring set_rotation.
    pub fn set_bend(&mut self, id: u64, bend: f32) -> Result<bool, ModelError> {
        let prim = self
            .primitives
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(ModelError::UnknownId(id))?;
        match &mut prim.kind {
            PrimitiveKind::Arrow { bend: b, .. } => {
                *b = bend.clamp(-1.2, 1.2);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Set a stamp's rotation (degrees clockwise, normalized to
    /// [0, 360)). Ok(true) when applied, Ok(false) when the primitive
    /// exists but can't rotate (yet — stamps first), UnknownId when it
    /// doesn't exist at all.
    pub fn set_rotation(&mut self, id: u64, deg: f32) -> Result<bool, ModelError> {
        let prim = self
            .primitives
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(ModelError::UnknownId(id))?;
        match &mut prim.kind {
            PrimitiveKind::Stamp { rotation_deg, .. } => {
                *rotation_deg = ((deg % 360.0) + 360.0) % 360.0;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Drag-move a primitive by a normalized delta (clamped in-range).
    pub fn translate_primitive(&mut self, id: u64, dx: f32, dy: f32) -> Result<(), ModelError> {
        let w2h = self.image.width as f32 / self.image.height.max(1) as f32;
        let prim = self
            .primitives
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(ModelError::UnknownId(id))?;
        prim.translate(dx, dy);
        // Primitive::translate clamps with the aspect-blind bbox; give
        // stamps their true pixel-square bounds so they can reach the
        // bottom of a wide screenshot and never slide past it.
        if let PrimitiveKind::Stamp { at, scale, .. } = &mut prim.kind {
            at.x = at.x.clamp(0.0, (1.0 - *scale).max(0.0));
            at.y = at.y.clamp(0.0, (1.0 - *scale * w2h).max(0.0));
        }
        Ok(())
    }

    /// (id, 1-based number) for every StepBadge, in z-order. Renumbers
    /// automatically after removals/reorders — nothing is stored.
    pub fn step_numbers(&self) -> Vec<(u64, u32)> {
        let mut n = 0;
        self.primitives
            .iter()
            .filter(|p| matches!(p.kind, PrimitiveKind::StepBadge { .. }))
            .map(|p| {
                n += 1;
                (p.id, n)
            })
            .collect()
    }

    /// Structural validation: version, unique ids, geometry in range.
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.format_version > FORMAT_VERSION {
            return Err(ModelError::UnsupportedVersion(self.format_version));
        }
        let mut ids: Vec<u64> = self.primitives.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        for w in ids.windows(2) {
            if w[0] == w[1] {
                return Err(ModelError::DuplicateId(w[0]));
            }
        }
        for p in &self.primitives {
            let ok = match &p.kind {
                PrimitiveKind::Arrow { from, to, .. } | PrimitiveKind::Line { from, to } => {
                    from.in_range() && to.in_range()
                }
                PrimitiveKind::Freehand { points } | PrimitiveKind::Highlight { points } => {
                    points.iter().all(|pt| pt.in_range())
                }
                PrimitiveKind::Rect { rect, .. }
                | PrimitiveKind::Ellipse { rect }
                    | PrimitiveKind::Spotlight { rect }
                | PrimitiveKind::Blur { rect }
                | PrimitiveKind::Pixelate { rect } => rect.in_range(),
                PrimitiveKind::Callout { at, tail, .. } => at.in_range() && tail.in_range(),
                PrimitiveKind::Text { at, .. } | PrimitiveKind::StepBadge { at } => at.in_range(),
                PrimitiveKind::Stamp { at, scale, .. } => {
                    at.in_range() && *scale > 0.0 && *scale <= 1.0
                }
            };
            if !ok {
                return Err(ModelError::OutOfRange(format!("primitive {}", p.id)));
            }
        }
        if let Some(crop) = &self.beautify.crop {
            if !crop.in_range() {
                return Err(ModelError::OutOfRange("crop".to_string()));
            }
        }
        Ok(())
    }

    /// Serialize as the TOML sidecar text.
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Parse a sidecar, then validate.
    pub fn from_toml(text: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let mut value: toml::Value = toml::from_str(text)?;
        migrate_highlight_rects(&mut value);
        let doc: Document = value.try_into()?;
        doc.validate()?;
        Ok(doc)
    }
}

/// v1 sidecars stored Highlight as a filled rect; the tool is a pen
/// now. Migrate in the parsed TOML: a rect highlight becomes a
/// two-point stroke along the rect's horizontal midline — the swipe
/// the user actually made — so old libraries open unchanged.
fn migrate_highlight_rects(value: &mut toml::Value) {
    let Some(prims) = value
        .get_mut("primitives")
        .and_then(|p| p.as_array_mut())
    else {
        return;
    };
    for prim in prims.iter_mut() {
        let Some(kind) = prim.get_mut("kind").and_then(|k| k.as_table_mut()) else {
            continue;
        };
        if kind.get("type").and_then(|t| t.as_str()) != Some("highlight") {
            continue;
        }
        if kind.contains_key("points") {
            continue;
        }
        let Some(rect) = kind.remove("rect") else {
            continue;
        };
        let get = |k: &str| rect.get(k).and_then(|v| v.as_float());
        let (x, y, w, h) = match (get("x"), get("y"), get("w"), get("h")) {
            (Some(x), Some(y), Some(w), Some(h)) => (x, y, w, h),
            _ => continue,
        };
        let mid = y + h / 2.0;
        let pt = |px: f64, py: f64| {
            let mut t = toml::map::Map::new();
            t.insert("x".into(), toml::Value::Float(px));
            t.insert("y".into(), toml::Value::Float(py));
            toml::Value::Table(t)
        };
        kind.insert(
            "points".into(),
            toml::Value::Array(vec![pt(x, mid), pt(x + w, mid)]),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img() -> ImageRef {
        ImageRef { path: "shot.png".into(), width: 2560, height: 1080, sha256: None }
    }

    fn pt(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn toml_round_trip_preserves_document() {
        let mut doc = Document::new(img());
        doc.add(
            PrimitiveKind::Arrow { from: pt(0.1, 0.2), to: pt(0.5, 0.6), bend: 0.0 },
            Style::default(),
        );
        doc.add(PrimitiveKind::StepBadge { at: pt(0.3, 0.3) }, Style::default());
        doc.add(
            PrimitiveKind::Blur {
                rect: RectN { x: 0.6, y: 0.1, w: 0.2, h: 0.1 },
            },
            Style::default(),
        );
        let text = doc.to_toml().unwrap();
        let back = Document::from_toml(&text).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    fn step_numbers_follow_z_order_and_renumber() {
        let mut doc = Document::new(img());
        let a = doc.add(PrimitiveKind::StepBadge { at: pt(0.1, 0.1) }, Style::default());
        let _mid = doc.add(
            PrimitiveKind::Arrow { from: pt(0.0, 0.0), to: pt(0.2, 0.2), bend: 0.0 },
            Style::default(),
        );
        let b = doc.add(PrimitiveKind::StepBadge { at: pt(0.5, 0.5) }, Style::default());
        assert_eq!(doc.step_numbers(), vec![(a, 1), (b, 2)]);
        // move b below a -> numbers swap
        doc.move_z(b, 0).unwrap();
        assert_eq!(doc.step_numbers(), vec![(b, 1), (a, 2)]);
        // remove a -> b renumbers to 1
        doc.remove(a).unwrap();
        assert_eq!(doc.step_numbers(), vec![(b, 1)]);
    }

    #[test]
    fn linked_padding_uses_one_value() {
        let p = Padding { linked: true, top: 0.08, right: 0.01, bottom: 0.02, left: 0.03 };
        assert_eq!(p.effective(), (0.08, 0.08, 0.08, 0.08));
        let q = Padding { linked: false, ..p };
        assert_eq!(q.effective(), (0.08, 0.01, 0.02, 0.03));
    }

    #[test]
    fn validate_rejects_out_of_range_and_future_versions() {
        let mut doc = Document::new(img());
        doc.add(
            PrimitiveKind::Arrow { from: pt(-0.5, 0.2), to: pt(0.5, 0.6), bend: 0.0 },
            Style::default(),
        );
        assert!(matches!(doc.validate(), Err(ModelError::OutOfRange(_))));
        let mut doc2 = Document::new(img());
        doc2.format_version = FORMAT_VERSION + 1;
        assert!(matches!(doc2.validate(), Err(ModelError::UnsupportedVersion(_))));
    }

    #[test]
    fn translate_moves_and_clamps() {
        let mut doc = Document::new(img());
        let id = doc.add(
            PrimitiveKind::Rect {
                rect: RectN { x: 0.8, y: 0.8, w: 0.15, h: 0.15 },
                corner_radius: 0.0,
            },
            Style::default(),
        );
        doc.translate_primitive(id, 0.5, 0.5).unwrap(); // would exit at right/bottom
        {
            let p = doc.get(id).unwrap();
            assert_eq!(p.bbox().x + p.bbox().w, 1.0);
            assert_eq!(p.bbox().y + p.bbox().h, 1.0);
        }
        doc.translate_primitive(id, -2.0, -2.0).unwrap(); // clamps to origin
        assert_eq!(doc.get(id).unwrap().bbox().x, 0.0);
        assert!(doc.translate_primitive(999, 0.1, 0.1).is_err());
    }

    #[test]
    fn set_bbox_scales_geometry() {
        let mut doc = Document::new(img());
        let id = doc.add(
            PrimitiveKind::Arrow {
                from: Point::new(0.2, 0.2),
                to: Point::new(0.4, 0.6), bend: 0.0,
            },
            Style::default(),
        );
        // double the bbox, shifted
        doc.resize_primitive(id, RectN { x: 0.1, y: 0.1, w: 0.4, h: 0.8 }).unwrap();
        let p = doc.get(id).unwrap();
        let b = p.bbox();
        let close = |a: f32, e: f32| (a - e).abs() < 1e-6;
        assert!(close(b.x, 0.1) && close(b.y, 0.1) && close(b.w, 0.4) && close(b.h, 0.8), "bbox {:?}", b);
        if let PrimitiveKind::Arrow { from, to, .. } = &p.kind {
            assert!(close(from.x, 0.1) && close(from.y, 0.1), "from {:?}", from);
            assert!(close(to.x, 0.5) && close(to.y, 0.9), "to {:?}", to);
        } else {
            panic!("kind changed");
        }
        // rect kinds adopt the box verbatim, clamped in-range
        let rid = doc.add(
            PrimitiveKind::Pixelate { rect: RectN { x: 0.0, y: 0.0, w: 0.5, h: 0.5 } },
            Style::default(),
        );
        doc.resize_primitive(rid, RectN { x: 0.9, y: 0.9, w: 0.5, h: 0.5 }).unwrap();
        let rb = doc.get(rid).unwrap().bbox();
        assert!(rb.x + rb.w <= 1.0 && rb.y + rb.h <= 1.0);
        assert!(doc
            .resize_primitive(999, RectN { x: 0.0, y: 0.0, w: 0.1, h: 0.1 })
            .is_err());
    }

    #[test]
    fn commit_crop_remaps_drops_and_resizes() {
        let mut doc = Document::new(ImageRef {
            path: "big.png".into(),
            width: 1000,
            height: 500,
            sha256: None,
        });
        // arrow fully inside the right half
        let inside = doc.add(
            PrimitiveKind::Arrow {
                from: Point::new(0.6, 0.2),
                to: Point::new(0.8, 0.6), bend: 0.0,
            },
            Style::default(),
        );
        // badge fully outside the crop (left edge)
        doc.add(PrimitiveKind::StepBadge { at: Point::new(0.05, 0.5) }, Style::default());

        let (applied, dropped) = doc.commit_crop(RectN { x: 0.5, y: 0.0, w: 0.5, h: 1.0 });
        assert_eq!(applied, RectN { x: 0.5, y: 0.0, w: 0.5, h: 1.0 });
        assert_eq!(dropped, 1);
        assert_eq!((doc.image.width, doc.image.height), (500, 500));
        assert_eq!(doc.primitives.len(), 1);
        let close = |a: f32, e: f32| (a - e).abs() < 1e-6;
        if let PrimitiveKind::Arrow { from, to, .. } = &doc.get(inside).unwrap().kind {
            assert!(close(from.x, 0.2) && close(from.y, 0.2), "from {:?}", from);
            assert!(close(to.x, 0.6) && close(to.y, 0.6), "to {:?}", to);
        } else {
            panic!("kind changed");
        }
        doc.validate().unwrap();
    }

    #[test]
    fn spotlight_and_callout_round_trip_and_validate() {
        let mut doc = Document::new(img());
        doc.add(
            PrimitiveKind::Spotlight {
                rect: RectN { x: 0.2, y: 0.2, w: 0.4, h: 0.3 },
            },
            Style::default(),
        );
        let c = doc.add(
            PrimitiveKind::Callout {
                at: pt(0.55, 0.6),
                tail: pt(0.3, 0.35),
                text: "click here".into(),
            },
            Style {
                font_size: Some(0.04),
                ..Style::default()
            },
        );
        doc.validate().unwrap();
        // bbox covers both the box and the tail point
        let b = doc.get(c).unwrap().bbox();
        assert!(b.x <= 0.3 && b.y <= 0.35, "bbox {:?} must reach the tail", b);
        let text = doc.to_toml().unwrap();
        let back = Document::from_toml(&text).unwrap();
        assert_eq!(doc, back);
        // an out-of-range tail is rejected
        let mut bad = Document::new(img());
        bad.add(
            PrimitiveKind::Callout {
                at: pt(0.1, 0.1),
                tail: pt(1.5, 0.5),
                text: "x".into(),
            },
            Style::default(),
        );
        assert!(bad.validate().is_err());
    }

    #[test]
    fn aspect_defaults_none_and_survives_round_trip() {
        let mut doc = Document::new(img());
        assert_eq!(doc.beautify.aspect, None);
        doc.beautify.aspect = Some(16.0 / 9.0);
        let text = doc.to_toml().unwrap();
        let back = Document::from_toml(&text).unwrap();
        assert_eq!(back.beautify.aspect, Some(16.0 / 9.0));
        // sidecars WITHOUT the field (every v1 file; aspect is skipped
        // when None) parse via the serde default — covered by all the
        // aspect-less round-trip tests above.
    }

    #[test]
    fn arrow_bbox_is_corner_box() {
        let p = Primitive {
            id: 1,
            kind: PrimitiveKind::Arrow { from: pt(0.7, 0.1), to: pt(0.2, 0.9), bend: 0.0 },
            style: Style::default(),
        };
        let b = p.bbox();
        let close = |a: f32, e: f32| (a - e).abs() < 1e-6;
        assert!(close(b.x, 0.2) && close(b.y, 0.1) && close(b.w, 0.5) && close(b.h, 0.8));
    }

    #[test]
    fn stamp_resize_on_a_wide_screenshot_keeps_its_center() {
        // Regression (dogfooded on a 2560×1080 screen): stamp resize
        // used aspect-blind math, so on any non-square image the stamp
        // teleported vertically and scaling felt broken. A 1000×400
        // document: a stamp of scale 0.1 is 100px square = normalized
        // height 0.25.
        let mut doc = Document::new(ImageRef {
            path: "x.png".into(),
            width: 1000,
            height: 400,
            sha256: None,
        });
        let id = doc.add(
            PrimitiveKind::Stamp {
                at: pt(0.40, 0.30),
                stamp_id: "check".into(),
                scale: 0.1,
                rotation_deg: 0.0,
            },
            Style::default(),
        );
        // center now: (0.45, 0.30 + 0.125) = (0.45, 0.425). Ask for
        // scale 0.2 (200px → normalized 0.2 × 0.5) around the SAME
        // center, expressed as the editor's AABB target:
        let target = RectN { x: 0.35, y: 0.425 - 0.25, w: 0.2, h: 0.5 };
        doc.resize_primitive(id, target).unwrap();
        let p = doc.get(id).unwrap();
        match &p.kind {
            PrimitiveKind::Stamp { at, scale, .. } => {
                assert!((scale - 0.2).abs() < 1e-4, "scale = {}", scale);
                assert!((at.x - 0.35).abs() < 1e-4, "at.x = {}", at.x);
                assert!((at.y - 0.175).abs() < 1e-4, "at.y = {}", at.y);
            }
            _ => panic!("not a stamp"),
        }
    }

    #[test]
    fn stamp_bbox_aspect_uses_true_pixel_square_extent() {
        // 21:9 frame: a stamp's height-normalized extent is its
        // width extent × (2560/1080) — the blind bbox() says 0.1,
        // the box the user sees is 0.237 tall.
        let p = Primitive {
            id: 1,
            kind: PrimitiveKind::Stamp {
                at: pt(0.5, 0.5),
                stamp_id: "check".into(),
                scale: 0.1,
                rotation_deg: 0.0,
            },
            style: Style::default(),
        };
        let w2h = 2560.0 / 1080.0;
        let b = p.bbox_aspect(w2h);
        assert!((b.x - 0.5).abs() < 1e-5, "x = {}", b.x);
        assert!((b.y - 0.5).abs() < 1e-5, "y = {}", b.y);
        assert!((b.w - 0.1).abs() < 1e-5, "w = {}", b.w);
        assert!((b.h - 0.1 * w2h).abs() < 1e-5, "h = {}", b.h);
        // Rotated 45°: the AABB grows by √2 around the true center.
        let p2 = Primitive {
            id: 2,
            kind: PrimitiveKind::Stamp {
                at: pt(0.5, 0.5),
                stamp_id: "check".into(),
                scale: 0.1,
                rotation_deg: 45.0,
            },
            style: Style::default(),
        };
        let b2 = p2.bbox_aspect(w2h);
        let f = std::f32::consts::SQRT_2;
        assert!((b2.w - 0.1 * f).abs() < 1e-4, "w2 = {}", b2.w);
        assert!((b2.h - 0.1 * f * w2h).abs() < 1e-4, "h2 = {}", b2.h);
        let cx = b2.x + b2.w / 2.0;
        let cy = b2.y + b2.h / 2.0;
        assert!((cx - 0.55).abs() < 1e-4, "cx = {}", cx);
        assert!((cy - (0.5 + 0.05 * w2h)).abs() < 1e-4, "cy = {}", cy);
    }

    #[test]
    fn callout_estimate_mirrors_renderer_formula() {
        // "Hello" at fs 0.04: w = 5 × 0.62 × 0.04 + 2 × (0.45 × 0.04);
        // h (width units) = 1.3 × 0.04 + 1.4 × (0.45 × 0.04).
        let (w, h) = callout_box_estimate("Hello", Some(0.04));
        assert!((w - 0.160).abs() < 1e-5, "w = {}", w);
        assert!((h - 0.0772).abs() < 1e-5, "h = {}", h);
        // bbox_aspect converts the height into height units.
        let p = Primitive {
            id: 3,
            kind: PrimitiveKind::Callout {
                at: pt(0.1, 0.1),
                tail: pt(0.1, 0.1),
                text: "Hello".into(),
            },
            style: Style {
                font_size: Some(0.04),
                ..Style::default()
            },
        };
        let b = p.bbox_aspect(2.0);
        assert!((b.h - h * 2.0).abs() < 1e-5, "bh = {}", b.h);
    }

    #[test]
    fn v1_rect_highlight_sidecar_migrates_to_a_pen_stroke() {
        // A sidecar in the OLD shape (highlight = filled rect) must
        // still parse, and must come back as a two-point stroke along
        // the rect's midline.
        let old = r#"
format_version = 1

[image]
path = "shot.png"
width = 2560
height = 1080

[beautify]
background = { type = "solid", color = { r = 24, g = 24, b = 28, a = 255 } }
padding = { top = 0.0, right = 0.0, bottom = 0.0, left = 0.0, linked = true }
corner_radius = 0.0

[beautify.shadow]
enabled = false
blur = 0.0
offset_y = 0.0
opacity = 0.0

[[primitives]]
id = 1

[primitives.kind]
type = "highlight"

[primitives.kind.rect]
x = 0.2
y = 0.4
w = 0.5
h = 0.1

[primitives.style]
stroke = { r = 255, g = 235, b = 0, a = 255 }
stroke_width = 0.004
"#;
        let doc = Document::from_toml(old).expect("old sidecar must still load");
        assert_eq!(doc.primitives.len(), 1);
        match &doc.primitives[0].kind {
            PrimitiveKind::Highlight { points } => {
                assert_eq!(points.len(), 2);
                assert!((points[0].x - 0.2).abs() < 1e-6);
                assert!((points[0].y - 0.45).abs() < 1e-6, "midline y");
                assert!((points[1].x - 0.7).abs() < 1e-6);
                assert!((points[1].y - 0.45).abs() < 1e-6);
            }
            other => panic!("expected pen highlight, got {:?}", other),
        }
        // And the migrated document round-trips in the new shape.
        let text = doc.to_toml().unwrap();
        let doc2 = Document::from_toml(&text).unwrap();
        assert_eq!(doc, doc2);
    }
}
