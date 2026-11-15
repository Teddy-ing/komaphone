#!/usr/bin/env python3
"""Bounded real-service protocol/quota/corruption checks on synthetic loopback data.

Runs with a 1 MiB SQLite quota, then retries the identical refused upload at
4 MiB. Never fills a disk or contacts a cloud service. Not a desktop UI test.
"""
import argparse
import hashlib
import json
import pathlib
import re
import shutil
import signal
import socket
import sqlite3
import subprocess
import time
import urllib.error
import urllib.request
import uuid

PIN = "d30065ddafc5d0b7b770692210031803b477e9259a227fb80f597554ba6ca998"


def fresh():
    return uuid.uuid4().hex


def sha(path):
    with path.open("rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--service", required=True, type=pathlib.Path)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--expected-service-sha256", default=PIN)
    parser.add_argument("--world-version", type=int, default=1)
    args = parser.parse_args()
    assert 1 <= args.world_version < 999, "Invalid fixture world version"
    service = args.service.resolve(strict=True)
    expected_pin = args.expected_service_sha256.lower()
    assert re.fullmatch(r"[0-9a-f]{64}", expected_pin), "Invalid expected binary pin"
    assert sha(service) == expected_pin, "Unexpected service binary"
    root = args.output.resolve() / ("failure-" + fresh())
    root.mkdir(parents=True, exist_ok=False)
    data = root / "data"
    database = data / "service.sqlite3"
    result = {"passed": False, "service_sha256": expected_pin, "synthetic_only": True,
              "scope": "real loopback Rust service and SQLite; no desktop UI, cloud or full-disk test"}
    processes, logs = [], []
    base = None

    def start(quota):
        nonlocal base
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        log = (root / f"service-{quota}.log").open("wb")
        logs.append(log)
        process = subprocess.Popen([str(service), "--data-dir", str(data), "--bind", f"127.0.0.1:{port}",
                                    "--quota-mib", str(quota)], stdout=log, stderr=subprocess.STDOUT)
        processes.append(process)
        until = time.monotonic() + 15
        while time.monotonic() < until:
            assert process.poll() is None, "Service exited before readiness"
            try:
                with urllib.request.urlopen(base + "/health", timeout=1) as response:
                    assert json.load(response)["service"] == "Komaphone"
                return process
            except (OSError, urllib.error.URLError):
                time.sleep(0.05)
        raise AssertionError("Readiness timeout")

    def stop(process):
        process.send_signal(signal.SIGINT)
        assert process.wait(timeout=15) == 0

    def post(path, body, token, expected=200, observe=False):
        headers = {"Content-Type": "application/json"}
        if token:
            headers["Authorization"] = "Bearer " + token
        request = urllib.request.Request(base + path, json.dumps(body).encode(), headers, method="POST")
        try:
            response = urllib.request.urlopen(request, timeout=10)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            status = response.status
            body = response.read(4 * 1024 * 1024 + 1)
        assert len(body) <= 4 * 1024 * 1024
        assert status in ((expected,) if isinstance(expected, int) else expected), f"Unexpected HTTP status {status}: {json.loads(body).get('message', 'no fault message')}"
        return (status, json.loads(body)) if observe else json.loads(body)

    def rpc(action):
        return {"protocol_version": 1, "request_id": fresh(), "operation": action}

    def receipt():
        # Capture exact rows; reads do not mutate the authority or its receipt log.
        with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as db:
            return {table: db.execute(f"SELECT * FROM {table} ORDER BY rowid").fetchall()
                    for table in ("objects", "receipts", "worlds", "world_events", "world_members")}

    try:
        app = start(1)
        profile, device, token = fresh(), fresh(), fresh() + fresh()
        post("/v1/rpc", rpc({"kind": "pair", "participant": {"id": profile, "device": device, "display_name": "Synthetic failure"},
                            "credential": token}), None)
        room = post("/v1/rpc", rpc({"kind": "create", "mode": {"kind": "animation", "rotations": 1}, "timer_ms": None}), token)["view"]["id"]
        prior = b"synthetic acknowledged immutable asset" * 80
        digest = hashlib.sha256(prior).hexdigest()
        acknowledgement = post("/v1/rpc", rpc({"kind": "upload", "room": room, "hash": digest, "total": len(prior),
                                                "offset": 0, "bytes": list(prior), "finish": True}), token)
        assert acknowledgement["asset"] == {"hash": digest, "bytes": len(prior)}
        canvas = fresh()
        created = post("/v1/canvas", {"version": args.world_version, "action": {"kind": "create", "canvas": canvas, "name": "Failure test"}}, token)
        stroke = {"id": fresh(), "canvas": canvas, "owner": profile, "revision": 0,
                  "anchor": {"depth": 0, "x": "0", "y": "0"}, "points": [[1.0, 2.0], [8.0, 9.0]],
                  "width": 2.0, "color": {"rgba": [0.2, 0.3, 0.4, 1.0]}, "erase": False}
        operation = {"id": fresh(), "canvas": canvas, "author": profile, "device": device, "sequence": 1,
                     "authority_epoch": created["info"]["epoch"], "action": {"kind": "add_stroke", "stroke": stroke}}
        assert post("/v1/canvas", {"version": args.world_version, "action": {"kind": "append", "operation": operation}}, token)["kind"] == "accepted"
        before = receipt()
        mismatch = rpc({"kind": "create", "mode": {"kind": "animation", "rotations": 1}, "timer_ms": None})
        mismatch["protocol_version"] = 999
        fault = post("/v1/rpc", mismatch, token, 426)
        assert fault["code"] == "version" and "version" in fault["message"].lower()
        world_status, world_fault = post("/v1/canvas", {"version": 999, "action": {"kind": "list", "after": None, "limit": 10}},
                                        token, (400, 426), observe=True)
        # Record the known pinned-binary gap without hiding independent checks.
        canvas_version_passed = world_status == 426 and world_fault["code"] == "version"
        assert receipt() == before
        # Valid bounded upload requests reserve declared bytes through SQLite
        # zeroblob, so this exercises actual page quota rather than wire limits.
        chunk = bytes([37]) * 32768
        upload = rpc({"kind": "upload", "room": room, "hash": hashlib.sha256(chunk).hexdigest(), "total": 1024 * 1024,
                      "offset": 0, "bytes": list(chunk), "finish": False})
        quota_fault = post("/v1/rpc", upload, token, 413)
        assert quota_fault["code"] == "limit"
        assert "full" in quota_fault["message"].lower() or "quota" in quota_fault["message"].lower()
        assert receipt() == before
        assert app.poll() is None
        stop(app)
        with sqlite3.connect(database) as db:
            assert db.execute("SELECT data FROM objects WHERE hash=?", [digest]).fetchone()[0] == prior
            assert db.execute("SELECT COUNT(*) FROM objects").fetchone()[0] == 1
            pages = db.execute("PRAGMA page_count").fetchone()[0]
            page_size = db.execute("PRAGMA page_size").fetchone()[0]
        app = start(4)
        # Same request ID/content succeeds after configured headroom increases:
        # refusal did not record a successful receipt or partial object.
        retried = post("/v1/rpc", upload, token)
        assert retried == {"kind": "uploaded", "asset": None, "next_offset": len(chunk)}
        with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as db:
            assert db.execute("SELECT data FROM objects WHERE hash=?", [digest]).fetchone()[0] == prior
            assert db.execute("SELECT COUNT(*) FROM world_events").fetchone()[0] == 1
        stop(app)
        original = sha(database)
        corrupt = root / "corrupt"
        corrupt.mkdir()
        corrupt_db = corrupt / "service.sqlite3"
        shutil.copyfile(database, corrupt_db)
        with sqlite3.connect(corrupt_db) as db:
            db.execute("UPDATE objects SET data=zeroblob(total) WHERE hash=?", [digest])
        damaged = sha(corrupt_db)
        refused = subprocess.run([str(service), "--data-dir", str(corrupt), "--bind", "127.0.0.1:0"],
                                 capture_output=True, timeout=15)
        assert refused.returncode != 0 and sha(corrupt_db) == damaged and sha(database) == original
        result.update(passed=canvas_version_passed, independent_storage_checks_passed=True,
                      canvas_protocol_actionable=canvas_version_passed,
                      party_protocol_status=426, canvas_protocol_status=world_status, canvas_protocol_code=world_fault["code"],
                      quota_status=413, quota_fault_code=quota_fault["code"], quota_bytes=1024 * 1024,
                      previous_asset_bytes=len(prior), accepted_strokes=1, unchanged_rows_after_failures=True,
                      same_request_retried_at_4mib=True, previous_asset_survived_restart=True,
                      corrupt_copy_rejected_unchanged=True, original_database_preserved=True,
                      database_bytes_at_rejection=pages * page_size)
    finally:
        for process in processes:
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        for log in logs:
            log.close()
        (root / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(root / "result.json")
    assert result["passed"], "Canvas protocol mismatch still lacks an actionable upgrade response; see result.json"


if __name__ == "__main__":
    main()
