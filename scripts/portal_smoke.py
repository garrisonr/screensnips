#!/usr/bin/env python3
"""Screensnips Step 0 — live portal capture smoke test.

Calls org.freedesktop.portal.Screenshot.Screenshot (non-interactive) over the
session D-Bus, waits for org.freedesktop.portal.Request.Response, and writes a
small JSON result atomically (temp file + rename) so an interrupted run never
leaves a half-written report.

Exit codes: 0 success, 2 portal error/cancelled, 3 timeout, 4 setup error.
Environment: SMOKE_OUT (result path, default /tmp/screensnips-portal-smoke-result.json),
SMOKE_TIMEOUT (seconds, default 120).
"""

import json
import os
import struct
import sys
import tempfile
import time
from urllib.parse import unquote, urlparse

try:
    from gi.repository import Gio, GLib
except Exception as exc:  # PyGObject missing -> setup error, never a silent pass
    print(f"SETUP_ERROR importing PyGObject: {exc}")
    sys.exit(4)

RESULT_PATH = os.environ.get("SMOKE_OUT", "/tmp/screensnips-portal-smoke-result.json")
TIMEOUT = int(os.environ.get("SMOKE_TIMEOUT", "120"))
TOKEN = "screensnips_smoke"

result = {
    "test": "org.freedesktop.portal.Screenshot (non-interactive)",
    "started_unix": time.time(),
    "status": "started",
}


def save() -> None:
    result["updated_unix"] = time.time()
    directory = os.path.dirname(RESULT_PATH) or "."
    fd, tmp = tempfile.mkstemp(prefix=".smoke-", dir=directory)
    with os.fdopen(fd, "w") as fh:
        json.dump(result, fh, indent=2, sort_keys=True)
        fh.write("\n")
    os.chmod(tmp, 0o644)  # report carries metadata only; let the operator read it back
    os.replace(tmp, RESULT_PATH)


def png_dimensions(path):
    """Return (width, height) for a PNG from its IHDR, else (None, None)."""
    try:
        with open(path, "rb") as fh:
            head = fh.read(33)
        if len(head) >= 33 and head[:8] == b"\x89PNG\r\n\x1a\n" and head[12:16] == b"IHDR":
            width, height = struct.unpack(">II", head[16:24])
            return width, height
    except OSError:
        pass
    return None, None


def main() -> int:
    save()
    try:
        conn = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    except Exception as exc:
        result.update(status="setup_error", error=f"no session bus: {exc}")
        save()
        print(f"SETUP_ERROR no session bus: {exc}")
        return 4

    loop = GLib.MainLoop()
    state = {"done": False, "exit": 3}
    unique = conn.get_unique_name() or ""
    expected_path = (
        "/org/freedesktop/portal/desktop/request/"
        + unique.lstrip(":").replace(".", "_")
        + "/"
        + TOKEN
    )
    result["expected_request_path"] = expected_path
    save()
    print(f"WAITING timeout={TIMEOUT}s expected_request={expected_path}", flush=True)

    def finish(code, **fields):
        if state["done"]:
            return False
        state["done"] = True
        state["exit"] = code
        result.update(fields)
        result["status"] = fields.get("status", result.get("status"))
        save()
        print(f"RESULT {json.dumps(result, sort_keys=True)}", flush=True)
        loop.quit()
        return False

    def on_signal(_conn, sender, path, iface, member, params, _data):
        if iface != "org.freedesktop.portal.Request" or member != "Response":
            return
        try:
            status, results = params.unpack()
        except Exception as exc:
            finish(2, status="response_parse_error", error=str(exc))
            return
        result["response_sender"] = sender
        result["response_path"] = path
        result["portal_response_code"] = status
        if status == 0:
            uri = None
            if isinstance(results, dict):
                uri = results.get("uri")
            fields = {"status": "success", "uri": uri}
            if uri:
                parsed = urlparse(uri)
                if parsed.scheme == "file":
                    fpath = unquote(parsed.path)
                    fields["file_path"] = fpath
                    try:
                        fields["file_bytes"] = os.path.getsize(fpath)
                        width, height = png_dimensions(fpath)
                        if width:
                            fields["width"] = width
                            fields["height"] = height
                    except OSError as exc:
                        fields["file_stat_error"] = str(exc)
            finish(0, **fields)
        elif status == 1:
            finish(2, status="user_cancelled")
        else:
            finish(2, status="portal_error")

    # Broad subscription (any Request.Response) + exact-path subscription.
    conn.signal_subscribe(
        None, "org.freedesktop.portal.Request", "Response", None, None,
        Gio.DBusSignalFlags.NONE, on_signal, None,
    )
    conn.signal_subscribe(
        "org.freedesktop.portal.Desktop", "org.freedesktop.portal.Request",
        "Response", expected_path, None, Gio.DBusSignalFlags.NONE, on_signal, None,
    )

    def on_call(obj, res, _data):
        try:
            out = obj.call_finish(res)
            handle = out.unpack()[0]
            result["handle"] = handle
            save()
            print(f"HANDLE {handle}", flush=True)
        except Exception as exc:
            finish(2, status="call_error", error=str(exc))

    params = GLib.Variant(
        "(sa{sv})",
        (
            "",
            {
                "handle_token": GLib.Variant("s", TOKEN),
                "modal": GLib.Variant("b", False),
                "interactive": GLib.Variant("b", False),
            },
        ),
    )
    conn.call(
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Screenshot",
        "Screenshot",
        params,
        GLib.VariantType.new("(o)"),
        Gio.DBusCallFlags.NONE,
        -1,
        None,
        on_call,
        None,
    )
    GLib.timeout_add_seconds(TIMEOUT, lambda: finish(3, status="timeout"))
    loop.run()
    return state["exit"]


if __name__ == "__main__":
    sys.exit(main())
