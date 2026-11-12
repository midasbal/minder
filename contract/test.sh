#!/usr/bin/env bash
# The canonical way to run the whole test suite, reproducibly:
#
#     contract/test.sh
#
# It runs `cargo test --no-fail-fast` (every test binary runs even if one fails,
# so one failure never hides the rest) under the settings in
# `../.cargo/config.toml` (notably NEAR_RPC_TIMEOUT_SECS, which keeps the
# sandbox tests from failing to start under load), prints one line per test
# binary and its result, and exits non-zero if anything failed. Extra arguments
# go to `cargo test`, for example `contract/test.sh --test evm_gate`.
#
# Needs: Rust 1.93+, and `cargo-near` (cargo install --locked cargo-near), which
# the sandbox tests use to build the contract and the test doubles. The first
# run downloads the near-sandbox binary.
set -u
cd "$(dirname "$0")"

if ! command -v cargo-near >/dev/null 2>&1; then
    echo "cargo-near is required: cargo install --locked cargo-near" >&2
    exit 2
fi

log="$(mktemp -t minder-tests.XXXXXX)"
cargo test --no-fail-fast "$@" >"$log" 2>&1
status=$?

grep -E "^\s+Running |^test result:|^test .* FAILED|panicked at|^error" "$log"
if [ "$status" -eq 0 ]; then
    echo "ALL TESTS PASSED (full log: $log)"
else
    echo "TESTS FAILED, exit status $status (full log: $log)" >&2
fi
exit "$status"
