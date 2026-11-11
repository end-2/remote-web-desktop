#!/usr/bin/env python3
"""Measure a native server and its real headless Chromium client without host input.

Keep the intended static/moving scene visible on the host. This script changes
only the remote client's session controls; it does not create or animate the scene.
RWD_PASSWORD=... uv run --with playwright --with psutil python \
  scripts/resource_benchmark.py --label static

Six streaming cases (H264/JPEG at 15/30/60 FPS), each with three seconds of
warmup, plus a short idle sample take about 85 seconds by default.
"""
import argparse
import hashlib
from datetime import datetime, timezone
import json
import math
import os
from pathlib import Path
import platform
import statistics
import time

import psutil
from playwright.sync_api import expect, sync_playwright


PRESETS = {"balanced": (15, 70, 1600), "smooth": (30, 65, 1600),
           "fast_hq": (60, 65, 1600), "fast": (60, 45, 1280)}
MIB = 1024 * 1024

# Binary frames stay in Chromium. Forwarding each frame through CDP/Python would
# add a second large byte transfer and distort the resource measurements.
INSTRUMENTATION = r"""(() => {
  const pending = {h264: [], jpeg: []};
  const state = window.resourceBenchmark = {
    hello: null, settings: null, errors: [], inputCommands: [],
    totalDraws: {h264: 0, jpeg: 0}, measuring: false,
    reset() {
      this.started = performance.now(); this.measuring = true;
      this.bytes = 0; this.messages = 0; this.draws = {h264: 0, jpeg: 0};
      this.drawMs = []; this.receiveToDrawMs = []; this.stats = [];
    },
    stop() {
      this.measuring = false;
      const c = document.querySelector('#remote-canvas');
      return {elapsed_ms: performance.now() - this.started,
        binary_bytes: this.bytes, binary_messages: this.messages,
        draws: this.draws, draw_ms: this.drawMs,
        receive_to_draw_ms: this.receiveToDrawMs, stats: this.stats,
        frame_size: c ? {width: c.width, height: c.height} : null,
        client_path: {canvas_desynchronized: c?.getContext('2d')?.getContextAttributes?.()?.desynchronized ?? null,
          jpeg_transport: document.querySelector('#jpeg-transport-select')?.value ?? null},
        settings: this.settings, errors: this.errors,
        input_commands: this.inputCommands};
    }
  };
  const NativeWebSocket = window.WebSocket;
  window.WebSocket = class extends NativeWebSocket {
    constructor(...args) {
      super(...args);
      this.addEventListener('message', event => {
        if (typeof event.data === 'string') {
          const message = JSON.parse(event.data);
          if (message.type === 'hello') {
            state.hello = message; state.settings = message.settings;
          } else if (message.type === 'settings') {
            state.settings = message; pending.h264 = []; pending.jpeg = [];
          } else if (message.type === 'stats' && state.measuring) {
            state.stats.push({...message, elapsed_ms: performance.now() - state.started});
          } else if (message.type === 'error') state.errors.push(message);
          return;
        }
        const bytes = event.data.byteLength ?? event.data.size ?? 0;
        if (state.measuring) { state.bytes += bytes; state.messages++; }
        // The application uses ArrayBuffer and a five-byte H264 envelope.
        const prefix = event.data instanceof ArrayBuffer
          ? new Uint8Array(event.data, 0, Math.min(bytes, 4)) : new Uint8Array(0);
        const codec = prefix.length === 4 && prefix[0] === 82 && prefix[1] === 87
          && prefix[2] === 72 && prefix[3] === 49 ? 'h264' : 'jpeg';
        pending[codec].push(performance.now());
        if (pending[codec].length > 8) pending[codec].shift();
      });
    }
    send(data) {
      if (typeof data === 'string') {
        const message = JSON.parse(data);
        if (['pointer', 'button', 'key', 'text', 'scroll'].includes(message.type))
          state.inputCommands.push(message.type);
      }
      return super.send(data);
    }
  };
  const draw = CanvasRenderingContext2D.prototype.drawImage;
  CanvasRenderingContext2D.prototype.drawImage = function(...args) {
    if (this.canvas.id !== 'remote-canvas') return Reflect.apply(draw, this, args);
    const started = performance.now();
    const result = Reflect.apply(draw, this, args);
    const ended = performance.now();
    const codec = typeof VideoFrame !== 'undefined' && args[0] instanceof VideoFrame ? 'h264' : 'jpeg';
    state.totalDraws[codec]++;
    const received = pending[codec].shift();
    if (state.measuring) {
      state.draws[codec]++; state.drawMs.push(ended - started);
      if (received !== undefined && received >= state.started)
        state.receiveToDrawMs.push(ended - received);
    }
    return result;
  };
})();"""


def distribution(values):
    """Small JSON-friendly distribution; p95 uses the nearest-rank definition."""
    values = sorted(float(value) for value in values if value is not None and math.isfinite(value))
    if not values:
        return {"samples": 0, "mean": None, "median": None, "p95": None, "max": None}
    return {"samples": len(values), "mean": statistics.fmean(values),
            "median": statistics.median(values),
            "p95": values[math.ceil(len(values) * .95) - 1], "max": values[-1]}


def server_process(pid):
    if pid is not None:
        return psutil.Process(pid)
    expected = str(Path(__file__).resolve().parents[1] / "target" /
                   "Remote Web Desktop.app/Contents/MacOS/remote-web-desktop")
    matches = []
    for process in psutil.process_iter(["pid", "exe"]):
        if process.info["exe"] == expected:
            matches.append(process)
    if len(matches) != 1:
        raise RuntimeError(f"expected one running packaged server, found {len(matches)}; use --server-pid")
    return matches[0]


def chromium_process(previous_pids):
    candidates = []
    for process in psutil.Process().children(recursive=True):
        if process.pid in previous_pids:
            continue
        try:
            command = process.cmdline()
            if "--remote-debugging-pipe" in command and not any(arg.startswith("--type=") for arg in command):
                candidates.append(process)
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            continue
    if len(candidates) != 1:
        raise RuntimeError(f"could not isolate the new Chromium browser process: {len(candidates)} candidates")
    return candidates[0]


class ProcessSampler:
    """CPU-time deltas, not psutil's initially-zero cpu_percent sample."""

    def __init__(self, root, subtree=False):
        self.root, self.subtree = root, subtree
        self.started_epoch = time.time()
        self.started = self.previous_time = time.monotonic()
        self.previous, self.identities, self.exited = {}, {}, set()
        self.cpu_seconds = 0.0
        self.initial = self.sample(baseline=True)

    def sample(self, baseline=False):
        if not self.root.is_running():
            raise RuntimeError(f"measured process {self.root.pid} exited")
        processes = [self.root]
        if self.subtree:
            processes += self.root.children(recursive=True)
        current, rss, delta = {}, 0, 0.0
        for process in processes:
            try:
                with process.oneshot():
                    identity = (process.pid, process.create_time())
                    times = process.cpu_times()
                    cpu = times.user + times.system
                    rss += process.memory_info().rss
                    self.identities[identity] = {"pid": process.pid, "name": process.name()}
                current[identity] = cpu
                if not baseline:
                    if identity in self.previous:
                        delta += max(0, cpu - self.previous[identity])
                    elif identity[1] >= self.started_epoch:
                        delta += cpu
            except psutil.NoSuchProcess:
                continue
        now = time.monotonic()
        self.exited.update(set(self.previous) - set(current))
        self.previous.update(current)
        self.cpu_seconds += delta
        elapsed = now - self.previous_time
        self.previous_time = now
        return {"elapsed_seconds": now - self.started,
                "cpu_percent_one_core": 100 * delta / elapsed if not baseline and elapsed else 0,
                "rss_mib": rss / MIB, "process_count": len(current),
                "cpu_seconds_observed": self.cpu_seconds}

    def summary(self, samples):
        return {"cpu_percent_one_core": 100 * self.cpu_seconds / (self.previous_time - self.started),
                "cpu_interval_percent": distribution(sample["cpu_percent_one_core"] for sample in samples),
                "rss_mib": distribution(sample["rss_mib"] for sample in samples),
                "processes_observed": list(self.identities.values()),
                "exited_processes_with_unobserved_final_cpu": len(self.exited)}


def measure(page, server, client, seconds, warmup_seconds=3):
    page.wait_for_timeout(warmup_seconds * 1000)
    server_sampler = ProcessSampler(server)
    client_sampler = ProcessSampler(client, subtree=True)
    page.evaluate("window.resourceBenchmark.reset()")
    deadline = time.monotonic() + seconds
    samples = []
    while time.monotonic() < deadline:
        page.wait_for_timeout(max(1, min(1, deadline - time.monotonic()) * 1000))
        samples.append({"server": server_sampler.sample(), "client": client_sampler.sample(),
                        "ui_render_ms": page.locator("#render-metric").evaluate(
                            "e => Number.isFinite(parseFloat(e.textContent)) ? parseFloat(e.textContent) : null")})
    measured = page.evaluate("window.resourceBenchmark.stop()")
    elapsed = measured["elapsed_ms"] / 1000
    capture_stats = [sample for sample in measured['stats'] if 'unique_capture_frames' in sample]
    unique_fps = None
    if len(capture_stats) >= 2:
        first, last = capture_stats[0], capture_stats[-1]
        unique_fps = (last['unique_capture_frames'] - first['unique_capture_frames']) * 1000 / (last['elapsed_ms'] - first['elapsed_ms'])
    assert not measured["errors"], measured["errors"]
    assert not measured["input_commands"], "resource benchmark must not send native input"
    return {"elapsed_seconds": elapsed,
            "rendered_fps": sum(measured["draws"].values()) / elapsed,
            "received_binary_payload_mbps": measured["binary_bytes"] * 8 / elapsed / 1_000_000,
            "server": server_sampler.summary([sample["server"] for sample in samples]),
            "client": client_sampler.summary([sample["client"] for sample in samples]),
            "draw_submission_ms": distribution(measured["draw_ms"]),
            "receive_to_draw_ms": distribution(measured["receive_to_draw_ms"]),
            "ui_render_ms": distribution(sample["ui_render_ms"] for sample in samples),
            "capture_ms": distribution(sample.get("capture_ms") for sample in measured["stats"]),
            "encode_ms": distribution(sample.get("encode_ms") for sample in measured["stats"]),
            "capture_backends": sorted({sample["capture_backend"] for sample in measured["stats"] if "capture_backend" in sample}),
            "unique_capture_fps": unique_fps,
            "capture_age_ms": distribution(sample.get('capture_age_ms') for sample in measured['stats']),
            "raw": {"browser": measured, "process_samples": samples,
                    "server_initial": server_sampler.initial, "client_initial": client_sampler.initial}}


def run(args):
    server = server_process(args.server_pid)
    report = {
        "completed": False, "timestamp_utc": datetime.now(timezone.utc).isoformat(),
        "conditions": {"label": args.label, "server": args.server, "server_pid": server.pid,
                       "server_executable": server.exe(), "system": platform.platform(),
                       "server_sha256": hashlib.sha256(Path(server.exe()).read_bytes()).hexdigest(),
                       "server_command": server.cmdline(),
                       "client_query": args.client_query,
                       "machine": platform.machine(), "cpu_logical": psutil.cpu_count(),
                       "cpu_physical": psutil.cpu_count(logical=False),
                       "system_memory_gib": psutil.virtual_memory().total / 1024 ** 3,
                       "warmup_seconds_per_case": 3, "sample_seconds_per_case": args.seconds,
                       "idle_sample_seconds": min(3, args.seconds), "idle_warmup_seconds": 0,
                       "sampling_interval_seconds": 1, "headless": True,
                       "viewport": {"width": 1440, "height": 1050}, "device_scale_factor": 1,
                       "workload_control": "Externally prepared visible host scene; label is descriptive only."},
        "definitions": {
            "cpu_percent_one_core": "100% is one CPU core; multi-threaded totals can exceed 100%.",
            "server": "Only the selected Rust server process. WindowServer, fixture browser, Python and other OS services are excluded.",
            "client": "The newly launched Chromium browser and its descendants, including renderer and GPU-process CPU/RSS. Python and the Playwright Node driver are excluded.",
            "memory": "Resident set size sampled once per second, MiB. Client RSS is summed across processes and may double-count shared pages; it is not unique physical memory or GPU memory.",
            "gpu": "GPU hardware utilization, video-engine utilization, GPU memory and WindowServer resources are not measured.",
            "rendered_fps": "Successful remote-canvas drawImage submissions per browser monotonic second; not physical display presentation FPS.",
            "received_binary_payload_mbps": "WebSocket binary payload bytes including the 5-byte H264 envelope; excludes TCP/TLS/WebSocket framing and text control messages. Not an external-network throughput test.",
            "receive_to_draw_ms": "In-browser binary message arrival through drawImage return, FIFO within each codec (baseline H264 without B-frames and serial JPEG). Includes client decode/queue/draw submission; excludes capture, encode, network transit and GPU completion.",
            "draw_submission_ms": "Only Canvas2D drawImage call duration; GPU completion is asynchronous.",
            "ui_render_ms": "Once-per-second samples of the app's displayed rolling mean: JPEG decode+draw; H264 packet processing through decoded draw. These sample means are not frame-weighted.",
            "capture_ms": "Server's roughly once-per-second capture call timing. XCap: synchronous capture. SCK: latest async sample map/copy only; OS scaling and capture run upstream. Excludes worker queue wait; not every frame.",
            "unique_capture_fps": "Distinct source sequence numbers in transmitted frames per second, using first/last server stats. SCK duplicate latest samples count once; XCap/demo each synchronous capture counts once. Not pixel-difference or physical presentation FPS.",
            "capture_age_ms": "SCK callback arrival through selected-sample CPU copy completion, not total glass-to-glass latency.",
            "encode_ms": "Server's roughly once-per-second resize, color conversion and encoding timing samples; not every frame.",
            "cpu_limitations": "CPU deltas include polling instrumentation overhead in the client. Final CPU between the last sample and a child process exit cannot be recovered and may slightly undercount.",
            "comparison": "One sequential run, no quality-equivalence assertion. Static and motion results require separately prepared scenes. Same-machine server/client compete for resources."},
        "cases": []}
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)

    def save():
        output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")

    with sync_playwright() as playwright:
        previous_pids = {process.pid for process in psutil.Process().children(recursive=True)}
        browser = playwright.chromium.launch(channel=os.environ.get("PLAYWRIGHT_CHANNEL", "chrome"), headless=True)
        client = chromium_process(previous_pids)
        report["conditions"].update({"browser_version": browser.version, "browser_root_pid": client.pid,
                                    "browser_executable": client.exe()})
        page = browser.new_page(viewport=report["conditions"]["viewport"], device_scale_factor=1)
        errors = []
        page.on("pageerror", lambda error: errors.append(str(error)))
        page.add_init_script(INSTRUMENTATION)
        try:
            page.goto(args.server.rstrip('/') + '/' + ('?' + args.client_query if args.client_query else ''))
            page.locator("#login-view").wait_for(state="visible")
            print(f"Measuring idle: sample {min(3, args.seconds):g}s", flush=True)
            report["cases"].append({"name": "idle", "state": "unauthenticated login page; no stream",
                                    **measure(page, server, client, min(3, args.seconds), warmup_seconds=0)})
            save()
            page.locator("#password").fill(os.environ["RWD_PASSWORD"])
            page.locator("#login-button").click()
            page.wait_for_function("() => window.resourceBenchmark.hello !== null", timeout=15000)
            hello = page.evaluate("window.resourceBenchmark.hello")
            assert hello["mode"] == "desktop", "requires the actual native server, not --demo"
            report["conditions"]["hello"] = hello
            page.locator("#remote-canvas").wait_for(state="visible", timeout=15000)
            expect(page.locator('#codec-select option[value="h264"]')).to_be_enabled(timeout=15000)
            # Pointer hover follows the host in control mode, so make the entire
            # measurement session read-only before navigating local settings.
            page.locator("#session-toggle").click()
            page.locator("#view-only-button").click()
            expect(page.locator("#view-only-button")).to_have_attribute("aria-pressed", "true")
            page.locator("#session-close").click()
            report["conditions"]["view_only"] = True
            for codec in args.codecs:
                for preset in args.presets:
                    fps, quality, max_width = PRESETS[preset]
                    page.locator("#session-toggle").click()
                    expect(page.locator("#session-panel")).to_be_visible()
                    if not page.locator("#stream-settings").evaluate("e => e.open"):
                        page.locator("#stream-settings > summary").click()
                    page.locator("#codec-select").select_option(codec)
                    page.locator("#performance-select").select_option(preset)
                    # Measure the default unobstructed session, with controls closed.
                    page.locator("#session-close").click()
                    expect(page.locator("#session-panel")).to_be_hidden()
                    expected = {"codec": codec, "fps": fps, "quality": quality, "max_width": max_width}
                    page.wait_for_function("expected => Object.entries(expected).every(([k,v]) => window.resourceBenchmark.settings?.[k] === v)", arg=expected, timeout=15000)
                    before = page.evaluate("codec => window.resourceBenchmark.totalDraws[codec]", codec)
                    page.wait_for_function("v => window.resourceBenchmark.totalDraws[v.codec] >= v.before + 3", arg={"codec": codec, "before": before}, timeout=15000)
                    print(f"Measuring {codec}/{preset}: warmup 3s + sample {args.seconds:g}s", flush=True)
                    result = measure(page, server, client, args.seconds)
                    actual = result["raw"]["browser"]
                    assert all(actual["settings"][key] == value for key, value in expected.items()), "settings changed during measurement"
                    assert actual["draws"][codec] > 0 and sum(actual["draws"].values()) == actual["draws"][codec], "codec fallback or no rendered frames"
                    assert result["capture_ms"]["samples"] and result["encode_ms"]["samples"], "no server timing samples"
                    if args.expected_capture:
                        assert result["capture_backends"] == [args.expected_capture], result["capture_backends"]
                    assert not errors, errors
                    report["cases"].append({"name": f"{codec}_{preset}", "requested": expected, **result})
                    save()
                    print(f"  {result['rendered_fps']:.1f} FPS, {result['received_binary_payload_mbps']:.2f} Mbps; "
                          f"server {result['server']['cpu_percent_one_core']:.1f}% / {result['server']['rss_mib']['median']:.1f} MiB; "
                          f"client {result['client']['cpu_percent_one_core']:.1f}% / {result['client']['rss_mib']['median']:.1f} MiB", flush=True)
            report["completed"] = True
            save()
            print(f"Saved metadata and samples: {output}", flush=True)
        except Exception as error:
            report["failure"] = {"type": type(error).__name__, "message": str(error)}
            save()
            raise
        finally:
            try:
                page.evaluate("fetch('/api/logout', {method: 'POST'})")
            except Exception:
                pass
            browser.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--seconds", type=float, default=10)
    parser.add_argument("--server", default="http://127.0.0.1:8080")
    parser.add_argument("--server-pid", type=int)
    parser.add_argument("--output", default="artifacts/resource-usage.json")
    parser.add_argument("--label", choices=("static", "motion"), default="static")
    parser.add_argument("--presets", nargs='+', choices=PRESETS, default=list(PRESETS))
    parser.add_argument("--codecs", nargs='+', choices=['h264', 'jpeg'], default=['h264', 'jpeg'])
    parser.add_argument("--client-query", default='')
    parser.add_argument("--expected-capture", choices=['sck', 'xcap'])
    arguments = parser.parse_args()
    if arguments.seconds < 2 or not math.isfinite(arguments.seconds):
        parser.error("--seconds must be finite and at least 2")
    if not os.environ.get("RWD_PASSWORD"):
        parser.error("set RWD_PASSWORD to the running native server's password")
    run(arguments)
