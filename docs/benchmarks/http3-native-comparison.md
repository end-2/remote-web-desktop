# WebSocket / HTTP/3 WebTransport comparison

Recorded: 2026-11-11T14:14:34.099813+00:00 · Completed: True

Server: `http://127.0.0.1:8080` · desktop · Chromium 157.0.8103.39
Each case: 3s warmup + 8s sample; 3 alternating rounds, fresh browser per case.

| Codec / preset | Transport | Draw FPS | Ping median / P95 ms | First frame ms | Receive → draw P95 ms | Server / client CPU % | Server / client RSS MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| h264 / fast | websocket | 54.54 | 0.30 / 0.60 | 60.90 | 2.50 | 44.45 / 28.85 | 79.03 / 748.59 |
| h264 / fast | http3 | 54.78 | 0.40 / 0.80 | 70.10 | 2.30 | 45.52 / 26.76 | 67.42 / 754.88 |

Table values are medians across runs; the JSON preserves every run, per-frame timing and process sample. P95 columns are medians of each run's P95, not a pooled P95.

| Codec / preset | HTTP/3 draw FPS gain | Ping P95 reduction | Receive → draw P95 reduction | Server / client CPU reduction |
|---|---:|---:|---:|---:|
| h264 / fast | +0.4% | -33.3% | +4.2% | -2.4% / +10.0% |

Relative changes are the median of paired per-round percentages; positive means HTTP/3 improved, negative means it regressed. These short rounds do not establish statistical significance.

- HTTP/3 means a successful WebTransport QUIC session carrying the actual screen/control stream. Forced cases reject WebSocket fallback.
- Ping is application request/response latency while streaming, including browser/server scheduling. Draw FPS measures Canvas2D submission, not monitor presentation. Receive → draw excludes network transit and capture/encoding.
- First frame starts at the client transport attempt and ends at first canvas draw; it includes protocol setup and JPEG decoder initialization, but excludes page navigation/login. JPEG is pinned before login for the same startup workload; the requested streaming codec is selected after the first frame.
- CPU 100% is one core. Client RSS sums Chromium process RSS and can count shared pages more than once. OS screen services and GPU hardware utilization are excluded.
- Payload bytes exclude TCP/UDP/TLS/QUIC framing. Both transports use reliable delivery, identical stream settings and ArrayBuffer JPEG reception. The HTTP/3 path uses TLS; a plain HTTP server's WebSocket path is unencrypted.
- This captures the actual host screen; its visible scene is not controlled by the harness. View-only is enabled before login, no host input is sent, and no screen pixels are saved or sent to Python.
- Loopback results describe this host under this workload only; they do not establish WAN, packet-loss or multi-client superiority. No CDP network throttling is applied because it would not affect both protocols equally.

Raw report: [http3-native-comparison.json](http3-native-comparison.json)

To reproduce, set `RWD_PASSWORD` in both terminals to the same chosen password. Start the release server with the recorded arguments (adjust ports if occupied):

```sh
'/Users/sejong/workspace/remote-web-desktop/target/Remote Web Desktop.app/Contents/MacOS/remote-web-desktop' --host 127.0.0.1 --port 8080 --http3-port 8081
```

Then run the harness with that server's PID. Replace `SERVER_PID` below; the original PID is recorded in the JSON. The harness leaves the server running.

```sh
uv run --with playwright --with psutil python scripts/transport_benchmark.py --server http://127.0.0.1:8080 --server-pid SERVER_PID --mode desktop --startup-codec jpeg --rounds 3 --seconds 8.0 --warmup 3 --codecs h264 --presets fast --output docs/benchmarks/http3-native-comparison.json
```
