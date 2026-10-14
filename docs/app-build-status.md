# Screensnips app-shell build — status (sandbox-side checkpoint)

Build state lives on the G9 at /home/richard/screensnips-app. This file is the
resume record: steps marked DONE must be skipped on re-run.

- [x] STEP 0: target checks — DONE. Disk 8.6G free. Fable-files build was
  running at first check; waited on-target (30s polls); BOX_IDLE after 120s.
  Disk unchanged at 8.6G free.
- [x] STEP 1: copy tree to /home/richard/screensnips-app — DONE. Tarball piped
  over the ssh wrapper (58,745 bytes; md5 8a42fcb1b263afcef9890b3d2019951a
  identical both sides). Extracted as richard; layout verified:
  crates/app/{Cargo.toml,build.rs,tauri.conf.json,icons/icon.png,
  src/main.rs,ui/index.html,README.md}.
- [x] STEP 2: cargo build --release — DONE, attempt 1 of 4, ZERO code fixes
  needed. `Finished release profile in 7m 52s` (cargo's own line;
  BUILD_EXIT=0). Only warning: proc-macro-error2
  future-incompatibility notice from a transitive dep (not our code). Binary:
  /home/richard/screensnips-app/crates/app/target/release/screensnips.
  Full log on target: /tmp/screensnips-build1.log.
- [x] STEP 3: SCREENSNIPS_SELFTEST=1 run — DONE. Exit 0.
  stdout (verbatim): `SELFTEST_OK canvas=896x546 png_bytes=420788 unrendered=0`
  stderr (verbatim): `selftest detect: backend=Portal ux=CompositorPicker session=`
  /tmp/screensnips-selftest.png on target: 420,788 bytes — exactly png_bytes. ✓
  NOTE on canvas size: the brief expected 928x578, actual is 896x546. 896x546 is
  CORRECT for the shipped shot-model semantics: synthetic source 800x450,
  default uniform padding 0.06, padding unit = image WIDTH for all sides
  (PLAN.md design) → 48 px/side → 800+96=896, 450+96=546. The binary's own
  selftest assert (expect_w=896) passed. 928x578 would require padding 0.08.
  No code changed for this — flagged for parent to confirm intent.
- [x] STEP 4: GUI smoke launch — DONE. Launched in richard's live session with
  the full session env from the brief. App process PID 131357 alive and stable
  (ps STAT=Sl at 22 s elapsed), log /tmp/screensnips-app.log EMPTY (0 bytes,
  0 panic matches). No interaction performed. Terminated via `sudo kill 131357`
  (plain kill from grizz = operation not permitted); verified APP_TERMINATED,
  no app processes left. Copies of the tree remain on the G9 at
  /home/grizz/tauri-framebench-style path equivalent: /home/richard/screensnips-app
  (with target/ build dir) — cleanup is parent's call.
- [x] STEP 5: final report — this file. Disk after all work: 7.1G free
  (was 8.6G; the target/ build tree accounts for the ~1.5G).

RESULT: BUILD_OK / SELFTEST_OK / GUI_OK
(One flag for parent: selftest canvas 896x546 vs brief's 928x578 — see STEP 3
note; code + its own assert + shot-model padding semantics agree on 896x546.)

## Log

- 2026-10-14 ~16:08 EDT — STEP 0 partial: target reachable (richard-NucBoxG9).
  Disk: /dev/mmcblk0p2 57G, 8.6G free (85% used) — above the 6 GB floor, OK.
  A fable-files release build IS running (cargo build --release -p fable-files
  --features custom-protocol -j 1; rustc on fable_files_lib). Per one-heavy-thing
  rule: STEP 1 copy proceeds (light I/O), STEP 2 build waits for idle.

---

## MVP completion build — 2026-10-14 (late)

Shipped on top of the state above (library rail + trio + crop already in):

- shot-model: + Spotlight, + Callout {at, tail, text}, + Beautify.aspect
  (sidecar format stays v1). shot-render: system-font text via ab_glyph
  (bitmap fallback kept), procedural stamps (check/cross/star/warning/
  arrow_up; unknown ids still report unrendered), spotlight, callout,
  aspect canvas growth. Sandbox tests: model 10, render 11, capture 3,
  library 1 — all green.
- App: Copy (wl-copy/xclip + pkexec auto-install), export PNG/JPG/WebP
  1x/2x, presets (Bondi/Newspaper/Grizzette seeds), forced-backend
  setting, delay + hide-before-instant, --capture/--delay CLI,
  diagnostics panel, Report a Bug / Request a Feature, quickbar,
  first-run banner, in-family icon (icon/screensnips.svg).
- G9 release build: attempt 1, zero code fixes, 23m49s (-j 1, opt-level
  2). Binary 17,267,272 B, MD5 4166be6487b96ac38532fc2c717f9246.
  SELFTEST_OK canvas=896x546 png_bytes=420788 unrendered=0 library=ok.
- wl-copy mechanism verified in richard's session: wl-paste lists
  image/png and returns the exact 70 test bytes after the client exits.
- Installed: ~/Applications/screensnips (MD5 == built), hicolor icons
  16..512, ~/.local/share/applications/screensnips.desktop with
  picker/full actions. GUI smoke: PID stable, empty log; left running
  for Dickles' dogfood. Disk after: 4.2G free (build tree grew; watch).
- Click-path gates (portal pick, copy -> close -> paste, screenshot QA)
  are Dickles' dogfood tests — headless seat cannot execute them.
