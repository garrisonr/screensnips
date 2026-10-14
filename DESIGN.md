# Screensnips — Design Language

**Status:** LAW for this project, inherited — not invented. Screensnips is the
third app in the family; it adopts the Fable Files design law
(`~/workspace/fable-files/DESIGN.md`, Apple fluid-interface principles) and the
shared brand themes (`~/workspace/fable-files/docs/BRAND-THEMES.md`, the single
source of truth for token values across Fable Files + Linux System Profiler +
Screensnips). If a token value changes, it changes there first, then here.

## 1. Brand themes (identical names/values in all three apps)

### Liquid Glass — `liquidglass` (dark, default)
- accent `#6fd3ff` · ink `#f5f9ff` · muted `#a9bed6`
- surface `rgba(26,32,44,.55)` · rail `rgba(18,22,32,.58)` · border `rgba(255,255,255,.22)`
- danger `#ff6b81` · green `#7ee2a8` · amber `#ffd479`
- radius 12px · card radius 18px · material blur 18px + saturate(1.5)
- font `system-ui, "Inter", sans-serif` · color-scheme: dark

### Material You — `materialyou` (dark, MD3)
- accent `#cfbcff` · ink `#e6e0e9` · muted `#9d97a6`
- surface `rgba(20,18,24,.95)` · rail `rgba(16,13,20,.97)` · border `rgba(147,143,153,.38)`
- danger `#ffb4ab` · radius 16px · card radius 24px
- font `"Roboto", "Inter", system-ui, sans-serif` · color-scheme: dark

## 2. Shared control geometry (family law)

- Controls are **2rem** tall; inputs have **9px radius**; buttons are **pills**.
- Springs, not fixed durations: critically damped (damping 1.0, response
  0.3–0.4) by default; bounce (~0.8) only when the gesture carried momentum.
  Every animation is interruptible and re-targetable mid-flight; animate from
  the live value, never the logical target. Velocity handoff on drag release.
- Feedback on pointer-down, not release. Drags track 1:1 with the grab offset.
- Translucent materials for chrome (toolbar/rail/sheets) with backdrop blur;
  never stack translucent on translucent without a blur layer.
- `prefers-reduced-motion`: springs become short opacity cross-fades.
  Also honor reduced-transparency and increased-contrast preferences.
- Typography: system stack first; tracking tightens on large text (−0.02em),
  body near 0; spacing in relative units (Dynamic Type safe).

## 3. Icon & branding

- The app icon is born in the family: see `~/workspace/design-system/ICON-FAMILY.md`
  (Fable tile geometry/radius, one flat white glyph — crop-frame + snip — on the
  Screensnips brand tint). No icon ships without a dock-size family check.

## 4. Multi-format layout modes (one codebase)

Breakpoints are layout modes, not separate apps:
- **Phone (<600px):** single column; capture button primary; editor tools in a
  bottom sheet; canvas fits width.
- **Tablet (600–1024px):** canvas + collapsible tool rail; library as overlay grid.
- **Laptop/Desktop (>1024px):** three-zone editor — library rail (left,
  translucent), canvas (center), inspector (right); full toolbar.
Touch targets ≥ 44px on coarse pointers; rem-based spacing throughout.

## 5. Editor-specific design rules

- **Selection & crop handles — Zorin is the floor, not the ceiling** (Dickles,
  2026-10-14: he likes the handles in Zorin/GNOME's screenshot selection; ours
  must be at least that good, then better). Eight handles (corners + edges),
  visually ~12px with hit areas ≥24px (≥44px on coarse pointers); 1:1 pointer
  tracking that respects the grab offset — the region never jumps; drag inside
  to move; live physical-pixel dimension readout attached to the selection;
  magnetic snap to window and screen edges; dimmed surround + rule-of-thirds
  guides while dragging; arrow keys nudge 1px (Shift = 10px); aspect lock on a
  modifier; Esc cancels, Enter confirms. Applies to the capture surface
  (wlroots/X11), the editor crop tool, and every region picker we draw
  ourselves (OCR/redact regions). Where the GNOME portal draws its own picker
  we don't fight it — but our re-crop must feel like the upgrade it is.
- **Canvas first.** Chrome frames the screenshot; the screenshot is the hero.
  Default canvas backdrop: neutral checkerboard is FORBIDDEN in previews —
  beautify backgrounds render exactly as they will export.
- **Annotation defaults:** accent-red arrow/callout (`#ff3b30`-family) is the
  default annotation color, theme-aware; step badges are filled circles in the
  accent with white numerals, numbered automatically in z-order — they must
  read at thumbnail size.
- **Presentation discipline (Step 0 law):** full frames cross the Rust↔webview
  protocol boundary only on capture/open. Editing interactions re-present only
  dirty regions/tiles. Shadows render via the downscaled-blur path. Any design
  that requires a whole-frame re-present per pointer move is rejected at
  review, no matter how pretty the demo looks.
- **Fast Render mode** (auto-suggested on low-spec hardware per the Step 0
  probe): reduced material blur, no live shadow during drags (shadow settles
  on release), thumbnail-first library. It is a designed mode, not a degraded
  accident.
- **First-run privacy banner:** local-only, no account, no cloud — stated in
  the UI in plain language, with the "setting things up for you" notice style
  used when the app auto-installs a helper (never a dependency error).

## 6. What "done" includes

Report a Bug + Request a Feature entries (GitHub issues, diagnostics attached
where gatherable), icon family check, reduced-motion pass, and the full
capture → annotate → beautify → copy/save QA workflow on the QA matrix
(GNOME/portal first — the G9 — then KDE Plasma 6, then Hyprland/wlroots).
