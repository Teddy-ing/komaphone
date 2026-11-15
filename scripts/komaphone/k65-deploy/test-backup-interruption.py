#!/usr/bin/env python3
"""Synthetic Linux backup interruption tests; no cloud, TLS or real credentials.

Run with Python 3.11+ and Bash on Linux/WSL. The production uploader runs against
real numeric-loopback HTTP after replacing only its metadata-token provider and
HTTPS connection factory. backup.sh is copied with its two absolute roots mapped
into a temporary directory; mountpoint and snapshot creation are fixture adapters.
Its locking, staging, manifest generation, upload order and cleanup run unchanged.
"""
import contextlib
import hashlib
import http.client
import http.server
import importlib.util
import io
import json
import os
import pathlib
import shlex
import socket
import subprocess
import sys
import tempfile
import threading
import unittest
import urllib.parse
from unittest.mock import patch

sys.dont_write_bytecode = True
HERE = pathlib.Path(__file__).resolve().parent
FAILURE = "Backup object creation failed; preserve staging and inspect IAM/connectivity.\n"
PAYLOAD = b"synthetic-backup-data\x00" * (400 * 1024)


def invoke_uploader_main(port):
    """Run unmodified production CLI; prevent every external connection."""
    spec = importlib.util.spec_from_file_location("synthetic_uploader", HERE / "upload-object.py")
    uploader = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(uploader)
    original_http = http.client.HTTPConnection

    def connection(host, timeout):
        if host != "storage.googleapis.com" or not 1024 <= port <= 65535:
            raise ValueError("Unexpected fixture destination")
        return original_http("127.0.0.1", port, timeout=min(timeout, 3))

    with patch.object(uploader, "metadata_token", return_value="synthetic-private-marker"), \
            patch.object(uploader.http.client, "HTTPSConnection", connection):
        return uploader.main()


@contextlib.contextmanager
def receiver(fail_name):
    records = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def setup(self):
            super().setup()
            self.connection.settimeout(3)

        def do_POST(self):
            query = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
            name = query.get("name", [""])[0].rsplit("/", 1)[-1]
            length = int(self.headers.get("Content-Length", "0"))
            if name not in ("service.sqlite3", "manifest.json") or not 0 < length <= len(PAYLOAD) + 4096:
                self.send_error(400)
                return
            record = {"name": name, "expected": length, "received": 0,
                      "conditional_create": query.get("ifGenerationMatch") == ["0"]}
            records.append(record)
            digest = hashlib.sha256()
            remaining = min(4096, length - 1) if name == fail_name else length
            while remaining:
                block = self.rfile.read(min(4096, remaining))
                if not block:
                    break
                digest.update(block)
                record["received"] += len(block)
                remaining -= len(block)
            record["sha256"] = digest.hexdigest()
            if name == fail_name:
                # Actual TCP close after only part of the request body, no ACK.
                self.close_connection = True
                self.connection.shutdown(socket.SHUT_RDWR)
                return
            body = json.dumps({"size": str(length), "generation": "12345"}).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server.server_port, records
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
        if thread.is_alive():
            raise AssertionError("Fixture listener did not stop")


class BackupInterruptionTests(unittest.TestCase):
    def test_real_partial_body_disconnect_is_sanitized_and_source_unchanged(self):
        with tempfile.TemporaryDirectory(prefix="komaphone-upload-cut-") as directory:
            source = pathlib.Path(directory) / "service.sqlite3"
            source.write_bytes(PAYLOAD)
            stdout, stderr = io.StringIO(), io.StringIO()
            with receiver("service.sqlite3") as (port, records), \
                    patch.object(sys, "argv", ["upload-object.py", str(source), "synthetic-bucket", "backups/test/service.sqlite3"]), \
                    contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                result = invoke_uploader_main(port)
            self.assertEqual(result, 1)
            self.assertEqual(stdout.getvalue(), "")
            self.assertEqual(stderr.getvalue(), FAILURE)
            self.assertEqual(source.read_bytes(), PAYLOAD)
            self.assertEqual(len(records), 1)
            self.assertGreater(records[0]["received"], 0)
            self.assertLess(records[0]["received"], records[0]["expected"])
            self.assertTrue(records[0]["conditional_create"])

    def run_backup(self, fail_name):
        original = (HERE / "backup.sh").read_bytes()
        uploader_hash = hashlib.sha256((HERE / "upload-object.py").read_bytes()).hexdigest()
        with tempfile.TemporaryDirectory(prefix="komaphone-backup-cut-") as directory:
            root = pathlib.Path(directory)
            data, current, tools = root / "data", root / "current", root / "tools"
            for path in (data, current, tools):
                path.mkdir()
            source = data / "source.sqlite3"
            source.write_bytes(PAYLOAD)
            snapshot = current / "photocraft-sync"
            snapshot.write_text("#!/usr/bin/env python3\nimport pathlib, shutil, sys\n"
                                "args = sys.argv[1:]\n"
                                "assert args[0] == '--data-dir' and args[2] == '--backup-to'\n"
                                "shutil.copyfile(pathlib.Path(args[1]) / 'source.sqlite3', args[3])\n"
                                "print('{\"backup_verified\":true}')\n", encoding="utf-8")
            snapshot.chmod(0o755)
            mountpoint = tools / "mountpoint"
            mountpoint.write_text("#!/bin/sh\n[ \"$1\" = '-q' ] && [ \"$2\" = " + shlex.quote(str(data)) + " ]\n", encoding="utf-8")
            mountpoint.chmod(0o755)
            # Literal path-only substitutions; no control-flow replacement.
            mapped = original.decode().replace("/var/lib/komaphone", str(data)).replace("/opt/komaphone/current", str(current))
            self.assertNotIn("/var/lib/komaphone", mapped)
            self.assertNotIn("/opt/komaphone/current", mapped)
            script = root / "backup.sh"
            script.write_text(mapped, encoding="utf-8", newline="\n")
            with receiver(fail_name) as (port, records):
                shim = current / "upload-object.py"
                shim.write_text("import importlib.util, sys\nsys.dont_write_bytecode = True\n"
                                f"spec = importlib.util.spec_from_file_location('fixture', {str(pathlib.Path(__file__).resolve())!r})\n"
                                "fixture = importlib.util.module_from_spec(spec)\nspec.loader.exec_module(fixture)\n"
                                f"sys.exit(fixture.invoke_uploader_main({port}))\n", encoding="utf-8")
                env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"],
                           KOMAPHONE_BACKUP_BUCKET="synthetic-bucket", PYTHONDONTWRITEBYTECODE="1")
                result = subprocess.run(["bash", str(script)], env=env, capture_output=True, text=True, timeout=20)
            self.assertEqual(source.read_bytes(), PAYLOAD)
            stages = list(data.glob("backup-staging.*"))
            self.assertEqual((HERE / "backup.sh").read_bytes(), original)
            self.assertEqual(hashlib.sha256((HERE / "upload-object.py").read_bytes()).hexdigest(), uploader_hash)
            self.assertNotIn("synthetic-private-marker", result.stdout + result.stderr)
            if fail_name:
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertEqual(result.stderr, FAILURE)
                self.assertNotIn("Backup uploaded:", result.stdout)
                self.assertEqual(len(stages), 1)
                self.assertEqual((stages[0] / "service.sqlite3").read_bytes(), PAYLOAD)
                manifest = json.loads((stages[0] / "manifest.json").read_text())
                self.assertEqual(manifest["database_sha256"], hashlib.sha256(PAYLOAD).hexdigest())
                self.assertEqual(manifest["database_bytes"], len(PAYLOAD))
                self.assertEqual(manifest["service_sha256"], hashlib.sha256(snapshot.read_bytes()).hexdigest())
                self.assertLess(records[-1]["received"], records[-1]["expected"])
            else:
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stderr, "")
                self.assertIn("Backup uploaded:", result.stdout)
                self.assertEqual(stages, [])
                self.assertTrue(all(row["received"] == row["expected"] for row in records))
            self.assertTrue(all(row["conditional_create"] for row in records))
            return records

    def test_database_upload_failure_never_attempts_manifest_and_keeps_staging(self):
        records = self.run_backup("service.sqlite3")
        self.assertEqual([row["name"] for row in records], ["service.sqlite3"])

    def test_manifest_upload_failure_retains_staging_without_claiming_completion(self):
        records = self.run_backup("manifest.json")
        self.assertEqual([row["name"] for row in records], ["service.sqlite3", "manifest.json"])
        self.assertEqual(records[0]["sha256"], hashlib.sha256(PAYLOAD).hexdigest())

    def test_success_uploads_complete_database_then_manifest_and_cleans_only_staging(self):
        records = self.run_backup(None)
        self.assertEqual([row["name"] for row in records], ["service.sqlite3", "manifest.json"])
        self.assertEqual(records[0]["sha256"], hashlib.sha256(PAYLOAD).hexdigest())


if __name__ == "__main__":
    unittest.main(verbosity=2)
