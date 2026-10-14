#!/bin/bash
# Screensnips Step 0 — guarded framebench runner for the G9.
# Refuses to run while any cargo/rustc build is active (one heavy thing at a
# time, and benchmark numbers on a loaded box are garbage anyway).
# Exits 10 = busy, 11 = low disk, 0 = report written to /tmp/framebench-g9-report.txt
set -u
if pgrep -af 'bin/cargo|bin/rustc' | grep -v "$$" >/dev/null 2>&1; then
  echo "BUSY: a cargo/rustc build is active; not running."
  exit 10
fi
AVAIL_G=$(df -BG --output=avail / | tail -1 | tr -dc '0-9')
if [ "${AVAIL_G:-0}" -lt 5 ]; then
  echo "LOW DISK: ${AVAIL_G}G free on /; not running."
  exit 11
fi
/tmp/screensnips-framebench --save /tmp/framebench-g9-report.txt
