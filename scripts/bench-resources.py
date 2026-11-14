#!/usr/bin/env python3
"""Measure the native Linux desktop and single-file CPU budget on loopback."""
import argparse
import contextlib
import http.server
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.request

MIB = 1024**2
BLOCK = bytes(MIB)
CLOCK = os.sysconf("SC_CLK_TCK")


class Fixture(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def do_GET(self):
        fast = self.path == "/cpu.bin"
        total = (768 if fast else 64) * MIB
        match = re.fullmatch(r"bytes=(\d+)-(\d*)", self.headers.get("Range", ""))
        first = int(match[1]) if match else 0
        last = min(total - 1, int(match[2])) if match and match[2] else total - 1
        self.send_response(206 if match else 200)
        if match:
            self.send_header("Content-Range", f"bytes {first}-{last}/{total}")
        self.send_header("Content-Length", str(last - first + 1))
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("ETag", '"fixture-v1"')
        self.end_headers()
        try:
            for at in range(first, last + 1, MIB):
                self.wfile.write(memoryview(BLOCK)[: min(MIB, last - at + 1)])
                if not fast:
                    time.sleep(0.08)
        except (BrokenPipeError, ConnectionResetError):
            pass


def sample(pid):
    counts = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    status = Path(f"/proc/{pid}/status").read_text()
    return (
        (int(counts[11]) + int(counts[12])) / CLOCK,
        int(re.search(r"^VmRSS:\s*(\d+)", status, re.M)[1]) * 1024,
    )


def check_process(process):
    if process.poll() is not None:
        raise RuntimeError(f"{process.args[0]} exited with {process.returncode}")


def wait_until(predicate, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if result := predicate():
            return result
        time.sleep(0.02)
    raise RuntimeError("Timed out waiting for benchmark fixture")


def call_api(endpoint, path, body=None):
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(
        f"http://127.0.0.1:{endpoint['port']}/api/v1/{path}",
        data,
        {"Authorization": f"Bearer {endpoint['token']}", "Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=5) as response:
        payload = response.read()
    return json.loads(payload) if payload else None


def window_visible():
    tree = subprocess.run(
        ["xwininfo", "-root", "-tree"], capture_output=True, text=True, check=True
    ).stdout
    return '"Ruscello"' in tree


def desktop(binary, root, url, backend):
    data = root / "data" / "ruscello"
    config = root / "config" / "ruscello"
    config.mkdir(parents=True)
    (root / "state" / "ruscello" / "logs" / "ruscello").mkdir(parents=True)
    (config / "gui.json").write_text(json.dumps({
        "locale": "English",
        "motion": False,
        "check_updates": False,
        "browser_helper": False,
        "notifications": False,
        "default_folder": str(root / "downloads"),
        "renderer": backend,
        "torrent": {"dht": False, "upnp": False, "lsd": False},
    }))
    env = dict(os.environ, XDG_DATA_HOME=str(data.parent), XDG_CONFIG_HOME=str(config.parent),
               XDG_STATE_HOME=str(root / "state"), XDG_CACHE_HOME=str(root / "cache"),
               WINIT_UNIX_BACKEND="x11", ICED_PRESENT_MODE="fifo",
               ICED_BACKEND="wgpu" if backend == "Hardware" else "tiny-skia")
    env.pop("WAYLAND_DISPLAY", None)
    startup, idle_rss, idle_cpu = idle_desktop(binary, env, data)
    with contextlib.closing(sqlite3.connect(data / "ruscello.db")) as db:
        db.execute("INSERT OR REPLACE INTO settings(key, record) VALUES (?, ?)",
                   ("scheduler_limits", json.dumps({"concurrent_downloads": 10,
                       "connections_per_host": 16, "connections_per_download": 1})))
        db.commit()
    with subprocess.Popen([binary], env=env) as process:
        try:
            endpoint = desktop_ready(process, data)
            pid = endpoint["pid"]
            call_api(endpoint, "downloads", {
                "urls": [f"{url}/slow-{index}.bin" for index in range(10)],
            })

            def bytes_downloaded():
                return {entry["id"]: entry["downloaded_bytes"]
                        for entry in call_api(endpoint, "queue")["downloads"]
                        if entry["status"] == "Downloading"}

            wait_until(lambda: len(bytes_downloaded()) == 10, 15)
            beginning = time.monotonic()
            previous = bytes_downloaded()
            counts = {job: 0 for job in previous}
            active_rss = []
            while time.monotonic() - beginning < 3:
                time.sleep(0.04)
                current = bytes_downloaded()
                for job, value in current.items():
                    if job in previous and value != previous[job]:
                        counts[job] += 1
                previous = current
                active_rss.append(sample(pid)[1])
            progress_hz = max(counts.values()) / (time.monotonic() - beginning)
            return [
                ("Cold startup to window and queue (s)", startup, 1.0),
                ("Idle RAM (MB)", max(idle_rss) / 10**6, 80),
                ("Idle CPU (% of one core)", idle_cpu, 1),
                ("10 active HTTP downloads RAM (MB)", max(active_rss) / 10**6, 150),
                ("Per-row progress (Hz, 3-second sample)", progress_hz, 4.25),
            ]
        finally:
            process.terminate()


def desktop_ready(process, data):
    def ready():
        check_process(process)
        api = data / "local-api.json"
        if api.is_file() and window_visible():
            endpoint = json.loads(api.read_text())
            if endpoint["pid"] == process.pid and call_api(endpoint, "health")["ok"]:
                return endpoint
        return None

    return wait_until(ready, 30)


def idle_desktop(binary, env, data):
    started = time.monotonic()
    with subprocess.Popen([binary], env=env) as process:
        try:
            pid = desktop_ready(process, data)["pid"]
            startup = time.monotonic() - started
            time.sleep(1)
            before = sample(pid)[0]
            start_idle = time.monotonic()
            idle_rss = []
            for _ in range(30):
                time.sleep(0.1)
                idle_rss.append(sample(pid)[1])
            idle_cpu = 100 * (sample(pid)[0] - before) / (time.monotonic() - start_idle)
            return startup, idle_rss, idle_cpu
        finally:
            process.terminate()


def single_transfer(binary, root, url):
    destination = root / "single"
    destination.mkdir()
    (root / "logs").mkdir()
    env = dict(os.environ, RUSCELLO_LOG_DIR=str(root / "logs"),
               RUSCELLO_DATA_DIR=str(root / "cli-data"), RUSCELLO_RECIPES_DIR=str(root / "recipes"))
    before = os.times().children_user
    start = time.monotonic()
    subprocess.run([binary, "get", f"{url}/cpu.bin", "-o", str(destination), "-x", "1", "--quiet"],
                   stdout=subprocess.DEVNULL, env=env, check=True)
    elapsed = time.monotonic() - start
    user = os.times().children_user - before
    bytes_per_second = (destination / "cpu.bin").stat().st_size / elapsed
    # This budget excludes kernel I/O. Normalize measured CPU per byte to 1 Gbit/s.
    cpu = 100 * user / elapsed * (125_000_000 / bytes_per_second)
    return [
        ("Loopback throughput (Gbit/s, minimum 1.0)", bytes_per_second * 8 / 10**9, None),
        ("User CPU at 1 Gbit/s (% of one core)", cpu, 15),
    ]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, default=Path("target/release"))
    parser.add_argument("--backend", choices=["Hardware", "Software"], default="Hardware")
    args = parser.parse_args()
    if not os.environ.get("DISPLAY") or not shutil.which("xwininfo"):
        parser.error("use an empty X11 display (for example xvfb-run with --backend Software)")
    with (
        tempfile.TemporaryDirectory() as directory,
        http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture) as server,
    ):
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            url = f"http://127.0.0.1:{server.server_port}"
            root = Path(directory)
            metrics = desktop(args.bin_dir.resolve() / "ruscello", root, url, args.backend)
            metrics.extend(single_transfer(args.bin_dir.resolve() / "ruscello-cli", root, url))
        finally:
            server.shutdown()

    passed = True
    for label, value, upper in metrics:
        ok = value >= 1 if upper is None else value < upper
        print(f"{label}: {value:.3f} {'PASS' if ok else 'FAIL'}")
        passed &= ok
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
