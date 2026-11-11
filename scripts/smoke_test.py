#!/usr/bin/env python3
"""Black-box HTTP/WebSocket checks against an already running --demo server.

Requires websockets and RWD_PASSWORD in the environment.
Run: uv run --with websockets python scripts/smoke_test.py http://127.0.0.1:8080
"""

import argparse
import http.client
import json
import os
import struct
import sys
import time
from urllib.parse import urlsplit

from websockets.exceptions import ConnectionClosed, InvalidStatus, WebSocketException
from websockets.sync.client import connect


class UpgradeRejected(Exception):
    def __init__(self, status):
        self.status = status
        super().__init__(f"WebSocket upgrade rejected: HTTP {status}")


class WebSocketClosed(Exception):
    def __init__(self, message, code=None):
        self.code = code
        super().__init__(message)


class Client:
    def __init__(self, base_url, origin=None):
        self.url = urlsplit(base_url.rstrip("/"))
        if self.url.scheme not in ("http", "https") or not self.url.hostname:
            raise ValueError("URL must use http:// or https://")
        if self.url.path not in ("", "/") or self.url.query or self.url.fragment:
            raise ValueError("URL must be the server origin, without a path")
        self.origin = origin or f"{self.url.scheme}://{self.url.netloc}"
        self.cookie = None

    def request(self, method, path, data=None, origin=True, cookie=True):
        factory = (
            http.client.HTTPSConnection
            if self.url.scheme == "https"
            else http.client.HTTPConnection
        )
        connection = factory(self.url.hostname, self.url.port, timeout=8)
        headers = {}
        if origin:
            headers["Origin"] = self.origin if origin is True else origin
        if cookie and self.cookie:
            headers["Cookie"] = self.cookie
        body = None
        if data is not None:
            body = json.dumps(data).encode()
            headers["Content-Type"] = "application/json"
        try:
            connection.request(method, path, body, headers)
            response = connection.getresponse()
            payload = response.read()
            try:
                payload = json.loads(payload)
            except (ValueError, UnicodeDecodeError):
                pass
            return response.status, dict(response.getheaders()), payload
        finally:
            connection.close()

    def login(self, password):
        status, headers, body = self.request(
            "POST", "/api/login", {"password": password}
        )
        check(status == 200, f"login returned HTTP {status}: {body}")
        cookie = next(
            (value for key, value in headers.items() if key.lower() == "set-cookie"),
            "",
        )
        check("httponly" in cookie.lower(), "session cookie must be HttpOnly")
        check("samesite=strict" in cookie.lower(), "cookie must be SameSite=Strict")
        if self.url.scheme == "https":
            check("secure" in cookie.lower(), "HTTPS cookie must be Secure")
        self.cookie = cookie.split(";", 1)[0]
        check("=" in self.cookie, "login did not set a session cookie")

    def websocket(self, origin=True, cookie=True):
        return WebSocket(
            self.url,
            self.origin if origin is True else origin,
            self.cookie if cookie else None,
        )


class WebSocket:
    """Thin adapter; websockets owns handshakes, framing, TLS and close handling."""

    def __init__(self, url, origin, cookie):
        scheme = "wss" if url.scheme == "https" else "ws"
        try:
            self.connection = connect(
                f"{scheme}://{url.netloc}/ws",
                origin=origin or None,
                additional_headers={"Cookie": cookie} if cookie else None,
                compression=None,
                proxy=None,
                open_timeout=8,
                close_timeout=2,
                ping_interval=None,
                max_size=32 * 1024 * 1024,
                max_queue=4,
            ).__enter__()
        except InvalidStatus as error:
            raise UpgradeRejected(error.response.status_code) from error

    def send_json(self, message):
        try:
            self.connection.send(json.dumps(message))
        except ConnectionClosed as error:
            raise self.closed(error) from error

    def receive(self, timeout=8):
        try:
            message = self.connection.recv(timeout=timeout)
            return json.loads(message) if isinstance(message, str) else message
        except ConnectionClosed as error:
            raise self.closed(error) from error

    @staticmethod
    def closed(error):
        return WebSocketClosed(str(error), error.rcvd.code if error.rcvd else None)

    def close(self):
        self.connection.close()


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def expect_rejected(client, expected_status, **kwargs):
    try:
        websocket = client.websocket(**kwargs)
    except UpgradeRejected as error:
        check(error.status == expected_status, str(error))
    else:
        websocket.close()
        raise AssertionError(f"expected WebSocket HTTP {expected_status}")


def expect_unauthenticated(client):
    websocket = client.websocket()
    try:
        try:
            while True:
                message = websocket.receive()
                check(
                    isinstance(message, dict) and message.get("type") == "error",
                    "unauthenticated WebSocket received desktop data",
                )
        except WebSocketClosed as error:
            check(error.code == 4401, f"unauthenticated close code was {error.code}")
    finally:
        websocket.close()


def jpeg_size(payload):
    check(payload[:2] == b"\xff\xd8", "frame is not JPEG (missing SOI)")
    check(payload[-2:] == b"\xff\xd9", "frame is not JPEG (missing EOI)")
    offset = 2
    while offset + 4 <= len(payload):
        check(payload[offset] == 0xFF, "invalid JPEG marker")
        while payload[offset] == 0xFF:
            offset += 1
        marker = payload[offset]
        offset += 1
        length = struct.unpack("!H", payload[offset : offset + 2])[0]
        check(length >= 2 and offset + length <= len(payload), "bad JPEG segment")
        if marker in (0xC0, 0xC1, 0xC2):
            height, width = struct.unpack("!HH", payload[offset + 3 : offset + 7])
            check(width > 0 and height > 0, "empty JPEG dimensions")
            return width, height
        if marker == 0xDA:
            break
        offset += length
    raise AssertionError("JPEG has no supported frame header")


def receive_until(websocket, predicate, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        message = websocket.receive(timeout=max(0.01, deadline - time.monotonic()))
        if isinstance(message, dict) and message.get("type") == "error":
            raise AssertionError(f"server error: {message}")
        matched = predicate(message)
        if isinstance(message, bytes):
            websocket.send_json({"type": "ack"})
        if matched:
            return message
    raise AssertionError("expected WebSocket message was not received")


def run(client, password):
    status, _, _ = client.request("GET", "/healthz")
    check(status == 200, f"health endpoint returned HTTP {status}")
    status, _, session = client.request("GET", "/api/session")
    check(status == 200 and session.get("authenticated") is False, "anonymous session")
    expect_unauthenticated(client)
    for origin in (False, "https://untrusted.example"):
        status, _, _ = client.request(
            "POST", "/api/login", {"password": password}, origin=origin
        )
        check(status == 403, f"login accepted invalid Origin: HTTP {status}")
    status, _, _ = client.request(
        "POST", "/api/login", {"password": "intentionally-incorrect-password"}
    )
    check(status == 401, f"incorrect password returned HTTP {status}")
    print("PASS health, anonymous access and invalid login/Origin rejection")

    client.login(password)
    status, _, session = client.request("GET", "/api/session")
    check(status == 200 and session.get("authenticated") is True, "authenticated session")
    expect_rejected(client, 403, origin=False)
    expect_rejected(client, 403, origin="https://untrusted.example")
    status, _, _ = client.request("POST", "/api/logout", {}, origin=False)
    check(status == 403, "logout accepted missing Origin")
    print("PASS session cookie and authenticated Origin enforcement")

    websocket = client.websocket()
    try:
        hello = websocket.receive()
        check(isinstance(hello, dict) and hello.get("type") == "hello", "missing hello")
        check(
            hello.get("mode") == "demo",
            "smoke test requires a --demo server; native input checks were not sent",
        )
        monitors = hello.get("monitors", [])
        check(bool(monitors), "hello has no monitors")
        monitor = monitors[0]["id"]
        settings = {
            "type": "settings", "monitor": monitor, "fps": 15,
            "quality": 65, "max_width": 640, "codec": "jpeg",
        }

        # A slow consumer must not receive an unbounded backlog of desktop frames.
        initial_frames = 0
        initial_size = None
        while True:
            try:
                message = websocket.receive(timeout=2 if not initial_frames else 0.4)
            except TimeoutError:
                break
            if isinstance(message, bytes):
                check(message[:4] != b"RWH1", "start demo server with default --codec jpeg")
                initial_size = jpeg_size(message)
                initial_frames += 1
                check(initial_frames <= 2, "more than two unacknowledged frames sent")
            elif message.get("type") == "error":
                raise AssertionError(f"initial stream error: {message}")
        check(initial_frames > 0, "no JPEG frame received")
        for _ in range(initial_frames):
            websocket.send_json({"type": "ack"})
        receive_until(websocket, lambda message: isinstance(message, bytes))
        print(f"PASS JPEG {initial_size[0]}x{initial_size[1]}, bounded pipeline and ACK")

        second = client.websocket()
        try:
            try:
                while True:
                    message = second.receive()
                    if isinstance(message, dict):
                        check(message.get("type") == "error", "second controller was accepted")
            except WebSocketClosed as error:
                check(error.code == 4409, f"busy connection close code was {error.code}")
        finally:
            second.close()
        print("PASS single-controller exclusion")

        websocket.send_json(settings)
        resized = receive_until(
            websocket,
            lambda message: isinstance(message, bytes)
            and message[:4] != b"RWH1" and jpeg_size(message)[0] == 640,
        )
        check(jpeg_size(resized)[1] > 0, "invalid resized JPEG")
        for event in (
            {"type": "pointer", "x": 0.5, "y": 0.5},
            {"type": "button", "button": "left", "down": True, "x": 0.5, "y": 0.5},
            {"type": "button", "button": "left", "down": False, "x": 0.5, "y": 0.5},
            {"type": "scroll", "dx": 1, "dy": -1},
            {"type": "key", "code": "KeyA", "key": "a", "down": True},
            {"type": "key", "code": "KeyA", "key": "a", "down": False},
            {"type": "text", "text": "원격 제어 smoke test"},
            {"type": "release"},
            {"type": "ping", "sent": 1234.5},
        ):
            websocket.send_json(event)
        pong = receive_until(
            websocket,
            lambda message: isinstance(message, dict) and message.get("type") == "pong",
        )
        check(pong.get("sent") == 1234.5, "ping timestamp was not echoed")
        print("PASS resolution settings, demo input and ping/pong")

        websocket.send_json({**settings, "codec": "h264"})
        decoder = receive_until(
            websocket,
            lambda message: isinstance(message, dict) and message.get("type") == "decoder",
        )
        check(decoder.get("codec", "").startswith("avc1."), "missing AVC decoder config")
        h264 = receive_until(
            websocket, lambda message: isinstance(message, bytes) and message[:4] == b"RWH1"
        )
        check(len(h264) > 9 and h264[4] == 1, "H264 stream did not start with a keyframe")
        check(h264[5:9] == b"\x00\x00\x00\x01" or h264[5:8] == b"\x00\x00\x01", "H264 is not Annex B")
        websocket.send_json({**settings, "codec": "image"})
        receive_until(
            websocket, lambda message: isinstance(message, bytes) and message[:2] == b"\xff\xd8"
        )
        print("PASS H264 negotiation, keyframe envelope and JPEG baseline fallback")

        old_cookie = client.cookie
        status, _, _ = client.request("POST", "/api/logout", {})
        check(status == 200, f"logout returned HTTP {status}")
        status, _, session = client.request("GET", "/api/session")
        check(status == 200 and session.get("authenticated") is False, "logout did not revoke cookie")
        deadline = time.monotonic() + 8
        try:
            while time.monotonic() < deadline:
                message = websocket.receive(timeout=max(0.01, deadline - time.monotonic()))
                if isinstance(message, bytes):
                    websocket.send_json({"type": "ack"})
            raise AssertionError("logout did not close the active WebSocket")
        except WebSocketClosed:
            pass
        client.cookie = old_cookie
        expect_unauthenticated(client)
        print("PASS logout revokes HTTP session and active WebSocket")
    finally:
        websocket.close()

    client.login(password)
    reconnect = client.websocket()
    try:
        hello = reconnect.receive()
        check(isinstance(hello, dict) and hello.get("type") == "hello", "controller was not released")
        receive_until(reconnect, lambda message: isinstance(message, bytes))
    finally:
        reconnect.close()
        client.request("POST", "/api/logout", {})
    print("PASS controller release and reconnect")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("url", nargs="?", default="http://127.0.0.1:8080")
    parser.add_argument("--origin", help="Origin override for local reverse-proxy checks")
    args = parser.parse_args()
    password = os.environ.get("RWD_PASSWORD")
    if not password:
        parser.error("set RWD_PASSWORD to the running demo server's password")
    try:
        run(Client(args.url, args.origin), password)
    except (AssertionError, OSError, ValueError, UpgradeRejected, WebSocketClosed, WebSocketException) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    print("All smoke checks passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
