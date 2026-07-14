#!/usr/bin/env python3
"""
Local dev HTTP server with no-cache headers.
Solves persistent ES module caching issues during development.

Usage: python dev-server.py [port]   (default port 8765)
"""

import sys
import mimetypes
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer

# Ensure .wasm files are served with the correct MIME type so that
# WebAssembly.instantiateStreaming works without a Content-Type mismatch.
mimetypes.add_type('application/wasm', '.wasm')


class NoCacheHandler(SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header('Cache-Control', 'no-store, no-cache, must-revalidate, max-age=0')
        self.send_header('Pragma', 'no-cache')
        self.send_header('Expires', '0')
        super().end_headers()


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
    server = ThreadingHTTPServer(('0.0.0.0', port), NoCacheHandler)
    print(f'Dev server (no-cache) on http://localhost:{port}/')
    server.serve_forever()


if __name__ == '__main__':
    main()
