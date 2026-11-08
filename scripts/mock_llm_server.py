#!/usr/bin/env python3
"""
Zero-dependency keyless OpenAI-compatible SSE server (`GET /v1/models`, `POST /v1/chat/completions`)
for deterministic local & CI verification of `thetalk`'s streaming Markdown + optical blur-to-focus.
"""

import argparse
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SAMPLE_MARKDOWN = """\
## Native Wayland Focus on `niri`

`thetalk` stays visible above your workspace **without stealing keystrokes**:

- [x] Maps on `zwlr_layer_shell_v1` (`Layer::Top`, `exclusive_zone = -1`)
- [x] Masks pointer hit-testing to the ==exact animated card bounds==
- [x] Releases `KeyboardInteractivity::None` when unfocused

```rust
fn desired_focus(focused: bool, over_input: bool) -> KeyboardInteractivity {
    if focused || over_input {
        KeyboardInteractivity::OnDemand
    } else {
        KeyboardInteractivity::None
    }
}
```

Every newly streamed word condenses smoothly from **6.6px Gaussian blur** into crisp *Geist* typography."""


def tokenize_chunks(text: str):
    out = []
    cur = []
    for ch in text:
        cur.append(ch)
        if ch in (" ", "\n") and len(cur) >= 4:
            out.append("".join(cur))
            cur = []
    if cur:
        out.append("".join(cur))
    return out


class MockLlmHandler(BaseHTTPRequestHandler):
    delay_ms = 28

    def log_message(self, fmt, *args):
        sys.stderr.write("[mock-llm] " + (fmt % args) + "\n")

    def do_GET(self):
        if self.path.rstrip("/") in ("/v1/models", "/models", "/health"):
            body = json.dumps(
                {
                    "object": "list",
                    "data": [
                        {"id": "mock-quiet-paper-7b", "object": "model"},
                        {"id": "qwen2.5-coder:14b", "object": "model"},
                    ],
                }
            ).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self.send_error(404, "Not Found")

    def do_POST(self):
        if not self.path.rstrip("/").endswith("/chat/completions"):
            self.send_error(404, "Not Found")
            return

        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) if length > 0 else b"{}"
        try:
            payload = json.loads(raw.decode("utf-8"))
        except Exception:
            payload = {}

        stream = payload.get("stream", True)
        if not stream:
            body = json.dumps(
                {
                    "id": "chatcmpl-mock",
                    "object": "chat.completion",
                    "choices": [
                        {
                            "index": 0,
                            "message": {"role": "assistant", "content": SAMPLE_MARKDOWN},
                            "finish_reason": "stop",
                        }
                    ],
                }
            ).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return

        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()

        for piece in tokenize_chunks(SAMPLE_MARKDOWN):
            frame = {
                "id": "chatcmpl-mock",
                "object": "chat.completion.chunk",
                "choices": [{"index": 0, "delta": {"content": piece}, "finish_reason": None}],
            }
            line = f"data: {json.dumps(frame)}\n\n".encode("utf-8")
            try:
                self.wfile.write(line)
                self.wfile.flush()
            except BrokenPipeError:
                return
            time.sleep(self.delay_ms / 1000.0)

        try:
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except BrokenPipeError:
            pass


def main():
    parser = argparse.ArgumentParser(description="Keyless mock SSE LLM server for thetalk")
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=18080)
    parser.add_argument("--delay-ms", type=int, default=28)
    args = parser.parse_args()

    MockLlmHandler.delay_ms = max(1, args.delay_ms)
    server = ThreadingHTTPServer((args.host, args.port), MockLlmHandler)
    print(f"Mock keyless LLM SSE server listening on http://{args.host}:{args.port}/v1", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
