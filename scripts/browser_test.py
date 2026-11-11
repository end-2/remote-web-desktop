#!/usr/bin/env python3
"""Real Chromium checks against --demo. Install playwright; no host input is injected.

RWD_PASSWORD=... uv run --with playwright python scripts/browser_test.py
Uses installed Chrome, or PLAYWRIGHT_CHANNEL=chromium after `playwright install chromium`.
"""
import json
import os
from pathlib import Path
import sys

from playwright.sync_api import sync_playwright, expect


def main():
    origin = sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8080"
    password = os.environ["RWD_PASSWORD"]
    artifacts = Path("artifacts")
    artifacts.mkdir(exist_ok=True)
    with sync_playwright() as p:
        channel = os.environ.get("PLAYWRIGHT_CHANNEL", "chrome")
        browser = p.chromium.launch(channel=channel, headless=True)
        page = browser.new_page(viewport={"width": 1440, "height": 1050}, device_scale_factor=1)
        errors = []
        sent = []
        received = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.on("console", lambda msg: errors.append(msg.text) if msg.type == "error" else None)

        def ws_opened(ws):
            ws.on("framesent", lambda data: sent.append(json.loads(data)) if isinstance(data, str) else None)
            ws.on("framereceived", lambda data: received.append(json.loads(data)) if isinstance(data, str) else None)

        page.on("websocket", ws_opened)
        page.goto(origin)
        page.locator("#password").fill(password)
        page.locator("#login-button").click()
        page.locator("#remote-canvas").wait_for(state="visible", timeout=15000)
        assert page.locator("#demo-badge").is_visible(), "Only run browser tests against --demo"
        # WebCodecs is a real browser decode path; allow JPEG only if AVC unsupported.
        supported = page.evaluate("async () => typeof VideoDecoder !== 'undefined' && (await VideoDecoder.isConfigSupported({codec:'avc1.42E02A',optimizeForLatency:true})).supported")
        if supported:
            expect(page.locator("#transport-label")).to_contain_text("H.264", timeout=15000)
        page.wait_for_timeout(1500)
        assert int(page.locator("#fps-metric").inner_text()) > 0
        canvas = page.locator("#remote-canvas")
        assert canvas.evaluate("c => c.width > 600 && c.height > 300")
        page.screenshot(path=str(artifacts / "desktop.png"))
        initial_transport = page.locator("#transport-label").inner_text()

        # Verify native input protocol without touching the real desktop.
        canvas.click(position={"x": 100, "y": 100})
        page.keyboard.press("Control+a")
        box = canvas.bounding_box()
        page.mouse.move(box["x"] + 180, box["y"] + 180)
        page.mouse.down()
        page.mouse.move(box["x"] + 250, box["y"] + 210, steps=5)
        page.mouse.up()
        page.mouse.wheel(0, 200)
        page.keyboard.down("Shift")
        page.locator("#text-button").click()
        page.keyboard.up("Shift")
        page.locator("#remote-text").fill("원격 입력 테스트")
        page.locator("#text-send-button").click()
        page.wait_for_timeout(100)
        assert any(e.get("type") == "text" and e.get("text") == "원격 입력 테스트" for e in sent)
        assert any(e.get("type") == "key" and e.get("code") == "KeyA" for e in sent)
        assert any(e.get("type") == "button" and e.get("down") is False for e in sent)
        assert any(e.get("type") == "scroll" for e in sent)
        assert any(e.get("type") == "release" for e in sent)

        page.locator("#view-only-button").click()
        count = len(sent)
        canvas.click(position={"x": 150, "y": 150})
        page.keyboard.press("a")
        assert not any(e.get("type") in ("key", "pointer", "button", "text") for e in sent[count:])
        page.locator("#view-only-button").click()

        # Settings restart the decoder, including repeated settings with same values.
        for preset in ("fast", "fast_hq", "sharp", "smooth"):
            page.locator("#performance-select").select_option(preset)
            page.wait_for_timeout(1000)
            assert page.locator("#connection-label").inner_text() == "연결됨"
        if supported:
            assert "H.264" in page.locator("#transport-label").inner_text()
        # Dispatch both changes before the host can acknowledge either one.
        # The second request must preserve the first request's desired codec.
        page.evaluate("""() => {
          const codec = document.querySelector('#codec-select');
          codec.value = 'image'; codec.dispatchEvent(new Event('change'));
          const preset = document.querySelector('#performance-select');
          preset.value = 'light'; preset.dispatchEvent(new Event('change'));
        }""")
        expect(page.locator("#transport-label")).to_contain_text("JPEG BASELINE")
        expect(page.locator("#performance-select")).to_have_value("light")
        page.wait_for_timeout(400)
        latest_settings = [e for e in received if e.get("type") == "settings"][-1]
        assert latest_settings["codec"] == "image" and latest_settings["fps"] == 10
        for codec in ("image", "jpeg"):
            page.locator("#codec-select").select_option(codec)
            expect(page.locator("#transport-label")).to_contain_text("JPEG BASELINE" if codec == "image" else "JPEG FAST")
            page.wait_for_timeout(1000)
            assert int(page.locator("#fps-metric").inner_text()) > 0

        page.locator("#benchmark-button").click()
        # Both native WebSocket delivery paths must keep JPEG flowing. H.264
        # remains ArrayBuffer-backed independently of this JPEG-only setting.
        for transport in ("arraybuffer", "blob"):
            page.locator("#jpeg-transport-select").select_option(transport)
            page.wait_for_timeout(1200)
            assert int(page.locator("#fps-metric").inner_text()) > 0
            assert page.locator("#connection-label").inner_text() == "연결됨"
        page.locator("#benchmark-run-button").click()
        page.locator("#benchmark-apply-button").wait_for(state="visible")
        expect(page.locator("#benchmark-apply-button")).to_be_enabled(timeout=20000)
        benchmark = {"image_bitmap": page.locator("#bitmap-result").inner_text(), "html_image": page.locator("#image-result").inner_text()}
        page.locator("#benchmark-apply-button").click()
        page.screenshot(path=str(artifacts / "client-benchmark.png"))
        page.locator("#benchmark-close-button").click()

        # Small-screen UI remains usable, then explicitly reconnect and log out.
        page.set_viewport_size({"width": 390, "height": 844})
        page.wait_for_timeout(100)
        assert page.evaluate("document.documentElement.scrollWidth <= window.innerWidth + 1")
        page.screenshot(path=str(artifacts / "mobile.png"))
        page.set_viewport_size({"width": 1440, "height": 1050})
        page.locator("#connection-button").click()
        page.wait_for_timeout(200)
        page.locator("#connection-button").click()
        page.locator("#remote-canvas").wait_for(state="visible", timeout=15000)
        page.locator("#logout-button").click()
        page.locator("#login-view").wait_for(state="visible")
        assert not errors, errors

        # A decoder can advertise AVC and still fail when configured. The ACK
        # ledger must drain so automatic JPEG recovery does not deadlock.
        recovery = browser.new_page()
        recovery.add_init_script("""window.VideoDecoder = class {
          static async isConfigSupported() { return {supported: true}; }
          constructor() { throw new Error('Test decoder unavailable'); }
        };""")
        recovery.goto(origin)
        recovery.locator("#password").fill(password)
        recovery.locator("#login-button").click()
        expect(recovery.locator("#webcodecs-support")).to_have_text("디코딩 실패 · JPEG로 전환", timeout=15000)
        expect(recovery.locator("#transport-label")).to_contain_text("JPEG FAST", timeout=15000)
        recovery.locator("#remote-canvas").wait_for(state="visible")
        recovery.wait_for_timeout(1200)
        assert int(recovery.locator("#fps-metric").inner_text()) > 0
        recovery.locator("#logout-button").click()
        recovery.close()
        result = {"browser": browser.version, "initial_transport": initial_transport, "webcodecs_supported": supported, "jpeg_decoder_benchmark": benchmark, "errors": errors, "checks": "auth, H264 decode, JPEG variants, JPEG Blob/ArrayBuffer delivery, settings, rapid-settings merge, input, view-only, benchmark, responsive, reconnect, logout, decoder-failure recovery"}
        (artifacts / "browser-results.json").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps(result, ensure_ascii=False, indent=2))
        browser.close()


if __name__ == "__main__":
    main()
