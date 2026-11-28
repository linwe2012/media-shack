#!/usr/bin/env python3
"""Serve repo UI with an in-memory Tauri mock injected before the real app.js."""
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import os

ROOT = Path(__file__).resolve().parents[1]
MOCK = (ROOT / "stress" / "mock-tauri.js").read_text()

class Handler(SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(ROOT), **kwargs)

    def do_GET(self):
        if self.path.split("?", 1)[0] == "/ui/index.html" and "stress=1" in self.path:
            source = (ROOT / "ui" / "index.html").read_text()
            source = source.replace('<script src="app.js"></script>', '<script>window.__STRESS_SIZE__=new URLSearchParams(location.search).get("size")||"smoke";</script><script src="/stress/mock-tauri.js"></script><script src="app.js"></script>')
            data = source.encode()
            self.send_response(200); self.send_header("Content-Type", "text/html; charset=utf-8"); self.send_header("Content-Length", str(len(data))); self.end_headers(); self.wfile.write(data)
            return
        return super().do_GET()

if __name__ == "__main__":
    port = int(os.environ.get("STRESS_PORT", "4173"))
    print(f"Stress harness: http://127.0.0.1:{port}/stress/?size=smoke|10k|50k", flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
