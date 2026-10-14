# Screensnips app (Tauri shell)

Tauri v2 shell wiring the family crates end-to-end:

- **Capture** — `shot-capture` portal backend, PNG decoded in Rust.
- **Document** — `shot-model`; commands (`add_primitive`, `move_primitive`,
  `remove_primitive`, `set_beautify`) mutate it; frames never cross IPC.
- **Frames** — `shot://localhost/source` and `shot://localhost/rendered`
  custom protocol (RGBA + X-Width/X-Height/X-Rev headers), the Step 0
  winning path.
- **Export** — `shot-render` output encoded to PNG, written atomically to
  `~/Pictures/Screensnips/` with a `.annotations.toml` sidecar beside it.

Headless check (no display needed):

```
SCREENSNIPS_SELFTEST=1 ./screensnips   # prints SELFTEST_OK …
```

Build (needs WebKitGTK dev stack, e.g. the G9):

```
cargo build --release --manifest-path crates/app/Cargo.toml
```

Detached workspace on purpose: the family crates stay buildable on machines
without WebKitGTK dev libraries.

Build lessons:
- The `shot://` protocol MUST send `Access-Control-Expose-Headers: *` —
  with only `Access-Control-Allow-Origin`, `fetch()` succeeds but custom
  response headers (X-Width/X-Height/X-Rev) read as `null` in the webview,
  which surfaced as a bogus ImageData length error on first dogfood
  (2026-10-14). The bench never hit it because it derived frame sizes from
  the request URL, not response headers.

Known prototype limits: editor overlay previews primitives in JS (Rust
render is the export truth and the preview pane shows it); selection handles
are display + move for now (per-handle resize lands with the editor
milestone); the icon is a placeholder — the shipping icon is designed
in-family per `~/workspace/design-system/ICON-FAMILY.md`.
