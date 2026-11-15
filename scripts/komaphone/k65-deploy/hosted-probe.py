#!/usr/bin/env python3
"""Fail-closed, credential-free hosted readiness check (loopback by default)."""
import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

CLIENT_ID = "981926394354-07f4s3dss4vgp7mteh0qe8e6dhk3s99o.apps.googleusercontent.com"


def get_json(base, path):
    with urllib.request.urlopen(base + path, timeout=5) as response:
        raw = response.read(65537)
        if len(raw) > 65536:
            raise ValueError("readiness response exceeds bound")
        if response.status != 200:
            raise ValueError("readiness response is not HTTP 200")
        return json.loads(raw)


def check(base, client_id):
    health = get_json(base, "/health")
    config = get_json(base, "/v1/auth/config")
    if health.get("hosted") is not True:
        raise ValueError("service is not in hosted mode")
    if config.get("hosted") is not True or config.get("protocol_version") != 1:
        raise ValueError("unsupported hosted authentication protocol")
    if config.get("client_id") != client_id:
        raise ValueError("OAuth client does not match this release")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:43127")
    parser.add_argument("--wait-seconds", type=int, default=0)
    args = parser.parse_args()
    url = urllib.parse.urlsplit(args.base_url)
    if url.username or url.password or url.query or url.fragment or url.path:
        parser.error("base URL must contain only scheme, host and optional port")
    if url.scheme != "https" and not (url.scheme == "http" and url.hostname in ("127.0.0.1", "::1")):
        parser.error("non-loopback checks require trusted HTTPS")
    if not 0 <= args.wait_seconds <= 60:
        parser.error("wait must be 0 through 60 seconds")
    deadline = time.monotonic() + args.wait_seconds
    while True:
        try:
            check(args.base_url, os.environ.get("KOMAPHONE_GOOGLE_CLIENT_ID", CLIENT_ID))
            print(json.dumps({"hosted_ready": True, "protocol_version": 1}))
            return 0
        except (OSError, ValueError, urllib.error.URLError) as error:
            if time.monotonic() >= deadline:
                print("Hosted readiness failed: " + str(error), file=sys.stderr)
                return 1
            time.sleep(0.5)


if __name__ == "__main__":
    sys.exit(main())
