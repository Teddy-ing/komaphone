#!/usr/bin/env python3
"""Create one immutable backup object with metadata credentials kept in memory."""
import argparse
import http.client
import json
import pathlib
import re
import sys
import urllib.parse

MAX_BYTES = 4 * 1024 * 1024 * 1024

def bounded_json(response):
    body = response.read(65537)
    if len(body) > 65536:
        raise ValueError("Response exceeded limit")
    return json.loads(body)

def metadata_token():
    connection = http.client.HTTPConnection("metadata.google.internal", timeout=10)
    try:
        connection.request("GET", "/computeMetadata/v1/instance/service-accounts/default/token", headers={"Metadata-Flavor": "Google"})
        response = connection.getresponse()
        if response.status != 200 or response.getheader("Metadata-Flavor") != "Google":
            raise ValueError("Metadata credential request failed")
        token = bounded_json(response).get("access_token")
        if not isinstance(token, str) or not 1 <= len(token) <= 16384 or not all(33 <= ord(character) <= 126 for character in token):
            raise ValueError("Invalid metadata credential")
        return token
    finally:
        connection.close()

def upload(source, bucket, name):
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{1,61}[a-z0-9]", bucket):
        raise ValueError("Invalid bucket")
    if not re.fullmatch(r"backups/[A-Za-z0-9-]+/(service\.sqlite3|manifest\.json)", name):
        raise ValueError("Invalid backup object name")
    if source.is_symlink() or not source.is_file():
        raise ValueError("Backup source must be a regular file")
    size = source.stat().st_size
    if not 1 <= size <= MAX_BYTES:
        raise ValueError("Backup source exceeded limit")
    token = metadata_token()
    query = urllib.parse.urlencode({"uploadType": "media", "name": name, "ifGenerationMatch": "0"})
    connection = http.client.HTTPSConnection("storage.googleapis.com", timeout=120)
    try:
        with source.open("rb") as stream:
            # http.client streams the open file in bounded blocks; no object GET,
            # listing, redirect, overwrite, administrator credential or whole-file allocation.
            connection.request("POST", f"/upload/storage/v1/b/{bucket}/o?{query}", body=stream,
                               headers={"Authorization": "Bearer " + token,
                                        "Content-Type": "application/octet-stream", "Content-Length": str(size)})
            response = connection.getresponse()
            if response.status not in (200, 201):
                raise ValueError(f"Object create failed with HTTP {response.status}")
            result = bounded_json(response)
            generation = result.get("generation")
            if result.get("size") != str(size) or not isinstance(generation, str) or not generation.isascii() or not generation.isdigit():
                raise ValueError("Upload acknowledgement did not match source size/generation")
            return {"uploaded": True, "bytes": size, "generation": generation}
    finally:
        connection.close()

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=pathlib.Path)
    parser.add_argument("bucket")
    parser.add_argument("object")
    args = parser.parse_args()
    try:
        print(json.dumps(upload(args.source, args.bucket, args.object)))
    except Exception:
        # Never emit HTTP bodies, headers, credential values or raw parser errors.
        print("Backup object creation failed; preserve staging and inspect IAM/connectivity.", file=sys.stderr)
        return 1
    return 0

if __name__ == "__main__":
    sys.exit(main())
