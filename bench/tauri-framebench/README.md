# tauri-framebench — Step 0 gate, WebKit leg

Measures the one thing the Rust-side framebench cannot: getting a full-resolution
RGBA frame from Rust into a Tauri/WebKit canvas on the target machine, two ways —

1. **Custom protocol streaming** (`frame://localhost/<w>x<h>` → fetch → ImageData →
   putImageData + forced paint) — the architecture the Screensnips plan mandates.
2. **Base64 over command IPC** (the forbidden path) — measured only for contrast.

Sizes: 2560×1080 (the G9's actual ultrawide), 3840×2160, 5120×2880. 5 reps, medians.
Results JSON is written atomically by the app to `/tmp/tauri-framebench-results.json`.

## Run it (on the G9, when the box is idle)

```sh
export PATH="$HOME/.cargo/bin:$PATH"
export RUST_MIN_STACK=33554432
cd tauri-framebench
cargo build --release   # opt-level 2 per the house build rules
TB_OUT=/tmp/tauri-framebench-results.json ./target/release/tauri-framebench
```

Prereqs on Zorin/Ubuntu: Tauri v2 system deps — webkit2gtk-4.1 dev stack verified
present on the G9 2026-10-14 (pkg-config: webkit2gtk-4.1 2.52.6, gtk+-3.0 3.24.41).

## Status

RAN ON THE G9, 2026-10-14 — Step 0 WebKit leg CLOSED. Results:
`../../docs/tauri-framebench-results-g9.json` (protocol fetch+present 100 ms vs
IPC 200 ms @2560×1080; 273 vs 712 @4K; 676 vs 1690 @5K). First compile needed
only harness fixes, no bench-logic or Tauri-API changes: a generated
`icons/icon.png` (tauri-build requires one), `Access-Control-Allow-Origin: *`
on protocol responses (WebKit blocks fetch() of custom schemes without CORS),
page→Rust progress logging so a stall can't be silent, and an rAF timeout
race in `present()`.
