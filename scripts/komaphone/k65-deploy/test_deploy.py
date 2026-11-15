#!/usr/bin/env python3
"""Local safety regressions. Uses fake gcloud and loopback HTTP, never cloud access."""
import contextlib
import http.server
import importlib.util
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import threading
import unittest

HERE = pathlib.Path(__file__).resolve().parent
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("hosted_probe", HERE / "hosted-probe.py")
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


@contextlib.contextmanager
def server(health, config):
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            body = json.dumps(health if self.path == "/health" else config).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_args):
            pass

    instance = http.server.HTTPServer(("127.0.0.1", 0), Handler)
    worker = threading.Thread(target=instance.serve_forever, daemon=True)
    worker.start()
    try:
        yield "http://127.0.0.1:" + str(instance.server_port)
    finally:
        instance.shutdown()
        worker.join(timeout=5)
        instance.server_close()


class DeployTests(unittest.TestCase):
    def test_all_shell_scripts_parse(self):
        for script in HERE.glob("*.sh"):
            result = subprocess.run(["bash", "-n", str(script)], capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0, str(script) + ": " + result.stderr)

    def run_script(self, mode, approval=None, collision=False, fail=False):
        with tempfile.TemporaryDirectory(prefix="komaphone-deploy-test-") as name:
            temp = pathlib.Path(name)
            fake = temp / "gcloud"
            fake.write_text("""#!/usr/bin/env python3
import json, os, sys
with open(os.environ['CALLS'], 'a') as output:
    output.write(json.dumps(sys.argv[1:]) + '\\n')
if os.environ.get('FAIL') == '1':
    sys.exit(17)
if os.environ.get('COLLISION') == '1' and 'list' in sys.argv:
    print('komaphone-existing-art')
""", encoding="utf-8")
            fake.chmod(0o755)
            env = dict(os.environ, PATH=str(temp) + os.pathsep + os.environ["PATH"],
                       CALLS=str(temp / "calls"), COLLISION=str(int(collision)), FAIL=str(int(fail)))
            env.pop("KOMAPHONE_APPROVED_PROJECT", None)
            if approval is not None:
                env["KOMAPHONE_APPROVED_PROJECT"] = approval
            result = subprocess.run(["bash", str(HERE / "gcp-resources.sh"), mode], env=env,
                                    capture_output=True, text=True, timeout=20)
            calls = [json.loads(line) for line in (temp / "calls").read_text().splitlines()] if (temp / "calls").exists() else []
            return result, calls

    def test_plan_never_calls_gcloud_even_with_approval(self):
        result, calls = self.run_script("--plan", "komaphone")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, [])
        self.assertIn("--disabled", result.stdout)
        self.assertIn("auto-delete=no", result.stdout)

    def test_apply_requires_exact_project_approval_before_any_call(self):
        for approval in (None, "wrong-project"):
            result, calls = self.run_script("--apply", approval)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(calls, [])

    def test_existing_art_refuses_all_resource_creation(self):
        result, calls = self.run_script("--apply", "komaphone", collision=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any("create" in call or "update" in call for call in calls))

    def test_permission_failure_stops_before_resource_creation(self):
        result, calls = self.run_script("--apply", "komaphone", fail=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(calls), 1)

    def test_apply_scopes_every_call_and_keeps_public_ingress_disabled(self):
        result, calls = self.run_script("--apply", "komaphone")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(all("--project=komaphone" in call for call in calls))
        self.assertFalse(any("config" in call for call in calls))
        web = [call for call in calls if "komaphone-web" in call]
        self.assertEqual(len(web), 1)
        self.assertIn("--disabled", web[0])
        iam = [call for call in calls if "add-iam-policy-binding" in call]
        self.assertEqual(len(iam), 1)
        self.assertIn("--role=roles/storage.objectCreator", iam[0])
        self.assertIn("buckets", iam[0])

    def test_probe_accepts_matching_hosted_service(self):
        with server({"hosted": True}, {"hosted": True, "protocol_version": 1, "client_id": probe.CLIENT_ID}) as base:
            probe.check(base, probe.CLIENT_ID)

    def test_probe_rejects_legacy_and_wrong_client(self):
        fixtures = [({}, {"hosted": True, "protocol_version": 1, "client_id": probe.CLIENT_ID}),
                    ({"hosted": True}, {"hosted": False, "protocol_version": 1, "client_id": probe.CLIENT_ID}),
                    ({"hosted": True}, {"hosted": True, "protocol_version": 2, "client_id": probe.CLIENT_ID}),
                    ({"hosted": True}, {"hosted": True, "protocol_version": 1, "client_id": "other-client"})]
        for health, config in fixtures:
            with self.subTest(health=health, config=config), server(health, config) as base:
                with self.assertRaises(ValueError):
                    probe.check(base, probe.CLIENT_ID)

    def test_probe_rejects_untrusted_http_and_url_credentials(self):
        for url in ("http://example.com", "https://secret@example.com", "https://example.com/?token=secret"):
            result = subprocess.run(["python3", str(HERE / "hosted-probe.py"), "--base-url", url],
                                    capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main(verbosity=2)
