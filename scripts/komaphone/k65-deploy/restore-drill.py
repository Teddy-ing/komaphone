#!/usr/bin/env python3
"""Restore an exact completed backup into a new isolated directory, never live data."""
import argparse
import hashlib
import importlib.util
import json
import pathlib
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import time

sys.dont_write_bytecode = True
here = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("verify_backup", here / "verify-backup.py")
verification = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verification)
probe_spec = importlib.util.spec_from_file_location("probe", here / "hosted-probe.py")
probe = importlib.util.module_from_spec(probe_spec)
probe_spec.loader.exec_module(probe)
receipt_spec = importlib.util.spec_from_file_location("content_receipt", here / "content-receipt.py")
receipt = importlib.util.module_from_spec(receipt_spec)
receipt_spec.loader.exec_module(receipt)
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--database", required=True, type=pathlib.Path)
parser.add_argument("--manifest", required=True, type=pathlib.Path)
parser.add_argument("--service", required=True, type=pathlib.Path)
parser.add_argument("--output", required=True, type=pathlib.Path)
parser.add_argument("--expectations", type=pathlib.Path)
parser.add_argument("--require-populated", action="store_true")
args = parser.parse_args()
service = args.service.resolve(strict=True)
with service.open("rb") as binary:
    service_hash = hashlib.file_digest(binary, "sha256").hexdigest()
verified = verification.verify(args.database, args.manifest, service_hash)
args.output.mkdir(parents=False, exist_ok=False)
result = {"passed": False, "backup": verified, "original_service_modified": False}
process = None

def content(path):
    expectations = receipt.read_json(args.expectations) if args.expectations else None
    return receipt.inspect(path, expectations, args.require_populated)

try:
    corrupt = args.output / "corrupt.sqlite3"
    shutil.copyfile(args.database, corrupt)
    with corrupt.open("r+b") as file:
        first = file.read(1)
        file.seek(0)
        file.write(bytes([first[0] ^ 1]))
    for database, manifest in ((corrupt, args.manifest), (args.database, args.output / "missing-manifest.json")):
        try:
            verification.verify(database, manifest, service_hash)
            raise AssertionError("Corrupt/incomplete backup was accepted")
        except ValueError:
            pass
    result["corrupt_and_incomplete_refused_before_restore"] = True
    restored = args.output / "restored"
    completed = subprocess.run([str(service), "--data-dir", str(restored), "--restore-from", str(args.database)], capture_output=True, timeout=60)
    assert completed.returncode == 0, "Verified restore command failed"
    result["content"] = content(args.database.resolve(strict=True))
    result["restored_content"] = content((restored / "service.sqlite3").resolve(strict=True))
    result["content_comparison"] = receipt.compare_restored(args.database, restored / "service.sqlite3", result["content"], result["restored_content"])
    with sqlite3.connect((restored / "service.sqlite3").as_uri() + "?mode=ro", uri=True) as database:
        assert database.execute("SELECT COUNT(*) FROM hosted_profiles WHERE revoked=0").fetchone()[0] == 0
        assert database.execute("SELECT COUNT(*) FROM identity_sessions WHERE revoked=0").fetchone()[0] == 0
        assert database.execute("SELECT COUNT(*) FROM hosted_admissions WHERE revoked=0").fetchone()[0] == 0
        assert database.execute("SELECT COUNT(*) FROM invitations WHERE revoked=0").fetchone()[0] == 0
        assert database.execute("SELECT COUNT(*) FROM world_invites WHERE status=0").fetchone()[0] == 0
        assert database.execute("SELECT COUNT(*) FROM identity_logins").fetchone()[0] == 0
        result["profiles"] = database.execute("SELECT COUNT(*) FROM hosted_profiles").fetchone()[0]
    result["restored_access_quarantined"] = True
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    with (args.output / "restored-service.log").open("wb") as log:
        process = subprocess.Popen([str(service), "--data-dir", str(restored), "--bind", f"127.0.0.1:{port}", "--google-client-id", probe.CLIENT_ID], stdout=log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 30
        while True:
            assert process.poll() is None, "Isolated restored service exited"
            try:
                probe.check(f"http://127.0.0.1:{port}", probe.CLIENT_ID)
                break
            except Exception:
                if time.monotonic() >= deadline:
                    raise ValueError("Restored readiness deadline")
                time.sleep(0.1)
        subprocess.run([sys.executable, str(here / "verify-live.py"), "--base-url", f"http://127.0.0.1:{port}"], check=True, capture_output=True, timeout=60)
    result["isolated_hosted_readiness_and_denials"] = True
    result["passed"] = True
finally:
    if process is not None and process.poll() is None:
        process.send_signal(signal.SIGINT)
        process.wait(timeout=15)
    (args.output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(args.output / "result.json")
