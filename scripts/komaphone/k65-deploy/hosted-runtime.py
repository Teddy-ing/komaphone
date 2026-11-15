#!/usr/bin/env python3
"""Real Linux binary hosted-mode smoke. Local synthetic data, no provider login."""
import argparse
import hashlib
import importlib.util
import json
import pathlib
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid

sys.dont_write_bytecode = True
here = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("probe", here / "hosted-probe.py")
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--service", required=True, type=pathlib.Path)
parser.add_argument("--output", required=True, type=pathlib.Path)
args = parser.parse_args()
service = args.service.resolve(strict=True)
args.output.mkdir(parents=True, exist_ok=True)
root = pathlib.Path(tempfile.mkdtemp(prefix="hosted-", dir=args.output))
with socket.socket() as reservation:
    reservation.bind(("127.0.0.1", 0))
    port = reservation.getsockname()[1]
base = "http://127.0.0.1:" + str(port)
result = {"passed": False, "service_sha256": hashlib.sha256(service.read_bytes()).hexdigest(),
          "google_login_tested": False, "public_trust_tested": False, "cloud_deployed": False}
process = None


def post(path, body, expected, token=None, origin=None):
    headers = {"Content-Type": "application/json"}
    if token is not None:
        headers["Authorization"] = "Bearer " + token
    if origin is not None:
        headers["Origin"] = origin
    request = urllib.request.Request(base + path, json.dumps(body).encode(), headers)
    try:
        response = urllib.request.urlopen(request, timeout=5)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        assert response.code == expected, (path, response.code, expected)
        data = response.read(65537)
        assert len(data) <= 65536
        return json.loads(data)


try:
    with (root / "service.log").open("wb") as log:
        process = subprocess.Popen([str(service), "--bind", "127.0.0.1:" + str(port), "--data-dir", str(root / "data"),
                                    "--google-client-id", probe.CLIENT_ID], stdout=log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 30
        while True:
            if process.poll() is not None:
                raise AssertionError("service exited before readiness")
            try:
                probe.check(base, probe.CLIENT_ID)
                break
            except (OSError, ValueError, urllib.error.URLError):
                if time.monotonic() >= deadline:
                    raise AssertionError("hosted readiness deadline")
                time.sleep(0.1)
        result["real_hosted_readiness"] = True
        device = uuid.uuid4().hex
        challenge = post("/v1/auth", {"action": "begin", "device": device}, 200)
        assert challenge["kind"] == "challenge" and challenge["client_id"] == probe.CLIENT_ID
        assert len(challenge["nonce"]) == 64
        post("/v1/auth", {"action": "cancel", "transaction": challenge["transaction"], "nonce": challenge["nonce"]}, 200)
        result["real_bounded_login_begin_cancel"] = True
        # Never write secret-bearing request/response bodies to the evidence file.
        denial = post("/v1/auth", {"action": "finish", "transaction": challenge["transaction"], "id_token": "invalid.jwt.signature",
                                  "device_proof": "1" * 64, "label": "Synthetic actor"}, 403)
        assert denial["code"] == "unauthorized"
        pair = {"protocol_version": 1, "request_id": uuid.uuid4().hex,
                "operation": {"kind": "pair", "participant": {"id": uuid.uuid4().hex, "device": device, "display_name": "Synthetic actor"}, "credential": "2" * 64}}
        assert post("/v1/rpc", pair, 403)["code"] == "unauthorized"
        create = {"protocol_version": 1, "request_id": uuid.uuid4().hex, "operation": {"kind": "create", "mode": {"kind": "animation", "rotations": 1}, "timer_ms": None}}
        assert post("/v1/rpc", create, 401, "2" * 64)["code"] == "session"
        post("/v1/auth", {"action": "begin", "device": device}, 403, origin="https://untrusted.invalid")
        result["legacy_pair_bad_token_and_cross_origin_denied"] = True
        admin = subprocess.run([str(service), "--data-dir", str(root / "data"), "--create-admission"], capture_output=True, timeout=10)
        assert admin.returncode != 0 and not admin.stdout and b"already running" in admin.stderr
        result["admin_writer_lock_enforced"] = True
        result["passed"] = True
finally:
    if process is not None and process.poll() is None:
        process.send_signal(signal.SIGINT)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
    (root / "result.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(root / "result.json")
