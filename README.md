# Screensnips

A local, no-subscription screenshot studio for Linux: Snagit-style capture + annotation
(arrows, callouts, step numbers, stamps) and CleanShot X-style beautification
(backgrounds, padding, shadows, presets) in one app. No account, no cloud, offline-capable.

- Rust workspace, small crates, Tauri v2 shell (Phase 1).
- Plan: see the project plan (PLAN.md, v0.5) in the Linux Screenshot Studio project workspace.
- Name cleared: full USPTO trademark screen, GREEN, 2026-10-14.

## Step 0 — capability probe

`crates/probe` builds `screensnips-probe`, a dependency-free binary that reports what a
given machine can actually support before we promise anything: session/compositor,
xdg-desktop-portal interfaces (Screenshot / ScreenCast / GlobalShortcuts / Clipboard),
wlroots protocol signals, helper tools (PipeWire, Tesseract, grim-class), hardware class,
and — the important part — **which capture UX that machine gets**: our own selection
surface (loupe, dimensions, countdown) on wlroots/X11, or the compositor's native
picker on the Portal path.

Run it:

```sh
cargo run --release -p screensnips-probe
./target/release/screensnips-probe --json
./target/release/screensnips-probe --save report.txt
```

Step 0 CLOSED 2026-10-14: probe run on the target rig (portal backend, compositor
picker UX, GlobalShortcuts absent), live portal capture smoke test passed
(2560×1080 PNG end-to-end), Rust-side framebench passed (downscale-first blur is
mandatory: 14–32× faster than full-res), and the Tauri/WebKit bench
(bench/tauri-framebench) proved custom-protocol streaming beats base64-over-IPC
2–2.6× (100 ms vs 200 ms fetch+present at 2560×1080). Phase 1 constraint: full
frames cross the protocol boundary only on capture/open; editing presents
incrementally (dirty regions / tiles). Reports in docs/.
