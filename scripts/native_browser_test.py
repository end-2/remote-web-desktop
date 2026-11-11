#!/usr/bin/env python3
"""Opt-in native end-to-end client check; sends real OS input to the visible fixture.

Keep native_test.py --serve open and focused in a visible host browser.
RWD_PASSWORD=... uv run --with playwright --with Pillow python scripts/native_browser_test.py \
  --content-origin X,Y
Only JSON evidence is saved; desktop pixels remain in memory. Color families
identify the fixture's presence and location; they do not measure color fidelity.
"""
import argparse
import base64
import json
import os
from pathlib import Path
import time

from playwright.sync_api import expect, sync_playwright

from native_test import fixture_request, pair, sample_fixture_colors


def run(args):
    generation = fixture_request(args.fixture, "/reset", {})["generation"]
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(channel=os.environ.get("PLAYWRIGHT_CHANNEL", "chrome"), headless=True)
        page = browser.new_page(viewport={"width": 1440, "height": 1050}, device_scale_factor=1)
        messages, errors, renders = [], [], []
        page.on("pageerror", lambda error: errors.append(str(error)))
        page.on("websocket", lambda ws: ws.on("framereceived", lambda data: messages.append(json.loads(data)) if isinstance(data, str) else None))
        # Observe actual draw calls, distinguishing VideoDecoder output from JPEG.
        page.add_init_script("""window.nativeRenderCounts = {h264: 0, jpeg: 0};
          const draw = CanvasRenderingContext2D.prototype.drawImage;
          CanvasRenderingContext2D.prototype.drawImage = function(...args) {
            const result = Reflect.apply(draw, this, args);
            if (this.canvas.id === 'remote-canvas') {
              const kind = typeof VideoFrame !== 'undefined' && args[0] instanceof VideoFrame ? 'h264' : 'jpeg';
              window.nativeRenderCounts[kind]++;
            }
            return result;
          };""")

        def wait_fixture(predicate, description):
            deadline = time.monotonic() + 8
            while time.monotonic() < deadline:
                state = fixture_request(args.fixture, "/state")
                if predicate(state):
                    return state
                page.wait_for_timeout(100)
            raise AssertionError(f"fixture did not observe {description}")

        try:
            initial = wait_fixture(lambda state: state["state"] and state["state"]["generation"] == generation, "reset")
            assert initial["state"]["focused"], "keep the visible fixture focused"
            layout = initial["layout"]
            page.goto(args.server)
            page.locator("#password").fill(os.environ["RWD_PASSWORD"])
            page.locator("#login-button").click()
            canvas = page.locator("#remote-canvas")
            canvas.wait_for(state="visible", timeout=15000)
            hello = next(message for message in messages if message.get("type") == "hello")
            assert hello["mode"] == "desktop", "requires a non-demo desktop server"
            monitor = next(monitor for monitor in hello["monitors"] if monitor["id"] == args.monitor)
            page.locator("#monitor-select").select_option(str(args.monitor))
            for codec, label in (("h264", "H.264"), ("jpeg", "JPEG FAST")):
                expect(page.locator('#codec-select option[value="h264"]')).to_be_enabled(timeout=15000)
                before = page.evaluate("codec => window.nativeRenderCounts[codec]", codec)
                page.locator("#codec-select").select_option(codec)
                expect(page.locator("#transport-label")).to_contain_text(label, timeout=15000)
                page.wait_for_function("p => window.nativeRenderCounts[p.codec] >= p.before + 3", arg={"codec": codec, "before": before}, timeout=15000)
                pixels = base64.b64decode(canvas.evaluate("c => c.toDataURL('image/png')").split(",", 1)[1])
                samples = sample_fixture_colors(pixels, monitor, layout, args.content_origin)
                del pixels
                renders.append({"codec": codec, "color_samples": samples,
                                "draw_counts": page.evaluate("window.nativeRenderCounts")})
                print(f"PASS native {codec} browser decode and four fixture color-family matches", flush=True)

            def canvas_point(name, delta=0):
                target, box = layout["targets"][name], canvas.bounding_box()
                nx = (args.content_origin[0] + target["centerX"] + delta - monitor["left"]) / (monitor["width"] - 1)
                ny = (args.content_origin[1] + target["centerY"] - monitor["top"]) / (monitor["height"] - 1)
                assert 0 < nx < 1 and 0 < ny < 1, "target is outside selected monitor"
                return box["x"] + nx * box["width"], box["y"] + ny * box["height"]

            def click_target(name):
                before = fixture_request(args.fixture, "/state")
                assert before["state"]["focused"], "fixture lost OS focus"
                sequence = max((event["sequence"] for event in before["events"]), default=0)
                target = layout["targets"][name]
                page.mouse.click(*canvas_point(name))
                # Check each event's actual host target; merely seeing text later
                # can hide a stale-coordinate click or delayed focus transition.
                observed = wait_fixture(lambda state: any(
                    event["type"] == "pointerup" and event["sequence"] > sequence
                    for event in state["events"]), f"pointerup on {name}")
                events = [event for event in observed["events"]
                          if event["sequence"] > sequence and event["type"] in ("pointerdown", "pointerup", "click")]
                assert {event["type"] for event in events} == {"pointerdown", "pointerup", "click"}, events
                assert all(event.get("trusted") and event.get("target") == name
                           and abs(event["clientX"] - target["centerX"]) <= 1
                           and abs(event["clientY"] - target["centerY"]) <= 1
                           for event in events), f"native click landed on wrong target: {events}"

            click_target("click")
            clicked = wait_fixture(lambda state: state["state"]["clicks"] == 1 and any(
                event["type"] == "click" and event.get("target") == "click" and event.get("trusted")
                for event in state["events"]), "trusted native click from client canvas")
            print("PASS client canvas click reached native fixture", flush=True)
            click_target("text")
            for _ in range(3):
                click_target("click")
                click_target("text")
            assert fixture_request(args.fixture, "/state")["state"]["clicks"] == 4
            print("PASS eight alternating clicks reached exact native coordinates", flush=True)
            page.mouse.move(*canvas_point("drag", -100))
            page.mouse.down()
            page.mouse.move(*canvas_point("drag", 100), steps=5)
            page.mouse.up()
            wait_fixture(lambda state: any(
                event["type"] == "pointermove" and event.get("target") == "drag"
                and event.get("trusted") and event.get("buttons") == 1
                for event in state["events"])
                and any(event["type"] == "pointerup" and event.get("target") == "drag"
                        and abs(event["clientX"] - layout["targets"]["drag"]["centerX"] - 100) <= 1
                        for event in state["events"]), "native drag from client canvas")
            print("PASS browser drag retained the native held button and final coordinates", flush=True)
            click_target("text")
            text = "브라우저 원격 검증 👋"
            page.locator("#text-button").click()
            page.locator("#remote-text").fill(text)
            page.locator("#text-send-button").click()
            final = wait_fixture(lambda state: text in state["state"]["text"] and any(
                event["type"] == "input" and event.get("target") == "text" and event.get("trusted")
                for event in state["events"]), "trusted Unicode insertion from client text dialog")
            assert not errors, errors
            report = {"browser": browser.version, "mode": hello["mode"], "monitor": monitor,
                      "content_origin": args.content_origin, "renders": renders,
                      "click_count": clicked["state"]["clicks"], "fixture": final, "errors": errors,
                      "checks": ["native H264 fixture presence", "native JPEG fixture presence",
                                 "trusted canvas click", "eight exact alternating native clicks",
                                 "trusted native browser drag", "trusted Korean and emoji input"]}
            Path(args.report).parent.mkdir(parents=True, exist_ok=True)
            Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
            print(f"PASS client Unicode text reached native fixture; metadata: {args.report}", flush=True)
        finally:
            try:
                page.evaluate("fetch('/api/logout', {method: 'POST'})")
            except Exception:
                pass  # Closing the browser also releases the controller connection.
            finally:
                browser.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", default="http://127.0.0.1:8080")
    parser.add_argument("--fixture", default="http://127.0.0.1:18083")
    parser.add_argument("--monitor", type=int, default=1)
    parser.add_argument("--content-origin", type=pair, required=True)
    parser.add_argument("--report", default="artifacts/native-browser-results.json")
    arguments = parser.parse_args()
    if not os.environ.get("RWD_PASSWORD"):
        parser.error("set RWD_PASSWORD to the native server's password")
    run(arguments)
