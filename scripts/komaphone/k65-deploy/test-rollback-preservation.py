#!/usr/bin/env python3
"""Execute activation in a rewritten temporary filesystem with fake systemctl.

No production paths, network, services or databases are accessed. The fake prior
binary writes a synthetic backup: these tests cover orchestration/preservation,
not SQLite recovery or production rollback.
"""
import hashlib
import os
import pathlib
import shutil
import subprocess
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
UNITS = ("komaphone.service", "komaphone-backup.service", "komaphone-backup.timer")


class RollbackTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="komaphone-rollback-")
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name)
        self.bundle = self.root / "bundle"
        self.bundle.mkdir()
        for name in (*UNITS, "hosted-probe.py", "backup.sh", "upload-object.py"):
            shutil.copyfile(HERE / name, self.bundle / name)
        script = (HERE / "activate-release.sh").read_text()
        self.assertEqual(script.count("$EUID == 0"), 1)
        script = script.replace("$EUID == 0", "1 == 1")
        for path in ("/opt/komaphone", "/etc/komaphone", "/etc/systemd/system", "/var/lib/komaphone"):
            script = script.replace(path, str(self.root) + path)
            (self.root / path.lstrip("/")).mkdir(parents=True, exist_ok=True)
        self.script = self.bundle / "activate-release.sh"
        self.script.write_text(script)
        self.prior = self.root / "opt/komaphone/releases/prior"
        self.prior.mkdir(parents=True)
        self.binary = self.prior / "photocraft-sync"
        self.binary.write_text("#!/bin/bash\nif [[ $1 == --help ]]; then echo --google-client-id; exit; fi\n"
                               "[[ ${FAIL_BACKUP:-0} != 1 ]] || exit 7\n"
                               "[[ $3 == --backup-to ]] || exit 8\nprintf preserved-art > \"$4\"\n")
        self.binary.chmod(0o755)
        (self.root / "opt/komaphone/current").symlink_to(self.prior)
        self.originals = {}
        for name in UNITS:
            path = self.root / "etc/systemd/system" / name
            path.write_text("previous effective " + name)
            self.originals[path] = path.read_bytes()
        for name in ("service.env", "backup.env"):
            path = self.root / "etc/komaphone" / name
            path.write_text("synthetic-private-configuration=" + name)
            self.originals[path] = path.read_bytes()
        self.fake = self.root / "fake-bin"
        self.fake.mkdir()
        self.calls = self.root / "calls"
        self.wrapper("mountpoint", "exit 0")
        self.wrapper("runuser", 'shift 3; exec "$@"')
        self.wrapper("systemctl", 'printf "%s\\n" "$*" >> "$CALLS"\n'
                     '[[ "$*" != "start komaphone.service" || ${FAIL_START:-0} != 1 ]]')
        # Preserve real file copying and permission changes, omit root ownership.
        self.wrapper("install", 'args=(); while (( $# )); do case "$1" in -o|-g) shift 2;; *) args+=("$1"); shift;; esac; done\n'
                     'exec /usr/bin/install "${args[@]}"')

    def wrapper(self, name, body):
        path = self.fake / name
        path.write_text("#!/bin/bash\nset -eu\n" + body + "\n")
        path.chmod(0o755)

    def activate(self, **flags):
        env = dict(os.environ, PATH=str(self.fake) + ":" + os.environ["PATH"], CALLS=str(self.calls), **flags)
        digest = hashlib.sha256(self.binary.read_bytes()).hexdigest()
        return subprocess.run(["bash", str(self.script), str(self.binary), digest], env=env,
                              capture_output=True, text=True, timeout=20)

    def release(self):
        releases = list((self.root / "opt/komaphone/releases").glob("*-*"))
        self.assertEqual(len(releases), 1)
        return releases[0]

    def verify_preserved(self):
        release = self.release()
        snapshot = release / "pre-update-config"
        self.assertEqual(snapshot.stat().st_mode & 0o777, 0o700)
        for path, data in self.originals.items():
            saved = snapshot / path.name
            self.assertEqual(saved.read_bytes(), data)
            self.assertEqual(saved.stat().st_mode & 0o777, 0o600)
        for name in UNITS:
            self.assertEqual((release / name).read_bytes(), (self.bundle / name).read_bytes())
        for directory in (release, snapshot):
            result = subprocess.run(["sha256sum", "--check", "SHA256SUMS"], cwd=directory, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
        return release

    def test_success_retains_effective_config_and_units_without_public_start(self):
        result = self.activate()
        self.assertEqual(result.returncode, 0, result.stderr)
        release = self.verify_preserved()
        backup = pathlib.Path((release / "pre-update-backup.txt").read_text().strip())
        self.assertEqual((backup / "service.sqlite3").read_bytes(), b"preserved-art")
        self.assertEqual((release / "previous-release.txt").read_text().strip(), str(self.prior))
        self.assertNotIn("start caddy", self.calls.read_text())
        self.assertNotIn("synthetic-private-configuration", result.stdout + result.stderr)

    def test_failed_start_keeps_snapshot_and_stops_service_without_automatic_rollback(self):
        result = self.activate(FAIL_START="1")
        self.assertNotEqual(result.returncode, 0)
        release = self.verify_preserved()
        self.assertEqual((self.root / "opt/komaphone/current").resolve(), release)
        self.assertTrue(self.calls.read_text().endswith("stop komaphone.service\n"))
        self.assertNotIn("start caddy", self.calls.read_text())

    def test_failed_backup_preserves_old_units_and_current_release(self):
        result = self.activate(FAIL_BACKUP="1")
        self.assertNotEqual(result.returncode, 0)
        self.verify_preserved()
        self.assertEqual((self.root / "opt/komaphone/current").resolve(), self.prior)
        for path, data in self.originals.items():
            self.assertEqual(path.read_bytes(), data)
        self.assertNotIn("start ", self.calls.read_text())

    def test_symlink_configuration_refused_before_stopping_service(self):
        path = self.root / "etc/systemd/system/komaphone.service"
        path.unlink()
        path.symlink_to(self.root / "etc/komaphone/service.env")
        result = self.activate()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.calls.exists())
        self.assertEqual((self.root / "opt/komaphone/current").resolve(), self.prior)


if __name__ == "__main__":
    unittest.main(verbosity=2)
