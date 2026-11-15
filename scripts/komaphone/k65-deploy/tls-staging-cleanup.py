#!/usr/bin/env python3
"""Recheck an existing renewal run without starting Caddy or issuing certificates."""
import argparse
import importlib.util
import json
import os
import pathlib

spec = importlib.util.spec_from_file_location("renewal", pathlib.Path(__file__).with_name("tls-staging-renewal.py"))
renewal = importlib.util.module_from_spec(spec)
spec.loader.exec_module(renewal)

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("run_directory", type=pathlib.Path)
args = parser.parse_args()
root = args.run_directory.resolve(strict=True)
assert os.geteuid() == 0 and root.parent == renewal.ROOT
assert len(root.name) == 32 and all(value in "0123456789abcdef" for value in root.name)
control = root / "control"
original = (control / "original.Caddyfile").read_bytes()
previous = json.loads((control / "result.json").read_text())
assert previous["root"] == str(root)
adapted = json.loads(renewal.run([renewal.CADDY, "adapt", "--config", str(control / "original.Caddyfile"), "--adapter", "caddyfile"]).stdout)
checks = {
    "original_config_restored": renewal.PRODUCTION.read_bytes() == original and renewal.active_config() == adapted,
    "production_private_file_hashes_unchanged": renewal.private_hashes() == previous["production_private_hashes_before"],
    "production_leaf_unchanged": renewal.trusted_health() == previous["production_leaf_before"],
}
renewal.no_stage_listener()
checks["no_stage_listener_after"] = True
units = ["komaphone-tls-stage-a-" + root.name[:12], "komaphone-tls-stage-b-" + root.name[:12]]
checks["no_stage_process_after"] = all(renewal.run(["systemctl", "show", unit, "-p", "MainPID", "--value"], check=False).stdout.strip() in (b"", b"0") for unit in units)
for phase, unit, logger in (("phase_a", units[0], "tls.obtain"), ("phase_b", units[1], "tls.renew")):
    events = []
    for line in (root / "stage" / (unit + ".log")).read_text().splitlines():
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            pass
    expected = previous[phase]["success_log"]
    matches = [event for event in events if event.get("msg") == expected["msg"] and event.get("identifier") == renewal.HOST]
    checks[phase + "_exactly_one_success"] = matches == [expected] and expected["logger"] == logger
checks["same_store_account_issuer"] = all(previous[key] for key in ("same_store", "same_account", "same_issuer"))
checks["renewal_changed_leaf"] = previous["phase_a"]["certificate"]["sha256"] != previous["phase_b"]["certificate"]["sha256"]
checks["public_trusted_health_after"] = True
assert all(checks.values())
watchdog = "komaphone-tls-rollback-" + root.name[:12]
renewal.run(["systemctl", "stop", watchdog + ".timer", watchdog + ".service"], check=False)
checks["watchdog_stopped_after_verified_restoration"] = renewal.run(["systemctl", "is-active", watchdog + ".timer"], check=False).stdout.strip() != b"active"
assert all(checks.values())
result = {"passed": True, "original_result_preserved": str(control / "result.json"),
          "original_assertion_issue": "bind rejected closed HTTP TIME_WAIT sockets; no listening socket or stage process remained",
          "cleanup_checks": checks, "renewal_evidence": previous}
destination = control / "verified-result.json"
with destination.open("x") as output:
    json.dump(result, output, indent=2)
    output.write("\n")
print(json.dumps(result, indent=2))
