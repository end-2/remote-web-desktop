#!/usr/bin/env python3
"""HTTP/3 browser integration checks. Run ONLY against --demo --http3-port PORT.

RWD_PASSWORD=... uv run --with playwright python scripts/transport_test.py URL
No real desktop input: the demo hello is required before sending test controls.
"""
import json
import os
from pathlib import Path
import sys

from playwright.sync_api import expect, sync_playwright

from browser_test import close_panel, expand_section, open_panel


OBSERVE = """(() => {
  window.transportEvents = [];
  addEventListener('rwd-transport', event => {
    const detail = {...event.detail};
    if (typeof detail.data === 'string') {
      try { detail.message = JSON.parse(detail.data); } catch {}
    }
    transportEvents.push(detail);
    if (transportEvents.length > 500) transportEvents.shift();
  });
})();"""


def main():
    origin = (sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8080").rstrip("/")
    password = os.environ["RWD_PASSWORD"]
    results = []
    with sync_playwright() as p:
        browser = p.chromium.launch(channel=os.environ.get("PLAYWRIGHT_CHANNEL", "chrome"), headless=True)
        errors = []

        def new_page(mode="http3", init=None):
            context = browser.new_context(viewport={"width": 1280, "height": 800})
            context.add_init_script(OBSERVE)
            if init:
                context.add_init_script(init)
            page = context.new_page()
            page.on("pageerror", lambda error: errors.append(str(error)))
            query = "benchmark=1" if mode == "default" else f"transport={mode}&benchmark=1"
            page.goto(f"{origin}/?{query}")
            return context, page

        def login(page):
            page.locator("#password").fill(password)
            page.locator("#login-button").click()

        def connected(page, protocol):
            page.wait_for_function("expected => document.documentElement.dataset.transport === expected", arg=protocol, timeout=15000)
            page.wait_for_function("() => transportEvents.some(e => e.message?.type === 'hello' && e.message.mode === 'demo')", timeout=15000)
            page.wait_for_function("() => document.querySelector('#remote-canvas').width > 100 && !document.querySelector('#remote-canvas').hidden", timeout=15000)

        def cleanup(context, page):
            if not page.is_closed():
                page.evaluate("fetch('/api/logout', {method:'POST'})")
                page.wait_for_timeout(300)
            context.close()

        context, page = new_page("default")
        assert page.evaluate("async () => (await fetch('/api/transport', {method:'POST'})).status") == 401
        login(page)
        connected(page, "http3")
        assert page.request.post(f"{origin}/api/transport", headers={"Origin": "https://other.example"}).status == 403
        assert page.request.post(f"{origin}/api/transport").status == 403
        expect(page.locator("#session-panel")).to_be_hidden()
        expect(page.locator("#transport-label")).to_contain_text("HTTP/3")
        results.append("Default selects available HTTP/3 and renders with hidden controls")

        # Both codecs and ACK draining use the same shared session engine.
        expand_section(page, "#stream-settings")
        page.locator("#codec-select").select_option("jpeg")
        page.wait_for_function("() => transportEvents.some(e => e.message?.type === 'settings' && e.message.codec === 'jpeg')")
        expect(page.locator("#transport-label")).to_contain_text("JPEG")
        page.locator("#performance-select").select_option("smooth")
        page.wait_for_function("() => transportEvents.some(e => e.message?.type === 'settings' && e.message.fps === 30)")
        close_panel(page)
        page.locator("#remote-canvas").click(position={"x": 100, "y": 100})
        page.keyboard.press("a")
        page.wait_for_function("() => transportEvents.some(e => e.event === 'send' && e.message?.type === 'key')")
        open_panel(page)
        page.locator("#view-only-button").click()
        close_panel(page)
        page.evaluate("transportEvents.length = 0")
        page.locator("#remote-canvas").click(position={"x": 120, "y": 120})
        page.keyboard.press("b")
        assert not page.evaluate("transportEvents.some(e => e.event === 'send' && ['pointer','button','key','text','scroll'].includes(e.message?.type))")
        results.append("HTTP/3 codec/settings/input and view-only restrictions")

        other_context, other_page = new_page()
        login(other_page)
        expect(other_page.locator("#reconnect-button")).to_be_visible(timeout=15000)
        assert other_page.locator("#remote-canvas").is_hidden()
        assert other_page.evaluate("transportEvents.some(e => e.message?.type === 'error')")
        cleanup(other_context, other_page)
        assert page.evaluate("document.documentElement.dataset.transport") == "http3"
        results.append("HTTP/3 enforces the single controller across sessions")

        open_panel(page)
        page.locator("#connection-button").click()
        expect(page.locator("#reconnect-button")).to_be_visible()
        page.wait_for_timeout(300)
        page.evaluate("transportEvents.length = 0")
        page.locator("#reconnect-button").click()
        connected(page, "http3")
        assert page.evaluate("async () => (await fetch('/api/logout', {method:'POST'})).ok")
        page.wait_for_function("() => !document.documentElement.dataset.transport", timeout=15000)
        page.reload()
        expect(page.locator("#login-view")).to_be_visible()
        context.close()
        results.append("HTTP/3 disconnect/reconnect and session revocation")

        context, page = new_page()
        assert page.evaluate("""async password => (await fetch('/api/login', {
          method:'POST', headers:{'Content-Type':'application/json'}, body:JSON.stringify({password})
        })).ok""", password)
        replay = page.evaluate("""async () => {
          const config = await (await fetch('/api/transport', {method:'POST'})).json();
          const options = {serverCertificateHashes: config.certificate_hashes.map(hash =>
            ({algorithm:'sha-256',value:new Uint8Array(hash)}))};
          const wait = async promise => {
            let timer;
            try { return await Promise.race([promise, new Promise((_,reject) => {
              timer = setTimeout(() => reject(new Error('Timed out')), 5000);
            })]); } finally { clearTimeout(timer); }
          };
          const first = new WebTransport(config.url, options);
          first.closed.catch(() => {});
          await wait(first.ready);
          first.close();
          await wait(first.closed).catch(() => {});
          const reused = new WebTransport(config.url, options);
          reused.closed.catch(() => {});
          let rejected = false;
          try { await wait(reused.ready); } catch (error) {
            if (error.message === 'Timed out') throw error;
            rejected = true;
          } finally { reused.close(); }
          return rejected;
        }""")
        assert replay, "a consumed HTTP/3 ticket must not be accepted twice"
        cleanup(context, page)
        results.append("HTTP/3 handshake rejects a replayed one-use ticket")

        context, page = new_page("auto", "Object.defineProperty(window, 'WebTransport', {value:undefined, configurable:true});")
        login(page)
        connected(page, "websocket")
        cleanup(context, page)
        results.append("Unsupported HTTP/3 automatically falls back to WebSocket")

        context, page = new_page("http3", "Object.defineProperty(window, 'WebTransport', {value:undefined, configurable:true});")
        login(page)
        expect(page.locator("#reconnect-button")).to_be_visible(timeout=15000)
        assert not page.evaluate("transportEvents.some(e => e.transport === 'websocket' && e.event === 'open')")
        cleanup(context, page)
        results.append("Forced HTTP/3 cannot silently contaminate benchmark with WebSocket")

        # A network black hole must have a bounded fallback and no unhandled rejection.
        context, page = new_page("auto", """window.WebTransport = class {
          constructor() { this.ready = new Promise(() => {}); this.closed = new Promise(resolve => {this.done = resolve;}); }
          close() { this.done({closeCode:0, reason:'test timeout'}); }
        };""")
        login(page)
        connected(page, "websocket")
        cleanup(context, page)
        results.append("HTTP/3 timeout falls back without leaking a client connection")
        browser.close()
        assert not errors, errors

    report = {"passed": results, "page_errors": errors}
    output = Path("artifacts/transport-tests.json")
    output.parent.mkdir(exist_ok=True)
    output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
