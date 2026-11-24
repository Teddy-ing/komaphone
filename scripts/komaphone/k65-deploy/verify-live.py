#!/usr/bin/env python3
"""Check a live hosted service without enrolling a profile or retaining login proofs."""
import argparse
import importlib.util
import json
import pathlib
import sys
import urllib.error
import urllib.request
import uuid

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("probe", pathlib.Path(__file__).with_name("hosted-probe.py"))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--base-url", default="http://127.0.0.1:43127")
args = parser.parse_args()
probe.check(args.base_url, probe.CLIENT_ID)
health = probe.get_json(args.base_url, "/health")
rpc_protocol_version = int(health.get("protocol_version", 1))

def post(path, body, expected, token=None, origin=None):
    headers = {"Content-Type": "application/json"}
    if token is not None:
        headers["Authorization"] = "Bearer " + token
    if origin is not None:
        headers["Origin"] = origin
    request = urllib.request.Request(args.base_url + path, json.dumps(body).encode(), headers)
    try:
        response = urllib.request.urlopen(request, timeout=10)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        if response.code != expected:
            raise ValueError(f"Unexpected HTTP status for {path}: {response.code}, expected {expected}")
        data = response.read(65537)
        if len(data) > 65536:
            raise ValueError("Response too large")
        return json.loads(data)

device = uuid.uuid4().hex
challenge = post("/v1/auth", {"action": "begin", "device": device}, 200)
assert challenge["kind"] == "challenge" and challenge["client_id"] == probe.CLIENT_ID
assert len(challenge["nonce"]) == 64
post("/v1/auth", {"action": "cancel", "transaction": challenge["transaction"], "nonce": challenge["nonce"]}, 200)
assert post("/v1/auth", {"action": "finish", "transaction": challenge["transaction"], "id_token": "invalid.jwt.signature",
                         "device_proof": "1" * 64, "label": "Synthetic denied actor"}, 403)["code"] == "unauthorized"
pair = {"protocol_version": rpc_protocol_version, "request_id": uuid.uuid4().hex,
        "operation": {"kind": "pair", "participant": {"id": uuid.uuid4().hex, "device": device, "display_name": "Synthetic denied actor"}, "credential": "2" * 64}}
assert post("/v1/rpc", pair, 403)["code"] == "unauthorized"
create = {"protocol_version": rpc_protocol_version, "request_id": uuid.uuid4().hex,
          "operation": {"kind": "create", "mode": {"kind": "animation", "rotations": 1}, "timer_ms": None}}
assert post("/v1/rpc", create, 401, "2" * 64)["code"] == "session"
post("/v1/auth", {"action": "begin", "device": device}, 403, origin="https://untrusted.invalid")
print(json.dumps({"passed": True, "base_url": args.base_url, "hosted_readiness": True,
                  "begin_cancel": True, "invalid_token_legacy_pair_unknown_bearer_cross_origin_denied": True,
                  "profile_enrolled": False, "real_google_login_tested": False}))
