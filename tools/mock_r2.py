#!/usr/bin/env python3
"""Local stand-in for AAB + R2, for testing a debug build of AA Ext end to end.

Serves a manifest and a video with presigned-style expiry and HTTP Range, then prints the
ranomalyext:// link AAB would open. Debug builds of AA Ext accept http://127.0.0.1 URLs
whose first path segment is an allowed bucket; release builds never do.

    python3 tools/mock_r2.py --video clip.mov            # prints the link
    python3 tools/mock_r2.py --video clip.mov --open     # and opens it (macOS)
    python3 tools/mock_r2.py --video clip.mov --throttle 20 --drop-at 0.3 --video-expires 60

Only the Python standard library is used.
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
import threading
import time
import urllib.parse
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MANIFEST_BUCKET = "aab-temp"
MEDIA_BUCKET = "aab-media"


def presign(base: str, path: str, expires: int) -> str:
    date = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    q = urllib.parse.urlencode({
        "X-Amz-Algorithm": "AWS4-HMAC-SHA256",
        "X-Amz-Date": date,
        "X-Amz-Expires": str(expires),
        "X-Amz-Signature": "mock",
    })
    return f"{base}{urllib.parse.quote(path)}?{q}"


def expired(query: str) -> bool:
    q = urllib.parse.parse_qs(query)
    try:
        start = datetime.strptime(q["X-Amz-Date"][0], "%Y%m%dT%H%M%SZ").replace(tzinfo=timezone.utc)
        return time.time() > start.timestamp() + int(q["X-Amz-Expires"][0])
    except (KeyError, ValueError):
        return True


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--video", required=True, help="video file to serve")
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("--manifest-expires", type=int, default=300, help="seconds")
    ap.add_argument("--video-expires", type=int, default=6 * 3600, help="seconds")
    ap.add_argument("--throttle", type=float, default=0, help="MB/s cap for the video (0 = none)")
    ap.add_argument("--drop-at", type=float, default=0, help="drop the first video response at this fraction")
    ap.add_argument("--frames", type=int, help="override params.frames (default: ffprobe)")
    ap.add_argument("--no-sha", action="store_true", help="omit input.sha256")
    ap.add_argument("--open", action="store_true", help="open the link with `open` (macOS)")
    args = ap.parse_args()

    video = os.path.abspath(args.video)
    name = os.path.basename(video)
    size = os.path.getsize(video)
    base = f"http://127.0.0.1:{args.port}"

    probe = json.loads(subprocess.check_output([
        "ffprobe", "-v", "error", "-select_streams", "v:0", "-count_packets",
        "-show_entries", "stream=width,height,pix_fmt,avg_frame_rate,nb_read_packets", "-of", "json", video,
    ]))["streams"][0]
    num, den = probe["avg_frame_rate"].split("/")
    sha = None
    if not args.no_sha:
        h = hashlib.sha256()
        with open(video, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        sha = h.hexdigest()

    manifest = {
        "version": 1,
        "op": "video_to_png",
        "job_id": f"mock_{int(time.time())}",
        "title": f"Mock job — {name}",
        "input": {
            "url": presign(base, f"/{MEDIA_BUCKET}/{name}", args.video_expires),
            "filename": name,
            "size": size,
            **({"sha256": sha} if sha else {}),
        },
        "params": {
            "frames": args.frames or int(probe["nb_read_packets"]),
            "width": probe["width"],
            "height": probe["height"],
            "fps": round(int(num) / int(den), 3),
            "pix_fmt": probe["pix_fmt"],
        },
    }
    manifest_bytes = json.dumps(manifest, indent=2).encode()
    served = {"video_requests": 0}
    lock = threading.Lock()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, fmt, *a):
            sys.stderr.write(f"[mock] {self.command} {self.path.split('?')[0]} {fmt % a}\n")

        def do_GET(self):
            path, _, query = self.path.partition("?")
            path = urllib.parse.unquote(path)
            if path.startswith(f"/{MANIFEST_BUCKET}/"):
                if expired(query):
                    return self.send_error(403)
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(manifest_bytes)))
                self.end_headers()
                self.wfile.write(manifest_bytes)
            elif path == f"/{MEDIA_BUCKET}/{name}":
                if expired(query):
                    return self.send_error(403)
                self.serve_video()
            else:
                self.send_error(404)

        def serve_video(self):
            with lock:
                n = served["video_requests"]
                served["video_requests"] += 1
            start = 0
            rng = self.headers.get("Range", "")
            if rng.startswith("bytes=") and rng.endswith("-"):
                start = int(rng[6:-1])
            if start:
                self.send_response(206)
                self.send_header("Content-Range", f"bytes {start}-{size - 1}/{size}")
            else:
                self.send_response(200)
            self.send_header("Content-Length", str(size - start))
            self.send_header("Accept-Ranges", "bytes")
            self.end_headers()
            stop = int(size * args.drop_at) if (n == 0 and args.drop_at) else size
            chunk = 256 * 1024
            per_chunk = chunk / (args.throttle * 1e6) if args.throttle else 0
            with open(video, "rb") as f:
                f.seek(start)
                pos = start
                while pos < stop:
                    data = f.read(min(chunk, stop - pos))
                    if not data:
                        break
                    try:
                        self.wfile.write(data)
                    except (BrokenPipeError, ConnectionResetError):
                        return
                    pos += len(data)
                    if per_chunk:
                        time.sleep(per_chunk)
            if stop < size:
                sys.stderr.write(f"[mock] dropped connection at {pos} bytes\n")
                self.close_connection = True

    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    manifest_url = presign(base, f"/{MANIFEST_BUCKET}/{manifest['job_id']}.json", args.manifest_expires)
    link = "ranomalyext://run?manifest=" + urllib.parse.quote(manifest_url, safe="")
    print(json.dumps(manifest, indent=2))
    print(f"\nLink:\n{link}\n", flush=True)
    if args.open:
        threading.Timer(0.3, lambda: subprocess.run(["open", link])).start()
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
