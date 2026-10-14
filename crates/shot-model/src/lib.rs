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
    Arrow { from: Point, to: Point },
    Line { from: Point, to: Point },
    Freehand { points: Vec<Point> },
    Rect { rect: RectN, corner_radius: f32 },
    Ellipse { rect: RectN },
    Highlight { rect: RectN },
    /// Dim everything OUTSIDE the rect (Snagit spotlight).
    Spotlight { rect: RectN },
    Text { at: Point, text: String },
    /// A text box (anchored at `at`, its top-left) with a leader tail
    /// pointing at `tail` — the thing being called out.
    Callout { at: Point, tail: Point, text: String },
    StepBadge { at: Point },
    Stamp { at: Point, stamp_id: String, scale: f32 },
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
            PrimitiveKind::Arrow { from, to } | PrimitiveKind::Line { from, to } => {
                mv(from);
                mv(to);
            }
            PrimitiveKind::Freehand { points } => points.iter_mut().for_each(mv),
            PrimitiveKind::Rect { rect, .. }
            | PrimitiveKind::Ellipse { rect }
            | PrimitiveKind::Highlight { rect }
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
            PrimitiveKind::Arrow { from, to } | PrimitiveKind::Line { from, to } => {
                map_pt(from);
                map_pt(to);
            }
            PrimitiveKind::Freehand { points } => points.iter_mut().for_each(map_pt),
            PrimitiveKind::Rect { rect, .. }
            | PrimitiveKind::Ellipse { rect }
            | PrimitiveKind::Highlight { rect }
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
            PrimitiveKind::Stamp { at, scale, .. } => {
                *at = Point::new(nx, ny);
                *scale = nw;
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
            PrimitiveKind::Arrow { from, to } | PrimitiveKind::Line { from, to } => {
                mp(from);
                mp(to);
            }
            PrimitiveKind::Freehand { points } => points.iter_mut().for_each(mp),
            PrimitiveKind::Rect { rect, .. }
            | PrimitiveKind::Ellipse { rect }
            | PrimitiveKind::Highlight { rect }
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
            PrimitiveKind::Arrow { from, to } | PrimitiveKind::Line { from, to } => {
                RectN::from_corners(*from, *to)
            }
            PrimitiveKind::Freehand { points } => points
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
            | PrimitiveKind::Highlight { rect }
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
            PrimitiveKind::Stamp { at, scale, .. } => {
                RectN { x: at.x, y: at.y, w: *scale, h: *scale }
            }
        }
    }
}

/// Estimated callout box size in normalized units, given the text and the
/// style font size (a fraction of image WIDTH). The renderer measures with
/// the real font and may differ slightly; this estimate drives bboxes
/// (handles, dirty regions). Height is approximate — normalized Y can't
/// know the image aspect — and errs generous so handles never clip text.
pub fn callout_box_estimate(text: &str, font_size: Option<f32>) -> (f32, f32) {
    let fs = font_size.unwrap_or(0.035);
    let longest = text.split('\n').map(|l| l.chars().count()).max().unwrap_or(0);
    let lines = text.split('\n').count().max(1) as f32;
    // average glyph advance ~0.60 em for the bold system face; em = fs
    let w = 0.03 + longest as f32 * 0.60 * fs;
    let h = lines * 1.9 * fs;
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

        let mut dropped = 0usize;
        let mut kept: Vec<Primitive> = Vec::with_capacity(self.primitives.len());
        for mut prim in self.primitives.drain(..) {
            let b = prim.bbox();
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
            let nb = prim.bbox();
            if (nb.x - target.x).abs() > 1e-6
                || (nb.y - target.y).abs() > 1e-6
                || (nb.w - target.w).abs() > 1e-6
                || (nb.h - target.h).abs() > 1e-6
            {
                prim.set_bbox(target);
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
        let prim = self
            .primitives
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(ModelError::UnknownId(id))?;
        prim.set_bbox(bbox);
        Ok(())
    }

    /// Drag-move a primitive by a normalized delta (clamped in-range).
    pub fn translate_primitive(&mut self, id: u64, dx: f32, dy: f32) -> Result<(), ModelError> {
        let prim = self
            .primitives
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(ModelError::UnknownId(id))?;
        prim.translate(dx, dy);
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
                PrimitiveKind::Arrow { from, to } | PrimitiveKind::Line { from, to } => {
                    from.in_range() && to.in_range()
                }
                PrimitiveKind::Freehand { points } => points.iter().all(|pt| pt.in_range()),
                PrimitiveKind::Rect { rect, .. }
                | PrimitiveKind::Ellipse { rect }
                | PrimitiveKind::Highlight { rect }
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
        let doc: Document = toml::from_str(text)?;
        doc.validate()?;
        Ok(doc)
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
            PrimitiveKind::Arrow { from: pt(0.1, 0.2), to: pt(0.5, 0.6) },
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
            PrimitiveKind::Arrow { from: pt(0.0, 0.0), to: pt(0.2, 0.2) },
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
            PrimitiveKind::Arrow { from: pt(-0.5, 0.2), to: pt(0.5, 0.6) },
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
                to: Point::new(0.4, 0.6),
            },
            Style::default(),
        );
        // double the bbox, shifted
        doc.resize_primitive(id, RectN { x: 0.1, y: 0.1, w: 0.4, h: 0.8 }).unwrap();
        let p = doc.get(id).unwrap();
        let b = p.bbox();
        let close = |a: f32, e: f32| (a - e).abs() < 1e-6;
        assert!(close(b.x, 0.1) && close(b.y, 0.1) && close(b.w, 0.4) && close(b.h, 0.8), "bbox {:?}", b);
        if let PrimitiveKind::Arrow { from, to } = &p.kind {
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
                to: Point::new(0.8, 0.6),
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
        if let PrimitiveKind::Arrow { from, to } = &doc.get(inside).unwrap().kind {
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
            kind: PrimitiveKind::Arrow { from: pt(0.7, 0.1), to: pt(0.2, 0.9) },
            style: Style::default(),
        };
        let b = p.bbox();
        let close = |a: f32, e: f32| (a - e).abs() < 1e-6;
        assert!(close(b.x, 0.2) && close(b.y, 0.1) && close(b.w, 0.5) && close(b.h, 0.8));
    }
}
