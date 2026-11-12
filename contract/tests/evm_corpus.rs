//! Differential test of `src/evm.rs` against viem over a large, seeded corpus
//! (`vectors/evm-corpus.json`, produced by `vectors/gen-evm-corpus.mjs`; the
//! fixture's header records the pinned viem version, the seed and the exact
//! regeneration command). Hermetic: the corpus is compiled in, so the test
//! needs no Node and no network.
//!
//! For every case it asserts, byte for byte against viem:
//! - `encode_unsigned` equals viem's unsigned serialization,
//! - `signing_hash` equals viem's keccak of those bytes,
//! - `encode_signed` and `try_assemble_signed` on the canonical signature
//!   equal viem's signed serialization,
//! - for cases with a high-s form, `normalize_s` maps it back to the canonical
//!   `(s, y_parity)` and `try_assemble_signed` on it yields the same signed
//!   bytes.
//!
//! - for every ERC-20 transfer case, `erc20_transfer_calldata` built from the
//!   recipient and amount that viem's ABI encoder put in the data reproduces
//!   viem's calldata byte for byte, and the transaction (data included) hashes
//!   and serializes as viem does.
//!
//! It also asserts the corpus really contains the boundary cases it claims, so
//! it cannot pass vacuously.

use std::collections::HashSet;
use std::str::FromStr;

use minder::evm::{
    encode_signed, encode_unsigned, erc20_transfer_calldata, hex_decode, normalize_s, signing_hash,
    try_assemble_signed, Eip1559Tx,
};
use near_sdk::serde_json::{self, Value};

const CORPUS: &str = include_str!("../vectors/evm-corpus.json");
const PINNED_VIEM: &str = "2.57.16";

/// secp256k1 order and half of it (rounded down), restated here so the test
/// does not borrow the constants it is checking.
const ORDER: &str = "fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141";
const HALF_ORDER: &str = "7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0";
const HALF_ORDER_PLUS_ONE: &str =
    "7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a1";
const ORDER_MINUS_ONE: &str = "fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364140";

struct Case {
    name: String,
    kind: String,
    tx: Eip1559Tx,
    unsigned: Vec<u8>,
    signing_hash: [u8; 32],
    r: [u8; 32],
    s: [u8; 32],
    y_parity: u8,
    synthetic_signature: bool,
    signed: Vec<u8>,
    high_s: Option<([u8; 32], u8)>,
}

fn bytes(value: &Value) -> Vec<u8> {
    let text = value.as_str().expect("a string");
    hex_decode(text.strip_prefix("0x").expect("0x prefix")).expect("hex")
}

fn array<const N: usize>(value: &Value) -> [u8; N] {
    bytes(value).try_into().expect("fixed length")
}

fn number<T: FromStr>(value: &Value) -> T
where
    T::Err: std::fmt::Debug,
{
    value
        .as_str()
        .expect("a decimal string")
        .parse()
        .expect("a number")
}

fn word(hex: &str) -> [u8; 32] {
    hex_decode(hex).expect("hex").try_into().expect("32 bytes")
}

fn load() -> (Value, Vec<Case>) {
    let corpus: Value = serde_json::from_str(CORPUS).expect("the corpus is valid JSON");
    let cases = corpus["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|case| {
            let tx = &case["tx"];
            Case {
                name: case["name"].as_str().expect("name").to_string(),
                kind: case["kind"].as_str().expect("kind").to_string(),
                tx: Eip1559Tx {
                    chain_id: number(&tx["chain_id"]),
                    nonce: number(&tx["nonce"]),
                    max_priority_fee_per_gas: number(&tx["max_priority_fee_per_gas"]),
                    max_fee_per_gas: number(&tx["max_fee_per_gas"]),
                    gas_limit: number(&tx["gas_limit"]),
                    to: array(&tx["to"]),
                    value: number(&tx["value"]),
                    data: bytes(&tx["data"]),
                },
                unsigned: bytes(&case["unsigned"]),
                signing_hash: array(&case["signing_hash"]),
                r: array(&case["signature"]["r"]),
                s: array(&case["signature"]["s"]),
                y_parity: case["signature"]["y_parity"].as_u64().expect("y_parity") as u8,
                synthetic_signature: case["signature"]["source"] == "synthetic",
                signed: bytes(&case["signed"]),
                high_s: case.get("high_s").map(|high| {
                    (
                        array(&high["s"]),
                        high["y_parity"].as_u64().expect("y_parity") as u8,
                    )
                }),
            }
        })
        .collect();
    (corpus, cases)
}

/// The compressed point the signer would return for `r`: a prefix and `r` as
/// the x coordinate. The prefix alternates by case, since the contract must
/// accept both.
fn affine_point(r: &[u8; 32], index: usize) -> Vec<u8> {
    let mut point = vec![if index.is_multiple_of(2) { 0x02 } else { 0x03 }];
    point.extend_from_slice(r);
    point
}

/// The failures of one case, empty if it matches viem everywhere.
fn check(index: usize, case: &Case) -> Vec<String> {
    let mut failures = Vec::new();
    let mut expect = |ok: bool, what: &str| {
        if !ok {
            failures.push(format!("{}: {what}", case.name));
        }
    };

    if case.kind.starts_with("token-transfer") {
        // The calldata the contract would build for the recipient and amount
        // viem encoded is viem's own, byte for byte.
        let data = &case.tx.data;
        expect(data.len() == 68, "a token transfer's calldata is 68 bytes");
        expect(
            data[..4] == [0xa9, 0x05, 0x9c, 0xbb],
            "the transfer selector",
        );
        let recipient: [u8; 20] = data[16..36].try_into().expect("20 bytes");
        let amount = u128::from_be_bytes(data[52..68].try_into().expect("16 bytes"));
        expect(
            data[4..16] == [0u8; 12] && data[36..52] == [0u8; 16],
            "viem pads the recipient and the amount with zeros",
        );
        expect(
            erc20_transfer_calldata(&recipient, amount) == *data,
            "erc20_transfer_calldata differs from viem's ABI encoding",
        );
    }
    expect(
        encode_unsigned(&case.tx) == case.unsigned,
        "encode_unsigned differs from viem",
    );
    expect(
        signing_hash(&case.tx) == case.signing_hash,
        "signing_hash differs from viem",
    );
    expect(
        encode_signed(&case.tx, case.r, case.s, case.y_parity) == case.signed,
        "encode_signed differs from viem",
    );

    // The canonical signature is already low-s, so normalization is the identity.
    expect(
        normalize_s(case.s, case.y_parity) == (case.s, case.y_parity),
        "normalize_s changed a canonical signature",
    );
    let point = affine_point(&case.r, index);
    expect(
        try_assemble_signed(&case.tx, &point, &case.s, case.y_parity).as_ref() == Ok(&case.signed),
        "try_assemble_signed on the canonical signature differs from viem",
    );

    if let Some((high_s, high_y)) = case.high_s {
        expect(
            high_s > case.s,
            "the corpus high-s form is not actually the higher one",
        );
        expect(
            normalize_s(high_s, high_y) == (case.s, case.y_parity),
            "normalize_s did not map the high-s form to the canonical (s, y_parity)",
        );
        expect(
            try_assemble_signed(&case.tx, &point, &high_s, high_y).as_ref() == Ok(&case.signed),
            "try_assemble_signed on the high-s form differs from viem's canonical bytes",
        );
    }
    failures
}

#[test]
fn encoder_matches_viem_byte_for_byte_over_the_whole_corpus() {
    let started = std::time::Instant::now();
    let (corpus, cases) = load();

    // The corpus is what its header says it is.
    let header = &corpus["header"];
    assert_eq!(
        header["viem"], PINNED_VIEM,
        "the corpus was made by another viem"
    );
    assert_eq!(header["case_count"].as_u64(), Some(cases.len() as u64));
    assert!(
        (1000..=2000).contains(&cases.len()),
        "expected 1000 to 2000 cases, found {}",
        cases.len()
    );

    let mut failures = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        failures.extend(check(index, case));
    }
    assert!(
        failures.is_empty(),
        "{} failed checks across {} cases; first few:\n{}",
        failures.len(),
        cases.len(),
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );

    let with_high_s = cases.iter().filter(|case| case.high_s.is_some()).count();
    println!(
        "evm corpus: {} cases, {} with a high-s form, all byte-identical to viem {} ({:.2}s)",
        cases.len(),
        with_high_s,
        PINNED_VIEM,
        started.elapsed().as_secs_f64()
    );
}

#[test]
fn corpus_really_contains_the_boundaries_it_claims() {
    let (_, cases) = load();
    let count = |kind: &str| cases.iter().filter(|case| case.kind == kind).count();

    // Volume per category.
    assert!(count("random") >= 1000);
    assert!(count("boundary-field") >= 100);
    assert!(count("boundary-list-length") >= 100);
    assert!(count("boundary-signed-list-length") >= 50);
    assert!(count("normalize-edge") >= 20);
    assert!(cases.iter().filter(|case| case.high_s.is_some()).count() >= 500);
    assert!(count("token-transfer") >= 300);
    assert!(count("token-transfer-edge") >= 40);
    assert!(count("data-length") >= 20);
    assert!(count("data-single-byte") >= 4);
    assert!(cases.iter().any(|case| case.synthetic_signature));
    assert!(cases.iter().any(|case| !case.synthetic_signature));

    // Every numeric field at its minimum and its maximum.
    let any = |predicate: &dyn Fn(&Eip1559Tx) -> bool| cases.iter().any(|case| predicate(&case.tx));
    assert!(any(&|tx| tx.chain_id == 1) && any(&|tx| tx.chain_id == u64::MAX));
    assert!(any(&|tx| tx.nonce == 0) && any(&|tx| tx.nonce == u64::MAX));
    assert!(any(&|tx| tx.gas_limit == 0) && any(&|tx| tx.gas_limit == u64::MAX));
    assert!(any(&|tx| tx.max_fee_per_gas == 0) && any(&|tx| tx.max_fee_per_gas == u128::MAX));
    assert!(
        any(&|tx| tx.max_priority_fee_per_gas == 0)
            && any(&|tx| tx.max_priority_fee_per_gas == u128::MAX)
    );
    assert!(any(&|tx| tx.value == 0) && any(&|tx| tx.value == u128::MAX));
    // The single-byte RLP boundary and a byte-width boundary, for a u128 field.
    for value in [
        127u128,
        128,
        255,
        256,
        u64::MAX as u128,
        u64::MAX as u128 + 1,
    ] {
        assert!(any(&|tx| tx.value == value), "no case with value {value}");
    }

    // Calldata: every RLP string-length edge (empty, a single byte below and at
    // 0x80, 55 and 56 bytes, and longer), and token transfers at the amount
    // edges (zero, one, the RLP single-byte boundary, 2^64, 2^127, the maximum).
    for length in [0usize, 1, 54, 55, 56, 57, 255, 256, 1000] {
        assert!(
            cases.iter().any(|case| case.tx.data.len() == length),
            "no case with {length} bytes of data"
        );
    }
    for byte in [0x00u8, 0x7f, 0x80, 0xff] {
        assert!(
            cases.iter().any(|case| case.tx.data == [byte]),
            "no case with the single data byte {byte:#x}"
        );
    }
    let token_amount = |case: &Case| {
        (case.kind == "token-transfer-edge")
            .then(|| u128::from_be_bytes(case.tx.data[52..68].try_into().expect("16 bytes")))
    };
    for amount in [
        0u128,
        1,
        127,
        128,
        255,
        256,
        u64::MAX as u128,
        1 << 64,
        1 << 127,
        u128::MAX,
    ] {
        assert!(
            cases.iter().any(|case| token_amount(case) == Some(amount)),
            "no token transfer of {amount}"
        );
    }

    // All-zero and all-ones addresses.
    assert!(any(&|tx| tx.to == [0u8; 20]) && any(&|tx| tx.to == [0xff; 20]));

    // The RLP list-length boundary, in both encodings: a payload of exactly 55
    // bytes is the longest short list (0xf7), 56 the first long one (0xf8 0x38).
    let unsigned_header = |case: &Case| (case.unsigned[1], case.unsigned.get(2).copied());
    assert!(cases.iter().any(|case| unsigned_header(case).0 == 0xf7));
    assert!(cases
        .iter()
        .any(|case| unsigned_header(case) == (0xf8, Some(0x38))));
    let signed_header = |case: &Case| (case.signed[1], case.signed.get(2).copied());
    assert!(cases.iter().any(|case| signed_header(case).0 == 0xf7));
    assert!(cases
        .iter()
        .any(|case| signed_header(case) == (0xf8, Some(0x38))));

    // Low-s normalization edges: the largest low s whose mirror is the smallest
    // high s, the smallest s whose mirror is n - 1, and both parities.
    let has_edge = |s: &str, mirror: &str, y: u8| {
        cases.iter().any(|case| {
            case.s == word(s)
                && case.y_parity == y
                && case.high_s.is_some_and(|(high, _)| high == word(mirror))
        })
    };
    for y in [0, 1] {
        assert!(
            has_edge(HALF_ORDER, HALF_ORDER_PLUS_ONE, y),
            "no half-order edge for y={y}"
        );
        assert!(
            has_edge(&format!("{}1", "0".repeat(63)), ORDER_MINUS_ONE, y),
            "no s=1 edge for y={y}"
        );
    }
    // Every mirror lies strictly in the high half, below the order.
    for case in &cases {
        if let Some((high, _)) = case.high_s {
            assert!(
                high > word(HALF_ORDER) && high < word(ORDER),
                "{}",
                case.name
            );
        }
    }

    // The cases are not duplicates of one another.
    let distinct: HashSet<&Vec<u8>> = cases.iter().map(|case| &case.unsigned).collect();
    assert_eq!(
        distinct.len(),
        cases.len(),
        "duplicate transactions in the corpus"
    );
}
