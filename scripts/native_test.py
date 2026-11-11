#!/usr/bin/env python3
"""Disposable native desktop fixture and opt-in, real-input verification.

Serve: python3 scripts/native_test.py --serve
Open http://127.0.0.1:18083 in a visible Chrome window; inspect /layout and
visually confirm the content area's desktop origin (logical points on macOS).
Verify: RWD_PASSWORD=... uv run --with websockets --with Pillow python scripts/native_test.py \
  --verify --server http://127.0.0.1:8080 --content-origin X,Y

--verify sends real OS mouse/keyboard input. Keep the disposable fixture focused.
No desktop image is saved. GET /state, /layout and /events expose fixture evidence.
"""

import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
from pathlib import Path
from statistics import median
import sys
import threading
import time
from urllib.request import Request, urlopen


def pair(value):
    try:
        x, y = value.split(",")
        return float(x), float(y)
    except ValueError as error:
        raise argparse.ArgumentTypeError("expected X,Y in desktop logical coordinates") from error


def serve(port):
    lock = threading.Lock()
    evidence = {"generation": 0, "events": [], "layout": None, "state": None}
    document = Path(__file__).with_name("native-test.html").read_bytes()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def reply(self, code, value, content_type="application/json; charset=utf-8"):
            payload = value if isinstance(value, bytes) else json.dumps(value, ensure_ascii=False).encode()
            self.send_response(code)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(payload)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(payload)

        def do_GET(self):
            if self.path == "/":
                return self.reply(200, document, "text/html; charset=utf-8")
            with lock:
                if self.path == "/state":
                    return self.reply(200, evidence)
                if self.path in ("/events", "/layout"):
                    return self.reply(200, evidence[self.path[1:]])
            self.reply(404, {"error": "not found"})

        def do_POST(self):
            length = int(self.headers.get("Content-Length", 0))
            if length > 1024 * 1024:
                return self.reply(413, {"error": "too large"})
            try:
                data = json.loads(self.rfile.read(length) or b"{}")
            except ValueError:
                return self.reply(400, {"error": "invalid JSON"})
            with lock:
                if self.path == "/reset":
                    evidence["generation"] += 1
                    evidence["events"] = []
                    return self.reply(200, {"generation": evidence["generation"]})
                if self.path == "/report":
                    evidence["layout"] = data["layout"]
                    evidence["state"] = data["state"]
                    if data["state"]["generation"] == evidence["generation"]:
                        evidence["events"].extend(data["events"])
                        del evidence["events"][:-4000]
                    return self.reply(200, {"generation": evidence["generation"]})
            self.reply(404, {"error": "not found"})

    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"Native verification fixture: http://127.0.0.1:{server.server_port}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


def fixture_request(origin, path, data=None):
    request = Request(origin.rstrip("/") + path,
                      data=json.dumps(data).encode() if data is not None else None,
                      headers={"Content-Type": "application/json"})
    with urlopen(request, timeout=3) as response:
        return json.load(response)


def sample_fixture_colors(jpeg, monitor, layout, origin):
    """Verify fixture presence by color families; this is not a color-fidelity test.

    The native capture backend can provide display-profile RGB without preserving
    its ICC profile in JPEG. Distinct target families remain identifiable even
    when their channel values differ from the HTML sRGB literals.
    """
    from PIL import Image

    expected = {
        "click": ((20, 16), (255, 0, 204)),
        "drag": ((20, 16), (0, 238, 153)),
        "text": ((-24, -20), (255, 255, 255)),
        "scroll": ((-24, 40), (0, 34, 153)),
    }
    classifiers = {
        "click": lambda r, g, b: r > 190 and b > 155 and g < 110,
        "drag": lambda r, g, b: g > 180 and r < 160 and 100 <= b <= 220 and g - r > 60,
        "text": lambda r, g, b: min(r, g, b) >= 180 and max(r, g, b) - min(r, g, b) < 30,
        "scroll": lambda r, g, b: b > 90 and r < 70 and g < 110 and b - g > 60,
    }
    samples = []
    with Image.open(io.BytesIO(jpeg)) as source:
        decoded = source.convert("RGB")
        try:
            for name, (offset, color) in expected.items():
                target = layout["targets"][name]
                dx = offset[0] if offset[0] > 0 else target["width"] + offset[0]
                dy = offset[1] if offset[1] > 0 else target["height"] + offset[1]
                screen_x = origin[0] + target["x"] + dx
                screen_y = origin[1] + target["y"] + dy
                nx = (screen_x - monitor["left"]) / monitor["width"]
                ny = (screen_y - monitor["top"]) / monitor["height"]
                if not (0 <= nx < 1 and 0 <= ny < 1):
                    raise AssertionError(f"{name} capture sample is outside the selected monitor")
                px = round(nx * decoded.width)
                py = round(ny * decoded.height)
                pixels = [decoded.getpixel((x, y))
                          for y in range(max(0, py - 2), min(decoded.height, py + 3))
                          for x in range(max(0, px - 2), min(decoded.width, px + 3))]
                rgb = [int(median(pixel[channel] for pixel in pixels)) for channel in range(3)]
                samples.append({"target": name, "screen_point": [screen_x, screen_y],
                                "frame_point": [px, py], "sample_rgb": rgb,
                                "expected_rgb": color,
                                "max_channel_error": max(abs(a - b) for a, b in zip(rgb, color)),
                                "color_family_matches": classifiers[name](*rgb),
                                "verification": "fixture presence only; not color fidelity"})
        finally:
            decoded.close()
    mismatches = [sample for sample in samples if not sample["color_family_matches"]]
    if mismatches:
        raise AssertionError("captured frame does not show the visible fixture at the confirmed origin: "
                             + json.dumps(mismatches))
    return samples


def verify(args):
    from smoke_test import Client, check, jpeg_size, receive_until

    password = os.environ.get("RWD_PASSWORD")
    check(password, "set RWD_PASSWORD")
    check(args.content_origin is not None, "--verify requires a visually confirmed --content-origin X,Y")
    generation = fixture_request(args.fixture, "/reset", {})["generation"]

    def wait_fixture(predicate, description, timeout=4):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            result = fixture_request(args.fixture, "/state")
            if predicate(result):
                return result
            time.sleep(0.1)
        raise AssertionError(f"fixture did not observe {description}; state={json.dumps(result, ensure_ascii=False)}")

    initial = wait_fixture(lambda v: v["state"] and v["state"]["generation"] == generation,
                           "reset acknowledgement")
    check(initial["state"]["focused"], "fixture must be the focused visible window before --verify")
    layout = initial["layout"]
    client = Client(args.server)
    client.login(password)
    websocket = client.websocket()
    frames = []
    results = []
    reader = None
    reader_errors = []

    def start_reader(socket):
        stop = threading.Event()
        count = {"frames_acknowledged": 0}

        def consume():
            while not stop.is_set():
                try:
                    message = socket.receive(timeout=0.5)
                    if isinstance(message, bytes):
                        socket.send_json({"type": "ack"})
                        count["frames_acknowledged"] += 1
                    elif message.get("type") == "error":
                        raise AssertionError(f"native stream error: {message}")
                except TimeoutError:
                    continue
                except Exception as error:
                    if not stop.is_set():
                        reader_errors.append(str(error))
                    return

        thread = threading.Thread(target=consume, daemon=True)
        thread.start()
        return stop, thread, count

    def close_stream(socket, consumer):
        if consumer is not None:
            consumer[0].set()
        socket.close()
        if consumer is not None:
            consumer[1].join(timeout=2)

    try:
        hello = websocket.receive()
        check(hello.get("type") == "hello" and hello.get("mode") == "desktop", "requires native desktop server")
        monitor = next(m for m in hello["monitors"] if m["id"] == args.monitor)

        def point(name, delta=0):
            rect = layout["targets"][name]
            screen_x = args.content_origin[0] + rect["centerX"] + delta
            screen_y = args.content_origin[1] + rect["centerY"]
            x = (screen_x - monitor["left"]) / (monitor["width"] - 1)
            y = (screen_y - monitor["top"]) / (monitor["height"] - 1)
            check(0 <= x <= 1 and 0 <= y <= 1, f"{name} target outside selected monitor")
            return {"x": x, "y": y}

        def send(message):
            check(not reader_errors, f"background stream failed: {reader_errors}")
            websocket.send_json(message)
            time.sleep(0.08)

        def click(name):
            send({"type": "pointer", **point(name)})
            for down in (True, False):
                send({"type": "button", "button": "left", "down": down, **point(name)})

        def has_event(snapshot, event_type, target=None, **values):
            return any(event["type"] == event_type and event.get("trusted")
                       and (target is None or event.get("target") == target)
                       and all(event.get(key) == value for key, value in values.items())
                       for event in snapshot["events"])

        def passed(message):
            results.append(message)
            print("PASS", message, flush=True)

        settings = {"type": "settings", "monitor": args.monitor, "fps": 15,
                    "quality": 65, "max_width": 1280, "codec": "jpeg"}
        websocket.send_json(settings)
        # Drain/ACK old frames before accepting pixels from the requested monitor.
        confirmed = receive_until(websocket, lambda m: isinstance(m, dict)
                                  and m.get("type") == "settings"
                                  and all(m.get(key) == value for key, value in settings.items()))
        jpeg = receive_until(websocket, lambda m: isinstance(m, bytes) and m[:2] == b"\xff\xd8")
        dimensions = jpeg_size(jpeg)
        samples = sample_fixture_colors(jpeg, monitor, layout, args.content_origin)
        frames.append({"codec": "jpeg", "bytes": len(jpeg), "width": dimensions[0],
                       "height": dimensions[1], "settings": confirmed, "color_samples": samples})
        passed(f"native JPEG capture {dimensions[0]}x{dimensions[1]} with four fixture color-family matches ({len(jpeg)} bytes; not saved)")
        websocket.send_json({**settings, "codec": "h264"})
        decoder = receive_until(websocket, lambda m: isinstance(m, dict) and m.get("type") == "decoder")
        h264 = receive_until(websocket, lambda m: isinstance(m, bytes) and m[:4] == b"RWH1")
        check(h264[4] == 1 and len(h264) > 9, "missing H.264 keyframe")
        frames.append({"codec": decoder["codec"], "bytes": len(h264), "keyframe": True})
        passed(f"native H.264 keyframe {decoder['codec']} ({len(h264)} bytes; not saved)")
        del jpeg, h264
        reader = start_reader(websocket)

        click("click")
        wait_fixture(lambda v: v["state"]["clicks"] == 1 and has_event(v, "click", "click"), "native click")
        passed("trusted native pointer down/up and click")
        send({"type": "pointer", **point("drag", -100)})
        send({"type": "button", "button": "left", "down": True, **point("drag", -100)})
        for dx in (-50, 0, 50, 100):
            send({"type": "pointer", **point("drag", dx)})
        send({"type": "button", "button": "left", "down": False, **point("drag", 100)})
        wait_fixture(lambda v: has_event(v, "pointermove", "drag", buttons=1)
                     and has_event(v, "pointerup", "drag"), "native drag")
        passed("trusted native drag with held left mouse button")

        click("text")
        for code, key in (("KeyA", "a"), ("KeyB", "b"), ("KeyC", "c")):
            for down in (True, False):
                send({"type": "key", "code": code, "key": key, "down": down})
        wait_fixture(lambda v: all(has_event(v, "keydown", "text", code=code)
                                  and has_event(v, "keyup", "text", code=code)
                                  for code in ("KeyA", "KeyB", "KeyC")), "physical ASCII keys")
        keyboard = fixture_request(args.fixture, "/state")["state"]["text"]
        passed(f"trusted physical KeyA/KeyB/KeyC down/up; keyboard layout produced {keyboard!r}")
        unicode_text = " 원격 검증 👋"
        send({"type": "text", "text": unicode_text})
        wait_fixture(lambda v: unicode_text in v["state"]["text"], "Unicode text")
        passed("native Unicode text insertion (Korean + emoji)")

        send({"type": "pointer", **point("scroll")})
        send({"type": "scroll", "dx": 0, "dy": -5})
        wait_fixture(lambda v: v["state"]["scrollTop"] > 0 and has_event(v, "wheel"), "native downward scroll")
        passed("trusted native wheel event and scroll position change")

        click("text")
        # A bounded burst reproduces Enigo's old per-event accumulating Drop
        # delay, while all coordinates stay inside the disposable text area.
        for index in range(args.burst_events):
            websocket.send_json({"type": "pointer", **point("text", index % 2)})
        send({"type": "key", "code": "ShiftLeft", "key": "Shift", "down": True})
        send({"type": "button", "button": "left", "down": True, **point("text")})
        wait_fixture(lambda v: has_event(v, "keydown", "text", code="ShiftLeft"), "held Shift before disconnect")
        disconnect_started = time.monotonic()
        close_stream(websocket, reader)
        disconnect_ms = (time.monotonic() - disconnect_started) * 1000
        websocket = None
        stream_frames = reader[2]["frames_acknowledged"]
        reader = None
        check(not reader_errors, f"background stream failed: {reader_errors}")
        wait_fixture(lambda v: has_event(v, "keyup", "text", code="ShiftLeft")
                     and any(e["type"] == "pointerup" and e.get("target") == "text"
                             and e["time"] > max(k["time"] for k in v["events"] if k["type"] == "keydown" and k.get("code") == "ShiftLeft")
                             for e in v["events"]), "disconnect releases keyboard and mouse")
        passed("disconnect releases held Shift and left mouse button at OS level")
        check(disconnect_ms < 1500, f"disconnect took {disconnect_ms:.1f} ms after input burst")
        passed(f"bounded disconnect after {args.burst_events} rapid pointer events ({disconnect_ms:.1f} ms)")

        websocket = client.websocket()
        check(websocket.receive().get("type") == "hello", "reconnect controller acquisition")
        reader = start_reader(websocket)
        send({"type": "key", "code": "KeyZ", "key": "z", "down": True})
        send({"type": "key", "code": "KeyZ", "key": "z", "down": False})
        final = wait_fixture(lambda v: has_event(v, "keydown", "text", code="KeyZ", shiftKey=False), "unmodified key after reconnect")
        passed("reconnect and trusted unmodified key (no stuck Shift)")
        report = {"monitor": monitor, "content_origin": args.content_origin, "frames": frames,
                  "checks": results, "fixture": final,
                  "burst_events": args.burst_events, "disconnect_elapsed_ms": disconnect_ms,
                  "frames_acknowledged_during_input": stream_frames + reader[2]["frames_acknowledged"]}
        if args.report:
            Path(args.report).parent.mkdir(parents=True, exist_ok=True)
            Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps({"passed": len(results), "text": final["state"]["text"],
                          "scroll_top": final["state"]["scrollTop"], "report": args.report}, ensure_ascii=False))
    finally:
        if websocket is not None:
            try:
                websocket.send_json({"type": "release"})
            finally:
                close_stream(websocket, reader)
        client.request("POST", "/api/logout", {})


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--serve", action="store_true")
    mode.add_argument("--verify", action="store_true")
    parser.add_argument("--port", type=int, default=18083)
    parser.add_argument("--server", default="http://127.0.0.1:8080")
    parser.add_argument("--fixture", default="http://127.0.0.1:18083")
    parser.add_argument("--monitor", type=int, default=1)
    parser.add_argument("--content-origin", type=pair)
    parser.add_argument("--burst-events", type=int, choices=range(0, 301), default=200,
                        metavar="0..300", help="pointer burst before disconnect, within fixture only")
    parser.add_argument("--report", help="JSON metadata/event evidence output; no screenshots")
    args = parser.parse_args()
    if args.serve:
        serve(args.port)
    else:
        try:
            verify(args)
        except Exception as error:
            print(f"FAIL: {error}", file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
