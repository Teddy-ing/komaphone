#!/usr/bin/env python3
"""Validate proxy and unit syntax in a temporary directory; never install/start them."""
import argparse
import json
import os
import pathlib
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--caddy", required=True, type=pathlib.Path)
args = parser.parse_args()
here = pathlib.Path(__file__).resolve().parent
with tempfile.TemporaryDirectory(prefix="komaphone-k65-validate-") as work:
    root = pathlib.Path(work)
    env = dict(os.environ, XDG_DATA_HOME=str(root / "data"), XDG_CONFIG_HOME=str(root / "config"))
    subprocess.run([str(args.caddy.resolve()), "validate", "--config", str(here / "Caddyfile"), "--adapter", "caddyfile"], env=env, check=True)
    stub = root / "stub"
    stub.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    stub.chmod(0o755)
    units = []
    for source in sorted(here.glob("*.service")) + sorted(here.glob("*.timer")):
        text = source.read_text(encoding="utf-8")
        # Syntax-only substitution for executable paths not installed on this test host.
        text = text.replace("/opt/komaphone/current/photocraft-sync", str(stub))
        text = text.replace("/opt/komaphone/current/backup.sh", str(stub))
        target = root / source.name
        target.write_text(text, encoding="utf-8")
        units.append(str(target))
    subprocess.run(["systemd-analyze", "verify", *units], check=True)
    print(json.dumps({"caddy_config_valid": True, "systemd_units_valid": True,
                      "executable_paths_substituted_for_syntax_only": True,
                      "installed_or_started_services": False}))
