#!/usr/bin/env bash
# Keyless deployment of Minder: one transaction that creates the contract
# account, funds it, deploys the wasm and calls `init`, with NO AddKey action,
# so the account never holds a full-access key and nobody (the deployer
# included) can act as it, redeploy it or change its code. The only keys it
# ever holds are the method-restricted function-call keys that the contract
# adds to itself in `grant_mandate`. See DESIGN.md, "Keyless deployment".
#
# It is the same four actions the sandbox test `tests/deploy.rs` sends, in the
# same order: CreateAccount, Transfer, DeployContract, FunctionCall(init).
#
#   deploy/deploy-keyless.sh deploy --signer OWNER --account OWNER_SUB \
#       --fund "10 NEAR" --window-len NS --num-buckets N [options]
#   deploy/deploy-keyless.sh verify --account ACCOUNT
#
# Testnet only: any other network is refused. Without --send, `deploy` only
# prints the command it would run and sends nothing.
#
# Needs: near-cli-rs (`near`), jq, and for `verify` curl. Credentials come from
# the signer's key in the keychain (see --signing); nothing secret is read,
# printed or stored by this script.
set -euo pipefail

TESTNET_RPC_DEFAULT="https://archival-rpc.testnet.fastnear.com/"
# The only methods the agent's function-call key may carry (`AGENT_METHODS` in
# src/lib.rs). `verify` fails on any other key.
AGENT_METHODS_JSON='["request_evm_payment","request_evm_token_payment","request_local_transfer"]'

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

die() { echo "error: $*" >&2; exit 2; }

usage() {
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
    exit "${1:-2}"
}

need() { command -v "$1" >/dev/null 2>&1 || die "$1 is required"; }

# ---- verify ----

# Fails unless the keys on an account are only the contract's own restricted
# function-call keys: no full-access key; every function-call key limited to
# the account itself, to exactly the agent methods, with a finite allowance.
# Reads the `view_access_key_list` result as JSON on stdin.
check_keys() {
    local account="$1"
    local report
    report="$(jq -r --arg account "$account" --argjson methods "$AGENT_METHODS_JSON" '
        .result.keys // error("not a view_access_key_list result")
        | map(
            if .access_key.permission == "FullAccess" then
                {key: .public_key, verdict: "FULL ACCESS"}
            elif (.access_key.permission | type) == "object" and (.access_key.permission | has("FunctionCall")) then
                (.access_key.permission.FunctionCall) as $fc
                | if $fc.receiver_id == $account
                     and (($fc.method_names | sort) == $methods)
                     and ($fc.allowance != null)
                  then {key: .public_key, verdict: "restricted"}
                  else {key: .public_key, verdict: "FUNCTION-CALL KEY OUTSIDE THE AGENT SCOPE"}
                  end
            else
                {key: .public_key, verdict: "UNKNOWN PERMISSION"}
            end)
        | (map(select(.verdict != "restricted")) | length) as $bad
        | (length) as $total
        | (.[] | "  \(.key): \(.verdict)"),
          "keys: \($total), not restricted: \($bad)",
          (if $bad == 0 then "OK" else "FAIL" end)
    ')" || die "could not read the access keys of $account"
    printf '%s\n' "$report"
    [ "$(printf '%s\n' "$report" | tail -n 1)" = "OK" ]
}

cmd_verify() {
    local account="" network="testnet" rpc="" keys_json=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --account) account="$2"; shift 2 ;;
            --network) network="$2"; shift 2 ;;
            --rpc-url) rpc="$2"; shift 2 ;;
            --keys-json) keys_json="$2"; shift 2 ;; # a saved RPC result, for offline checks
            -h|--help) usage 0 ;;
            *) die "unknown option for verify: $1" ;;
        esac
    done
    [ -n "$account" ] || die "--account is required"
    [ "$network" = "testnet" ] || die "testnet only: refusing network '$network'"
    need jq
    if [ -n "$keys_json" ]; then
        check_keys "$account" < "$keys_json"
        return
    fi
    need curl
    rpc="${rpc:-$TESTNET_RPC_DEFAULT}"
    local request
    request="$(jq -cn --arg a "$account" '{jsonrpc:"2.0",id:"minder",method:"query",
        params:{request_type:"view_access_key_list",finality:"final",account_id:$a}}')"
    curl -fsS -X POST -H 'content-type: application/json' --data "$request" "$rpc" \
        | check_keys "$account"
}

# ---- deploy ----

cmd_deploy() {
    local signer="" account="" fund="" window_len="" num_buckets=""
    local timelock_delay="" near_treasury="" evm_treasury="" guardian=""
    local wasm="$here/../target/near/minder.wasm" network="testnet"
    local signing="legacy-keychain" send=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --signer) signer="$2"; shift 2 ;;
            --account) account="$2"; shift 2 ;;
            --fund) fund="$2"; shift 2 ;;
            --window-len) window_len="$2"; shift 2 ;;
            --num-buckets) num_buckets="$2"; shift 2 ;;
            --timelock-delay) timelock_delay="$2"; shift 2 ;;
            --near-treasury) near_treasury="$2"; shift 2 ;;
            --evm-treasury) evm_treasury="$2"; shift 2 ;;
            --guardian) guardian="$2"; shift 2 ;;
            --wasm) wasm="$2"; shift 2 ;;
            --network) network="$2"; shift 2 ;;
            --signing) signing="$2"; shift 2 ;;
            --send) send=1; shift ;;
            -h|--help) usage 0 ;;
            *) die "unknown option for deploy: $1" ;;
        esac
    done

    # Refuse anything that is not what the property needs.
    [ "$network" = "testnet" ] || die "testnet only: refusing network '$network'"
    for required in signer account fund window_len num_buckets; do
        [ -n "${!required}" ] || die "--${required//_/-} is required"
    done
    [ "$signer" != "$account" ] || die "the signer (the owner) must be a different account from the contract account"
    case "$account" in
        *."$signer") ;;
        *) die "the contract account must be a sub-account of the signer (e.g. minder.$signer): only a parent can create it" ;;
    esac
    case "$signing" in
        keychain|legacy-keychain) ;;
        *) die "--signing must be keychain or legacy-keychain" ;;
    esac
    [[ "$window_len" =~ ^[0-9]+$ && "$window_len" -gt 0 ]] || die "--window-len must be a positive number of nanoseconds"
    [[ "$num_buckets" =~ ^[0-9]+$ && "$num_buckets" -ge 1 && "$num_buckets" -le 64 ]] || die "--num-buckets must be 1 to 64"
    [ -z "$timelock_delay" ] || [[ "$timelock_delay" =~ ^[0-9]+$ ]] || die "--timelock-delay must be a number of nanoseconds"
    [ -z "$evm_treasury" ] || [[ "$evm_treasury" =~ ^0x[0-9a-fA-F]{40}$ ]] || die "--evm-treasury must be 0x and 40 hex digits"
    [ "$window_len" -gt 0 ] && [ $((window_len % num_buckets)) -eq 0 ] || die "--window-len must be divisible by --num-buckets"
    need jq
    need near
    [ -f "$wasm" ] || die "no wasm at $wasm (build it: cargo near build non-reproducible-wasm --locked)"
    # The sandbox build carries an owner-only seeding method that must never be
    # deployed; its export name is a plain string in the wasm.
    if grep -aq "test_record_spend" "$wasm"; then
        die "$wasm was built with the test-helpers feature; deploy the production build"
    fi

    # `init`'s arguments. `owner` is the signer: with no key on the new account,
    # the only account that can call `init` in this transaction is the signer,
    # and `init` requires its caller to be the owner.
    local init_args
    init_args="$(jq -cn \
        --arg owner "$signer" --arg window_len "$window_len" --argjson num_buckets "$num_buckets" \
        --arg timelock_delay "$timelock_delay" --arg near_treasury "$near_treasury" \
        --arg evm_treasury "$evm_treasury" --arg guardian "$guardian" \
        '{owner: $owner, window_len: $window_len, num_buckets: $num_buckets}
         + (if $timelock_delay != "" then {timelock_delay: $timelock_delay} else {} end)
         + (if $near_treasury != "" then {near_treasury: $near_treasury} else {} end)
         + (if $evm_treasury != "" then {evm_treasury: ($evm_treasury | ascii_downcase)} else {} end)
         + (if $guardian != "" then {guardian: $guardian} else {} end)')"

    # One transaction, exactly these four actions, no AddKey: CreateAccount,
    # Transfer, DeployContract, then `init` (near-cli-rs sends the init call of
    # `with-init-call` as the FunctionCall action of the same transaction).
    local cmd=(
        near transaction construct-transaction "$signer"
        receiver-id "$account"
        add-action create-account
        add-action transfer "$fund"
        add-action deploy-contract use-file "$wasm"
        with-init-call init json-args "$init_args"
        prepaid-gas '30 Tgas' attached-deposit '0 NEAR'
        skip
        network-config "$network"
        "sign-with-$signing" send
    )

    echo "contract account : $account (a keyless sub-account of $signer)"
    echo "owner            : $signer"
    echo "funding          : $fund"
    echo "wasm             : $wasm"
    echo "init arguments   : $init_args"
    echo "network          : $network"
    echo "actions          : CreateAccount, Transfer, DeployContract, FunctionCall(init); no AddKey"
    echo
    printf 'command:\n  '
    printf '%q ' "${cmd[@]}"
    printf '\n\n'

    if [ "$send" -ne 1 ]; then
        echo "Not sent: pass --send to run it."
        return
    fi
    "${cmd[@]}"
    echo
    echo "Verifying the account holds no full-access key..."
    cmd_verify --account "$account" --network "$network"
}

[ $# -ge 1 ] || usage 2
sub="$1"; shift
case "$sub" in
    deploy) cmd_deploy "$@" ;;
    verify) cmd_verify "$@" ;;
    -h|--help|help) usage 0 ;;
    *) die "unknown command '$sub' (deploy or verify)" ;;
esac
