# Tauri Framebench — Status

- Updated: 2026-10-14 15:47 EDT (America/New_York)
- Phase: DONE — bench ran clean on the G9; results at ~/workspace/screensnips/docs/tauri-framebench-results-g9.json
- Run: waited for an unrelated `cargo test -p fable-files` to release the box, then launched the attempt-3 build in richard's live session; the page completed all 3 sizes x 5 reps and saved results in ~40 s. App killed afterwards.
- Headline numbers (fetch+present, medians): protocol 100 / 273 / 676 ms vs IPC+base64 200 / 712 / 1690 ms at 2560x1080 / 3840x2160 / 5120x2880.
- Fixes (all mirrored to local repo): icons/icon.png added; CORS header on frame protocol; bench_log progress command; page try/catch + protocol error ping; rAF timeout race.
- Final disk on G9: 8.7G free on /.
