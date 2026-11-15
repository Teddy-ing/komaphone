#!/usr/bin/env python3
"""One bounded staging issuance/renewal, with an independent exact-config rollback."""
import argparse
import copy
import hashlib
import http.client
import json
import os
import pathlib
import pwd
import socket
import ssl
import subprocess
import time
import uuid

HOST = "komaphone.tedsoftware.org"
CA = "https://acme-staging-v02.api.letsencrypt.org/directory"
CADDY = "/usr/bin/caddy"
PRODUCTION = pathlib.Path("/etc/caddy/Caddyfile")
STORE = pathlib.Path("/var/lib/caddy/.local/share/caddy")
ROOT = pathlib.Path("/var/lib/komaphone-tls-validation")

def run(arguments, timeout=25, check=True, **kwargs):
    return subprocess.run(arguments, check=check, capture_output=True, timeout=timeout, **kwargs)

def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()

def private_hashes():
    return {str(path.relative_to(STORE)): digest(path) for path in sorted(STORE.rglob("*"))
            if path.is_file() and path.suffix in (".key", ".crt", ".json")
            and path.relative_to(STORE).parts[0] in ("acme", "certificates")}

def json_response(connection, method, path, headers=None):
    connection.request(method, path, headers=headers or {})
    response = connection.getresponse()
    body = response.read(1024 * 1024 + 1)
    assert response.status == 200 and len(body) <= 1024 * 1024
    return json.loads(body)

def active_config():
    connection = http.client.HTTPConnection("127.0.0.1", 2019, timeout=10)
    try:
        return json_response(connection, "GET", "/config/")
    finally:
        connection.close()

def trusted_health():
    fingerprint = None
    for path in ("/health", "/v1/auth/config"):
        connection = http.client.HTTPSConnection(HOST, timeout=10, context=ssl.create_default_context())
        raw = socket.create_connection(("127.0.0.1", 443), timeout=10)
        connection.sock = connection._context.wrap_socket(raw, server_hostname=HOST)
        fingerprint = hashlib.sha256(connection.sock.getpeercert(binary_form=True)).hexdigest()
        try:
            assert json_response(connection, "GET", path)["hosted"] is True
        finally:
            connection.close()
    return fingerprint

def port_free():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 49080))

def no_stage_listener(port=49080):
    # A closed HTTP solver can leave TIME_WAIT sockets; those are not listeners.
    assert not run(["ss", "-H", "-ltn", "sport", "=", ":" + str(port)]).stdout.strip()
    with socket.socket() as probe:
        probe.settimeout(2)
        assert probe.connect_ex(("127.0.0.1", port)) != 0

def certificate(path):
    der = run(["openssl", "x509", "-in", str(path), "-outform", "DER"]).stdout
    text = run(["openssl", "x509", "-in", str(path), "-noout", "-serial", "-issuer", "-dates", "-ext", "subjectAltName"]).stdout.decode()
    key = path.with_suffix(".key")
    public_cert = run(["openssl", "x509", "-in", str(path), "-noout", "-pubkey"]).stdout
    public_key = run(["openssl", "pkey", "-in", str(key), "-pubout"]).stdout
    assert public_cert == public_key
    return {"sha256": hashlib.sha256(der).hexdigest(), "public_metadata": text, "key_matches": True}

def account_hashes(stage):
    return {str(path.relative_to(stage)): digest(path) for path in sorted((stage / "data/acme").rglob("*")) if path.is_file()}

def phase(unit, config, stage, marker, original_fingerprint=None):
    started = time.monotonic()
    log = stage / (unit + ".log")
    run(["systemd-run", "--quiet", "--unit=" + unit,
         "--property=Type=simple", "--property=User=caddy", "--property=Group=caddy",
         "--property=RuntimeMaxSec=180s", "--property=TimeoutStopSec=10s", "--property=KillMode=control-group",
         "--property=NoNewPrivileges=true", "--property=ProtectSystem=strict", "--property=ProtectHome=true",
         "--property=PrivateTmp=true", "--property=PrivateDevices=true", "--property=InaccessiblePaths=/var/lib/caddy",
         "--property=ReadWritePaths=" + str(stage), "--property=UMask=0077", "--property=MemoryMax=256M",
         "--property=StandardOutput=append:" + str(log), "--property=StandardError=append:" + str(log),
         "--setenv=XDG_DATA_HOME=" + str(stage / "xdg-data"), "--setenv=XDG_CONFIG_HOME=" + str(stage / "xdg-config"),
         "--setenv=HOME=" + str(stage), CADDY, "run", "--config", str(config)])
    properties = run(["systemctl", "show", unit, "-p", "RuntimeMaxUSec", "-p", "TimeoutStopUSec", "-p", "KillMode", "-p", "InaccessiblePaths"]).stdout.decode()
    assert "RuntimeMaxUSec=3min" in properties and "KillMode=control-group" in properties
    assert "TimeoutStopUSec=10s" in properties and "InaccessiblePaths=/var/lib/caddy" in properties
    try:
        while time.monotonic() - started < 180:
            lines = log.read_text(errors="replace").splitlines() if log.exists() else []
            events = []
            for line in lines:
                try:
                    events.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
            matches = [event for event in events if event.get("msg") == marker and event.get("identifier") == HOST]
            certificates = list((stage / "data/certificates").rglob(HOST + ".crt"))
            if matches and len(certificates) == 1 and certificates[0].with_suffix(".key").exists() and certificates[0].with_suffix(".json").exists():
                observed = certificate(certificates[0])
                if original_fingerprint is None or observed["sha256"] != original_fingerprint:
                    assert len(matches) == 1
                    return {"elapsed_seconds": time.monotonic() - started, "certificate": observed,
                            "success_log": matches[0], "unit_properties": properties}
            state = run(["systemctl", "is-active", unit], check=False).stdout.strip()
            if state not in (b"active", b"activating"):
                raise RuntimeError("Staging process stopped before certificate result")
            time.sleep(0.5)
        raise TimeoutError("Staging phase reached its 180-second limit")
    finally:
        run(["systemctl", "stop", unit], timeout=20, check=False)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--issue-config", required=True, type=pathlib.Path)
    parser.add_argument("--production-candidate", required=True, type=pathlib.Path)
    parser.add_argument("--execute-approved", action="store_true", required=True)
    args = parser.parse_args()
    assert os.geteuid() == 0 and socket.gethostname().split(".")[0] == "komaphone-sync-1"
    version = run([CADDY, "version"]).stdout.decode().strip()
    assert version.startswith("v2.11.9 ")
    port_free()
    ROOT.mkdir(mode=0o755, exist_ok=True)
    root = ROOT / uuid.uuid4().hex
    root.mkdir(mode=0o755)
    control = root / "control"
    control.mkdir(mode=0o700)
    stage = root / "stage"
    stage.mkdir(mode=0o700)
    account = pwd.getpwnam("caddy")
    os.chown(stage, account.pw_uid, account.pw_gid)
    original = PRODUCTION.read_bytes()
    (control / "original.Caddyfile").write_bytes(original)
    adapted = json.loads(run([CADDY, "adapt", "--config", str(PRODUCTION), "--adapter", "caddyfile"]).stdout)
    assert adapted == active_config(), "Production file and active configuration differ"
    before = private_hashes()
    assert before and any(path.endswith(".key") for path in before)
    public_before = trusted_health()
    pid_before = run(["systemctl", "show", "caddy", "-p", "MainPID", "--value"]).stdout.decode().strip()
    issue = json.loads(args.issue_config.read_text())
    assert issue["admin"] == {"disabled": True}
    assert set(issue["apps"]) == {"tls"}
    policy = issue["apps"]["tls"]["automation"]["policies"][0]
    assert policy["subjects"] == [HOST] and len(policy["issuers"]) == 1
    assert policy["issuers"][0]["ca"] == CA and policy["issuers"][0]["test_ca"] == CA
    assert policy["issuers"][0]["challenges"] == {"bind_host": "127.0.0.1", "http": {"alternate_port": 49080}, "tls-alpn": {"disabled": True}}
    assert issue["apps"]["tls"]["automation"]["renew_interval"] == "1h"
    assert len(issue["apps"]["tls"]["automation"]["policies"]) == 1
    assert policy["renewal_window_ratio"] == 0.3333333333333333
    assert issue["apps"]["tls"]["certificates"] == {"automate": [HOST]}
    issue["storage"] = {"module": "file_system", "root": str(stage / "data")}
    renew = copy.deepcopy(issue)
    renew["apps"]["tls"]["automation"]["policies"][0]["renewal_window_ratio"] = 1
    for name, value in (("issue.json", issue), ("renew.json", renew)):
        path = stage / name
        path.write_text(json.dumps(value))
        path.chmod(0o600)
        os.chown(path, account.pw_uid, account.pw_gid)
        run(["runuser", "-u", "caddy", "--", "env", "HOME=" + str(stage), "XDG_DATA_HOME=" + str(stage / "xdg-data"),
             "XDG_CONFIG_HOME=" + str(stage / "xdg-config"), CADDY, "validate", "--config", str(path)])
    candidate = args.production_candidate.read_bytes()
    assert candidate.startswith(original.rstrip() + b"\n"), "Temporary route must append to exact production site"
    temporary = control / "temporary.Caddyfile"
    temporary.write_bytes(candidate)
    run([CADDY, "validate", "--config", str(temporary), "--adapter", "caddyfile"])
    suffix = root.name[:12]
    units = ["komaphone-tls-stage-a-" + suffix, "komaphone-tls-stage-b-" + suffix]
    watchdog = "komaphone-tls-rollback-" + suffix
    restore = control / "restore.sh"
    restore.write_text("#!/bin/bash\nset -eu\nsystemctl stop " + " ".join(units) + " 2>/dev/null || true\n"
                       + "install -o root -g root -m 0644 " + str(control / "original.Caddyfile") + " /etc/caddy/Caddyfile\n"
                       + CADDY + " reload --config /etc/caddy/Caddyfile --adapter caddyfile --address localhost:2019\n")
    restore.chmod(0o700)
    result = {"passed": False, "root": str(root), "caddy_version": version, "caddy_sha256": digest(pathlib.Path(CADDY)),
              "original_config_sha256": hashlib.sha256(original).hexdigest(), "phase_a_config_sha256": digest(stage / "issue.json"),
              "phase_b_config_sha256": digest(stage / "renew.json"), "production_private_hashes_before": before,
              "production_leaf_before": public_before, "natural_production_renewal_tested": False}
    watchdog_started = False
    changed = False
    print(json.dumps({"stage": "validated", "run_directory": str(root)}), flush=True)
    try:
        run(["systemd-run", "--quiet", "--unit=" + watchdog, "--on-active=8min", "--timer-property=AccuracySec=1s",
             "--property=Type=oneshot", "/bin/bash", str(restore)])
        watchdog_started = True
        result["watchdog"] = run(["systemctl", "show", watchdog + ".timer", "-p", "ActiveState", "-p", "TimersMonotonic"]).stdout.decode()
        assert "ActiveState=active" in result["watchdog"]
        PRODUCTION.write_bytes(candidate)
        changed = True
        run([CADDY, "reload", "--config", str(PRODUCTION), "--adapter", "caddyfile", "--address", "localhost:2019"])
        assert trusted_health() == public_before
        for method, path, expected in (("GET", "/.well-known/acme-challenge/komaphone-route-check", 502),
                                      ("POST", "/.well-known/acme-challenge/komaphone-route-check", 308), ("GET", "/health", 308)):
            connection = http.client.HTTPConnection("127.0.0.1", 80, timeout=15)
            try:
                connection.request(method, path, headers={"Host": HOST})
                assert connection.getresponse().status == expected
            finally:
                connection.close()
        result["phase_a"] = phase(units[0], stage / "issue.json", stage, "certificate obtained successfully")
        print(json.dumps({"stage": "initial_staging_issuance_passed"}), flush=True)
        accounts_a = account_hashes(stage)
        assert accounts_a
        result["phase_b"] = phase(units[1], stage / "renew.json", stage, "certificate renewed successfully", result["phase_a"]["certificate"]["sha256"])
        print(json.dumps({"stage": "managed_staging_renewal_passed"}), flush=True)
        result["same_account"] = account_hashes(stage) == accounts_a
        result["same_store"] = True
        result["same_issuer"] = result["phase_a"]["success_log"].get("issuer") == result["phase_b"]["success_log"].get("issuer")
        assert result["same_account"] and result["same_issuer"]
        result["hard_deadline_enforced"] = True
        result["passed"] = True
    except Exception as error:
        result["error_type"] = type(error).__name__
        raise
    finally:
        for unit in units:
            run(["systemctl", "stop", unit], timeout=20, check=False)
        restored = False
        try:
            if changed:
                run(["/bin/bash", str(restore)], timeout=40)
            result["original_config_restored"] = PRODUCTION.read_bytes() == original and active_config() == adapted
            result["production_private_file_hashes_unchanged"] = private_hashes() == before
            result["production_leaf_unchanged"] = trusted_health() == public_before
            result["production_pid_unchanged"] = run(["systemctl", "show", "caddy", "-p", "MainPID", "--value"]).stdout.decode().strip() == pid_before
            no_stage_listener()
            result["no_stage_listener_after"] = True
            result["no_stage_process_after"] = all(run(["systemctl", "show", unit, "-p", "MainPID", "--value"], check=False).stdout.strip() in (b"", b"0") for unit in units)
            result["public_trusted_health_after"] = True
            restored = result["original_config_restored"]
            assert all(result[name] for name in ("original_config_restored", "production_private_file_hashes_unchanged", "production_leaf_unchanged", "production_pid_unchanged", "no_stage_process_after"))
        except Exception:
            result["passed"] = False
            raise
        finally:
            if watchdog_started and restored:
                run(["systemctl", "stop", watchdog + ".timer", watchdog + ".service"], check=False)
            (control / "result.json").write_text(json.dumps(result, indent=2) + "\n")
            print(control / "result.json", flush=True)

if __name__ == "__main__":
    main()
