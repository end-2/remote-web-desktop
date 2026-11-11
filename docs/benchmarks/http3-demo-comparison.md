# WebSocket / HTTP/3 WebTransport comparison

Recorded: 2026-11-11T14:09:44.118150+00:00 · Completed: True

Server: `http://127.0.0.1:55731` · demo · Chromium 157.0.8103.39
Each case: 3s warmup + 8s sample; 3 alternating rounds, fresh browser per case.

| Codec / preset | Transport | Draw FPS | Ping median / P95 ms | First frame ms | Receive → draw P95 ms | Server / client CPU % | Server / client RSS MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| h264 / fast | websocket | 51.54 | 0.20 / 0.30 | 12.10 | 0.90 | 27.59 / 9.77 | 49.91 / 731.51 |
| h264 / fast | http3 | 53.02 | 0.20 / 0.30 | 13.10 | 0.80 | 27.80 / 8.46 | 49.84 / 734.62 |
| jpeg / fast | websocket | 51.55 | 0.10 / 0.20 | 58.00 | 1.50 | 36.49 / 15.14 | 49.91 / 797.95 |
| jpeg / fast | http3 | 51.93 | 0.10 / 0.30 | 12.90 | 1.40 | 37.28 / 13.02 | 49.88 / 821.16 |

Table values are medians across runs; the JSON preserves every run, per-frame timing and process sample. P95 columns are medians of each run's P95, not a pooled P95.

| Codec / preset | HTTP/3 draw FPS gain | Ping P95 reduction | Receive → draw P95 reduction | Server / client CPU reduction |
|---|---:|---:|---:|---:|
| h264 / fast | +2.9% | -0.0% | +11.1% | -0.8% / +13.4% |
| jpeg / fast | +1.0% | -0.0% | +6.7% | -2.4% / +14.5% |

Relative changes are the median of paired per-round percentages; positive means HTTP/3 improved, negative means it regressed. These short rounds do not establish statistical significance.

- HTTP/3 means a successful WebTransport QUIC session carrying the actual screen/control stream. Forced cases reject WebSocket fallback.
- Ping is application request/response latency while streaming, including browser/server scheduling. Draw FPS measures Canvas2D submission, not monitor presentation. Receive → draw excludes network transit and capture/encoding.
- First-frame timing is descriptive only and is excluded from transport selection and relative comparisons: automatic codec selection produced a mix of JPEG and H264 first frames. Each case records its actual first_frame_codec. The timed steady-state samples use the same requested codec/settings after warmup, so those comparisons remain valid.
- CPU 100% is one core. Client RSS sums Chromium process RSS and can count shared pages more than once. OS screen services and GPU hardware utilization are excluded.
- Payload bytes exclude TCP/UDP/TLS/QUIC framing. Both transports use reliable delivery, identical stream settings and ArrayBuffer JPEG reception. The HTTP/3 path uses TLS; a plain HTTP server's WebSocket path is unencrypted.
- The demo is a fixed synthetic desktop with a once-per-second clock. It performs no native capture or host input. It is not a motion video or bit-for-bit repeated image sequence.
- Loopback results describe this host under this workload only; they do not establish WAN, packet-loss or multi-client superiority. No CDP network throttling is applied because it would not affect both protocols equally.

Raw report: [http3-demo-comparison.json](http3-demo-comparison.json)

To reproduce, set `RWD_PASSWORD` in both terminals to the same chosen password. Start the release server with the recorded arguments (adjust ports if occupied):

```sh
/Users/sejong/workspace/remote-web-desktop/target/release/remote-web-desktop --host 127.0.0.1 --port 55731 --http3-port 49523 --demo
```

Then run the harness with that server's PID. Replace `SERVER_PID` below; the original PID is recorded in the JSON. The harness leaves the server running.

```sh
uv run --with playwright --with psutil python scripts/transport_benchmark.py --server http://127.0.0.1:55731 --server-pid SERVER_PID --mode demo --startup-codec auto --rounds 3 --seconds 8.0 --warmup 3 --codecs h264 jpeg --presets fast --output docs/benchmarks/http3-demo-comparison.json
```
