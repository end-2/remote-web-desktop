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


def open_panel(page):
    if page.locator("#session-panel").is_hidden():
        page.locator("#session-toggle").click()
    expect(page.locator("#session-panel")).to_be_visible()
    expect(page.locator("#session-toggle")).to_have_attribute("aria-expanded", "true")
    page.locator("#session-panel").evaluate("async e => { await Promise.all(e.getAnimations().map(a => a.finished)); }")


def close_panel(page):
    if page.locator("#session-panel").is_visible():
        page.locator("#session-close").click()
    expect(page.locator("#session-panel")).to_be_hidden()
    expect(page.locator("#session-toggle")).to_have_attribute("aria-expanded", "false")


def expand_section(page, selector):
    open_panel(page)
    section = page.locator(selector)
    if not section.evaluate("e => e.open"):
        section.locator("summary").click()


def clear_session(page):
    assert page.evaluate("async () => (await fetch('/api/logout', {method: 'POST'})).ok"), "API session cleanup failed"
    page.reload()
    page.locator("#login-view").wait_for(state="visible")


def assert_full_viewport(page):
    assert page.locator("#viewport").evaluate("""e => {
      const r = e.getBoundingClientRect();
      return Math.abs(r.x) <= 1 && Math.abs(r.y) <= 1
        && Math.abs(r.width - innerWidth) <= 1 && Math.abs(r.height - innerHeight) <= 1;
    }"""), "the remote viewport must use the entire browser content area"
    assert page.evaluate("document.documentElement.scrollWidth <= innerWidth + 1")
    assert page.evaluate("document.documentElement.scrollHeight <= innerHeight + 1")


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
        assert any(e.get("type") == "hello" and e.get("mode") == "demo" for e in received), "Only run browser tests against --demo"
        # WebCodecs is a real browser decode path; allow JPEG only if AVC unsupported.
        supported = page.evaluate("async () => typeof VideoDecoder !== 'undefined' && (await VideoDecoder.isConfigSupported({codec:'avc1.42E02A',optimizeForLatency:true})).supported")
        if supported:
            expect(page.locator("#transport-label")).to_contain_text("H.264", timeout=15000)
        page.wait_for_timeout(1500)
        assert int(page.locator("#fps-metric").text_content()) > 0
        canvas = page.locator("#remote-canvas")
        assert canvas.evaluate("c => c.width > 600 && c.height > 300")
        expect(page.locator("#session-panel")).to_be_hidden()
        expect(page.locator("#session-toggle")).to_have_attribute("aria-expanded", "false")
        expect(page.locator("#stream-settings")).to_have_js_property("open", False)
        expect(page.locator("#connection-details")).to_have_js_property("open", False)
        expect(canvas).to_have_css("cursor", "none")
        assert_full_viewport(page)
        page.screenshot(path=str(artifacts / "desktop.png"))
        initial_transport = page.locator("#transport-label").text_content()

        # Hover follows the host cursor before a click, while keyboard focus stays local.
        count = len(sent)
        canvas.hover(position={"x": 80, "y": 80})
        page.wait_for_timeout(100)
        expect(canvas).not_to_be_focused()
        page.keyboard.press("z")
        assert any(e.get("type") == "pointer" for e in sent[count:])
        assert not any(e.get("type") in ("key", "button", "text") for e in sent[count:])

        # Session controls overlay the host without reducing or moving its image.
        initial_box = canvas.bounding_box()
        open_panel(page)
        expect(page.locator("#session-close")).to_be_focused()
        assert canvas.bounding_box() == initial_box
        for selector in ("#monitor-select", "#fit-button", "#fullscreen-button",
                         "#view-only-button", "#text-button", "#shortcut-menu",
                         "#connection-button"):
            expect(page.locator(selector)).to_be_visible()
        for selector in ("#session-panel", "#session-toggle", "#session-close", "#monitor-select"):
            assert page.locator(selector).evaluate("e => getComputedStyle(e).cursor !== 'none'"), selector
        count = len(sent)
        canvas.hover(position={"x": 100, "y": 100})
        page.wait_for_timeout(100)
        assert not any(e.get("type") == "pointer" for e in sent[count:]), "open controls suspend remote pointer movement"
        page.screenshot(path=str(artifacts / "desktop-controls.png"))
        # Reverse tabbing can reach the image behind the nonmodal overlay. Even
        # with canvas focus, an open panel must continue to suspend host input.
        count = len(sent)
        page.keyboard.press("Shift+Tab")
        page.keyboard.press("Shift+Tab")
        expect(canvas).to_be_focused()
        page.keyboard.press("x")
        page.mouse.wheel(0, 120)
        page.wait_for_timeout(100)
        assert not any(e.get("type") in ("key", "pointer", "button", "scroll") for e in sent[count:]), "canvas focus behind controls must not resume host input"
        page.keyboard.press("Tab")
        page.keyboard.press("Tab")
        expect(page.locator("#session-close")).to_be_focused()
        page.keyboard.press("Escape")
        expect(page.locator("#session-panel")).to_be_hidden()
        expect(page.locator("#session-toggle")).to_be_focused()

        # Escape also dismisses the panel when focus has left its own subtree.
        for target, reverse_tabs in (("#session-toggle", 1), ("#remote-canvas", 2)):
            open_panel(page)
            for _ in range(reverse_tabs):
                page.keyboard.press("Shift+Tab")
            expect(page.locator(target)).to_be_focused()
            count = len(sent)
            page.keyboard.press("Escape")
            expect(page.locator("#session-panel")).to_be_hidden()
            expect(page.locator("#session-toggle")).to_be_focused()
            expect(page.locator("#session-toggle")).to_have_attribute("aria-expanded", "false")
            page.wait_for_timeout(100)
            assert not any(e.get("type") == "key" for e in sent[count:]), "dismissing controls must not send Escape to the host"

        # The release chord exits remote input and moves keyboard focus to controls.
        canvas.click(position={"x": 100, "y": 100})
        count = len(sent)
        page.keyboard.down("Control")
        page.keyboard.down("Alt")
        page.keyboard.down("Shift")
        expect(page.locator("#session-panel")).to_be_visible()
        expect(page.locator("#session-close")).to_be_focused()
        page.keyboard.up("Shift")
        page.keyboard.up("Alt")
        page.keyboard.up("Control")
        assert any(e.get("type") == "release" for e in sent[count:])
        count = len(sent)
        page.keyboard.press("b")
        page.keyboard.press("Escape")
        expect(page.locator("#session-panel")).to_be_hidden()
        expect(page.locator("#session-toggle")).to_be_focused()
        assert not any(e.get("type") == "key" for e in sent[count:]), "panel keys must stay local"

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
        count = len(sent)
        open_panel(page)
        page.keyboard.up("Shift")
        assert any(e.get("type") == "release" for e in sent[count:]), "opening controls must release held input"
        page.locator("#text-button").click()
        expect(page.locator("#remote-text")).to_be_focused()
        count = len(sent)
        page.keyboard.press("Escape")
        expect(page.locator("#text-dialog")).to_be_hidden()
        expect(page.locator("#session-panel")).to_be_visible()
        expect(page.locator("#session-toggle")).to_have_attribute("aria-expanded", "true")
        expect(page.locator("#text-button")).to_be_focused()
        page.wait_for_timeout(100)
        assert not any(e.get("type") == "key" for e in sent[count:]), "dialog Escape must stay local and leave session controls open"
        page.locator("#text-button").click()
        page.locator("#remote-text").fill("원격 입력 테스트")
        page.locator("#text-send-button").click()
        page.wait_for_timeout(100)
        assert any(e.get("type") == "text" and e.get("text") == "원격 입력 테스트" for e in sent)
        assert any(e.get("type") == "key" and e.get("code") == "KeyA" for e in sent)
        assert any(e.get("type") == "button" and e.get("down") is False for e in sent)
        assert any(e.get("type") == "scroll" for e in sent)
        assert any(e.get("type") == "release" for e in sent)
        expect(page.locator("#session-panel")).to_be_hidden()
        expect(canvas).to_be_focused()

        open_panel(page)
        page.locator("#shortcut-menu > summary").click()
        count = len(sent)
        page.locator('[data-shortcut="escape"]').click()
        expect(page.locator("#session-panel")).to_be_hidden()
        expect(canvas).to_be_focused()
        assert any(e.get("type") == "key" and e.get("code") == "Escape" for e in sent[count:])

        open_panel(page)
        page.locator("#view-only-button").click()
        count = len(sent)
        canvas.click(position={"x": 150, "y": 150})
        page.keyboard.press("a")
        assert not any(e.get("type") in ("key", "pointer", "button", "text") for e in sent[count:])
        expect(page.locator("#session-panel")).to_be_hidden()
        expect(canvas).to_have_css("cursor", "none")
        open_panel(page)
        page.locator("#view-only-button").click()

        # Settings restart the decoder, including repeated settings with same values.
        expand_section(page, "#stream-settings")
        for preset in ("fast", "fast_hq", "sharp", "smooth"):
            page.locator("#performance-select").select_option(preset)
            page.wait_for_timeout(1000)
            assert page.locator("#connection-label").text_content() == "연결됨"
        if supported:
            assert "H.264" in page.locator("#transport-label").text_content()
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
            assert int(page.locator("#fps-metric").text_content()) > 0

        expand_section(page, "#connection-details")
        page.locator("#benchmark-button").click()
        # Both native WebSocket delivery paths must keep JPEG flowing. H.264
        # remains ArrayBuffer-backed independently of this JPEG-only setting.
        for transport in ("arraybuffer", "blob"):
            page.locator("#jpeg-transport-select").select_option(transport)
            page.wait_for_timeout(1200)
            assert int(page.locator("#fps-metric").text_content()) > 0
            assert page.locator("#connection-label").text_content() == "연결됨"
        page.locator("#benchmark-run-button").click()
        page.locator("#benchmark-apply-button").wait_for(state="visible")
        expect(page.locator("#benchmark-apply-button")).to_be_enabled(timeout=20000)
        benchmark = {"image_bitmap": page.locator("#bitmap-result").inner_text(), "html_image": page.locator("#image-result").inner_text()}
        page.locator("#benchmark-apply-button").click()
        page.screenshot(path=str(artifacts / "client-benchmark.png"))
        page.locator("#benchmark-close-button").click()

        # Fullscreen includes local dialogs and status feedback in its subtree.
        open_panel(page)
        page.locator("#fullscreen-button").click()
        page.wait_for_function("document.fullscreenElement?.id === 'desktop-view'")
        expect(page.locator("#session-panel")).to_be_hidden()
        for selector in ("#text-dialog", "#benchmark-dialog", "#toast"):
            assert page.locator(selector).evaluate("e => document.fullscreenElement.contains(e)"), selector
        open_panel(page)
        page.locator("#text-button").click()
        expect(page.locator("#text-dialog")).to_be_visible()
        expect(page.locator("#remote-text")).to_be_focused()
        page.locator("#remote-text").fill("전체 화면 문자 입력")
        page.locator("#text-send-button").click()
        expect(page.locator("#session-panel")).to_be_hidden()
        assert any(e.get("type") == "text" and e.get("text") == "전체 화면 문자 입력" for e in sent)
        expand_section(page, "#connection-details")
        page.locator("#benchmark-button").click()
        expect(page.locator("#benchmark-dialog")).to_be_visible()
        page.locator("#benchmark-close-button").click()
        open_panel(page)
        page.locator("#fullscreen-button").click()
        page.wait_for_function("document.fullscreenElement === null")

        # Both mobile orientations retain a full host view and reachable controls.
        for name, size in (("mobile", {"width": 390, "height": 844}),
                           ("mobile-landscape", {"width": 844, "height": 390})):
            close_panel(page)
            page.set_viewport_size(size)
            page.wait_for_timeout(150)
            assert_full_viewport(page)
            host_box = canvas.bounding_box()
            assert host_box["width"] <= size["width"] + 1 and host_box["height"] <= size["height"] + 1
            page.screenshot(path=str(artifacts / f"{name}.png"))
            open_panel(page)
            assert_full_viewport(page)
            assert canvas.bounding_box() == host_box
            panel_box = page.locator("#session-panel").bounding_box()
            assert panel_box["x"] >= -1 and panel_box["y"] >= -1
            assert panel_box["x"] + panel_box["width"] <= size["width"] + 1
            assert panel_box["y"] + panel_box["height"] <= size["height"] + 1
            page.locator("#connection-button").scroll_into_view_if_needed()
            expect(page.locator("#connection-button")).to_be_in_viewport()
            page.locator("#session-close").scroll_into_view_if_needed()
            expect(page.locator("#session-close")).to_be_in_viewport()
            page.screenshot(path=str(artifacts / f"{name}-controls.png"))

        # Disconnection restores a visible local pointer for recovery controls.
        page.set_viewport_size({"width": 1440, "height": 1050})
        open_panel(page)
        page.locator("#connection-button").click()
        expect(page.locator("#screen-placeholder")).to_be_visible()
        close_panel(page)
        assert page.locator("#screen-placeholder").evaluate("e => getComputedStyle(e).cursor !== 'none'")
        assert page.locator("#reconnect-button").evaluate("e => getComputedStyle(e).cursor !== 'none'")
        open_panel(page)
        page.locator("#connection-button").click()
        page.locator("#remote-canvas").wait_for(state="visible", timeout=15000)
        clear_session(page)
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
        assert int(recovery.locator("#fps-metric").text_content()) > 0
        clear_session(recovery)
        recovery.close()
        result = {"browser": browser.version, "initial_transport": initial_transport, "webcodecs_supported": supported, "jpeg_decoder_benchmark": benchmark, "errors": errors, "checks": "auth, H264 decode, JPEG variants, JPEG Blob/ArrayBuffer delivery, settings, rapid-settings merge, input, view-only, benchmark, overlay controls, remote-only canvas cursor, hover pointer without keyboard focus, release chord and focus, local Escape, shortcuts, fullscreen dialogs, mobile portrait and landscape, reconnect, API session cleanup, decoder-failure recovery"}
        (artifacts / "browser-results.json").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps(result, ensure_ascii=False, indent=2))
        browser.close()


if __name__ == "__main__":
    main()
