#!/usr/bin/env python3
"""Mock Ollama server for end-to-end testing of `nah`.

Speaks /api/tags and /api/chat (NDJSON streaming). Replies are scripted:
the first chat call asks for a WRITE_FILE + EDIT_FILE action, the second
call finishes. Persists a call log for assertions.
"""
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

REPLY_1 = (
    "یک فایل می‌سازم و بعد ویرایشش می‌کنم.\n"
    "```tool\n"
    "WRITE_FILE\n"
    "path: demo/notes.txt\n"
    "content:\n"
    "first line\n"
    "second line\n"
    "END_OF_CONTENT\n"
    "```\n"
    "```tool\n"
    "EDIT_FILE\n"
    "path: demo/notes.txt\n"
    "<<<<<<< SEARCH\n"
    "second line\n"
    "=======\n"
    "second line (edited)\n"
    ">>>>>>> REPLACE\n"
    "```\n"
)
REPLY_2 = "تمام شد — فایل ساخته و ویرایش شد."

STATE = {"calls": 0}
LOG = open("/tmp/mock_ollama_log.jsonl", "w")


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        if self.path == "/api/tags":
            body = json.dumps({
                "models": [
                    {
                        "name": "qwen2.5-coder:7b",
                        "size": 4900000000,
                        "details": {
                            "parameter_size": "7.6B",
                            "quantization_level": "Q4_K_M",
                        },
                    }
                ]
            }).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        else:
            self.send_error(404)

    def do_POST(self):
        if self.path != "/api/chat":
            self.send_error(404)
            return
        n = int(self.headers.get("Content-Length", 0))
        payload = json.loads(self.rfile.read(n) or b"{}")
        LOG.write(json.dumps({"calls": STATE["calls"], "payload": payload}, ensure_ascii=False) + "\n")
        LOG.flush()
        STATE["calls"] += 1

        reply = REPLY_1 if STATE["calls"] == 1 else REPLY_2

        self.send_response(200)
        self.send_header("Content-Type", "application/x-ndjson")
        self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()

        def chunk(data: bytes):
            self.wfile.write(b"%x\r\n" % len(data) + data + b"\r\n")
            self.wfile.flush()

        # stream the reply word by word to exercise the streaming path
        words = reply.split(" ")
        for i, w in enumerate(words):
            piece = w + (" " if i < len(words) - 1 else "")
            chunk(json.dumps({
                "message": {"role": "assistant", "content": piece},
                "done": False,
            }, ensure_ascii=False).encode() + b"\n")
        chunk(json.dumps({
            "message": {"role": "assistant", "content": ""},
            "done": True,
            "eval_count": 42,
        }).encode() + b"\n")
        self.wfile.write(b"0\r\n\r\n")


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 11435
    HTTPServer(("127.0.0.1", port), Handler).serve_forever()
