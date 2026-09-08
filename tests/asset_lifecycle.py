#!/usr/bin/env python3
"""HTTP lifecycle checks owned by probatum (0.9 lacks PUT/DELETE).

Only Python's standard library is required. Each run owns a temporary blog,
an ephemeral loopback port, and its server processes. No real blog is touched.
"""

import argparse
import http.cookiejar
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request


REPO = Path(__file__).resolve().parents[1]
DEFAULT_BINARY = REPO / "target/x86_64-unknown-linux-musl/release/stela"


class Blog:
    def __init__(self, binary, root):
        self.binary = binary
        self.root = root
        self.process = None
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            self.port = sock.getsockname()[1]
        self.jar = http.cookiejar.CookieJar()
        self.admin = urllib.request.build_opener(
            urllib.request.ProxyHandler({}),
            urllib.request.HTTPCookieProcessor(self.jar),
        )
        self.public = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def request(self, path, method="GET", data=None, expected=None):
        request = urllib.request.Request(
            f"http://127.0.0.1:{self.port}{path}",
            method=method,
            data=None if data is None else json.dumps(data).encode(),
            headers={"Content-Type": "application/json"},
        )
        # Public URLs are checked anonymously, even after the admin logs in.
        client = self.public if method == "GET" else self.admin
        try:
            with client.open(request, timeout=5) as response:
                result = response.status, response.read().decode()
        except urllib.error.HTTPError as error:
            result = error.code, error.read().decode()
        if expected is not None and result[0] != expected:
            raise RuntimeError(
                f"{method} {path}: expected {expected}, got {result[0]}"
            )
        return result

    def start(self):
        env = {k: v for k, v in os.environ.items() if not k.startswith("LT_")}
        env.update(STELA_ADMIN_PASSWORD="temporary-test-password", RUST_LOG="warn")
        with (self.root / "server.log").open("a") as log:
            self.process = subprocess.Popen(
                [
                    str(self.binary), "serve", "--port", str(self.port),
                    "--admin-route", "/secure-test", "--data", str(self.root / "data"),
                ],
                cwd=self.root, env=env, stdout=log, stderr=log,
            )
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError(f"server exited during startup: {self.process.returncode}")
            try:
                self.request("/", expected=200)
                return
            except urllib.error.URLError:
                time.sleep(0.05)
        raise RuntimeError("server readiness deadline exceeded")

    def stop(self):
        if self.process is not None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
            self.process = None

    def restart(self):
        self.stop()
        self.start()
        self.login()

    def login(self):
        self.jar.clear()
        self.admin.addheaders = []
        self.request(
            "/secure-test/login", "POST",
            {"username": "editor", "password": "temporary-test-password"}, 200,
        )
        # Python omits Secure cookies on loopback HTTP. Explicit transport is
        # restricted to this isolated test server; browser policy is tested in e2e.
        cookies = "; ".join(f"{c.name}={c.value}" for c in self.jar)
        if not cookies:
            raise RuntimeError("login did not issue a session cookie")
        self.admin.addheaders = [("Cookie", cookies)]

    def rebuild(self):
        self.request("/secure-test/rebuild", "POST", expected=200)

    def eventually(self, path, expected):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            actual, _ = self.request(path)
            if actual == expected:
                return
            time.sleep(0.05)
        raise RuntimeError(f"GET {path}: expected {expected} after mutation hook, got {actual}")

    def withdrawn(self, path, marker, is_post):
        _, body = self.request(path, expected=404)
        if "Nothing here" not in body or marker in body:
            raise RuntimeError(f"{path}: expected the theme's 404 without withdrawn content")
        for listing in ["/", "/rss.xml"] if is_post else ["/rss.xml"]:
            if marker in self.request(listing, expected=200)[1]:
                raise RuntimeError(f"{listing}: withdrawn content still listed")


def lifecycle(blog, scenario):
    retained = dict(slug="retained", title="Retained", body="KEEP_ME", published=True)
    blog.request("/api/posts", "POST", retained, 201)
    for collection, path in [("posts", "/posts/withdrawn"), ("pages", "/withdrawn")]:
        marker = f"WITHDRAWN_{collection.upper()}"
        item = dict(slug="withdrawn", title=marker, body=marker, published=True)
        api = f"/api/{collection}"
        blog.request(api, "POST", item, 201)
        blog.rebuild()
        if marker not in blog.request(path, expected=200)[1]:
            raise RuntimeError(f"{path}: published body missing")

        if scenario == "delete":
            blog.request(f"{api}/withdrawn", "DELETE", expected=204)
        else:
            item["published"] = False
            blog.request(f"{api}/withdrawn", "PUT", item, 200)
        blog.eventually(path, 404)
        blog.rebuild()
        blog.withdrawn(path, marker, collection == "posts")
        blog.restart()
        blog.withdrawn(path, marker, collection == "posts")

        # Reusing the URL must work after the persisted asset tombstone.
        item["published"] = True
        if scenario == "delete":
            blog.request(api, "POST", item, 201)
        else:
            blog.request(f"{api}/withdrawn", "PUT", item, 200)
        blog.rebuild()
        blog.restart()
        if marker not in blog.request(path, expected=200)[1]:
            raise RuntimeError(f"{path}: recreated body missing after restart")
        print(f"PASS {collection}: {scenario}, rebuild, restart, recreate, restart", flush=True)

    for path in ["/", "/style.css", "/rss.xml", "/posts/retained"]:
        blog.request(path, expected=200)
    if "KEEP_ME" not in blog.request("/posts/retained", expected=200)[1]:
        raise RuntimeError("unrelated post content was changed")
    blog.request("/missing", expected=404)
    print("PASS unrelated content and theme assets preserved", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument("--scenario", choices=["unpublish", "delete"], default="unpublish")
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"binary missing: {binary}; build Stela first or pass --binary")

    # Ensure probatum cancellation also runs our process cleanup.
    def interrupted(signum, _frame):
        raise SystemExit(128 + signum)

    signal.signal(signal.SIGTERM, interrupted)
    with tempfile.TemporaryDirectory(prefix="stela-lifecycle-") as directory:
        blog = Blog(binary, Path(directory))
        try:
            blog.start()
            blog.login()
            lifecycle(blog, args.scenario)
        except Exception as error:
            print(f"FAIL {args.scenario}: {error}", file=sys.stderr)
            log = blog.root / "server.log"
            if log.exists():
                print("\n".join(log.read_text().splitlines()[-30:]), file=sys.stderr)
            return 1
        finally:
            blog.stop()
    return 0


if __name__ == "__main__":
    sys.exit(main())
