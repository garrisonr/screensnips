# Screensnips

Your screenshot studio for Linux — Snagit's annotation desk and
CleanShot-style beautification in one local, offline app. No account, no
cloud, no subscription. Part of a family of Rust apps (Fable Files,
Linux System Profiler) sharing one design language.

## What it does

- **Capture** — the system screenshot picker frames your shot (area,
  window, or full screen), or ⚡ Instant grabs the whole screen.
  A 3/5/10-second delay gives you time to arrange windows; an instant
  grab hides Screensnips first so it never photographs itself.
- **Annotate** — arrows, lines (hold Shift for 45° snaps), freehand pen,
  rectangles, ellipses, highlight, numbered step badges (they renumber
  themselves), text, callouts, spotlight, blur and pixelate for the
  private bits, and clip-art stamps (✓ ✕ ★ ⚠ ⬆). Six ink colors.
  Crop with 8 fat resize handles; arrow keys nudge a selection 1px
  (Shift: 10px); Delete removes it.
- **Beautify** — padding (linked or per-side), corner radius, drop
  shadow, backgrounds (solid, gradient, or your own screenshot blurred),
  and canvas shapes (1:1, 16:9, 4:3, 3:2, 9:16) that grow the canvas
  around your shot — never crop it. Presets ship built in (Bondi,
  Newspaper, Grizzette) and you can save your own.
- **Get it out** — Copy puts the finished image on the clipboard (it
  stays there even after you close the app); Export saves PNG, JPG, or
  WebP at 1× or 2×. Every export also saves the original shot and an
  annotations file, so the built-in library can reopen any shot later
  with its annotations still editable.
- **Your shots live in** `~/Pictures/Screensnips`. Nothing leaves your
  machine, ever.

## No dependency errors, ever

If your system is missing a small tool Screensnips needs (like the
Wayland clipboard helper), Screensnips sets it up itself with the usual
OS permission prompt. It never dumps a raw error on you; if something
truly can't be done, it says so in plain words and everything else
keeps working.

## Diagnostics & honesty

The Help section shows exactly what Screensnips found on your machine
(backend, portal interfaces, text font, folders). Features that don't
exist yet — our own region-selection surface on GNOME, pin-to-desktop,
drag-out — are labeled as planned, never faked. Capture backends can be
forced in that panel (Auto / system picker / wlroots / X11); a backend
that isn't built yet says so instead of failing quietly.

Keyboard shortcut: this desktop offers no global-shortcut service to
apps, so bind a key in your desktop's keyboard settings to
`screensnips --capture=picker` or `screensnips --capture=full`
(add `--delay=3` if you like).

## Report a Bug / Request a Feature

In the app, under Help — the forms open with your diagnostics already
filled in:
https://github.com/garrisonr/screensnips/issues

## Notes for the curious

- Stamps are drawn procedurally in code — no bundled clip-art assets,
  no third-party artwork licenses to worry about.
- Annotation text rasterizes a system font (Inter, DejaVu, Liberation,
  or Noto, whichever your system has); if none is found it falls back
  to a built-in bitmap font.
- Documents save as a PNG plus a human-readable TOML sidecar
  (`<shot>.annotations.toml`, format version 1).

## Development

Tauri v2 shell wiring the family crates end-to-end:

- **Capture** — `shot-capture` portal backend (zbus Screenshot), PNG
  decoded in Rust; other backends are concrete per environment.
- **Document** — `shot-model`; Tauri commands mutate it; frames never
  cross IPC as base64.
- **Frames** — `shot://localhost/source` and `shot://localhost/rendered`
  custom protocol (raw RGBA + X-Width/X-Height/X-Rev headers), the
  Step 0 winning path; library thumbnails ride
  `shot://localhost/thumb`.
- **Render** — `shot-render`, a pure-Rust software rasterizer; the
  preview pane is its truth.

Headless check (no display needed):

```
SCREENSNIPS_SELFTEST=1 ./screensnips   # prints SELFTEST_OK …
```

Build (needs the WebKitGTK dev stack):

```
cargo build --release --manifest-path crates/app/Cargo.toml
```

Detached workspace on purpose: the family crates stay buildable on
machines without WebKitGTK dev libraries.

Build lessons:
- The `shot://` protocol MUST send `Access-Control-Expose-Headers: *` —
  with only `Access-Control-Allow-Origin`, `fetch()` succeeds but custom
  response headers (X-Width/X-Height/X-Rev) read as `null` in the
  webview, which surfaced as a bogus ImageData length error on first
  dogfood (2026-10-14). The bench never hit it because it derived frame
  sizes from the request URL, not response headers.
