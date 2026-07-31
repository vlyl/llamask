#!/usr/bin/env python3
"""Start llama-server, monitor it, and run the Qwen extraction benchmark."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import threading
import time
from pathlib import Path

import psutil
import requests


DEFAULT_SERVER = Path("evaluation/vendor/llama.cpp/build/bin/llama-server")
DEFAULT_CLIENT = Path("evaluation/scripts/run_qwen_extraction.py")


def wait_until_ready(base_url: str, process: subprocess.Popen[bytes], timeout: int) -> float:
    started = time.monotonic()
    deadline = started + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"llama-server exited with {process.returncode}")
        try:
            response = requests.get(f"{base_url}/health", timeout=2)
            if response.status_code == 200:
                return time.monotonic() - started
        except requests.RequestException:
            pass
        time.sleep(0.5)
    raise TimeoutError("llama-server did not become ready")


def monitor_rss(
    process: subprocess.Popen[bytes], stop: threading.Event, samples: list[int]
) -> None:
    root = psutil.Process(process.pid)
    while not stop.wait(0.1):
        try:
            rss = root.memory_info().rss
            for child in root.children(recursive=True):
                rss += child.memory_info().rss
            samples.append(rss)
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            return


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("model", type=Path)
    parser.add_argument("dataset", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--server", type=Path, default=DEFAULT_SERVER)
    parser.add_argument("--client", type=Path, default=DEFAULT_CLIENT)
    parser.add_argument("--port", type=int, default=18080)
    parser.add_argument("--ctx-size", type=int, default=8192)
    parser.add_argument("--parallel", type=int, default=1)
    parser.add_argument("--split", choices=["dev", "test", "all"], default="dev")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--batch-size", type=int, default=6)
    parser.add_argument("--startup-timeout", type=int, default=300)
    args = parser.parse_args()

    args.output.parent.mkdir(parents=True, exist_ok=True)
    log_path = args.output.with_suffix(".server.log")
    summary_path = args.output.with_suffix(".runtime.json")
    base_url = f"http://127.0.0.1:{args.port}"
    server_command = [
        str(args.server),
        "--model",
        str(args.model),
        "--host",
        "127.0.0.1",
        "--port",
        str(args.port),
        "--ctx-size",
        str(args.ctx_size),
        "--parallel",
        str(args.parallel),
        "--gpu-layers",
        "all",
        "--reasoning",
        "off",
        "--reasoning-budget",
        "0",
        "--no-ui",
    ]

    samples: list[int] = []
    stop = threading.Event()
    started = time.monotonic()
    with log_path.open("wb") as log:
        process = subprocess.Popen(
            server_command,
            stdout=log,
            stderr=subprocess.STDOUT,
        )
        monitor = threading.Thread(
            target=monitor_rss,
            args=(process, stop, samples),
            daemon=True,
        )
        monitor.start()
        client_return_code = 1
        load_seconds: float | None = None
        try:
            load_seconds = wait_until_ready(base_url, process, args.startup_timeout)
            client_command = [
                sys.executable,
                str(args.client),
                str(args.dataset),
                str(args.output),
                "--base-url",
                base_url,
                "--split",
                args.split,
                "--batch-size",
                str(args.batch_size),
            ]
            if args.limit is not None:
                client_command.extend(["--limit", str(args.limit)])
            client_return_code = subprocess.run(client_command, check=False).returncode
        finally:
            process.terminate()
            try:
                process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)
            stop.set()
            monitor.join(timeout=2)

    summary = {
        "model": str(args.model),
        "server_revision": "47f686f53f2a20fe82d87d79fa5c835714ee8f46",
        "load_seconds": load_seconds,
        "wall_seconds": time.monotonic() - started,
        "peak_rss_bytes": max(samples) if samples else None,
        "peak_rss_gb": max(samples) / 1024**3 if samples else None,
        "client_return_code": client_return_code,
        "server_log": str(log_path),
    }
    summary_path.write_text(
        json.dumps(summary, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )
    print(json.dumps(summary, ensure_ascii=False))
    return client_return_code


if __name__ == "__main__":
    raise SystemExit(main())
