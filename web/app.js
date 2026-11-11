"use strict";

(() => {
  const $ = (id) => document.getElementById(id);
  const canvas = $("remote-canvas");
  const viewport = $("viewport");
  const clientOptions = new URLSearchParams(location.search);
  const requestedTransport = clientOptions.get("transport");
  const transportPreference = ["http3", "auto", "websocket"].includes(requestedTransport) ? requestedTransport : "auto";
  let http3Available = false;
  const transportBenchmark = clientOptions.get("benchmark") === "1";
  // Native browser paths remain selectable for measurements on each client.
  // The low-latency canvas hint is optional; unsupported browsers ignore it.
  const context = canvas.getContext("2d", { alpha: false, desynchronized: clientOptions.get("canvas") !== "sync" });
  const presets = {
    balanced: { fps: 15, quality: 70, max_width: 1600 },
    smooth: { fps: 30, quality: 65, max_width: 1600 },
    fast: { fps: 60, quality: 45, max_width: 1280 },
    fast_hq: { fps: 60, quality: 65, max_width: 1600 },
    sharp: { fps: 15, quality: 90, max_width: 2560 },
    light: { fps: 10, quality: 45, max_width: 1280 },
  };
  let socket = null;
  let generation = 0;
  let connected = false;
  let intentionalClose = false;
  let hasFrame = false;
  let viewOnly = false;
  let fit = true;
  let isMacHost = false;
  let settings = { monitor: 1, codec: "jpeg", ...presets.balanced };
  let pendingDesiredSettings = null;
  let displayedMonitor = null;
  const settingFields = ["monitor", "codec", "fps", "quality", "max_width"];
  let monitors = [];
  let pendingPointer = null;
  let pointerTimer = null;
  let pingTimer = null;
  let connectionTimer = null;
  let toastTimer = null;
  let intervalFrames = 0;
  let intervalBytes = 0;
  let intervalRenderMs = 0;
  let metricsAt = performance.now();
  let lastServerError = "";
  let codecPreference = "auto";
  let jpegRenderer = typeof createImageBitmap === "function" ? "bitmap" : "image";
  let jpegTransport = clientOptions.get("jpegTransport") === "arraybuffer" ? "arraybuffer" : "blob";
  let latestJpeg = null;
  let jpegQueue = Promise.resolve();
  let decoder = null;
  let decoderCodec = "";
  let decoderConfiguring = false;
  let decoderGeneration = 0;
  let decoderReady = Promise.resolve();
  let discardH264 = false;
  let videoTimestamp = 0;
  let h264Supported = false;
  let benchmarkRunning = false;
  let benchmarkWinner = null;
  const h264Frames = new Map();
  const heldKeys = new Set();
  const heldButtons = new Set();
  const activePointers = new Set();

  function transportEvent(event, transport, extra = {}) {
    if (!transportBenchmark) return;
    window.dispatchEvent(new CustomEvent("rwd-transport", { detail: { event, transport, ...extra } }));
  }

  if (transportBenchmark) {
    Object.defineProperty(window, "rwdBenchmark", { value: Object.freeze({
      ping: () => send({ type: "ping", sent: performance.now() }),
      get transport() { return document.documentElement.dataset.transport || null; },
    }) });
  }

  // Keep codecs, frame ACKs and input handling identical for both transports.
  // WebTransport streams are byte streams, so each message has an explicit
  // kind + big-endian length. A single ordered stream preserves codec changes.
  class Http3Socket {
    constructor() {
      this.readyState = WebSocket.CONNECTING;
      this.binaryType = "arraybuffer";
      this.bufferedAmount = 0;
      this.abort = new AbortController();
      this.writeQueue = Promise.resolve();
      this.timer = setTimeout(() => this.fail("HTTP/3 연결 시간이 초과되었습니다."), 4000);
      queueMicrotask(() => this.start().catch(() => this.fail("HTTP/3 연결을 사용할 수 없습니다.")));
    }

    async start() {
      if (this.readyState !== WebSocket.CONNECTING) return;
      if (!window.isSecureContext || typeof WebTransport !== "function") {
        throw new Error("WebTransport unavailable");
      }
      const response = await fetch("/api/transport", {
        method: "POST", credentials: "same-origin", cache: "no-store",
        headers: { "Content-Type": "application/json" }, body: "{}", signal: this.abort.signal,
      });
      if (this.readyState !== WebSocket.CONNECTING) return;
      if (response.status === 401) { this.finish(4401, "Authentication expired"); return; }
      if (!response.ok) throw new Error("Transport bootstrap failed");
      const bootstrap = await response.json();
      if (this.readyState !== WebSocket.CONNECTING) return;
      if (bootstrap.mode !== "webtransport" || bootstrap.protocol !== "rwd-stream-v1") throw new Error("HTTP/3 disabled");
      const url = new URL(bootstrap.url);
      const hashes = bootstrap.certificate_hashes || [bootstrap.certificate_hash];
      if (url.protocol !== "https:" || url.hostname !== location.hostname || url.username || url.password || url.hash
        || !Array.isArray(hashes) || !hashes.length || hashes.length > 2
        || hashes.some((hash) => !Array.isArray(hash) || hash.length !== 32 || hash.some((byte) => !Number.isInteger(byte) || byte < 0 || byte > 255))) {
        throw new Error("Invalid transport bootstrap");
      }
      const transport = new WebTransport(url.href, {
        serverCertificateHashes: hashes.map((hash) => ({ algorithm: "sha-256", value: new Uint8Array(hash) })),
      });
      this.transport = transport;
      // Observe both promises immediately: a rejected CONNECT can close while
      // ready is still pending in some browser implementations.
      transport.closed.then(
        () => { if (!this.reader) this.finish(this.requestedClose?.code || 1006, this.requestedClose?.reason || "Connection closed"); },
        // Once reading starts, drain the stream's already received close frame
        // before reporting a connection error; it can carry auth/busy status.
        () => { if (!this.reader) this.fail("HTTP/3 연결이 종료되었습니다."); },
      );
      await transport.ready;
      if (this.readyState !== WebSocket.CONNECTING) { transport.close(); return; }
      const stream = await transport.createBidirectionalStream();
      if (this.readyState !== WebSocket.CONNECTING) { transport.close(); return; }
      this.writer = stream.writable.getWriter();
      this.reader = stream.readable.getReader();
      this.readyState = WebSocket.OPEN;
      this.onopen?.({});
      // QUIC does not expose an outgoing stream to the server until data is
      // written. This ping lets the server accept it and send the first hello.
      this.send(JSON.stringify({ type: "ping", sent: performance.now() }));
      this.readMessages().catch(() => this.fail("HTTP/3 응답을 읽지 못했습니다."));
    }

    packet(kind, bytes) {
      const packet = new Uint8Array(5 + bytes.length);
      packet[0] = kind;
      new DataView(packet.buffer).setUint32(1, bytes.length);
      packet.set(bytes, 5);
      return packet;
    }

    send(data) {
      if (this.readyState !== WebSocket.OPEN) throw new Error("Transport is not open");
      const bytes = new TextEncoder().encode(data);
      if (bytes.length > 16384 || this.bufferedAmount + bytes.length + 5 > 262144) {
        this.fail("호스트가 입력을 처리하지 못했습니다.");
        throw new Error("Transport send limit exceeded");
      }
      this.write(this.packet(0, bytes));
    }

    write(packet) {
      this.bufferedAmount += packet.length;
      this.writeQueue = this.writeQueue.then(() => this.writer.write(packet)).finally(() => { this.bufferedAmount -= packet.length; });
      this.writeQueue.catch(() => this.fail("HTTP/3 전송에 실패했습니다."));
    }

    async readMessages() {
      const header = new Uint8Array(5);
      const textDecoder = new TextDecoder("utf-8", { fatal: true });
      let headerOffset = 0;
      let payload = null;
      let payloadOffset = 0;
      while (this.readyState < WebSocket.CLOSED) {
        const { value, done } = await this.reader.read();
        if (done) {
          this.finish(this.requestedClose?.code || 1006, this.requestedClose?.reason || "Stream closed");
          return;
        }
        let offset = 0;
        while (offset < value.length && this.readyState < WebSocket.CLOSED) {
          if (headerOffset < 5) {
            const count = Math.min(5 - headerOffset, value.length - offset);
            header.set(value.subarray(offset, offset + count), headerOffset);
            headerOffset += count;
            offset += count;
            if (headerOffset < 5) continue;
            const length = new DataView(header.buffer).getUint32(1);
            if (header[0] > 2 || length > 16 * 1024 * 1024 || (header[0] !== 1 && length > 16384)) throw new Error("Invalid frame header");
            payload = new Uint8Array(length);
          }
          const count = Math.min(payload.length - payloadOffset, value.length - offset);
          payload.set(value.subarray(offset, offset + count), payloadOffset);
          payloadOffset += count;
          offset += count;
          if (payloadOffset !== payload.length) continue;
          const kind = header[0];
          if (kind === 2) {
            const close = JSON.parse(textDecoder.decode(payload));
            this.finish(Number.isInteger(close.code) ? close.code : 1006, typeof close.reason === "string" ? close.reason : "");
            return;
          }
          const data = kind === 0 ? textDecoder.decode(payload)
            : this.binaryType === "blob" ? new Blob([payload]) : payload.buffer;
          if (kind === 0) clearTimeout(this.timer);
          this.onmessage?.({ data });
          headerOffset = 0;
          payloadOffset = 0;
          payload = null;
        }
      }
    }

    fail(reason) {
      if (this.readyState >= WebSocket.CLOSING) return;
      this.onerror?.({ message: reason });
      this.finish(1006, reason);
    }

    close(code = 1000, reason = "") {
      if (this.readyState >= WebSocket.CLOSING) return;
      this.readyState = WebSocket.CLOSING;
      this.requestedClose = { code, reason };
      this.abort.abort();
      clearTimeout(this.timer);
      if (!this.writer) { this.finish(code, reason); return; }
      // Flush already queued release/input messages before closing. Bound this
      // wait so a dead UDP path cannot retain the session indefinitely.
      this.timer = setTimeout(() => this.finish(code, reason), 1000);
      this.write(this.packet(2, new TextEncoder().encode(JSON.stringify({ code, reason }))));
      // The peer's close frame can contain a more specific auth/busy code.
      // Half-close our stream, then allow the reader to drain that response.
      this.writeQueue.then(() => this.writer.close()).catch(() => this.finish(code, reason));
    }

    finish(code, reason) {
      if (this.readyState === WebSocket.CLOSED) return;
      this.readyState = WebSocket.CLOSED;
      clearTimeout(this.timer);
      this.abort.abort();
      this.reader?.cancel().catch(() => {});
      this.writer?.abort().catch(() => {});
      this.transport?.close();
      this.onclose?.({ code, reason });
    }
  }

  function toast(message) {
    $("toast").textContent = message;
    $("toast").hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { $("toast").hidden = true; }, 5000);
  }

  function send(message) {
    if (socket?.readyState !== WebSocket.OPEN) return false;
    try { socket.send(JSON.stringify(message)); return true; }
    catch { return false; }
  }

  function releaseInputs() {
    if (heldKeys.size || heldButtons.size || pendingPointer) send({ type: "release" });
    heldKeys.clear();
    heldButtons.clear();
    pendingPointer = null;
    if (pointerTimer !== null) cancelAnimationFrame(pointerTimer);
    pointerTimer = null;
    const pointers = [...activePointers];
    activePointers.clear();
    for (const id of pointers) {
      if (canvas.hasPointerCapture(id)) canvas.releasePointerCapture(id);
    }
  }

  function clearConnectionTimers() {
    clearTimeout(connectionTimer);
    clearInterval(pingTimer);
    connectionTimer = null;
    pingTimer = null;
  }

  function canControl() {
    const desired = pendingDesiredSettings || settings;
    return connected && hasFrame && !viewOnly && !benchmarkRunning
      && displayedMonitor === settings.monitor && desired.monitor === settings.monitor
      && socket?.readyState === WebSocket.OPEN;
  }

  function canvasHasInput() {
    return canMovePointer() && document.activeElement === canvas;
  }

  function canMovePointer() {
    // Follow the host pointer on hover without taking keyboard focus. The local
    // cursor is hidden over the image, including before the first click.
    return canControl() && document.hasFocus() && !document.hidden
      && $("session-panel").hidden && !$("text-dialog").open && !$("benchmark-dialog").open;
  }

  function setSessionPanel(open, focusTarget = null) {
    if (open) releaseInputs();
    const panel = $("session-panel");
    const toggle = $("session-toggle");
    const focusWasInside = panel.contains(document.activeElement);
    panel.hidden = !open;
    toggle.setAttribute("aria-expanded", String(open));
    toggle.setAttribute("aria-label", open ? "설정 닫기" : "설정 열기");
    toggle.title = `${open ? "설정 닫기" : "설정 열기"} · Ctrl + Alt + Shift`;
    if (!open) $("shortcut-menu").open = false;
    if (open) $("session-close").focus({ preventScroll: true });
    else if (focusTarget || focusWasInside) (focusTarget || toggle).focus({ preventScroll: true });
    updateInputUI();
  }

  function updateInputUI() {
    const enabled = canControl();
    $("text-button").disabled = !enabled;
    document.querySelectorAll("[data-shortcut]").forEach((button) => { button.disabled = !enabled; });
    $("view-only-badge").hidden = !viewOnly || !hasFrame;
    viewport.classList.toggle("control-disabled", !enabled);
    $("view-only-button").setAttribute("aria-pressed", String(viewOnly));
    $("benchmark-run-button").disabled = !connected || !hasFrame || benchmarkRunning;
  }

  function setState(state, label) {
    $("connection-state").dataset.state = state;
    $("session-toggle").dataset.state = state;
    $("connection-label").textContent = label;
    $("connection-button").textContent = state === "online" || state === "connecting" ? "연결 끊기" : "다시 연결";
    $("session-label").textContent = state === "online" ? "인증된 세션" : label;
    $("monitor-select").disabled = !connected;
    $("performance-select").disabled = !connected;
    $("codec-select").disabled = !connected;
    updateInputUI();
  }

  function showPlaceholder(title, description, reconnect = false) {
    $("screen-placeholder").hidden = false;
    $("placeholder-title").textContent = title;
    $("placeholder-description").textContent = description;
    $("reconnect-button").hidden = !reconnect;
    canvas.hidden = true;
    $("view-only-badge").hidden = true;
    viewport.classList.remove("native-size");
  }

  function resizeCanvas() {
    if (!hasFrame || canvas.hidden) return;
    const width = Math.max(1, viewport.clientWidth);
    const height = Math.max(1, viewport.clientHeight);
    const ratio = fit ? Math.min(width / canvas.width, height / canvas.height) : 1;
    canvas.style.width = `${Math.max(1, Math.floor(canvas.width * ratio))}px`;
    canvas.style.height = `${Math.max(1, Math.floor(canvas.height * ratio))}px`;
    viewport.classList.toggle("native-size", !fit);
  }

  async function api(path, options = {}) {
    const abort = new AbortController();
    const timeout = setTimeout(() => abort.abort(), 12000);
    try {
      const response = await fetch(path, { credentials: "same-origin", cache: "no-store", ...options, signal: abort.signal });
      const data = await response.json().catch(() => ({}));
      if (!response.ok) {
        const message = typeof data.detail === "string" ? data.detail : `요청을 처리하지 못했습니다 (${response.status}).`;
        throw new Error(message);
      }
      return data;
    } catch (error) {
      if (error.name === "AbortError") throw new Error("서버 응답 시간이 초과되었습니다. 호스트 연결을 확인하세요.");
      if (error instanceof TypeError) throw new Error("서버에 접근할 수 없습니다. 네트워크 연결을 확인하세요.");
      throw error;
    } finally { clearTimeout(timeout); }
  }

  function loginError(message) {
    $("login-error").textContent = message;
    $("login-error").hidden = !message;
  }

  function showLogin(message = "") {
    disconnect(false);
    setSessionPanel(false);
    $("desktop-view").hidden = true;
    $("login-view").hidden = false;
    if ($("text-dialog").open) $("text-dialog").close();
    if ($("benchmark-dialog").open) $("benchmark-dialog").close();
    loginError(message);
    $("password").value = "";
    $("password").focus();
  }

  function enterDesktop() {
    $("login-view").hidden = true;
    $("desktop-view").hidden = false;
    $("password").value = "";
    setSessionPanel(false);
    connect();
    $("session-toggle").focus({ preventScroll: true });
  }

  function resetMetrics() {
    intervalFrames = 0;
    intervalBytes = 0;
    metricsAt = performance.now();
    intervalRenderMs = 0;
    for (const id of ["fps-metric", "bandwidth-metric", "latency-metric", "render-metric"]) $(id).textContent = "—";
  }

  function disconnect(showMessage = true) {
    releaseInputs();
    intentionalClose = true;
    connected = false;
    pendingDesiredSettings = null;
    generation += 1;
    resetDecoder();
    latestJpeg = null;
    jpegQueue = Promise.resolve();
    clearConnectionTimers();
    const oldSocket = socket;
    socket = null;
    delete document.documentElement.dataset.transport;
    if (oldSocket && oldSocket.readyState < WebSocket.CLOSING) oldSocket.close(1000, "Client disconnected");
    resetMetrics();
    setState("offline", "연결 종료");
    if (showMessage) showPlaceholder("연결이 종료되었습니다", "호스트가 실행 중이면 언제든 다시 연결할 수 있습니다.", true);
  }

  function applySettings(next) {
    for (const key of settingFields) {
      if (next[key] !== undefined) settings[key] = next[key];
    }
    updateBinaryType();
    if (pendingDesiredSettings && settingFields.every((key) => pendingDesiredSettings[key] === settings[key])) pendingDesiredSettings = null;
    // Keep the latest user choices visible while earlier replies are in flight.
    const desired = pendingDesiredSettings || settings;
    $("monitor-select").value = String(desired.monitor);
    const preset = Object.entries(presets).find(([, value]) => value.fps === desired.fps && value.quality === desired.quality && value.max_width === desired.max_width);
    $("performance-select").value = preset ? preset[0] : "custom";
    const monitor = monitors.find((item) => item.id === settings.monitor);
    $("monitor-name").textContent = monitor ? `${monitor.name} / ${monitor.width} × ${monitor.height}` : `DISPLAY ${settings.monitor}`;
    updateTransportLabel();
    updateInputUI();
  }

  function requestSettings(next) {
    if (!connected) return;
    releaseInputs();
    const desired = { ...(pendingDesiredSettings || settings), ...next };
    if (send({ type: "settings", ...desired })) pendingDesiredSettings = desired;
    updateInputUI();
  }

  function handleHello(message) {
    connected = true;
    clearTimeout(connectionTimer);
    $("hostname").textContent = message.hostname || "원격 데스크톱";
    $("host-platform").textContent = message.platform || "Desktop";
    isMacHost = /darwin|macos|mac os/i.test(message.platform || "");
    $("demo-badge").hidden = message.mode !== "demo";
    $("transport-label").textContent = message.mode === "demo" ? "DEMO / SYNTHETIC DISPLAY" : "JPEG / WEBSOCKET";
    const modifier = isMacHost ? "⌘" : "Ctrl";
    $("copy-label").textContent = `${modifier} C`;
    $("paste-label").textContent = `${modifier} V`;
    $("select-all-label").textContent = `${modifier} A`;
    monitors = Array.isArray(message.monitors) ? message.monitors : [];
    $("monitor-select").replaceChildren(...monitors.map((monitor) => {
      const option = document.createElement("option");
      option.value = String(monitor.id);
      option.textContent = `${monitor.name} · ${monitor.width} × ${monitor.height}`;
      return option;
    }));
    applySettings(message.settings || {});
    setState("online", "연결됨");
    const ping = () => send({ type: "ping", sent: performance.now() });
    ping();
    clearInterval(pingTimer);
    pingTimer = setInterval(ping, 5000);
    const sessionGeneration = generation;
    webcodecsProbe.then(() => { if (generation === sessionGeneration && connected) chooseCodec(); });
  }

  function updateTransportLabel() {
    const demo = $("demo-badge").hidden ? "" : "DEMO / ";
    const codec = settings.codec === "h264" ? "H.264 / WEBCODECS" : `${settings.codec === "image" ? "JPEG BASELINE" : "JPEG FAST"} / ${jpegRenderer === "bitmap" ? "BITMAP" : "IMAGE"}`;
    const transport = document.documentElement.dataset.transport === "http3" ? "HTTP/3" : "WEBSOCKET";
    $("transport-label").textContent = `${demo}${codec} / ${transport}`;
  }

  function updateBinaryType() {
    // Settings are acknowledged only after the server drains old frames. H.264
    // keeps its ArrayBuffer envelope; JPEG can stay a native Blob from receipt
    // through decode, avoiding a new Blob copied from every JS ArrayBuffer.
    if (socket) socket.binaryType = settings.codec === "h264" ? "arraybuffer" : jpegTransport;
  }

  const decoderOptions = (codec) => ({ codec, optimizeForLatency: true, hardwareAcceleration: "no-preference" });

  const webcodecsProbe = (async () => {
    if (!window.isSecureContext || typeof VideoDecoder === "undefined" || typeof EncodedVideoChunk === "undefined") {
      $("webcodecs-support").textContent = window.isSecureContext ? "미지원 · JPEG 사용" : "HTTPS 또는 localhost 필요";
    } else {
      try {
        const support = await VideoDecoder.isConfigSupported(decoderOptions("avc1.42E02A"));
        h264Supported = support.supported;
        $("webcodecs-support").textContent = h264Supported ? "사용 가능" : "미지원 · JPEG 사용";
      } catch { $("webcodecs-support").textContent = "미지원 · JPEG 사용"; }
    }
    $("codec-select").querySelector('[value="h264"]').disabled = !h264Supported;
    return h264Supported;
  })();

  function chooseCodec() {
    const requestedCodec = (pendingDesiredSettings || settings).codec;
    const desired = codecPreference === "auto" ? (h264Supported ? "h264" : "jpeg") : codecPreference;
    if (desired === "h264" && !h264Supported) {
      toast("이 브라우저에서는 H.264를 사용할 수 없습니다. JPEG로 연결합니다.");
      codecPreference = "jpeg";
      $("codec-select").value = "jpeg";
      if (requestedCodec !== "jpeg") requestSettings({ codec: "jpeg" });
      return;
    }
    if (desired !== requestedCodec) requestSettings({ codec: desired });
  }

  function resetDecoder() {
    decoderGeneration += 1;
    try { if (decoder && decoder.state !== "closed") decoder.close(); } catch { /* Already closed. */ }
    decoder = null;
    decoderCodec = "";
    decoderConfiguring = false;
    decoderReady = Promise.resolve();
    for (const entry of h264Frames.values()) clearTimeout(entry.timeout);
    h264Frames.clear();
  }

  function ackH264(timestamp) {
    const entry = h264Frames.get(timestamp);
    if (!entry) return;
    clearTimeout(entry.timeout);
    h264Frames.delete(timestamp);
    if (entry.generation === generation && socket === entry.socket) send({ type: "ack" });
  }

  function fallbackToJpeg(reason) {
    if (discardH264 || !connected) return;
    discardH264 = true;
    // Drain each received frame exactly once before requesting the server's codec switch.
    // Further old H.264 packets are acknowledged and discarded until settings arrives.
    for (const timestamp of h264Frames.keys()) ackH264(timestamp);
    resetDecoder();
    h264Supported = false;
    $("webcodecs-support").textContent = "디코딩 실패 · JPEG로 전환";
    $("codec-select").querySelector('[value="h264"]').disabled = true;
    if (codecPreference === "h264") { codecPreference = "jpeg"; $("codec-select").value = "jpeg"; }
    toast(`H.264 재생을 사용할 수 없어 JPEG로 전환합니다. ${reason}`);
    requestSettings({ codec: "jpeg" });
  }

  function renderFrame(frame, started) {
    const width = frame.displayWidth || frame.width;
    const height = frame.displayHeight || frame.height;
    if (!width || !height) throw new Error("Empty frame");
    const first = !hasFrame;
    const monitorChanged = displayedMonitor !== settings.monitor;
    const resized = canvas.width !== width || canvas.height !== height;
    if (resized) {
      canvas.width = width;
      canvas.height = height;
      $("screen-size").textContent = `${width} × ${height}`;
    }
    context.drawImage(frame, 0, 0);
    displayedMonitor = settings.monitor;
    intervalRenderMs += performance.now() - started;
    intervalFrames += 1;
    hasFrame = true;
    if (first) {
      canvas.hidden = false;
      $("screen-placeholder").hidden = true;
      $("benchmark-status").textContent = "측정 준비됨";
    }
    if (first || monitorChanged) updateInputUI();
    if (first || resized) resizeCanvas();
  }

  function configureDecoder(codec, ws, sessionGeneration) {
    if (discardH264 || (decoderCodec === codec && (decoderConfiguring || decoder?.state === "configured"))) return;
    const currentDecoderGeneration = ++decoderGeneration;
    decoderCodec = codec;
    decoderConfiguring = true;
    decoderReady = (async () => {
      try {
        await webcodecsProbe;
        if (!h264Supported) throw new Error("WebCodecs 미지원");
        const config = decoderOptions(codec);
        const supported = await VideoDecoder.isConfigSupported(config);
        if (sessionGeneration !== generation || socket !== ws || currentDecoderGeneration !== decoderGeneration || discardH264) return;
        if (!supported.supported) throw new Error("호스트의 H.264 프로필 미지원");
        if (decoder?.state === "configured") decoder.close();
        decoder = new VideoDecoder({
          output: (frame) => {
            try {
              const entry = h264Frames.get(frame.timestamp);
              if (entry && sessionGeneration === generation && socket === ws && currentDecoderGeneration === decoderGeneration && !discardH264) {
                renderFrame(frame, entry.started);
              }
              ackH264(frame.timestamp);
            } catch { fallbackToJpeg("프레임을 표시하지 못했습니다."); }
            finally { frame.close(); }
          },
          error: () => {
            if (sessionGeneration === generation && socket === ws && currentDecoderGeneration === decoderGeneration) fallbackToJpeg("브라우저 디코더 오류");
          },
        });
        decoder.configure(config);
        decoderCodec = codec;
      } catch (error) {
        if (sessionGeneration === generation && socket === ws && currentDecoderGeneration === decoderGeneration) fallbackToJpeg(error.message);
      } finally { if (currentDecoderGeneration === decoderGeneration) decoderConfiguring = false; }
    })();
  }

  async function handleH264(data, ws, sessionGeneration) {
    if (discardH264) { send({ type: "ack" }); return; }
    videoTimestamp += Math.round(1000000 / Math.max(1, settings.fps));
    const timestamp = videoTimestamp;
    const timeout = setTimeout(() => {
      if (sessionGeneration === generation && socket === ws && h264Frames.has(timestamp)) fallbackToJpeg("디코더 응답 시간이 초과되었습니다.");
    }, 8000);
    h264Frames.set(timestamp, { started: performance.now(), generation: sessionGeneration, socket: ws, timeout });
    await decoderReady;
    if (sessionGeneration !== generation || socket !== ws) return;
    if (discardH264) { ackH264(timestamp); return; }
    try {
      if (!decoder || decoder.state !== "configured") throw new Error("H.264 설정 누락");
      decoder.decode(new EncodedVideoChunk({ type: data[4] === 1 ? "key" : "delta", timestamp, data: data.subarray(5) }));
    } catch (error) { fallbackToJpeg(error.message); }
  }

  async function decodeFrame(data, renderer = jpegRenderer) {
    const blob = data instanceof Blob ? data : new Blob([data], { type: "image/jpeg" });
    if (renderer === "bitmap" && typeof createImageBitmap === "function") return createImageBitmap(blob);
    const url = URL.createObjectURL(blob);
    try {
      return await new Promise((resolve, reject) => {
        const image = new Image();
        const timeout = setTimeout(() => { image.src = ""; reject(new Error("Image timeout")); }, 10000);
        image.onload = async () => {
          try {
            // decode() explicitly completes native decoding; onload alone may
            // leave deferred decode work outside the benchmark's timed section.
            if (typeof image.decode === "function") await image.decode();
            clearTimeout(timeout);
            resolve(image);
          } catch (error) { clearTimeout(timeout); reject(error); }
        };
        image.onerror = () => { clearTimeout(timeout); reject(new Error("Invalid image")); };
        image.src = url;
      });
    } finally { URL.revokeObjectURL(url); }
  }

  function connect() {
    disconnect(false);
    intentionalClose = false;
    hasFrame = false;
    displayedMonitor = null;
    discardH264 = false;
    lastServerError = "";
    canvas.width = 1;
    canvas.height = 1;
    $("screen-size").textContent = "— × —";
    showPlaceholder("데스크톱에 연결하는 중", "호스트의 화면을 기다리고 있습니다.");
    setState("connecting", "연결 중");
    const thisGeneration = generation;
    const useHttp3 = transportPreference === "http3" || (transportPreference === "auto"
      && http3Available && window.isSecureContext && typeof WebTransport === "function");
    openConnection(useHttp3 ? "http3" : "websocket", thisGeneration, useHttp3 && transportPreference === "auto");
  }

  function openConnection(transport, thisGeneration, allowFallback = false) {
    if (generation !== thisGeneration || intentionalClose) return;
    const url = new URL("/ws", location.href);
    url.protocol = location.protocol === "https:" ? "wss:" : "ws:";
    let ws;
    try { ws = transport === "http3" ? new Http3Socket() : new WebSocket(url); }
    catch {
      setState("error", "연결 실패");
      showPlaceholder("연결을 시작하지 못했습니다", "브라우저의 연결 설정과 호스트 주소를 확인하세요.", true);
      return;
    }
    socket = ws;
    ws.binaryType = "arraybuffer";
    let receivedHello = false;
    let receivedFatalError = false;
    const isCurrent = () => generation === thisGeneration && socket === ws;
    transportEvent("connect", transport);
    if (transportBenchmark) {
      const originalSend = ws.send.bind(ws);
      ws.send = (data) => {
        originalSend(data);
        transportEvent("send", transport, { data });
      };
    }
    ws.onopen = () => {
      if (!isCurrent()) return;
      document.documentElement.dataset.transport = transport;
      transportEvent("open", transport);
      updateTransportLabel();
    };
    connectionTimer = setTimeout(() => {
      if (!isCurrent() || connected) return;
      lastServerError = "호스트 응답 시간이 초과되었습니다. 서버가 실행 중인지 확인하세요.";
      ws.close();
    }, 12000);
    ws.onmessage = async (event) => {
      if (!isCurrent()) return;
      transportEvent("message", transport, { data: typeof event.data === "string" ? event.data : { byteLength: event.data.byteLength ?? event.data.size ?? 0 } });
      if (typeof event.data === "string") {
        let message;
        try { message = JSON.parse(event.data); }
        catch {
          lastServerError = "호스트에서 올바르지 않은 응답을 받았습니다.";
          ws.close(1002, "Invalid message");
          return;
        }
        if (message.type === "hello") { receivedHello = true; handleHello(message); }
        else if (message.type === "settings") {
          // The server drains outstanding ACKs before applying new capture settings.
          resetDecoder();
          discardH264 = false;
          applySettings(message);
        } else if (message.type === "decoder" && typeof message.codec === "string") {
          configureDecoder(message.codec, ws, thisGeneration);
        } else if (message.type === "stats") {
          const capture = Number(message.capture_ms);
          const encode = Number(message.encode_ms);
          if (Number.isFinite(capture) && Number.isFinite(encode)) {
            $("transport-label").title = `호스트 캡처 ${capture.toFixed(1)} ms · 인코딩 ${encode.toFixed(1)} ms · 전송 중 ${message.in_flight ?? 0} 프레임`;
          }
        } else if (message.type === "pong" && Number.isFinite(message.sent)) {
          $("latency-metric").textContent = String(Math.max(0, Math.round(performance.now() - message.sent)));
        } else if (message.type === "error") {
          const detail = typeof message.message === "string" ? message.message : "호스트에서 오류가 발생했습니다.";
          if (message.recoverable) toast(detail);
          else {
            receivedFatalError = true;
            lastServerError = detail;
            releaseInputs();
            connected = false;
            setState("error", "호스트 오류");
            showPlaceholder("호스트를 확인해 주세요", detail, true);
            ws.close(1000, "Host error");
          }
        }
        return;
      }
      intervalBytes += event.data.byteLength ?? event.data.size ?? 0;
      const bytes = event.data instanceof ArrayBuffer ? new Uint8Array(event.data) : null;
      if (bytes && bytes.length > 5 && bytes[0] === 82 && bytes[1] === 87 && bytes[2] === 72 && bytes[3] === 49) {
        handleH264(bytes, ws, thisGeneration);
        return;
      }
      latestJpeg = event.data;
      // Native image decodes can complete out of order. Serialize the two bounded
      // in-flight JPEG frames to preserve capture order without a growing queue.
      jpegQueue = jpegQueue.then(async () => {
        if (!isCurrent()) return;
        let frame;
        const started = performance.now();
        try {
          frame = await decodeFrame(event.data);
          if (!isCurrent()) return;
          renderFrame(frame, started);
          send({ type: "ack" });
        } catch {
          if (isCurrent()) {
            lastServerError = "화면을 해석하지 못했습니다. 다시 연결해 주세요.";
            ws.close(1003, "Invalid frame");
          }
        } finally { frame?.close?.(); }
      });
    };
    ws.onerror = (event) => {
      // The close event provides the final state; a browser intentionally hides handshake details.
      if (isCurrent() && !lastServerError) lastServerError = transport === "http3" && event.message
        ? event.message : "호스트에 연결할 수 없습니다. 서버 실행 상태와 네트워크를 확인하세요.";
    };
    ws.onclose = async (event) => {
      if (!isCurrent()) return;
      transportEvent("close", transport, { code: event.code, reason: event.reason });
      delete document.documentElement.dataset.transport;
      // Application refusals belong to this session; retry only a transport
      // failure, even if our local close races the server's final close code.
      if (allowFallback && !receivedHello && !receivedFatalError && !intentionalClose && ![4401, 4409, 4500].includes(event.code)) {
        clearConnectionTimers();
        lastServerError = "";
        // Give an accepted but failed HTTP/3 session time to release its host
        // worker before acquiring the same single-controller slot over TCP.
        connectionTimer = setTimeout(() => {
          if (isCurrent()) openConnection("websocket", thisGeneration);
        }, 200);
        return;
      }
      releaseInputs();
      connected = false;
      clearConnectionTimers();
      resetMetrics();
      if (event.code === 4401) { showLogin("인증이 만료되었습니다. 비밀번호를 다시 입력하세요."); return; }
      const busy = event.code === 4409;
      const description = busy ? "다른 브라우저에서 이 데스크톱을 제어하고 있습니다. 해당 연결을 종료한 뒤 다시 연결하세요."
        : lastServerError || (intentionalClose ? "언제든 다시 연결할 수 있습니다." : "호스트 연결이 종료되었습니다. 서버와 네트워크를 확인한 뒤 다시 연결하세요.");
      setState(busy || lastServerError ? "error" : "offline", busy ? "사용 중" : "연결 종료");
      showPlaceholder(busy ? "이미 사용 중인 데스크톱입니다" : "연결이 종료되었습니다", description, true);
      // HTTP handshake failures do not expose their status to WebSocket clients.
      // Check the session once so an expired cookie still returns to login.
      if (event.code === 1006) {
        try {
          const session = await api("/api/session");
          if (isCurrent() && !session.authenticated) showLogin("인증이 만료되었습니다. 비밀번호를 다시 입력하세요.");
        } catch { /* Preserve the actionable connection message. */ }
      }
    };
  }

  function coordinates(event) {
    const rect = canvas.getBoundingClientRect();
    return {
      x: Math.min(1, Math.max(0, (event.clientX - rect.left) / Math.max(1, rect.width))),
      y: Math.min(1, Math.max(0, (event.clientY - rect.top) / Math.max(1, rect.height))),
    };
  }

  function flushPointer() {
    pointerTimer = null;
    if (pendingPointer && canMovePointer()) send({ type: "pointer", ...coordinates(pendingPointer) });
    pendingPointer = null;
  }

  canvas.addEventListener("pointermove", (event) => {
    if (!canMovePointer() || event.isPrimary === false) return;
    // Coalesce before reading layout, so high-frequency mouse events need only
    // one getBoundingClientRect() per submitted pointer update.
    pendingPointer = { clientX: event.clientX, clientY: event.clientY };
    if (pointerTimer === null) pointerTimer = requestAnimationFrame(flushPointer);
  });
  canvas.addEventListener("pointerenter", (event) => {
    if (canMovePointer() && event.isPrimary !== false && event.pointerType !== "touch") {
      send({ type: "pointer", ...coordinates(event) });
    }
  });
  canvas.addEventListener("pointerdown", (event) => {
    if (!$("session-panel").hidden) setSessionPanel(false);
    if (!canControl() || event.isPrimary === false) return;
    const button = ["left", "middle", "right"][event.button];
    if (!button) return;
    event.preventDefault();
    canvas.focus({ preventScroll: true });
    pendingPointer = null;
    canvas.setPointerCapture(event.pointerId);
    activePointers.add(event.pointerId);
    heldButtons.add(button);
    send({ type: "button", button, down: true, ...coordinates(event) });
    updateInputUI();
  });
  canvas.addEventListener("pointerup", (event) => {
    const button = ["left", "middle", "right"][event.button];
    if (button && heldButtons.has(button)) {
      event.preventDefault();
      pendingPointer = null;
      send({ type: "button", button, down: false, ...coordinates(event) });
      heldButtons.delete(button);
    }
    if (event.buttons === 0) {
      activePointers.delete(event.pointerId);
      if (canvas.hasPointerCapture(event.pointerId)) canvas.releasePointerCapture(event.pointerId);
    }
  });
  canvas.addEventListener("pointercancel", releaseInputs);
  canvas.addEventListener("lostpointercapture", (event) => {
    if (activePointers.has(event.pointerId)) releaseInputs();
  });
  canvas.addEventListener("contextmenu", (event) => { if (canControl()) event.preventDefault(); });
  canvas.addEventListener("wheel", (event) => {
    if (!canvasHasInput()) return;
    event.preventDefault();
    const unit = event.deltaMode === WheelEvent.DOM_DELTA_LINE ? 1 : event.deltaMode === WheelEvent.DOM_DELTA_PAGE ? 0.1 : 80;
    const steps = (delta) => delta === 0 ? 0 : Math.sign(delta) * Math.min(20, Math.max(1, Math.round(Math.abs(delta) / unit)));
    pendingPointer = null;
    send({ type: "pointer", ...coordinates(event) });
    send({ type: "scroll", dx: steps(event.deltaX), dy: -steps(event.deltaY) });
  }, { passive: false });
  canvas.addEventListener("keydown", (event) => {
    if (!canvasHasInput()) return;
    if (event.isComposing || event.key === "Process" || event.key === "Dead" || !event.code) {
      toast("한글과 조합 문자는 설정의 ‘문자 입력’으로 보내세요.");
      return;
    }
    event.preventDefault();
    heldKeys.add(event.code);
    send({ type: "key", code: event.code, key: event.key, down: true });
    // macOS does not always dispatch keyup for a key used with Command.
    if (event.metaKey && event.code !== "MetaLeft" && event.code !== "MetaRight") {
      send({ type: "key", code: event.code, key: event.key, down: false });
      heldKeys.delete(event.code);
    }
  });
  canvas.addEventListener("keyup", (event) => {
    if (!canvasHasInput() && !heldKeys.has(event.code)) return;
    event.preventDefault();
    send({ type: "key", code: event.code, key: event.key, down: false });
    heldKeys.delete(event.code);
  });
  canvas.addEventListener("focus", updateInputUI);
  canvas.addEventListener("blur", () => { releaseInputs(); updateInputUI(); });
  window.addEventListener("blur", () => { releaseInputs(); updateInputUI(); });
  window.addEventListener("focus", updateInputUI);
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) releaseInputs();
    updateInputUI();
  });
  window.addEventListener("pagehide", () => disconnect(false));
  window.addEventListener("pageshow", (event) => {
    if (event.persisted && !$("desktop-view").hidden) showPlaceholder("세션이 일시 중지되었습니다", "다시 연결하여 원격 제어를 이어가세요.", true);
  });

  $("session-toggle").addEventListener("click", () => setSessionPanel($("session-panel").hidden));
  $("session-close").addEventListener("click", () => setSessionPanel(false, $("session-toggle")));
  document.addEventListener("keydown", (event) => {
    if ($("desktop-view").hidden || $("text-dialog").open || $("benchmark-dialog").open) return;
    // The nonmodal panel also closes when focus is on the toggle or canvas.
    // Leave Escape in native select menus to the browser.
    if (!$("session-panel").hidden && event.key === "Escape" && event.target.tagName !== "SELECT") {
      event.preventDefault();
      event.stopPropagation();
      setSessionPanel(false, $("session-toggle"));
    } else if (event.ctrlKey && event.altKey && event.shiftKey) {
      event.preventDefault();
      event.stopPropagation();
      if (!event.repeat) setSessionPanel(true);
    }
  }, true);

  $("login-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    if ($("login-button").disabled) return;
    loginError("");
    $("login-button").disabled = true;
    $("login-button").firstElementChild.textContent = "인증하는 중…";
    try {
      await api("/api/login", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ password: $("password").value }) });
      enterDesktop();
    } catch (error) { loginError(error.message); }
    finally { $("login-button").disabled = false; $("login-button").firstElementChild.textContent = "데스크톱 연결"; }
  });
  $("password-toggle").addEventListener("click", () => {
    const show = $("password").type === "password";
    $("password").type = show ? "text" : "password";
    $("password-toggle").textContent = show ? "숨김" : "표시";
    $("password-toggle").setAttribute("aria-pressed", String(show));
    $("password-toggle").setAttribute("aria-label", show ? "비밀번호 숨기기" : "비밀번호 표시");
  });
  $("connection-button").addEventListener("click", () => {
    if (socket && socket.readyState < WebSocket.CLOSING) disconnect();
    else connect();
  });
  $("reconnect-button").addEventListener("click", connect);
  $("monitor-select").addEventListener("change", () => requestSettings({ monitor: Number($("monitor-select").value) }));
  $("performance-select").addEventListener("change", () => {
    const preset = presets[$("performance-select").value];
    if (preset) requestSettings(preset);
  });
  $("codec-select").addEventListener("change", () => {
    codecPreference = $("codec-select").value;
    chooseCodec();
  });
  $("view-only-button").addEventListener("click", () => {
    releaseInputs();
    viewOnly = !viewOnly;
    if ($("text-dialog").open) $("text-dialog").close();
    updateInputUI();
  });
  $("fit-button").addEventListener("click", () => {
    fit = !fit;
    $("fit-button").setAttribute("aria-pressed", String(fit));
    $("fit-label").textContent = fit ? "화면 맞춤" : "원본 크기";
    resizeCanvas();
  });
  $("fullscreen-button").addEventListener("click", async () => {
    try {
      if (document.fullscreenElement) await document.exitFullscreen();
      else if ($("desktop-view").requestFullscreen) await $("desktop-view").requestFullscreen();
      else { toast("이 브라우저에서는 전체 화면을 지원하지 않습니다."); return; }
      setSessionPanel(false, canControl() ? canvas : $("session-toggle"));
    } catch { toast("전체 화면을 시작하지 못했습니다. 브라우저 설정을 확인하세요."); }
  });
  document.addEventListener("fullscreenchange", () => {
    const label = document.fullscreenElement ? "전체 화면 종료" : "전체 화면";
    $("fullscreen-button").setAttribute("aria-label", label);
    $("fullscreen-button").setAttribute("aria-pressed", String(Boolean(document.fullscreenElement)));
    $("fullscreen-label").textContent = label;
    $("fullscreen-button").title = label;
    resizeCanvas();
  });
  $("text-button").addEventListener("click", () => {
    if (!canControl()) return;
    releaseInputs();
    $("text-dialog").showModal();
    $("remote-text").focus();
  });
  $("text-close-button").addEventListener("click", () => $("text-dialog").close());
  $("remote-text").addEventListener("input", () => { $("text-count").textContent = `${$("remote-text").value.length.toLocaleString("ko-KR")} / 2,000`; });
  $("text-form").addEventListener("submit", (event) => {
    event.preventDefault();
    const text = $("remote-text").value;
    if (!text) { $("remote-text").focus(); return; }
    if (!canControl()) { toast("문자를 보내려면 제어 가능한 데스크톱에 연결하세요."); return; }
    $("text-dialog").close();
    setSessionPanel(false, canvas);
    if (send({ type: "text", text })) {
      $("remote-text").value = "";
      $("text-count").textContent = "0 / 2,000";
      toast("전송됨");
    }
  });
  document.querySelectorAll("[data-shortcut]").forEach((button) => {
    button.addEventListener("click", () => {
      if (!canControl()) return;
      $("shortcut-menu").open = false;
      releaseInputs();
      setSessionPanel(false, canvas);
      const modifier = isMacHost ? { code: "MetaLeft", key: "Meta" } : { code: "ControlLeft", key: "Control" };
      const letter = { copy: "c", paste: "v", "select-all": "a" }[button.dataset.shortcut];
      const keys = letter ? [modifier, { code: `Key${letter.toUpperCase()}`, key: letter }] : [{ code: "Escape", key: "Escape" }];
      for (const key of keys) send({ type: "key", ...key, down: true });
      for (const key of keys.reverse()) send({ type: "key", ...key, down: false });
    });
  });
  document.addEventListener("pointerdown", (event) => {
    if (!$("shortcut-menu").contains(event.target)) $("shortcut-menu").open = false;
  });
  function selectRenderer(renderer) {
    jpegRenderer = renderer === "bitmap" && typeof createImageBitmap === "function" ? "bitmap" : "image";
    $("renderer-select").value = jpegRenderer;
    updateTransportLabel();
    try { localStorage.setItem("relay.jpegRenderer", jpegRenderer); } catch { /* Private browsing may disable storage. */ }
  }
  try {
    if (localStorage.getItem("relay.jpegRenderer") === "image") jpegRenderer = "image";
  } catch { /* The session works without persistent preferences. */ }
  $("renderer-select").value = jpegRenderer;
  $("renderer-select").querySelector('[value="bitmap"]').disabled = typeof createImageBitmap !== "function";
  $("renderer-select").addEventListener("change", () => selectRenderer($("renderer-select").value));
  $("jpeg-transport-select").value = jpegTransport;
  $("jpeg-transport-select").addEventListener("change", () => {
    jpegTransport = $("jpeg-transport-select").value === "arraybuffer" ? "arraybuffer" : "blob";
    updateBinaryType();
  });
  $("benchmark-button").addEventListener("click", () => {
    releaseInputs();
    $("benchmark-dialog").showModal();
  });
  $("benchmark-close-button").addEventListener("click", () => $("benchmark-dialog").close());
  $("benchmark-apply-button").addEventListener("click", () => {
    if (!benchmarkWinner) return;
    selectRenderer(benchmarkWinner);
    $("benchmark-status").textContent = `${benchmarkWinner === "bitmap" ? "ImageBitmap" : "HTMLImageElement"} 적용됨`;
  });
  $("benchmark-run-button").addEventListener("click", async () => {
    if (!connected || !hasFrame || benchmarkRunning) return;
    releaseInputs();
    benchmarkRunning = true;
    benchmarkWinner = null;
    const sessionGeneration = generation;
    $("benchmark-apply-button").disabled = true;
    $("renderer-select").disabled = true;
    $("benchmark-run-button").textContent = "측정 중…";
    updateInputUI();
    const cancelled = () => sessionGeneration !== generation || !$("benchmark-dialog").open;
    try {
      const sample = settings.codec !== "h264" && latestJpeg
        ? new Blob([latestJpeg], { type: "image/jpeg" })
        : await new Promise((resolve, reject) => canvas.toBlob((blob) => blob ? resolve(blob) : reject(new Error("화면 샘플을 만들 수 없습니다.")), "image/jpeg", 0.8));
      const scratch = typeof OffscreenCanvas === "function" ? new OffscreenCanvas(canvas.width, canvas.height) : document.createElement("canvas");
      scratch.width = canvas.width;
      scratch.height = canvas.height;
      const scratchContext = scratch.getContext("2d", { alpha: false });
      const results = {};
      for (const renderer of ["bitmap", "image"]) {
        const target = $(renderer === "bitmap" ? "bitmap-result" : "image-result");
        if (renderer === "bitmap" && typeof createImageBitmap !== "function") { target.textContent = "미지원"; continue; }
        const measurements = [];
        try {
          for (let index = -3; index < 30; index += 1) {
            if (cancelled()) throw new Error("측정을 중단했습니다.");
            let frame;
            const started = performance.now();
            try {
              frame = await decodeFrame(sample, renderer);
              scratchContext.drawImage(frame, 0, 0);
              if (index >= 0) measurements.push(performance.now() - started);
            } finally { frame?.close?.(); }
            if (index >= 0 && index % 5 === 0) {
              $("benchmark-status").textContent = `${renderer === "bitmap" ? "ImageBitmap" : "HTMLImageElement"} 측정 중 · ${index + 1} / 30`;
              await new Promise((resolve) => setTimeout(resolve, 0));
            }
          }
          measurements.sort((a, b) => a - b);
          const median = (measurements[14] + measurements[15]) / 2;
          results[renderer] = median;
          target.textContent = `${median.toFixed(2)} ms`;
        } catch (error) {
          if (cancelled()) throw error;
          target.textContent = "측정 실패";
        }
      }
      if (cancelled()) throw new Error("측정을 중단했습니다.");
      const ranked = Object.entries(results).sort((a, b) => a[1] - b[1]);
      if (!ranked.length) throw new Error("이 브라우저에서 샘플을 디코딩하지 못했습니다.");
      benchmarkWinner = ranked[0][0];
      const name = benchmarkWinner === "bitmap" ? "ImageBitmap" : "HTMLImageElement";
      $("benchmark-status").textContent = `${name} 권장 · ${scratch.width} × ${scratch.height}`;
      $("benchmark-apply-button").disabled = false;
    } catch (error) { $("benchmark-status").textContent = error.message; }
    finally {
      benchmarkRunning = false;
      $("benchmark-run-button").textContent = "30회 비교 실행";
      $("renderer-select").disabled = false;
      updateInputUI();
    }
  });
  new ResizeObserver(resizeCanvas).observe(viewport);
  window.addEventListener("resize", resizeCanvas);
  setInterval(() => {
    const now = performance.now();
    const elapsed = Math.max(1, now - metricsAt);
    if (connected) {
      $("fps-metric").textContent = String(Math.round(intervalFrames * 1000 / elapsed));
      $("bandwidth-metric").textContent = (intervalBytes * 8 / elapsed / 1000).toFixed(1);
      $("render-metric").textContent = intervalFrames ? (intervalRenderMs / intervalFrames).toFixed(1) : "—";
    }
    intervalFrames = 0;
    intervalBytes = 0;
    intervalRenderMs = 0;
    metricsAt = now;
  }, 1000);

  updateInputUI();
  $("login-button").disabled = true;
  api("/api/session")
    .then((session) => {
      http3Available = session.http3_available === true;
      if (session.authenticated) enterDesktop();
    })
    .catch((error) => loginError(error.message))
    .finally(() => { $("login-button").disabled = false; });
})();
