#!/usr/bin/env bash
# Checks the two on-chain claims about the Minder deployment, from the repository:
#
#   1. Code integrity: the wasm built from this repository has the same SHA-256 as the code
#      the chain holds for the contract account.
#   2. Keyless custody: the contract account holds no full-access key, and every key it does
#      hold is a function-call key limited to the account itself and the three request methods.
#
#     scripts/verify-deployment.sh                    # build from a clean copy, then check
#     scripts/verify-deployment.sh --skip-build       # only the checks that need no Rust toolchain
#     scripts/verify-deployment.sh --wasm PATH        # compare a wasm you already have
#     scripts/verify-deployment.sh --account ID --rpc URL
#
# Read-only: it sends no transaction and needs no key. It talks to a public NEAR testnet RPC
# node (view calls only).
#
# Needs: bash, curl, jq, sha256sum, base64, and for the build: git, rustc, cargo and
# cargo-near (`cargo install --locked cargo-near`). The build takes a few minutes.
#
# WHAT THE CODE-INTEGRITY CHECK DOES AND DOES NOT PROVE. A match shows that building this
# repository, with the toolchain you have, produces exactly the bytes the chain runs. It does
# not make the build reproducible, and nothing on the chain says what source produced the
# deployed code: the contract's NEP-330 metadata carries no build info (the script reads and
# prints it). So source-to-bytecode provenance rests on the documented pins: Cargo.lock
# (used with --locked), near-sdk =5.29.1, and rustc 1.94.1 (contract/DESIGN.md, "Mutation
# testing" and "Running"). `cargo near build non-reproducible-wasm` is not a reproducible
# build: a different compiler or a different cargo-near can change the bytes, and so can the
# absolute path of the cargo registry, which the wasm embeds in its panic locations (measured:
# a different cargo home alone gave a different, larger wasm). This script maps your cargo
# home onto the one found in the deployed bytes (rustc's --remap-path-prefix, passed through
# cargo-near's --env) so that the path alone does not change the result. A FAIL here with a
# different toolchain is therefore not by itself evidence that the deployed code differs; a
# PASS is evidence that it does not.
#
# Exit status: 0 every check passed; 1 a check failed; 2 nothing failed but a check was
# skipped, so the result is incomplete; 64 bad usage or a missing tool.

set -u

DEFAULT_ACCOUNT="gate.minder-gate-owner-bf5fcd.testnet"
DEFAULT_RPCS=("https://rpc.testnet.fastnear.com" "https://rpc.testnet.near.org")
# The documented toolchain pin (contract/DESIGN.md).
PINNED_RUSTC="1.94.1"
# The cargo home the deployed wasm was built under (it is embedded in the deployed bytes).
DEPLOYED_CARGO_HOME="/root/.cargo"
# The only methods the agent's key may call (contract/DESIGN.md, "Agent authority"); keep sorted.
AGENT_METHODS='["request_evm_payment","request_evm_token_payment","request_local_transfer"]'

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"

account="$DEFAULT_ACCOUNT"
rpcs=("${DEFAULT_RPCS[@]}")
wasm=""
skip_build=0

die() { echo "error: $*" >&2; exit 64; }

while [ $# -gt 0 ]; do
    case "$1" in
        --account) account="${2:?--account needs a value}"; shift 2 ;;
        --rpc) rpcs=("${2:?--rpc needs a value}"); shift 2 ;;
        --wasm) wasm="${2:?--wasm needs a path}"; shift 2 ;;
        --skip-build) skip_build=1; shift ;;
        -h|--help) sed -n '2,/^[^#]/p' "${BASH_SOURCE[0]}" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
done

case "$account" in
    *.testnet) ;;
    *) die "this script is for testnet accounts; got '$account'" ;;
esac
[ -z "$wasm" ] || [ "$skip_build" -eq 0 ] || die "--wasm and --skip-build do not go together"
[ -z "$wasm" ] || [ -f "$wasm" ] || die "no file at $wasm"

missing=()
for tool in curl jq sha256sum base64; do command -v "$tool" >/dev/null 2>&1 || missing+=("$tool"); done
if [ -z "$wasm" ] && [ "$skip_build" -eq 0 ]; then
    for tool in git rustc cargo cargo-near; do command -v "$tool" >/dev/null 2>&1 || missing+=("$tool"); done
fi
[ ${#missing[@]} -eq 0 ] || die "missing: ${missing[*]} (use --skip-build to run without the Rust toolchain)"

workdir="$(mktemp -d "${TMPDIR:-/tmp}/minder-verify.XXXXXX")"
trap 'rm -rf "$workdir"' EXIT

passed=0; failed=0; skipped=0
report() { # report PASS|FAIL|SKIP "name" "detail"
    case "$1" in PASS) passed=$((passed + 1)) ;; FAIL) failed=$((failed + 1)) ;; SKIP) skipped=$((skipped + 1)) ;; esac
    printf '  [%s] %s\n' "$1" "$2"
    [ -z "${3:-}" ] || printf '         %s\n' "$3"
}

# One NEAR RPC `query`, trying each endpoint in turn. Prints the JSON-RPC response.
rpc_query() { # rpc_query '<params json>'
    local url body
    body="{\"jsonrpc\":\"2.0\",\"id\":\"verify\",\"method\":\"query\",\"params\":$1}"
    for url in "${rpcs[@]}"; do
        if curl -sS --fail -m 60 -H 'content-type: application/json' --data "$body" "$url" 2>/dev/null; then
            return 0
        fi
    done
    return 1
}

echo "Minder deployment check for $account"
echo "  read-only: view calls to a public testnet RPC node; no transaction is sent"
echo

# ---- what the chain says ----

if ! code_json="$(rpc_query "{\"request_type\":\"view_code\",\"finality\":\"final\",\"account_id\":\"$account\"}")" ||
   [ "$(jq -r '.result.code_base64 // empty' <<<"$code_json")" = "" ]; then
    report FAIL "the account's code could be read from the chain" "no code came back (the account is missing, has no contract, or no RPC node answered: ${rpcs[*]})"
    chain_hash=""
else
    jq -r '.result.code_base64' <<<"$code_json" | base64 -d > "$workdir/onchain.wasm"
    chain_hash="$(sha256sum "$workdir/onchain.wasm" | cut -d' ' -f1)"
    echo "  on-chain code: $(wc -c < "$workdir/onchain.wasm" | tr -d ' ') bytes, sha256 $chain_hash"
fi

if keys_json="$(rpc_query "{\"request_type\":\"view_access_key_list\",\"finality\":\"final\",\"account_id\":\"$account\"}")" &&
   [ "$(jq -r '.result.keys | type' <<<"$keys_json")" = "array" ]; then
    echo "  access keys on the account: $(jq '.result.keys | length' <<<"$keys_json")"
    keys_read=1
else
    keys_read=0
fi

if meta_json="$(rpc_query "{\"request_type\":\"call_function\",\"finality\":\"final\",\"account_id\":\"$account\",\"method_name\":\"contract_source_metadata\",\"args_base64\":\"e30=\"}")" &&
   [ "$(jq -r '.result.result // empty' <<<"$meta_json")" != "" ]; then
    meta="$(jq -r '.result.result | implode' <<<"$meta_json")"
    echo "  contract_source_metadata: $meta"
    if [ "$(jq -r '.build_info == null and .link == null' <<<"$meta")" = "true" ]; then
        echo "    -> no build info and no source link: the chain does not say what source produced this code"
    fi
else
    echo "  contract_source_metadata: not exposed by the contract (so no build info on chain either)"
fi
echo

# ---- the build ----

built_hash=""
build_note=""
if [ "$skip_build" -eq 1 ]; then
    build_note="skipped (--skip-build)"
elif [ -n "$wasm" ]; then
    built_hash="$(sha256sum "$wasm" | cut -d' ' -f1)"
    echo "  wasm supplied with --wasm: $wasm"
    echo "  its sha256: $built_hash"
else
    rustc_now="$(rustc --version | awk '{print $2}')"
    echo "  toolchain: rustc $rustc_now; cargo-near: $(cargo near --version 2>/dev/null | head -n 1)"
    echo "  documented pins: rustc $PINNED_RUSTC, near-sdk =5.29.1, Cargo.lock with --locked"
    if [ "$rustc_now" != "$PINNED_RUSTC" ]; then
        echo "  WARNING: rustc $rustc_now is not the documented $PINNED_RUSTC; a different compiler can produce different bytes"
    fi
    [ -z "${RUSTFLAGS:-}${CARGO_ENCODED_RUSTFLAGS:-}" ] || echo "  WARNING: RUSTFLAGS is set; it can change the bytes"
    # The deployed wasm embeds the absolute path of the cargo registry it was built with (in the
    # panic locations of dependencies). The deployment was built with cargo home /root/.cargo; if
    # the deployed bytes really contain that path, map this machine's cargo home onto it, so the
    # path alone cannot change the hash. Nothing is mapped if the deployed bytes do not contain it.
    my_cargo_home="${CARGO_HOME:-$HOME/.cargo}"
    build_env=()
    if [ "$my_cargo_home" != "$DEPLOYED_CARGO_HOME" ] && [ -f "$workdir/onchain.wasm" ] &&
       grep -a -q "$DEPLOYED_CARGO_HOME/registry/src/index.crates.io-" "$workdir/onchain.wasm"; then
        echo "  the deployed code embeds the path $DEPLOYED_CARGO_HOME/registry/...; mapping yours ($my_cargo_home) onto it for the build"
        # cargo-near replaces its default flags when given its own RUSTFLAGS, so the default one
        # (strip, `-C link-arg=-s`) is repeated here; without it the wasm keeps two extra sections.
        build_env=(--env "RUSTFLAGS=-C link-arg=-s --remap-path-prefix=$my_cargo_home=$DEPLOYED_CARGO_HOME")
    fi
    # A clean copy of what is committed, so build artifacts and local edits cannot leak in.
    if ! git -C "$repo" rev-parse --git-dir >/dev/null 2>&1; then
        build_note="this is not a git checkout, so there is no clean tree to build (pass --wasm instead)"
    else
        mkdir -p "$workdir/clean"
        git -C "$repo" archive HEAD contract .cargo | tar -x -C "$workdir/clean"
        echo "  building the production wasm from a clean copy of $(git -C "$repo" rev-parse --short HEAD) (a few minutes)..."
        if (cd "$workdir/clean/contract" && cargo near build non-reproducible-wasm --locked ${build_env[@]+"${build_env[@]}"} --out-dir "$workdir/out") >"$workdir/build.log" 2>&1 &&
           [ -f "$workdir/out/minder.wasm" ]; then
            built_hash="$(sha256sum "$workdir/out/minder.wasm" | cut -d' ' -f1)"
            echo "  built wasm: $(wc -c < "$workdir/out/minder.wasm" | tr -d ' ') bytes, sha256 $built_hash"
        else
            build_note="the build failed; the last lines of its log: $(tail -n 3 "$workdir/build.log" | tr '\n' ' ')"
        fi
    fi
fi
echo

# ---- the checks ----

echo "Checks"

if [ -z "$chain_hash" ]; then
    report SKIP "the built code equals the code on chain" "the on-chain code could not be read"
elif [ -n "$built_hash" ]; then
    if [ "$built_hash" = "$chain_hash" ]; then
        report PASS "the built code equals the code on chain" "both sha256 $chain_hash"
    else
        report FAIL "the built code equals the code on chain" "built $built_hash, on chain $chain_hash. Not by itself proof the deployed code differs: the build is not reproducible across toolchains (see the header of this script)"
    fi
else
    report SKIP "the built code equals the code on chain" "$build_note"
fi

recorded="$(sed -n "s/.*WASM_SHA256 = '\([0-9a-f]\{64\}\)'.*/\1/p" "$repo/web/client/src/links.ts" 2>/dev/null | head -n 1)"
if [ -n "$chain_hash" ] && [ -n "$recorded" ]; then
    if [ "$recorded" = "$chain_hash" ]; then
        report PASS "the on-chain code is the hash the repository records for the deployment" "sha256 $recorded (web/client/src/links.ts)"
    else
        report FAIL "the on-chain code is the hash the repository records for the deployment" "recorded $recorded, on chain $chain_hash"
    fi
fi

if [ "$keys_read" -eq 0 ]; then
    report FAIL "the account's access keys could be read from the chain" "no key list came back"
else
    full="$(jq '[.result.keys[] | select(.access_key.permission == "FullAccess")] | length' <<<"$keys_json")"
    if [ "$full" -eq 0 ]; then
        report PASS "the account holds no full-access key" "so no one can sign as the contract or change its code"
    else
        report FAIL "the account holds no full-access key" "$full full-access key(s): $(jq -r '[.result.keys[] | select(.access_key.permission == "FullAccess") | .public_key] | join(", ")' <<<"$keys_json")"
    fi
    off_policy="$(jq -r --arg account "$account" --argjson methods "$AGENT_METHODS" '
        [ .result.keys[]
          | select(.access_key.permission != "FullAccess")
          | select(.access_key.permission.FunctionCall as $f
              | $f.receiver_id != $account or ($f.method_names | sort) != $methods or $f.allowance == null)
          | .public_key ] | join(", ")' <<<"$keys_json")"
    if [ -z "$off_policy" ]; then
        restricted="$(jq '[.result.keys[] | select(.access_key.permission != "FullAccess")] | length' <<<"$keys_json")"
        report PASS "every key is a function-call key limited to the account and the three request methods, with a finite allowance" "$restricted such key(s)"
    else
        report FAIL "every key is a function-call key limited to the account and the three request methods, with a finite allowance" "outside that: $off_policy"
    fi
fi

echo
if [ "$failed" -gt 0 ]; then
    echo "RESULT: FAIL ($passed passed, $failed failed, $skipped skipped)"
    exit 1
elif [ "$skipped" -gt 0 ]; then
    echo "RESULT: INCOMPLETE ($passed passed, $skipped skipped): nothing failed, but a check did not run"
    exit 2
else
    echo "RESULT: PASS ($passed passed)"
fi
