//! The proofs. Each states its property, and says what it bounds and why.
//!
//! The secp256k1 group order and its half are restated here from the
//! standard, not borrowed from `evm.rs`, so a wrong constant in the module
//! cannot hide itself in its own proof.

use crate::evm::{
    encode_signed, encode_unsigned, normalize_s, normalized_signature, try_assemble_signed,
    Eip1559Tx,
};

/// `n`, the order of the secp256k1 group, big-endian.
const N: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe,
    0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36, 0x41, 0x41,
];
/// `floor(n / 2)`, big-endian.
const HALF_N: [u8; 32] = [
    0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0x5d, 0x57, 0x6e, 0x73, 0x57, 0xa4, 0x50, 0x1d, 0xdf, 0xe9, 0x2f, 0x46, 0x68, 0x1b, 0x20, 0xa0,
];

/// A 256-bit big-endian value as two `u128` halves `(high, low)`.
fn halves(value: &[u8; 32]) -> (u128, u128) {
    let high = u128::from_be_bytes(value[..16].try_into().unwrap());
    let low = u128::from_be_bytes(value[16..].try_into().unwrap());
    (high, low)
}

/// `a + b == c` over 256 bits, with no wrap.
fn adds_up(a: &[u8; 32], b: &[u8; 32], c: &[u8; 32]) -> bool {
    let ((ah, al), (bh, bl), (ch, cl)) = (halves(a), halves(b), halves(c));
    let (low, carry) = al.overflowing_add(bl);
    let (high, overflow_a) = ah.overflowing_add(bh);
    let (high, overflow_b) = high.overflowing_add(u128::from(carry));
    !overflow_a && !overflow_b && low == cl && high == ch
}

/// A plain transfer with every field concrete, the base the proofs below vary.
/// (It is a Sepolia transfer of 100 000 gwei; no field is special.)
fn base_tx() -> Eip1559Tx {
    Eip1559Tx {
        chain_id: 11_155_111,
        nonce: 7,
        max_priority_fee_per_gas: 1_500_000_000,
        max_fee_per_gas: 30_000_000_000,
        gas_limit: 21_000,
        to: [0x70; 20],
        value: 100_000_000_000_000,
        data: Vec::new(),
    }
}

/// A scalar `1 <= value < n`, the range `normalize_s` requires.
fn in_range(value: &[u8; 32]) -> bool {
    *value != [0u8; 32] && *value < N
}

// ---- normalize_s ----

/// For every `s` in `1..n` and recovery id 0 or 1, `normalize_s` does not
/// panic, and its output is a valid signature half: `s'` in the lower half of
/// the order (`1 <= s' <= n/2`), `y_parity` 0 or 1. It leaves a low `s` alone
/// and otherwise returns `n - s` with the parity flipped.
#[kani::proof]
#[kani::unwind(40)]
fn normalize_s_is_total_and_always_low_s() {
    let s: [u8; 32] = kani::any();
    let recovery_id: u8 = kani::any();
    kani::assume(in_range(&s));
    kani::assume(recovery_id <= 1);

    let (out, y_parity) = normalize_s(s, recovery_id);

    assert!(out != [0u8; 32], "s' is nonzero");
    assert!(out <= HALF_N, "s' is in the lower half of the order");
    assert!(y_parity <= 1, "y_parity is 0 or 1");
    if s <= HALF_N {
        assert!(
            out == s && y_parity == recovery_id,
            "a low s passes through"
        );
    } else {
        assert!(adds_up(&s, &out, &N), "a high s becomes n - s");
        assert!(y_parity == recovery_id ^ 1, "and the parity flips");
    }
}

// ---- The signing boundary ----

/// The signer's raw output is untrusted. For every point of 0 to 34 bytes, every
/// scalar of 0 to 34 bytes and every recovery id, `normalized_signature` does not
/// panic, and an accepted signature is exactly what `encode_signed` and the
/// authenticity check need: `1 <= r < n`, `1 <= s <= n/2`, `y_parity` 0 or 1.
///
/// Bounded: the lengths stop at 34 because the first check on each is "exactly
/// 33 bytes" and "exactly 32 bytes", so every longer input takes the same
/// early return as 34 does; the bytes themselves are fully symbolic.
#[kani::proof]
#[kani::unwind(40)]
fn normalized_signature_is_total_and_yields_only_valid_signatures() {
    let point_bytes: [u8; 34] = kani::any();
    let scalar_bytes: [u8; 34] = kani::any();
    let point_length: usize = kani::any();
    let scalar_length: usize = kani::any();
    kani::assume(point_length <= 34 && scalar_length <= 34);
    let recovery_id: u8 = kani::any();

    let result = normalized_signature(
        &point_bytes[..point_length],
        &scalar_bytes[..scalar_length],
        recovery_id,
    );

    if let Ok(signature) = result {
        assert!(in_range(&signature.r), "r is in 1..n");
        assert!(
            in_range(&signature.s) && signature.s <= HALF_N,
            "s is in 1..=n/2"
        );
        assert!(signature.y_parity <= 1);
        assert!(recovery_id <= 1);
        assert_eq!(point_length, 33);
        assert_eq!(scalar_length, 32);
        assert!(point_bytes[0] == 0x02 || point_bytes[0] == 0x03);
        assert_eq!(signature.r[..], point_bytes[1..33]);
    }
}

// ---- The transaction encoder ----
//
// What is proved here is the part of the encoder that fits the solver. The rest
// is NOT proved, because CBMC runs out of memory on it (11 GB): the RLP
// integer encoder over a symbolic integer or a symbolic signature (`rlp_uint`
// trims a symbolic number of leading zeros and copies a symbolic-length slice),
// which blocks panic-freedom of `encode_signed` over every `r`, `s` and every
// numeric field, and with it `try_assemble_signed` end to end. Those are
// covered by the differential corpus and the fuzz tests instead (see DESIGN.md).
// What follows holds for the base transaction with one field made symbolic.

fn encodes_without_panicking(tx: &Eip1559Tx) {
    let unsigned = encode_unsigned(tx);
    assert_eq!(unsigned[0], 0x02);
    let signed = encode_signed(tx, [1; 32], [1; 32], 0);
    assert_eq!(signed[0], 0x02);
}

#[kani::proof]
#[kani::unwind(25)]
fn encoding_any_recipient_never_panics() {
    encodes_without_panicking(&Eip1559Tx {
        to: kani::any(),
        ..base_tx()
    });
}

#[kani::proof]
#[kani::unwind(70)]
fn encoding_any_55_bytes_of_data_never_panics() {
    let data: [u8; 55] = kani::any();
    encodes_without_panicking(&Eip1559Tx {
        data: data.to_vec(),
        ..base_tx()
    });
}

#[kani::proof]
#[kani::unwind(70)]
fn encoding_any_56_bytes_of_data_never_panics() {
    let data: [u8; 56] = kani::any();
    encodes_without_panicking(&Eip1559Tx {
        data: data.to_vec(),
        ..base_tx()
    });
}

// ---- The sliding window ----

use crate::window::SpendRing;

/// How many spends each window proof makes. Three against a ring of two
/// buckets (`k` = 1) lets a bucket be reused (the case where an under-count could
/// hide), not only filled.
const SPENDS: usize = 3;

const MAX_TIME: u64 = 1600;

/// An arbitrary history for a window of `K` slots: `slot_len` ns (1 to 8),
/// `SPENDS` spends at arbitrary non-decreasing times below `MAX_TIME` with
/// arbitrary amounts, then a query at an arbitrary later time below it.
/// Returns the ring, the times and amounts, the query time, the slot length and
/// the window length.
///
/// Bounded, because the solver runs out of memory otherwise: `K` is fixed per
/// proof (a symbolic `K` makes the ring's `Vec` symbolic-sized), and so are the
/// number of spends, the slot length and the time range (about 200 slots, many
/// times the ring's span, so every wrap of the ring index and every expiry is
/// reached). The amounts are any `u8`, so that summing the true spend cannot
/// overflow; saturation is a separate matter and itself a fail-safe (a
/// saturated sum exceeds every limit).
fn any_history<const K: u32>() -> (SpendRing, [u64; SPENDS], [u8; SPENDS], u64, u64, u64) {
    let slot_len: u64 = kani::any();
    kani::assume((1..=8).contains(&slot_len));

    let mut ring = SpendRing::new(K);
    let mut times = [0u64; SPENDS];
    let amounts: [u8; SPENDS] = kani::any();
    let mut previous = 0u64;
    for i in 0..SPENDS {
        let time: u64 = kani::any();
        kani::assume(time >= previous && time < MAX_TIME);
        ring.record(time, slot_len, u128::from(amounts[i]));
        times[i] = time;
        previous = time;
    }
    let now: u64 = kani::any();
    kani::assume(now >= previous && now < MAX_TIME);
    (ring, times, amounts, now, slot_len, u64::from(K) * slot_len)
}

fn ring_never_under_counts<const K: u32>() {
    let (ring, times, amounts, now, slot_len, window) = any_history::<K>();
    let counted = ring.live_sum(now, slot_len);
    let mut in_window = 0u128;
    for i in 0..SPENDS {
        if now - times[i] < window {
            in_window += u128::from(amounts[i]);
        }
    }
    assert!(counted >= in_window);
}

fn ring_holds_spend_at_most_one_slot_too_long<const K: u32>() {
    let (ring, times, amounts, now, slot_len, window) = any_history::<K>();
    let counted = ring.live_sum(now, slot_len);
    let mut within_one_slot_late = 0u128;
    for i in 0..SPENDS {
        if now - times[i] < window + slot_len {
            within_one_slot_late += u128::from(amounts[i]);
        }
    }
    assert!(counted <= within_one_slot_late);
}

/// The fail-safe direction the policy relies on: the ring never under-counts.
/// At any time, the spend the ring counts is at least the true spend inside the
/// window, the spends made less than `window_len = k * slot_len` ago. Proved for
/// windows of one, two and three slots.
#[kani::proof]
#[kani::unwind(8)]
fn the_ring_never_under_counts_with_1_slot() {
    ring_never_under_counts::<1>();
}

#[kani::proof]
#[kani::unwind(8)]
fn the_ring_never_under_counts_with_2_slots() {
    ring_never_under_counts::<2>();
}

#[kani::proof]
#[kani::unwind(8)]
fn the_ring_never_under_counts_with_3_slots() {
    ring_never_under_counts::<3>();
}

/// And it forgets on time: the counted spend is at most the spend made less
/// than one slot beyond the window ago, so a spend is held for at most one extra
/// slot width and the window over-enforces by no more than that.
#[kani::proof]
#[kani::unwind(8)]
fn the_ring_holds_spend_at_most_one_slot_too_long_with_1_slot() {
    ring_holds_spend_at_most_one_slot_too_long::<1>();
}

#[kani::proof]
#[kani::unwind(8)]
fn the_ring_holds_spend_at_most_one_slot_too_long_with_2_slots() {
    ring_holds_spend_at_most_one_slot_too_long::<2>();
}

#[kani::proof]
#[kani::unwind(8)]
fn the_ring_holds_spend_at_most_one_slot_too_long_with_3_slots() {
    ring_holds_spend_at_most_one_slot_too_long::<3>();
}

// ---- The policy decision (`policy::evaluate`) ----
//
// `evaluate` is the one function every request passes through. These proofs
// are about the shipped `policy.rs`, included by path, over symbolic status,
// time, expiry and every numeric field.
//
// Bounds, shared by all of them: two fixed concrete keys (the mandate's and
// another); the asset is `Near` or `Evm(chain_id)` (no `String` is involved);
// scope lists hold 0 or 1 entries; the recipient is a NEAR account when the
// asset is `Near`, so the string comparison of account ids is the only one
// (a single fixed pair of names); `allowed_evm_recipients` is `None` (see the
// recipient note below).

use crate::policy::{evaluate, EvalInput, RecipientRef};
use crate::types::{AssetId, AssetPolicy, Decision, DenyReason, Mandate, MandateScope, Status};
use core::cmp::PartialEq;
use near_sdk::json_types::{U128, U64};
use near_sdk::{AccountId, CurveType, PublicKey};

/// Stand-in for `PublicKey`'s derived `==`, used by every `evaluate` proof
/// (`#[kani::stub]`). The derived comparison is a byte-by-byte loop over 33
/// bytes, which forces an unwind of 34 on every loop in the program and made the
/// solver run out of memory with a symbolic key. The proofs use two fixed keys
/// that differ in their first data byte, so comparing the curve tag and that
/// byte decides equality exactly for them. What is trusted instead of proved is
/// that the standard library's derived `==` on a key is a faithful equality.
pub(crate) fn key_equality(left: &PublicKey, right: &PublicKey) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    left[0] == right[0] && left[1] == right[1]
}

pub(crate) fn key(byte: u8) -> PublicKey {
    PublicKey::from_parts(CurveType::ED25519, vec![byte; 32]).unwrap()
}

/// Every symbolic field of one evaluation, plus what to vary.
struct Case {
    status: Status,
    mandate_present: bool,
    revoked: bool,
    expiry: u64,
    now: u64,
    /// 0: no key (a call from another account), 1: the mandate's key, 2: another.
    caller: u8,
    policy_present: bool,
    per_tx_cap: u128,
    window_limit: u128,
    budget: Option<u128>,
    recipient_allowed: bool,
    amount: u128,
    effective_spend: u128,
    epoch_usage: u128,
    // scope
    asset_scope: Option<Option<bool>>, // None: unscoped; Some(None): empty list; Some(Some(b)): list of Near (b) or Evm(1)
    recipient_scope: Option<Option<bool>>, // list of `bob` (true) or `carol` (false)
    scope_cap: Option<u128>,
    asset_is_near: bool,
    recipient_is_bob: bool,
}

fn any_option_bool() -> Option<bool> {
    if kani::any() {
        Some(kani::any())
    } else {
        None
    }
}

/// `RECIPIENT_SCOPE`: whether the scope may list recipients (the one place a
/// string comparison is reached). `ANY_CALLER`: whether the calling key is
/// symbolic, or always the mandate's.
fn any_case<const RECIPIENT_SCOPE: bool, const ANY_CALLER: bool>() -> Case {
    let status = if kani::any() {
        Status::Frozen
    } else {
        Status::Active
    };
    let caller: u8 = if ANY_CALLER { kani::any() } else { 1 };
    kani::assume(caller <= 2);
    Case {
        status,
        mandate_present: kani::any(),
        revoked: kani::any(),
        expiry: kani::any(),
        now: kani::any(),
        caller,
        policy_present: kani::any(),
        per_tx_cap: kani::any(),
        window_limit: kani::any(),
        budget: if kani::any() { Some(kani::any()) } else { None },
        recipient_allowed: kani::any(),
        amount: kani::any(),
        effective_spend: kani::any(),
        epoch_usage: kani::any(),
        asset_scope: if kani::any() {
            Some(any_option_bool())
        } else {
            None
        },
        recipient_scope: if RECIPIENT_SCOPE && kani::any() {
            Some(any_option_bool())
        } else {
            None
        },
        scope_cap: if kani::any() { Some(kani::any()) } else { None },
        asset_is_near: kani::any(),
        recipient_is_bob: kani::any(),
    }
}

// Built without validation (parsing runs a character loop that is not under
// proof here) and of equal length, so the comparison is a real byte comparison.
fn bob() -> AccountId {
    AccountId::new_unvalidated("bob.near".to_string())
}
fn carol() -> AccountId {
    AccountId::new_unvalidated("cat.near".to_string())
}

fn asset_of(near: bool) -> AssetId {
    if near {
        AssetId::Near
    } else {
        AssetId::Evm(1)
    }
}

fn scope_of(case: &Case) -> MandateScope {
    MandateScope {
        allowed_assets: case.asset_scope.map(|entry| match entry {
            None => Vec::new(),
            Some(near) => vec![asset_of(near)],
        }),
        allowed_near_recipients: case.recipient_scope.map(|entry| match entry {
            None => Vec::new(),
            Some(is_bob) => vec![if is_bob { bob() } else { carol() }],
        }),
        allowed_evm_recipients: None,
        per_tx_cap: case.scope_cap.map(U128),
    }
}

/// Run `evaluate` on the case, with the mandate's scope as given.
fn run(case: &Case, scope: MandateScope) -> Decision {
    let mandate = Mandate {
        agent_key: key(1),
        expiry: U64(case.expiry),
        epoch: 1,
        revoked: case.revoked,
        scope,
    };
    let caller = match case.caller {
        0 => None,
        1 => Some(key(1)),
        _ => Some(key(2)),
    };
    let policy = AssetPolicy {
        per_tx_cap: U128(case.per_tx_cap),
        window_limit: U128(case.window_limit),
        epoch_budget: case.budget.map(U128),
    };
    let asset = asset_of(case.asset_is_near);
    let recipient = if case.recipient_is_bob {
        bob()
    } else {
        carol()
    };
    let input = EvalInput {
        status: case.status,
        mandate: if case.mandate_present {
            Some(&mandate)
        } else {
            None
        },
        now: case.now,
        caller_key: caller.as_ref(),
        asset: &asset,
        policy: if case.policy_present {
            Some(&policy)
        } else {
            None
        },
        recipient: RecipientRef::Near(&recipient),
        recipient_allowed: case.recipient_allowed,
        amount: case.amount,
        effective_spend: case.effective_spend,
        epoch_usage: case.epoch_usage,
    };
    evaluate(&input)
}

/// Soundness of Allow. If the decision is Allow, every limit holds with no
/// wraparound (the sums are checked in 256 bits, by widening to the two halves
/// of a `u128` pair), the request is from the mandate's own key under a live,
/// unrevoked mandate and an active contract, the asset has a policy and the
/// recipient is allowlisted, and the mandate's scope (when set) names the asset
/// and recipient and its cap holds.
fn allow_implies_every_limit_holds_body<const R: bool>() {
    let case = any_case::<R, true>();
    let scope = scope_of(&case);
    if run(&case, scope) == Decision::Allow {
        assert!(case.status == Status::Active);
        assert!(case.mandate_present && !case.revoked);
        assert!(case.now < case.expiry);
        assert!(case.caller == 1);
        assert!(case.policy_present);
        assert!(case.recipient_allowed);
        assert!(case.amount <= case.per_tx_cap);
        // `effective_spend + amount <= window_limit`, without overflow:
        // the difference form cannot wrap.
        assert!(case.effective_spend <= case.window_limit);
        assert!(case.amount <= case.window_limit - case.effective_spend);
        if let Some(budget) = case.budget {
            assert!(case.epoch_usage <= budget);
            assert!(case.amount <= budget - case.epoch_usage);
        }
        if let Some(cap) = case.scope_cap {
            assert!(case.amount <= cap);
        }
        if let Some(entry) = case.asset_scope {
            assert!(entry == Some(case.asset_is_near));
        }
        if let Some(entry) = case.recipient_scope {
            assert!(entry == Some(case.recipient_is_bob));
        }
    }
}

#[kani::proof]
#[kani::unwind(12)]
#[kani::stub(<PublicKey as PartialEq>::eq, key_equality)]
fn allow_implies_every_limit_holds() {
    allow_implies_every_limit_holds_body::<false>();
}

/// The same, with the mandate's scope also allowed to list one NEAR recipient
/// (a string comparison of two equal-length account ids).
#[kani::proof]
#[kani::unwind(12)]
#[kani::stub(<PublicKey as PartialEq>::eq, key_equality)]
fn allow_implies_every_limit_holds_with_a_recipient_scope() {
    allow_implies_every_limit_holds_body::<true>();
}

/// A scope only subtracts authority: whatever the scope, if the scoped
/// evaluation allows, the same request under an unscoped mandate also allows.
fn a_scope_never_widens_authority_body<const R: bool>() {
    let case = any_case::<R, true>();
    let scope = scope_of(&case);
    if run(&case, scope) == Decision::Allow {
        assert_eq!(run(&case, MandateScope::default()), Decision::Allow);
    }
}

#[kani::proof]
#[kani::unwind(12)]
#[kani::stub(<PublicKey as PartialEq>::eq, key_equality)]
fn a_scope_never_widens_authority() {
    a_scope_never_widens_authority_body::<false>();
}

/// The same, with the mandate's scope also allowed to list one NEAR recipient
/// (a string comparison of two equal-length account ids).
#[kani::proof]
#[kani::unwind(12)]
#[kani::stub(<PublicKey as PartialEq>::eq, key_equality)]
fn a_scope_never_widens_authority_with_a_recipient_scope() {
    a_scope_never_widens_authority_body::<true>();
}

/// Hard denials dominate: a frozen contract, a revoked or expired mandate, and
/// a call by any key but the mandate's each deny, whatever every other field
/// holds (the policy, the amounts, the scope).
#[kani::proof]
#[kani::unwind(12)]
#[kani::stub(<PublicKey as PartialEq>::eq, key_equality)]
fn frozen_revoked_expired_and_wrong_key_always_deny() {
    let case = any_case::<false, true>();
    let decision = run(&case, scope_of(&case));
    if case.status == Status::Frozen {
        assert_eq!(decision, Decision::Deny(DenyReason::Frozen));
    } else if !case.mandate_present {
        assert_eq!(decision, Decision::Deny(DenyReason::NoMandate));
    } else if case.revoked {
        assert_eq!(decision, Decision::Deny(DenyReason::MandateRevoked));
    } else if case.now >= case.expiry {
        assert_eq!(decision, Decision::Deny(DenyReason::MandateExpired));
    } else if case.caller != 1 {
        assert_eq!(decision, Decision::Deny(DenyReason::NotAuthorized));
    }
}

/// The reason is the documented first failing check: Frozen, NoMandate,
/// MandateRevoked, MandateExpired, NotAuthorized, AssetNotAllowed,
/// RecipientNotAllowed, PerTxCapExceeded, WindowLimitExceeded,
/// MandateBudgetExceeded (the global checks), then AssetOutOfScope,
/// RecipientOutOfScope and the scope's PerTxCapExceeded. The expected reason is
/// computed here as a chain of conditions, restated from DESIGN.md rather than
/// from `policy.rs`.
fn the_deny_reason_is_the_first_failing_check_in_the_documented_order_body<const R: bool>() {
    let case = any_case::<R, true>();
    let decision = run(&case, scope_of(&case));

    let window_ok = case
        .effective_spend
        .checked_add(case.amount)
        .is_some_and(|total| total <= case.window_limit);
    let budget_ok = case.budget.is_none_or(|budget| {
        case.epoch_usage
            .checked_add(case.amount)
            .is_some_and(|total| total <= budget)
    });
    let asset_in_scope = case
        .asset_scope
        .is_none_or(|entry| entry == Some(case.asset_is_near));
    let recipient_in_scope = case
        .recipient_scope
        .is_none_or(|entry| entry == Some(case.recipient_is_bob));
    let scope_cap_ok = case.scope_cap.is_none_or(|cap| case.amount <= cap);

    let expected = if case.status == Status::Frozen {
        Some(DenyReason::Frozen)
    } else if !case.mandate_present {
        Some(DenyReason::NoMandate)
    } else if case.revoked {
        Some(DenyReason::MandateRevoked)
    } else if case.now >= case.expiry {
        Some(DenyReason::MandateExpired)
    } else if case.caller != 1 {
        Some(DenyReason::NotAuthorized)
    } else if !case.policy_present {
        Some(DenyReason::AssetNotAllowed)
    } else if !case.recipient_allowed {
        Some(DenyReason::RecipientNotAllowed)
    } else if case.amount > case.per_tx_cap {
        Some(DenyReason::PerTxCapExceeded)
    } else if !window_ok {
        Some(DenyReason::WindowLimitExceeded)
    } else if !budget_ok {
        Some(DenyReason::MandateBudgetExceeded)
    } else if !asset_in_scope {
        Some(DenyReason::AssetOutOfScope)
    } else if !recipient_in_scope {
        Some(DenyReason::RecipientOutOfScope)
    } else if !scope_cap_ok {
        Some(DenyReason::PerTxCapExceeded)
    } else {
        None
    };
    match expected {
        Some(reason) => assert_eq!(decision, Decision::Deny(reason)),
        None => assert_eq!(decision, Decision::Allow),
    }
}

#[kani::proof]
#[kani::unwind(12)]
#[kani::stub(<PublicKey as PartialEq>::eq, key_equality)]
fn the_deny_reason_is_the_first_failing_check_in_the_documented_order() {
    the_deny_reason_is_the_first_failing_check_in_the_documented_order_body::<false>();
}

/// The same, with the mandate's scope also allowed to list one NEAR recipient
/// (a string comparison of two equal-length account ids).
#[kani::proof]
#[kani::unwind(12)]
#[kani::stub(<PublicKey as PartialEq>::eq, key_equality)]
fn the_deny_reason_is_the_first_failing_check_in_the_documented_order_with_a_recipient_scope() {
    the_deny_reason_is_the_first_failing_check_in_the_documented_order_body::<true>();
}
