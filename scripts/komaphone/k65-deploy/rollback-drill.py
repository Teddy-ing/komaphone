#!/usr/bin/env python3
"""Local populated rollback drill; synthetic loopback data, no cloud/provider calls.

Injects an incompatible schema into a stopped disposable candidate database to
model a failed upgrade. Uses the real pinned service for writes, backup, refusal,
restore and hosted session checks. Synthetic sessions are seeded while stopped;
this is not Google login, systemd or production rollback. Optional pinned
--upgrade-service proves actual schema3-to4 migration and old-reader refusal
instead of injecting schema999, then restores the same preupgrade backup.
"""
import argparse
import hashlib
import json
import pathlib
import re
import signal
import socket
import sqlite3
import subprocess
import time
import urllib.error
import urllib.request
import uuid

PIN = "d30065ddafc5d0b7b770692210031803b477e9259a227fb80f597554ba6ca998"
CLIENT = "synthetic-rollback.apps.googleusercontent.com"


def fresh():
    return uuid.uuid4().hex


def sha(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--service", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--expected-service-sha256", default=PIN)
    parser.add_argument("--world-version", type=int, default=1)
    parser.add_argument("--upgrade-service", type=pathlib.Path)
    parser.add_argument("--expected-upgrade-sha256")
    args = parser.parse_args()
    assert 1 <= args.world_version <= 65535, "Invalid fixture world version"
    service = args.service.resolve(strict=True)
    expected_pin = args.expected_service_sha256.lower()
    assert re.fullmatch(r"[0-9a-f]{64}", expected_pin), "Invalid expected binary pin"
    assert sha(service) == expected_pin, "Service binary does not match reviewed pin"
    upgrade = args.upgrade_service.resolve(strict=True) if args.upgrade_service else None
    if upgrade:
        assert args.expected_upgrade_sha256 and re.fullmatch(r"[0-9a-fA-F]{64}", args.expected_upgrade_sha256)
        assert sha(upgrade) == args.expected_upgrade_sha256.lower(), "Upgrade binary pin mismatch"
    root = args.output.resolve() / ("rollback-" + fresh())
    root.mkdir(parents=True, exist_ok=False)
    result = {"passed": False, "service_sha256": expected_pin, "synthetic_only": True,
              "scope": "real SQLite and loopback service; injected incompatible schema; no production/systemd/Google login"}
    processes, handles = [], []
    base = None

    def cli(*arguments, success=True):
        response = subprocess.run([str(service), *map(str, arguments)], capture_output=True, timeout=30)
        assert (response.returncode == 0) == success, "Unexpected CLI result"
        return response

    def start(directory, name, hosted=False):
        nonlocal base
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        base = f"http://127.0.0.1:{port}"
        handle = (root / (name + ".log")).open("wb")
        handles.append(handle)
        arguments = [str(service), "--data-dir", str(directory), "--bind", f"127.0.0.1:{port}"]
        if hosted:
            arguments += ["--google-client-id", CLIENT]
        process = subprocess.Popen(arguments, stdout=handle, stderr=subprocess.STDOUT)
        processes.append(process)
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            assert process.poll() is None, "Service failed before readiness"
            try:
                with urllib.request.urlopen(base + "/health", timeout=1) as response:
                    assert json.load(response)["service"] == "Komaphone"
                    return process
            except (OSError, urllib.error.URLError):
                time.sleep(0.05)
        raise AssertionError("Service readiness timed out")

    def stop(process):
        process.send_signal(signal.SIGINT)
        assert process.wait(timeout=15) == 0

    def post(path, body, token=None, expected=200):
        headers = {"Content-Type": "application/json"}
        if token:
            headers["Authorization"] = "Bearer " + token
        request = urllib.request.Request(base + path, json.dumps(body).encode(), headers, method="POST")
        try:
            response = urllib.request.urlopen(request, timeout=10)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            payload = response.read(4 * 1024 * 1024 + 1)
            assert response.status == expected, f"Unexpected HTTP status {response.status}"
            assert len(payload) <= 4 * 1024 * 1024
        return json.loads(payload)

    def room(action, token=None):
        return post("/v1/rpc", {"protocol_version": 1, "request_id": fresh(), "operation": action}, token)

    def world(action, token, expected=200):
        return post("/v1/canvas", {"version": args.world_version, "action": action}, token, expected)

    try:
        data = root / "candidate-data"
        app = start(data, "populate")
        actors = [{"profile": fresh(), "device": fresh(), "legacy": fresh() + fresh(), "session": fresh() + fresh()} for _ in range(3)]
        for index, actor in enumerate(actors):
            room({"kind": "pair", "participant": {"id": actor["profile"], "device": actor["device"],
                  "display_name": f"Synthetic rollback {index}"}, "credential": actor["legacy"]})
        owner, editor, outsider = actors
        room_id = room({"kind": "create", "mode": {"kind": "animation", "rotations": 1}, "timer_ms": None}, owner["legacy"])["view"]["id"]
        room({"kind": "invite", "room": room_id, "secret": fresh() + fresh(), "expires_in_ms": 60000, "uses": 1}, owner["legacy"])
        canvas = fresh()
        epoch = world({"kind": "create", "canvas": canvas, "name": "Synthetic rollback canvas"}, owner["legacy"])["info"]["epoch"]
        invite, secret = fresh(), fresh() + fresh()
        world({"kind": "invite", "canvas": canvas, "id": invite, "recipient": editor["profile"],
               "secret": secret, "expires_at_ms": int(time.time() * 1000) + 60000}, owner["legacy"])
        editor_epoch = world({"kind": "accept", "canvas": canvas, "id": invite, "secret": secret}, editor["legacy"])["info"]["epoch"]
        world({"kind": "invite", "canvas": canvas, "id": fresh(), "recipient": outsider["profile"],
               "secret": fresh() + fresh(), "expires_at_ms": int(time.time() * 1000) + 60000}, owner["legacy"])
        for index, (actor, authority) in enumerate(((owner, epoch), (editor, editor_epoch))):
            stroke = {"id": fresh(), "canvas": canvas, "owner": actor["profile"], "revision": 0,
                      "anchor": {"depth": 0, "x": "0", "y": "0"}, "points": [[0.0, float(index)], [10.0, float(index + 10)]],
                      "width": 4.0, "color": {"rgba": [0.1, 0.2, 0.3, 1.0]}, "erase": False}
            operation = {"id": fresh(), "canvas": canvas, "author": actor["profile"], "device": actor["device"],
                         "sequence": 1, "authority_epoch": authority, "action": {"kind": "add_stroke", "stroke": stroke}}
            assert world({"kind": "append", "operation": operation}, actor["legacy"])["kind"] == "accepted"
        events_action = {"kind": "events", "canvas": canvas, "after": 0, "through": None, "limit": 128}
        events = world(events_action, owner["legacy"])["events"]
        assert len(events) == 2
        stop(app)
        database = data / "service.sqlite3"
        # Test-only authority setup, never applied to any real service database.
        with sqlite3.connect(database) as db:
            for actor in actors:
                verifier = hashlib.sha256(actor["session"].encode()).hexdigest()
                db.execute("INSERT INTO identity_sessions(verifier,device,expires) VALUES(?,?,?)",
                           (verifier, actor["device"], int(time.time() * 1000) + 3600000))
        app = start(data, "hosted-before", hosted=True)
        assert world(events_action, owner["session"])["events"] == events
        world({"kind": "info", "canvas": canvas}, outsider["session"], 403)
        stop(app)
        original = sha(database)
        backup = root / "pre-update.sqlite3"
        cli("--data-dir", data, "--backup-to", backup)
        assert sha(database) == original
        backup_hash = sha(backup)
        if upgrade:
            with sqlite3.connect(database) as db:
                assert db.execute("PRAGMA user_version").fetchone()[0] == 3
                assert db.execute("SELECT version FROM world_schema").fetchone()[0] == 3
            old_service, old_version = service, args.world_version
            service, args.world_version = upgrade, 2
            app = start(data, "actual-upgrade", hosted=True)
            assert world(events_action, owner["session"])["events"] == events
            world({"kind": "info", "canvas": canvas}, outsider["session"], 403)
            stop(app)
            service, args.world_version = old_service, old_version
            with sqlite3.connect(database) as db, sqlite3.connect(backup) as before:
                assert db.execute("PRAGMA user_version").fetchone()[0] == 4
                assert db.execute("SELECT version FROM world_schema").fetchone()[0] == 4
                for table in ("worlds", "world_events", "world_members", "identity_profiles", "identity_devices"):
                    assert db.execute(f"SELECT * FROM {table} ORDER BY rowid").fetchall() == before.execute(f"SELECT * FROM {table} ORDER BY rowid").fetchall(), table
            migrations = list(data.glob("service.sqlite3.before-world-v4-*.sqlite3"))
            assert len(migrations) == 1
            with sqlite3.connect(migrations[0]) as db:
                assert db.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
                assert db.execute("SELECT version FROM world_schema").fetchone()[0] == 3
            result.update(actual_v3_to_v4_upgrade=True, upgrade_service_sha256=sha(upgrade),
                          migration_backup_sha256=sha(migrations[0]),
                          scope="real pinned K6.5 schema3 upgrade to schema4; old reader refusal and preupgrade restore; synthetic loopback only")
        else:
            with sqlite3.connect(database) as db:
                db.execute("PRAGMA user_version=999")
                db.execute("CREATE TABLE synthetic_failed_upgrade(marker TEXT)")
                db.execute("INSERT INTO synthetic_failed_upgrade VALUES('preserve for diagnosis')")
        failed_hash = sha(database)
        cli("--data-dir", data, "--bind", "127.0.0.1:0", "--google-client-id", CLIENT, success=False)
        assert sha(database) == failed_hash
        restored = root / "rollback-restored"
        cli("--data-dir", restored, "--restore-from", backup)
        assert sha(database) == failed_hash and sha(backup) == backup_hash
        restored_db = restored / "service.sqlite3"
        tables = ("worlds", "world_events", "world_members", "identity_profiles", "identity_devices", "identities", "receipts")
        with sqlite3.connect(backup) as before, sqlite3.connect(restored_db) as after:
            for table in tables:
                assert before.execute(f"SELECT * FROM {table} ORDER BY rowid").fetchall() == after.execute(f"SELECT * FROM {table} ORDER BY rowid").fetchall(), table
            before_room = json.loads(before.execute("SELECT snapshot FROM rooms").fetchone()[0])
            after_room = json.loads(after.execute("SELECT snapshot FROM rooms").fetchone()[0])
            assert before_room["id"] == after_room["id"] and before_room["members"] != []
            assert [m["participant"] for m in before_room["members"]] == [m["participant"] for m in after_room["members"]]
            assert after.execute("SELECT COUNT(*) FROM hosted_profiles WHERE revoked=1").fetchone()[0] == 3
            assert after.execute("SELECT COUNT(*) FROM identity_sessions WHERE revoked=1").fetchone()[0] == 3
            assert after.execute("SELECT COUNT(*) FROM invitations WHERE revoked=0").fetchone()[0] == 0
            assert after.execute("SELECT COUNT(*) FROM world_invites WHERE status=0").fetchone()[0] == 0
        app = start(restored, "hosted-restored", hosted=True)
        world(events_action, owner["session"], 401)
        world(events_action, editor["session"], 401)
        stop(app)
        # A reviewed readmission must not silently resurrect an old session.
        cli("--data-dir", restored, "--admit-profile", owner["profile"])
        app = start(restored, "hosted-readmitted", hosted=True)
        world(events_action, owner["session"], 401)
        stop(app)
        assert sha(database) == failed_hash and sha(backup) == backup_hash
        if upgrade:
            assert sha(migrations[0]) == result["migration_backup_sha256"]
        result.update(passed=True, acknowledged_strokes=2, authors=2, profiles=3,
                      preserved_tables=list(tables), room_identity_and_members_preserved=True,
                      incompatible_schema_rejected_unchanged=True, failed_candidate_preserved=True,
                      backup_preserved=True, restored_profiles_sessions_invites_quarantined=True,
                      valid_session_before_200_restored_401=True, readmission_does_not_revive_session=True,
                      backup_bytes=backup.stat().st_size, backup_sha256=backup_hash,
                      failed_candidate_sha256=failed_hash,
                      canonical_events_sha256=hashlib.sha256(json.dumps(events, sort_keys=True).encode()).hexdigest())
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


if __name__ == "__main__":
    main()
