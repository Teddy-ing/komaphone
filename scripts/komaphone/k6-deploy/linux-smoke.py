#!/usr/bin/env python3
"""Isolated Linux/proxy feasibility fixture; no cloud access or personal data.

Python is test orchestration only. Komaphone product/runtime remain Rust.
Requires a built Linux service and official Caddy binary. No trust store changes.
"""
import argparse
import concurrent.futures
import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import ssl
import subprocess
import threading
import time
import urllib.error
import urllib.request
import uuid


def fresh():
    return uuid.uuid4().hex


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def sample(pid):
    status = Path(f"/proc/{pid}/status").read_text()
    rss = int(next(line.split()[1] for line in status.splitlines() if line.startswith("VmRSS:")))
    high = int(next(line.split()[1] for line in status.splitlines() if line.startswith("VmHWM:")))
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()
    ticks = os.sysconf("SC_CLK_TCK")
    return {"rss_kib": rss, "peak_rss_kib": high, "cpu_seconds": (int(fields[11]) + int(fields[12])) / ticks}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--service", type=Path, required=True)
    parser.add_argument("--caddy", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--world-version", type=int, default=1, choices=range(1, 65536))
    args = parser.parse_args()
    service = args.service.resolve(strict=True)
    caddy = args.caddy.resolve(strict=True)
    root = args.output.resolve() / ("run-" + fresh())
    root.mkdir(parents=True, exist_ok=False)
    backend_port, proxy_port = port(), port()
    while proxy_port == backend_port:
        proxy_port = port()
    data = root / "service-data"
    processes, handles = [], []
    measurements = {"request_json_bytes": 0, "response_body_bytes": 0, "requests": 0}
    latency = []
    lock = threading.Lock()
    result = {"passed": False, "root": str(root), "kernel": os.uname().release,
              "service_sha256": hashlib.sha256(service.read_bytes()).hexdigest(),
              "caddy_sha256": hashlib.sha256(caddy.read_bytes()).hexdigest(),
              "public_trust_tested": False, "cloud_deployed": False,
              "measurements_scope": "local WSL Linux service + TLS proxy; JSON payload bytes exclude HTTP/TLS overhead",
              "host_exclusivity_claimed": False,
              "e2_cpu_memory_limit_simulated": False,
              "data_filesystem": subprocess.check_output(["stat", "-f", "-c", "%T", str(root)], text=True).strip()}

    def launch(binary, arguments, name, env=None):
        log = (root / (name + ".log")).open("wb")
        handles.append(log)
        process = subprocess.Popen([str(binary), *arguments], stdout=log, stderr=subprocess.STDOUT, env=env)
        processes.append(process)
        return process

    def start_service(name, directory=data):
        process = launch(service, ["--bind", f"127.0.0.1:{backend_port}", "--data-dir", str(directory)], name)
        wait_health(f"http://127.0.0.1:{backend_port}/health", None, process)
        return process

    def measured_cli(arguments, name):
        started = time.monotonic()
        process = launch(service, arguments, name)
        # Linux wait4 reports this child's peak RSS, including a short-lived backup.
        deadline = started + 30
        while True:
            finished, status, usage = os.wait4(process.pid, os.WNOHANG)
            if finished:
                break
            if time.monotonic() >= deadline:
                process.kill()
                _, status, _ = os.wait4(process.pid, 0)
                process.returncode = os.waitstatus_to_exitcode(status)
                raise AssertionError((name, "deadline"))
            time.sleep(0.01)
        process.returncode = os.waitstatus_to_exitcode(status)
        assert process.returncode == 0, (name, process.returncode)
        return {"duration_seconds": time.monotonic() - started, "peak_rss_kib": usage.ru_maxrss,
                "cpu_seconds": usage.ru_utime + usage.ru_stime}

    def wait_health(url, context, process):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise AssertionError("process exited before readiness; inspect isolated log")
            try:
                with urllib.request.urlopen(url, context=context, timeout=1) as response:
                    value = json.load(response)
                    if value.get("service") == "Komaphone":
                        return
            except (OSError, urllib.error.URLError):
                time.sleep(0.05)
        raise AssertionError("health deadline")

    try:
        invalid = subprocess.run([str(service), "--bind", "0.0.0.0:0", "--data-dir", str(root / "invalid-public")],
                                 capture_output=True, timeout=10)
        assert invalid.returncode != 0 and not (root / "invalid-public").exists()
        result["public_plaintext_rejected"] = True
        app = start_service("service-initial")
        config = root / "Caddyfile"
        template = Path(__file__).with_name("Caddyfile").read_text()
        template = template.replace("admin localhost:2019", "admin off\n\tauto_https disable_redirects\n\tskip_install_trust")
        template = template.replace("{$KOMAPHONE_HOSTNAME} {", f"https://localhost:{proxy_port} {{\n\tbind 127.0.0.1\n\ttls internal")
        template = template.replace("127.0.0.1:43127", f"127.0.0.1:{backend_port}")
        config.write_text(template)
        env = os.environ.copy()
        env["XDG_DATA_HOME"] = str(root / "caddy-data")
        env["XDG_CONFIG_HOME"] = str(root / "caddy-config")
        proxy = launch(caddy, ["run", "--config", str(config), "--adapter", "caddyfile"], "caddy", env)
        ca = root / "caddy-data/caddy/pki/authorities/local/root.crt"
        deadline = time.monotonic() + 20
        while not ca.exists() and time.monotonic() < deadline:
            assert proxy.poll() is None
            time.sleep(0.05)
        context = ssl.create_default_context(cafile=str(ca))
        base = f"https://localhost:{proxy_port}"
        wait_health(base + "/health", context, proxy)
        try:
            urllib.request.urlopen(base + "/health", timeout=3)
            raise AssertionError("untrusted local CA unexpectedly accepted")
        except urllib.error.URLError as error:
            assert isinstance(error.reason, ssl.SSLCertVerificationError), repr(error.reason)
            result["untrusted_ca_rejected"] = True
        result["verified_local_tls_proxy"] = True

        def post(path, body, token=None, expected=200):
            encoded = json.dumps(body, separators=(",", ":")).encode()
            headers = {"Content-Type": "application/json"}
            if token:
                headers["Authorization"] = "Bearer " + token
            request = urllib.request.Request(base + path, encoded, headers, method="POST")
            started = time.monotonic()
            try:
                response = urllib.request.urlopen(request, context=context, timeout=30)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                payload = response.read(4 * 1024 * 1024)
                assert response.status == expected, (path, response.status, payload[:300])
            with lock:
                measurements["request_json_bytes"] += len(encoded)
                measurements["response_body_bytes"] += len(payload)
                measurements["requests"] += 1
                latency.append((time.monotonic() - started) * 1000)
            return json.loads(payload)

        def room(action, token=None, expected=200):
            return post("/v1/rpc", {"protocol_version": 1, "request_id": fresh(), "operation": action}, token, expected)

        def world(action, token, expected=200):
            return post("/v1/canvas", {"version": args.world_version, "action": action}, token, expected)

        actors = [{"profile": fresh(), "device": fresh(), "token": fresh() + fresh()} for _ in range(9)]
        for index, actor in enumerate(actors):
            room({"kind": "pair", "participant": {"id": actor["profile"], "device": actor["device"],
                  "display_name": f"Linux fixture {index}"}, "credential": actor["token"]})
        lobby = room({"kind": "create", "mode": {"kind": "animation", "rotations": 1}, "timer_ms": None}, actors[0]["token"])
        room_id = lobby["view"]["id"]
        result["lobby_room_id"] = room_id
        canvases = []
        for group in (2, 4, 8):
            canvas = fresh()
            created = world({"kind": "create", "canvas": canvas, "name": f"Linux {group} writers"}, actors[0]["token"])
            epochs = [created["info"]["epoch"]]
            for actor in actors[1:group]:
                invitation, secret = fresh(), fresh() + fresh()
                world({"kind": "invite", "canvas": canvas, "id": invitation, "recipient": actor["profile"],
                       "secret": secret, "expires_at_ms": int(time.time() * 1000) + 60_000}, actors[0]["token"])
                accepted = world({"kind": "accept", "canvas": canvas, "id": invitation, "secret": secret}, actor["token"])
                epochs.append(accepted["info"]["epoch"])
            world({"kind": "info", "canvas": canvas}, actors[8]["token"], 403)
            canvases.append((canvas, group, epochs))

        before_app, before_proxy = sample(app.pid), sample(proxy.pid)
        start = time.monotonic()
        for canvas, group, epochs in canvases:
            def draw(index):
                actor = actors[index]
                for sequence in range(1, 51):
                    stroke = {"id": fresh(), "canvas": canvas, "owner": actor["profile"], "revision": 0,
                              "anchor": {"depth": 0, "x": "0", "y": "0"},
                              "points": [[float(point), float(index * 20 + sequence)] for point in range(16)],
                              "width": 4.0, "color": {"rgba": [0.1, 0.2, 0.3, 1.0]}, "erase": False}
                    operation = {"id": fresh(), "canvas": canvas, "author": actor["profile"], "device": actor["device"],
                                 "sequence": sequence, "authority_epoch": epochs[index], "action": {"kind": "add_stroke", "stroke": stroke}}
                    assert world({"kind": "append", "operation": operation}, actor["token"])["kind"] == "accepted"
            with concurrent.futures.ThreadPoolExecutor(max_workers=group) as executor:
                list(executor.map(draw, range(group)))
        elapsed = time.monotonic() - start
        after_app, after_proxy = sample(app.pid), sample(proxy.pid)
        result["load"] = {"groups": [2, 4, 8], "operations_per_writer_per_canvas": 50, "total_operations": 700,
                          "points_per_stroke": 16, "duration_seconds": elapsed,
                          "service_before": before_app, "service_after": after_app,
                          "proxy_before": before_proxy, "proxy_after": after_proxy,
                          "cpu_seconds": after_app["cpu_seconds"] - before_app["cpu_seconds"] + after_proxy["cpu_seconds"] - before_proxy["cpu_seconds"]}

        def all_events(canvas, token):
            events, after = [], 0
            while True:
                page = world({"kind": "events", "canvas": canvas, "after": after, "through": None, "limit": 128}, token)
                events.extend(page["events"])
                if page["next"] is None:
                    break
                after = page["next"]
            return events

        canonical = []
        for canvas, group, _ in canvases:
            expected = all_events(canvas, actors[0]["token"])
            assert len(expected) == group * 50
            for actor in actors[1:group]:
                assert all_events(canvas, actor["token"]) == expected
            canonical.append(expected)
        result["all_member_event_streams_equal"] = True
        app.kill()
        app.wait(timeout=10)
        app = start_service("service-after-kill")
        restored_lobby = room({"kind": "poll", "room": room_id, "after_revision": 0}, actors[0]["token"])
        assert restored_lobby["view"]["id"] == room_id
        for (canvas, _, _), expected in zip(canvases, canonical):
            assert all_events(canvas, actors[0]["token"]) == expected
        result["acknowledged_operations_survived_sigkill"] = True
        backup = root / "backup.sqlite3"
        result["backup_process"] = measured_cli(["--data-dir", str(data), "--backup-to", str(backup)], "backup-cli")
        restored = root / "fresh-restore"
        result["restore_process"] = measured_cli(["--data-dir", str(restored), "--restore-from", str(backup)], "restore-cli")
        app.send_signal(signal.SIGINT)
        app.wait(timeout=10)
        app = start_service("service-restored", restored)
        for (canvas, _, _), expected in zip(canvases, canonical):
            assert all_events(canvas, actors[0]["token"]) == expected
            world({"kind": "info", "canvas": canvas}, actors[8]["token"], 403)
        result["fresh_restore_preserved_events_and_denials"] = True
        result["storage"] = {"live_database_bytes": (data / "service.sqlite3").stat().st_size,
                             "backup_bytes": backup.stat().st_size, "restored_database_bytes": (restored / "service.sqlite3").stat().st_size}
        result["http"] = measurements
        ordered = sorted(latency)
        result["request_latency_ms"] = {"p50": ordered[len(ordered) // 2], "p95": ordered[int((len(ordered) - 1) * 0.95)], "max": max(ordered)}
        result["canonical_event_sha256"] = [hashlib.sha256(json.dumps(events, sort_keys=True, separators=(",", ":")).encode()).hexdigest() for events in canonical]
        result["passed"] = True
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        for handle in handles:
            handle.close()
        (root / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(root / "result.json")
    assert result["passed"]


if __name__ == "__main__":
    main()
