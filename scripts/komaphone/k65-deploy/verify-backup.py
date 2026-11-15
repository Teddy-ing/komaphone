#!/usr/bin/env python3
"""Require the completion manifest and exact immutable database hash before restore."""
import argparse
import hashlib
import json
import pathlib
import re
import sys

def verify(database, manifest, service_sha256):
    if manifest.is_symlink() or database.is_symlink() or not manifest.is_file() or not database.is_file():
        raise ValueError("Missing regular backup or completion manifest")
    with manifest.open("rb") as source:
        body = source.read(65537)
    if len(body) > 65536:
        raise ValueError("Completion manifest too large")
    expected = json.loads(body)
    if expected.get("format") != 1 or expected.get("service_sha256") != service_sha256:
        raise ValueError("Backup version or binary pin mismatch")
    digest = expected.get("database_sha256")
    size = expected.get("database_bytes")
    if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest) or type(size) is not int or not 1 <= size <= 4 * 1024**3:
        raise ValueError("Invalid backup size or hash")
    if database.stat().st_size != size:
        raise ValueError("Backup size mismatch")
    with database.open("rb") as source:
        actual = hashlib.file_digest(source, "sha256").hexdigest()
    if actual != digest:
        raise ValueError("Backup hash mismatch")
    return {"verified": True, "database_bytes": size, "database_sha256": actual, "service_sha256": service_sha256}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("database", type=pathlib.Path)
    parser.add_argument("manifest", type=pathlib.Path)
    parser.add_argument("service_sha256")
    args = parser.parse_args()
    try:
        print(json.dumps(verify(args.database, args.manifest, args.service_sha256)))
    except Exception:
        print("Backup verification failed; do not restore or expose this copy.", file=sys.stderr)
        return 1
    return 0

if __name__ == "__main__":
    sys.exit(main())
