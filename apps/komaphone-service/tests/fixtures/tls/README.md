# Public loopback TLS fixture

This self-signed certificate and its deliberately public private key are synthetic
Komaphone test data. Never use either for a private-network or production listener.
They identify only `localhost` and `127.0.0.1`; tests bind numeric loopback addresses.

Generated with OpenSSL `req -x509 -newkey rsa:2048 -nodes -days 3650`, subject
`CN=Komaphone loopback TEST ONLY`, SAN `DNS:localhost,IP:127.0.0.1`, critical
`basicConstraints=CA:FALSE` and `extendedKeyUsage=serverAuth` on 2026-11-06 UTC.
The certificate expires 2036-11-03 UTC. Recreate this synthetic pair if it expires;
never turn off certificate validation to make the tests pass.

Original project fixture, distributed under MIT OR Apache-2.0; see LICENSE-MIT and
LICENSE-APACHE. It contains no person's name or production credential.

`localhost-private.der` (PKCS#1 private key) and `localhost-public.der` (PKCS#1
public key) are DER exports of this same deliberately public synthetic RSA key.
They exercise actual RS256 signatures in the hosted-login verifier tests, without
introducing a production test-verifier switch or contacting Google. They are
original project test fixtures under the same licenses and are never installed
into a service or client package.
