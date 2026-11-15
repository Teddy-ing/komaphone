#!/usr/bin/env python3
"""No-cloud uploader regressions: create-only streaming and bounded/redacted parsing."""
import importlib.util
import io
import json
import pathlib
import sys
import tempfile
import unittest
from unittest.mock import patch
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("upload", pathlib.Path(__file__).with_name("upload-object.py"))
upload = importlib.util.module_from_spec(spec)
spec.loader.exec_module(upload)

class Response:
    status = 200
    def __init__(self, body):
        self.body = io.BytesIO(body)
    def read(self, size):
        return self.body.read(size)
    def getheader(self, name):
        return "Google"

class Tests(unittest.TestCase):
    def test_streamed_conditional_create_never_gets_object(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "database"
            path.write_bytes(b"synthetic database")
            calls = []
            class Connection:
                def __init__(self, host, timeout):
                    self.host = host
                def request(self, method, target, body=None, headers=None):
                    calls.append((self.host, method, target))
                    self.assert_stream = body
                    if self.host == "storage.googleapis.com":
                        assert hasattr(body, "read") and not isinstance(body, bytes)
                        assert headers["Content-Length"] == str(path.stat().st_size)
                        assert "ifGenerationMatch=0" in target
                def getresponse(self):
                    value = {"access_token": "synthetic-token"} if self.host == "metadata.google.internal" else {"size": str(path.stat().st_size), "generation": "12345"}
                    return Response(json.dumps(value).encode())
                def close(self):
                    pass
            with patch.object(upload.http.client, "HTTPConnection", Connection), patch.object(upload.http.client, "HTTPSConnection", Connection):
                self.assertTrue(upload.upload(path, "synthetic-bucket", "backups/20260101-test/service.sqlite3")["uploaded"])
            self.assertEqual([method for _, method, _ in calls], ["GET", "POST"])
            self.assertEqual(calls[0][0], "metadata.google.internal")

    def test_bad_inputs_fail_before_metadata_access(self):
        with patch.object(upload, "metadata_token", side_effect=AssertionError("No metadata request allowed")):
            for bucket, name in [("invalid/bucket", "backups/x/service.sqlite3"), ("valid-bucket", "elsewhere/service.sqlite3")]:
                with self.assertRaises(ValueError):
                    upload.upload(pathlib.Path("missing"), bucket, name)

    def test_response_limit_is_enforced(self):
        with self.assertRaises(ValueError):
            upload.bounded_json(Response(b" " * 65537))

if __name__ == "__main__":
    unittest.main()
