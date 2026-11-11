#!/usr/bin/env python3
"""Compare real WebSocket and HTTP/3 WebTransport canvas sessions.

Start an isolated --demo server with HTTP/3 enabled, then run:
  RWD_PASSWORD=... uv run --with playwright --with psutil python \
    scripts/transport_benchmark.py --server http://127.0.0.1:8081 \
    --server-pid PID --output artifacts/transport-comparison.json

This records metadata only and enables view-only before login. Each case uses a
fresh Chromium process. Case order reverses every round to reduce order bias.
The supplied server is left running. No desktop pointer or keyboard input is sent.
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import shlex
import time
from urllib.parse import urlencode, urlsplit, urlunsplit, parse_qsl

import psutil
from playwright.sync_api import expect, sync_playwright

from resource_benchmark import PRESETS, ProcessSampler, chromium_process, distribution


INSTRUMENTATION = r"""(() => {
  const pending = {h264: [], jpeg: []};
  let pingTimer;
  const state = window.transportBenchmark = {
    hello: null, settings: null, errors: [], inputCommands: [], events: [],
    totalDraws: {h264: 0, jpeg: 0}, measuring: false,
    connectAt: null, openAt: null, firstFrameAt: null, firstFrameCodec: null, transport: null,
    reset() {
      this.started = performance.now(); this.measuring = true;
      this.bytes = 0; this.messages = 0; this.draws = {h264: 0, jpeg: 0};
      this.drawMs = []; this.receiveToDrawMs = []; this.pingRttMs = []; this.stats = [];
      clearInterval(pingTimer);
      pingTimer = setInterval(() => window.rwdBenchmark.ping(), 100);
      window.rwdBenchmark.ping();
    },
    stop() {
      this.measuring = false; clearInterval(pingTimer);
      const canvas = document.querySelector('#remote-canvas');
      return {elapsed_ms: performance.now() - this.started,
        transport: this.transport, negotiated_transport: window.rwdBenchmark?.transport,
        connection_open_ms: this.openAt - this.connectAt,
        first_frame_ms: this.firstFrameAt - this.connectAt,
        first_frame_codec: this.firstFrameCodec,
        binary_bytes: this.bytes, binary_messages: this.messages,
        draws: this.draws, draw_ms: this.drawMs,
        receive_to_draw_ms: this.receiveToDrawMs, ping_rtt_ms: this.pingRttMs,
        stats: this.stats, settings: this.settings, hello: this.hello,
        errors: this.errors, input_commands: this.inputCommands, events: this.events,
        frame_size: {width: canvas.width, height: canvas.height}};
    }
  };
  window.addEventListener('rwd-transport', ({detail}) => {
    const now = performance.now();
    if (detail.event === 'connect') {
      state.connectAt ??= now;
      state.events.push({event: 'connect', transport: detail.transport, at_ms: now});
    } else if (detail.event === 'open') {
      state.transport = detail.transport; state.openAt = now;
      state.events.push({event: 'open', transport: detail.transport, at_ms: now});
    } else if (detail.event === 'close') {
      state.events.push({event: 'close', transport: detail.transport,
        code: detail.code, reason: detail.reason, at_ms: now});
    } else if (detail.event === 'send' && typeof detail.data === 'string') {
      const message = JSON.parse(detail.data);
      if (['pointer', 'button', 'key', 'text', 'scroll'].includes(message.type))
        state.inputCommands.push(message.type);
    } else if (detail.event === 'message') {
      if (typeof detail.data === 'string') {
        const message = JSON.parse(detail.data);
        if (message.type === 'hello') {
          state.hello = message; state.settings = message.settings;
        } else if (message.type === 'settings') {
          state.settings = message; pending.h264.length = 0; pending.jpeg.length = 0;
        } else if (message.type === 'stats' && state.measuring) {
          state.stats.push({...message, elapsed_ms: now - state.started});
        } else if (message.type === 'pong' && state.measuring && message.sent >= state.started) {
          state.pingRttMs.push(now - message.sent);
        } else if (message.type === 'error') state.errors.push(message);
        return;
      }
      const bytes = detail.data?.byteLength ?? detail.data?.size ?? 0;
      if (state.measuring) { state.bytes += bytes; state.messages++; }
      const codec = state.settings?.codec === 'h264' ? 'h264' : 'jpeg';
      pending[codec].push(now);
      if (pending[codec].length > 8) pending[codec].shift();
    }
  });
  const draw = CanvasRenderingContext2D.prototype.drawImage;
  CanvasRenderingContext2D.prototype.drawImage = function(...args) {
    if (this.canvas.id !== 'remote-canvas') return Reflect.apply(draw, this, args);
    const start = performance.now();
    const result = Reflect.apply(draw, this, args);
    const end = performance.now();
    const codec = typeof VideoFrame !== 'undefined' && args[0] instanceof VideoFrame ? 'h264' : 'jpeg';
    if (state.firstFrameAt === null) { state.firstFrameAt = end; state.firstFrameCodec = codec; }
    state.totalDraws[codec]++;
    const received = pending[codec].shift();
    if (state.measuring) {
      state.draws[codec]++; state.drawMs.push(end - start);
      if (received !== undefined && received >= state.started)
        state.receiveToDrawMs.push(end - received);
    }
    return result;
  };
})();"""


def case_url(server, transport):
    parts = urlsplit(server)
    query = dict(parse_qsl(parts.query))
    query.update(transport=transport, benchmark="1", jpegTransport="arraybuffer")
    return urlunsplit(parts._replace(path=parts.path or "/", query=urlencode(query)))


def measure(page, server, client, seconds):
    server_sampler, client_sampler = ProcessSampler(server), ProcessSampler(client, subtree=True)
    page.evaluate("window.transportBenchmark.reset()")
    deadline, samples = time.monotonic() + seconds, []
    while time.monotonic() < deadline:
        page.wait_for_timeout(max(1, min(1, deadline - time.monotonic()) * 1000))
        samples.append({"server": server_sampler.sample(), "client": client_sampler.sample()})
    raw = page.evaluate("window.transportBenchmark.stop()")
    elapsed = raw["elapsed_ms"] / 1000
    assert not raw["input_commands"], "benchmark must not send desktop input"
    assert not raw["errors"], raw["errors"]
    assert raw["ping_rtt_ms"], "no application ping round trips recorded"
    assert raw["stats"], "no server streaming statistics received"
    return {
        "elapsed_seconds": elapsed,
        "connection_open_ms": raw["connection_open_ms"], "first_frame_ms": raw["first_frame_ms"],
        "rendered_fps": sum(raw["draws"].values()) / elapsed,
        "received_binary_payload_mbps": raw["binary_bytes"] * 8 / elapsed / 1_000_000,
        "ping_rtt_ms": distribution(raw["ping_rtt_ms"]),
        "receive_to_draw_ms": distribution(raw["receive_to_draw_ms"]),
        "draw_submission_ms": distribution(raw["draw_ms"]),
        "capture_ms": distribution(item.get("capture_ms") for item in raw["stats"]),
        "encode_ms": distribution(item.get("encode_ms") for item in raw["stats"]),
        "capture_backends": sorted({item["capture_backend"] for item in raw["stats"] if "capture_backend" in item}),
        "server": server_sampler.summary([sample["server"] for sample in samples]),
        "client": client_sampler.summary([sample["client"] for sample in samples]),
        "raw": {"browser": raw, "process_samples": samples,
                "server_initial": server_sampler.initial, "client_initial": client_sampler.initial}}


def run_case(playwright, args, server, transport, codec, preset):
    previous_pids = {process.pid for process in psutil.Process().children(recursive=True)}
    browser = playwright.chromium.launch(channel=os.environ.get("PLAYWRIGHT_CHANNEL", "chrome"), headless=True)
    client = chromium_process(previous_pids)
    page = browser.new_page(viewport={"width": 1440, "height": 1050}, device_scale_factor=1)
    errors = []
    page.on("pageerror", lambda error: errors.append(str(error)))
    page.add_init_script(INSTRUMENTATION)
    try:
        page.goto(case_url(args.server, transport))
        page.locator("#login-view").wait_for(state="visible")
        expect(page.locator("#login-button")).to_be_enabled()
        # DOM activation creates no pointer movement over the remote canvas.
        # View-only is enabled before login, before the server sends any frame.
        page.locator("#view-only-button").evaluate("button => button.click()")
        expect(page.locator("#view-only-button")).to_have_attribute("aria-pressed", "true")
        # Keep startup deterministic: otherwise the asynchronous WebCodecs probe
        # can switch to H264 before the first JPEG is drawn in only some cases.
        if args.startup_codec == "jpeg":
            page.locator("#codec-select").evaluate("select => { select.value = 'jpeg'; select.dispatchEvent(new Event('change')); }")
        page.locator("#password").fill(os.environ["RWD_PASSWORD"])
        page.locator("#login-button").evaluate("button => button.click()")
        page.wait_for_function("() => window.transportBenchmark.hello !== null", timeout=20000)
        hello = page.evaluate("window.transportBenchmark.hello")
        assert hello["mode"] == args.mode, f"expected {args.mode}, got {hello['mode']}"
        if args.startup_codec == "jpeg":
            assert hello["settings"]["codec"] == "jpeg", "start the measured server with --codec jpeg for identical startup frames"
        assert hello.get("transport") == transport, "server negotiated a different transport"
        assert page.evaluate("window.transportBenchmark.transport") == transport, "requested transport fell back"
        assert page.evaluate("window.rwdBenchmark.transport") == transport, "actual adapter differs from requested transport"
        page.wait_for_function("() => window.transportBenchmark.firstFrameAt !== null", timeout=15000)
        if args.startup_codec == "jpeg":
            assert page.evaluate("window.transportBenchmark.firstFrameCodec") == "jpeg", "startup codec changed"
        if codec == "h264":
            expect(page.locator('#codec-select option[value="h264"]')).to_be_enabled(timeout=15000)
        # Change only local session controls; never dispatch remote input events.
        page.locator("#codec-select").evaluate("(select, value) => { select.value = value; select.dispatchEvent(new Event('change')); }", codec)
        page.locator("#performance-select").evaluate("(select, value) => { select.value = value; select.dispatchEvent(new Event('change')); }", preset)
        fps, quality, max_width = PRESETS[preset]
        expected = {"codec": codec, "fps": fps, "quality": quality, "max_width": max_width}
        page.wait_for_function("expected => Object.entries(expected).every(([key,value]) => window.transportBenchmark.settings?.[key] === value)", arg=expected, timeout=15000)
        before = page.evaluate("codec => window.transportBenchmark.totalDraws[codec]", codec)
        page.wait_for_function("arg => window.transportBenchmark.totalDraws[arg.codec] >= arg.before + 3", arg={"codec": codec, "before": before}, timeout=15000)
        page.wait_for_timeout(args.warmup * 1000)
        result = measure(page, server, client, args.seconds)
        actual = result["raw"]["browser"]
        assert all(actual["settings"][key] == value for key, value in expected.items()), "stream settings changed"
        assert actual["draws"][codec] > 0 and sum(actual["draws"].values()) == actual["draws"][codec], "codec fallback or no frames"
        assert actual["transport"] == actual["negotiated_transport"] == transport, "transport changed"
        assert [event["transport"] for event in actual["events"] if event["event"] == "open"] == [transport], "reconnection or fallback occurred"
        assert not errors, errors
        return {"transport": transport, "codec": codec, "preset": preset, "requested": expected,
                "browser_version": browser.version, "client_pid": client.pid, **result}
    finally:
        try:
            page.evaluate("async () => { await fetch('/api/logout', {method:'POST'}); }")
        except Exception:
            pass
        browser.close()


METRICS = {
    "rendered_fps": lambda case: case["rendered_fps"],
    "first_frame_ms": lambda case: case["first_frame_ms"],
    "ping_rtt_median_ms": lambda case: case["ping_rtt_ms"]["median"],
    "ping_rtt_p95_ms": lambda case: case["ping_rtt_ms"]["p95"],
    "receive_to_draw_p95_ms": lambda case: case["receive_to_draw_ms"]["p95"],
    "server_cpu_percent": lambda case: case["server"]["cpu_percent_one_core"],
    "client_cpu_percent": lambda case: case["client"]["cpu_percent_one_core"],
    "server_rss_mib": lambda case: case["server"]["rss_mib"]["median"],
    "client_rss_mib": lambda case: case["client"]["rss_mib"]["median"],
    "payload_mbps": lambda case: case["received_binary_payload_mbps"],
}


def summarize(cases):
    groups = {}
    for case in cases:
        key = (case["codec"], case["preset"], case["transport"])
        groups.setdefault(key, []).append(case)
    result = []
    for (codec, preset, transport), group in groups.items():
        result.append({"codec": codec, "preset": preset, "transport": transport,
                       "rounds": len(group),
                       **{name: distribution(getter(case) for case in group) for name, getter in METRICS.items()}})
    return result


def compare(cases, startup_codec="jpeg"):
    # A lower compressed payload rate can reflect the synthetic clock or fewer
    # rendered frames; it is not evidence that a transport is more efficient.
    comparison_metrics = {name: getter for name, getter in METRICS.items() if name != "payload_mbps"}
    if startup_codec != "jpeg":
        comparison_metrics.pop("first_frame_ms")
    paired = {}
    for case in cases:
        paired.setdefault((case["codec"], case["preset"], case["round"]), {})[case["transport"]] = case
    groups = {}
    for (codec, preset, _), pair in paired.items():
        if set(pair) != {"websocket", "http3"}:
            continue
        group = groups.setdefault((codec, preset), {name: [] for name in comparison_metrics})
        for name, getter in comparison_metrics.items():
            ws, h3 = getter(pair["websocket"]), getter(pair["http3"])
            if ws is not None and h3 is not None and ws > 0:
                # Positive means HTTP/3 improved relative to current WebSocket.
                sign = 1 if name == "rendered_fps" else -1
                group[name].append(sign * (h3 - ws) / ws * 100)
    return [{"codec": codec, "preset": preset,
             "http3_improvement_percent": {name: distribution(values) for name, values in group.items()}}
            for (codec, preset), group in groups.items()]


def write_markdown(report, path, json_path):
    conditions = report["conditions"]
    lines = ["# WebSocket / HTTP/3 WebTransport comparison", "",
             f"Recorded: {report['timestamp_utc']} · Completed: {report['completed']}", "",
             f"Server: `{conditions['server']}` · {conditions['mode']} · Chromium {conditions.get('browser_version', 'unknown')}",
             f"Each case: {conditions['warmup_seconds']:g}s warmup + {conditions['sample_seconds']:g}s sample; {conditions['rounds']} alternating rounds, fresh browser per case.", "",
             "| Codec / preset | Transport | Draw FPS | Ping median / P95 ms | First frame ms | Receive → draw P95 ms | Server / client CPU % | Server / client RSS MiB |",
             "|---|---|---:|---:|---:|---:|---:|---:|"]
    for group in report.get("summary", []):
        def value(key):
            number = group[key]["median"]
            return "—" if number is None else f"{number:.2f}"
        lines.append(f"| {group['codec']} / {group['preset']} | {group['transport']} | {value('rendered_fps')} | {value('ping_rtt_median_ms')} / {value('ping_rtt_p95_ms')} | {value('first_frame_ms')} | {value('receive_to_draw_p95_ms')} | {value('server_cpu_percent')} / {value('client_cpu_percent')} | {value('server_rss_mib')} / {value('client_rss_mib')} |")
    lines += ["", "Table values are medians across runs; the JSON preserves every run, per-frame timing and process sample. P95 columns are medians of each run's P95, not a pooled P95.", "",
              "| Codec / preset | HTTP/3 draw FPS gain | Ping P95 reduction | Receive → draw P95 reduction | Server / client CPU reduction |",
              "|---|---:|---:|---:|---:|"]
    for group in report.get("comparisons", []):
        def improvement(key):
            number = group["http3_improvement_percent"][key]["median"]
            return "—" if number is None else f"{number:+.1f}%"
        lines.append(f"| {group['codec']} / {group['preset']} | {improvement('rendered_fps')} | {improvement('ping_rtt_p95_ms')} | {improvement('receive_to_draw_p95_ms')} | {improvement('server_cpu_percent')} / {improvement('client_cpu_percent')} |")
    lines += ["", "Relative changes are the median of paired per-round percentages; positive means HTTP/3 improved, negative means it regressed. These short rounds do not establish statistical significance.", "",
              "- HTTP/3 means a successful WebTransport QUIC session carrying the actual screen/control stream. Forced cases reject WebSocket fallback.",
              "- Ping is application request/response latency while streaming, including browser/server scheduling. Draw FPS measures Canvas2D submission, not monitor presentation. Receive → draw excludes network transit and capture/encoding.",
              ("- First frame starts at the client transport attempt and ends at first canvas draw; it includes protocol setup and JPEG decoder initialization, but excludes page navigation/login. JPEG is pinned before login for the same startup workload; the requested streaming codec is selected after the first frame."
               if conditions["startup_codec"] == "jpeg" else
               "- First-frame timing is descriptive only and is excluded from transport selection and relative comparisons: automatic codec selection produced a mix of JPEG and H264 first frames. Each case records its actual first_frame_codec. The timed steady-state samples use the same requested codec/settings after warmup, so those comparisons remain valid."),
              "- CPU 100% is one core. Client RSS sums Chromium process RSS and can count shared pages more than once. OS screen services and GPU hardware utilization are excluded.",
              "- Payload bytes exclude TCP/UDP/TLS/QUIC framing. Both transports use reliable delivery, identical stream settings and ArrayBuffer JPEG reception. The HTTP/3 path uses TLS; a plain HTTP server's WebSocket path is unencrypted.",
              ("- The demo is a fixed synthetic desktop with a once-per-second clock. It performs no native capture or host input. It is not a motion video or bit-for-bit repeated image sequence."
               if conditions["mode"] == "demo" else
               "- This captures the actual host screen; its visible scene is not controlled by the harness. View-only is enabled before login, no host input is sent, and no screen pixels are saved or sent to Python."),
              "- Loopback results describe this host under this workload only; they do not establish WAN, packet-loss or multi-client superiority. No CDP network throttling is applied because it would not affect both protocols equally.", "",
              f"Raw report: [{json_path.name}]({os.path.relpath(json_path, path.parent)})", "",
              "To reproduce, set `RWD_PASSWORD` in both terminals to the same chosen password. Start the release server with the recorded arguments (adjust ports if occupied):", "",
              "```sh", shlex.join(conditions["server_command"]), "```", "",
              "Then run the harness with that server's PID. Replace `SERVER_PID` below; the original PID is recorded in the JSON. The harness leaves the server running.", "",
              "```sh", shlex.join(["uv", "run", "--with", "playwright", "--with", "psutil", "python",
                   "scripts/transport_benchmark.py", "--server", conditions["server"], "--server-pid", "SERVER_PID",
                   "--mode", conditions["mode"], "--startup-codec", conditions["startup_codec"],
                   "--rounds", str(conditions["rounds"]), "--seconds", str(conditions["sample_seconds"]),
                   "--warmup", str(conditions["warmup_seconds"]), "--codecs", *conditions["codecs"], "--presets", *conditions["presets"],
                   "--output", str(json_path)]), "```", ""]
    if report.get("failure"):
        lines += [f"Failure: `{report['failure']['type']}`: {report['failure']['message']}", ""]
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines))


def run(args):
    server = psutil.Process(args.server_pid)
    report = {"completed": False, "timestamp_utc": datetime.now(timezone.utc).isoformat(),
              "conditions": {"server": args.server, "server_pid": server.pid,
                             "server_executable": server.exe(), "server_command": server.cmdline(),
                             "server_sha256": hashlib.sha256(Path(server.exe()).read_bytes()).hexdigest(),
                             "system": platform.platform(), "machine": platform.machine(),
                             "cpu_physical": psutil.cpu_count(logical=False), "cpu_logical": psutil.cpu_count(),
                             "system_memory_gib": psutil.virtual_memory().total / 1024 ** 3,
                             "mode": args.mode, "rounds": args.rounds,
                             "codecs": args.codecs, "presets": args.presets,
                             "warmup_seconds": args.warmup, "sample_seconds": args.seconds,
                             "headless": True, "viewport": {"width": 1440, "height": 1050},
                             "device_scale_factor": 1, "ping_interval_ms": 100,
                             "view_only_before_login": True, "jpeg_transport": "arraybuffer",
                             "startup_codec": args.startup_codec,
                             "network_emulation": None,
                             "ordering": "rounds alternate websocket,http3 and http3,websocket for each codec/preset"},
              "cases": []}
    output = Path(args.output)
    markdown = output.with_suffix(".md")
    output.parent.mkdir(parents=True, exist_ok=True)

    def save():
        report["summary"] = summarize(report["cases"])
        report["comparisons"] = compare(report["cases"], report["conditions"]["startup_codec"])
        output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
        write_markdown(report, markdown, output)

    save()
    try:
        with sync_playwright() as playwright:
            for index in range(args.rounds):
                order = ("websocket", "http3") if index % 2 == 0 else ("http3", "websocket")
                for codec in args.codecs:
                    for preset in args.presets:
                        for transport in order:
                            print(f"Round {index + 1}/{args.rounds} {codec}/{preset}/{transport}: {args.warmup:g}s warmup + {args.seconds:g}s sample", flush=True)
                            result = run_case(playwright, args, server, transport, codec, preset)
                            report["conditions"]["browser_version"] = result["browser_version"]
                            report["cases"].append({"round": index + 1, **result})
                            save()
                            print(f"  {result['rendered_fps']:.2f} FPS; ping P95 {result['ping_rtt_ms']['p95']:.2f} ms; first frame {result['first_frame_ms']:.2f} ms", flush=True)
            report["completed"] = True
            save()
    except Exception as error:
        report["failure"] = {"type": type(error).__name__, "message": str(error)}
        save()
        raise
    print(f"Saved: {output} and {markdown}", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", default="http://127.0.0.1:8081")
    parser.add_argument("--server-pid", type=int, required=True)
    parser.add_argument("--output", default="artifacts/transport-comparison.json")
    parser.add_argument("--mode", choices=["demo", "desktop"], default="demo")
    parser.add_argument("--startup-codec", choices=["jpeg", "auto"], default="jpeg",
                        help="JPEG pins identical startup frames; auto records real client policy but startup timings may mix codecs")
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--seconds", type=float, default=10)
    parser.add_argument("--warmup", type=float, default=3)
    parser.add_argument("--presets", nargs="+", choices=PRESETS, default=["fast"])
    parser.add_argument("--codecs", nargs="+", choices=["h264", "jpeg"], default=["h264", "jpeg"])
    arguments = parser.parse_args()
    if arguments.rounds < 1 or arguments.rounds > 100:
        parser.error("--rounds must be between 1 and 100")
    if not math.isfinite(arguments.seconds) or arguments.seconds < 2:
        parser.error("--seconds must be finite and at least 2")
    if not math.isfinite(arguments.warmup) or arguments.warmup < 0:
        parser.error("--warmup must be finite and nonnegative")
    if not os.environ.get("RWD_PASSWORD"):
        parser.error("set RWD_PASSWORD to the measured server's password")
    run(arguments)
