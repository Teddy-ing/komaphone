#!/usr/bin/env bash
# Build only the Rust service. Run in an isolated Linux environment with Rust 1.95.
# No system packages, credentials, cloud resources or existing service data are changed.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
cd "$repo"
: "${CARGO_TARGET_DIR:=$repo/target/agent-k6-deployment}"
export CARGO_TARGET_DIR CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2
cargo build --locked -p photocraft-sync --bin photocraft-sync
cargo test --locked -p photocraft-sync --no-fail-fast
