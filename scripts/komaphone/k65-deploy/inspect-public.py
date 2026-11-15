#!/usr/bin/env python3
"""Observe ordinary public TLS, HTTP redirect and restricted administration ports."""
import hashlib
import http.client
import json
import socket
import ssl
host = "komaphone.tedsoftware.org"
context = ssl.create_default_context()
with socket.create_connection((host, 443), timeout=10) as raw:
    with context.wrap_socket(raw, server_hostname=host) as secured:
        certificate = secured.getpeercert()
        result = {"host": host, "trusted_tls": True, "tls_version": secured.version(),
                  "certificate_sha256": hashlib.sha256(secured.getpeercert(binary_form=True)).hexdigest(),
                  "issuer": certificate["issuer"], "not_before": certificate["notBefore"], "not_after": certificate["notAfter"],
                  "subject_alt_names": certificate["subjectAltName"]}
connection = http.client.HTTPConnection(host, 80, timeout=10)
try:
    connection.request("GET", "/health")
    response = connection.getresponse()
    result["http_status"] = response.status
    result["http_location"] = response.getheader("Location")
    assert response.status in (301, 308) and result["http_location"] == f"https://{host}/health"
finally:
    connection.close()
result["external_port_denials"] = {}
for port in (22, 43127):
    try:
        with socket.create_connection((host, port), timeout=4):
            raise AssertionError(f"Unexpected public listener on {port}")
    except (TimeoutError, ConnectionRefusedError):
        result["external_port_denials"][str(port)] = True
print(json.dumps(result, indent=2))
