# Komaphone room client

`photocraft-room-client` is the native L4 background transport and durable outbox for K2.
Its portable status types compile for WASM; browser networking and credentials are not
implemented. The UI thread never waits on HTTP, SQLite or Credential Manager.

The native worker uses a reused blocking reqwest client on its own thread, a bounded
eight-command queue, 250 ms active-room polling (also interleaved during upload), and
bounded backoff after service failure. HTTP is permitted only for numeric loopback origins.
Other endpoints require HTTPS with certificate/name verification. Redirects, proxy
inheritance, request compression, URL credentials, queries, fragments and path prefixes
are disabled/rejected. An explicit trusted PEM can be injected by the native host;
certificate checks cannot be disabled.

`Outbox` stores its stable participant/device, authorized room projection and exact pending
requests in SQLite DELETE/EXTRA transactions. Pairing credentials and invitations use the
safe [keyring 3.6.3 Windows adapter](https://docs.rs/crate/keyring/3.6.3/source/README.md)
backed by Windows Credential Manager. There is no plaintext fallback. The reviewed
dependency is MIT/Apache-2.0; native details are target-gated. Each isolated database has
its own cryptographically random credential namespace. Token bytes do not enter SQLite,
room status, command journals, ordinary errors or logs. Intentional invitation sharing
uses a separate engine command and in-memory UI value.

Requests are committed locally before transmission. Network failure leaves them queued;
an authority rejection retains the exact operation and blocks automatic resubmission.
Retry resends the original ID/body; **Keep rejected work and refresh** retains a recovery
copy of the entire remaining pending batch while allowing the user to review current room
state and create a new request. Dependent upload chunks are not sent after their failed
predecessor has been retained.
Accepted responses update the local projection and remove the acknowledged request in
one transaction. Lost replies therefore cause an identical retry. The local drawing store
remains independently durable and supplies exact acknowledged submission revisions.

At a timer deadline the worker polls even while uploading, durably inserts the exact
queued manifest's FreezeReport ahead of its Submit, and persists the accepted freeze.
It does not replace its ID, artwork, note or revision. The engine separately freezes the
last acknowledged assigned draft when no submission is already queued. Before preparing
that package, it commits a `FreezePin` containing the room, epoch, assignment, project and
first acknowledged revision. Restart during heavy preparation reuses this exact pin, even
if a later local save finished afterward. Pending submission projects are indexed separately
from metadata requests, so an offline Pause request does not freeze ordinary drawing.
Correlated worker command sequences keep an older status response from clearing a newer
submission's transient edit lock. The service
enforces epoch/assignment/immutable-content checks; room metadata/moderation still use
revision preconditions.

Operational limits: 512 MiB SQLite database; 384 MiB serialized live/retained requests;
8192 requests; 256 KiB request and bounded 4.25 MiB response; 32 KiB binary upload chunks;
64 MiB per object and 512 MiB raw submission manifest budget. JSON overhead can exhaust
the outbox before the raw maximum. Limit/storage failure is explicit and leaves the
original local draft intact. No rejected or unacknowledged work is automatically pruned.
This store is recovery, not a portable credential/device cloning or backup format.

Tests use fresh roots. `cargo test -p photocraft-room-client` covers exact retry/restart,
secret separation, wrong acknowledgments, corruption/schema/byte limits, real Windows
credential readback+cleanup, transport limits/redirect rejection, and actual process kills
after a local pending acknowledgment and during an uncommitted write. The process test's
authority/credential data is a fixture; separate real-service and native multi-client
harnesses supply network acceptance evidence. On restricted Windows runners the native
tests require access to loopback sockets and Credential Manager; blocked tests are not
passes. `cargo clippy -p photocraft-room-client --all-targets -- -D warnings` is the lint gate.

After stopping **isolated test** app instances, remove only their own credentials with:

```powershell
cargo run -p photocraft-room-client --example cleanup_credentials -- --discard-isolated-test-credentials C:/absolute/isolated-test/rooms.sqlite3
```

This explicit command invalidates that data root's credentials, retains its database,
and prints only a count. Never use it on a profile you intend to reconnect.
