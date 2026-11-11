//! EIP-1559 (type 2) transaction encoding for native ETH transfers.
//!
//! Pure functions over plain data: no contract state, no storage. The only
//! host call is `env::keccak256_array`, which is deterministic. Everything is
//! checked against serializations produced by viem (`vectors/`), byte for byte.
//!
//! Rules this module encodes:
//! - Integers are RLP integers: big-endian with no leading zeros, so zero is
//!   the empty string (`0x80`). That applies to `r` and `s` too.
//! - There is **no EIP-155 step**. A typed transaction carries its chain id
//!   inside the signed payload, and the signature is `y_parity` (0 or 1, the
//!   recovery id itself, never `+27` and never `chain_id * 2 + 35`).
//! - Signatures are normalized to low-s ([`normalize_s`]) before assembly,
//!   because Ethereum rejects `s > n / 2` (EIP-2).
//! - Data and access list are empty: this is a plain value transfer.

use near_sdk::{env, near};

/// EIP-2718 transaction type byte for EIP-1559 transactions.
const TX_TYPE: u8 = 0x02;

/// Order `n` of the secp256k1 group, big-endian.
const SECP256K1_ORDER: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe,
    0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36, 0x41, 0x41,
];

/// `n / 2` rounded down, big-endian. A low `s` is at most this.
const SECP256K1_HALF_ORDER: [u8; 32] = [
    0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0x5d, 0x57, 0x6e, 0x73, 0x57, 0xa4, 0x50, 0x1d, 0xdf, 0xe9, 0x2f, 0x46, 0x68, 0x1b, 0x20, 0xa0,
];

/// An EIP-1559 native transfer: empty data, empty access list.
#[near(serializers = [borsh])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Eip1559Tx {
    pub chain_id: u64,
    pub nonce: u64,
    pub max_priority_fee_per_gas: u128,
    pub max_fee_per_gas: u128,
    pub gas_limit: u64,
    pub to: [u8; 20],
    /// Value in wei.
    pub value: u128,
}

/// The bytes that get signed over: `0x02 || rlp([...fields, data, access_list])`.
pub fn encode_unsigned(tx: &Eip1559Tx) -> Vec<u8> {
    with_type(rlp_list(&unsigned_fields(tx)))
}

/// `keccak256(encode_unsigned(tx))`: the 32-byte payload handed to the signer.
pub fn signing_hash(tx: &Eip1559Tx) -> [u8; 32] {
    env::keccak256_array(encode_unsigned(tx))
}

/// Brings `s` into the lower half of the curve order, as Ethereum requires.
/// If `s > n / 2` it becomes `n - s` and the parity flips, since negating `s`
/// negates the recovered point. Otherwise both pass through. Returns
/// `(s, y_parity)`.
///
/// # Panics
/// If `recovery_id` is not 0 or 1, or `s` is outside `1..n`. Neither can come
/// from a valid signature, and continuing would build a transaction that the
/// network rejects or that recovers to a different signer, so callers must
/// validate untrusted signer output before calling this.
pub fn normalize_s(s: [u8; 32], recovery_id: u8) -> ([u8; 32], u8) {
    assert!(recovery_id <= 1, "recovery id must be 0 or 1");
    assert!(s != [0u8; 32] && s < SECP256K1_ORDER, "s must be in 1..n");
    if s <= SECP256K1_HALF_ORDER {
        return (s, recovery_id);
    }
    (sub_be(&SECP256K1_ORDER, &s), recovery_id ^ 1)
}

/// The raw signed transaction, ready to broadcast:
/// `0x02 || rlp([...fields, data, access_list, y_parity, r, s])`. `s` should
/// already be normalized.
///
/// # Panics
/// If `y_parity` is not 0 or 1 (see [`normalize_s`]).
pub fn encode_signed(tx: &Eip1559Tx, r: [u8; 32], s: [u8; 32], y_parity: u8) -> Vec<u8> {
    assert!(y_parity <= 1, "y_parity must be 0 or 1");
    let mut payload = unsigned_fields(tx);
    payload.extend(rlp_uint(&[y_parity]));
    payload.extend(rlp_uint(&r));
    payload.extend(rlp_uint(&s));
    with_type(rlp_list(&payload))
}

/// Why a raw signature from the signer was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvmError {
    /// `recovery_id` is not 0 or 1.
    RecoveryId,
    /// `scalar` is not exactly 32 bytes, or not in `1..n`.
    Scalar,
    /// `affine_point` is not 33 bytes starting with `0x02` or `0x03`.
    AffinePoint,
    /// The x coordinate taken from `affine_point` is not in `1..n`, so it is
    /// not a usable ECDSA `r`.
    R,
}

/// A signature as it goes into a transaction: `s` in the lower half of the
/// curve order and `y_parity` 0 or 1. Producing one is
/// [`normalized_signature`]'s job; everything downstream (the encoded
/// transaction, the authenticity check) uses this exact triple.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signature {
    pub r: [u8; 32],
    pub s: [u8; 32],
    pub y_parity: u8,
}

/// Validates the MPC's raw signature components and normalizes them for a
/// transaction, or returns an error for any input that could not come from a
/// valid signature. **Never panics.** Every precondition of [`normalize_s`] is
/// checked here first, so its asserts are unreachable on this path.
///
/// - `affine_point`: the signer's `big_r.affine_point`, a 33-byte compressed
///   point. `r` is its x coordinate, `affine_point[1..]`. (The `02`/`03` byte
///   is not compared with `recovery_id`; the signer may have flipped `s`.)
/// - `scalar`: the signer's `s.scalar`, 32 bytes big-endian.
/// - `recovery_id`: 0 or 1, used as `y_parity` after low-s normalization.
pub fn normalized_signature(
    affine_point: &[u8],
    scalar: &[u8],
    recovery_id: u8,
) -> Result<Signature, EvmError> {
    if recovery_id > 1 {
        return Err(EvmError::RecoveryId);
    }
    let s: [u8; 32] = scalar.try_into().map_err(|_| EvmError::Scalar)?;
    if !is_valid_scalar(&s) {
        return Err(EvmError::Scalar);
    }
    let point: [u8; 33] = affine_point.try_into().map_err(|_| EvmError::AffinePoint)?;
    if point[0] != 0x02 && point[0] != 0x03 {
        return Err(EvmError::AffinePoint);
    }
    let mut r = [0u8; 32];
    r.copy_from_slice(&point[1..]);
    if !is_valid_scalar(&r) {
        return Err(EvmError::R);
    }
    let (s, y_parity) = normalize_s(s, recovery_id);
    Ok(Signature { r, s, y_parity })
}

/// The gate-facing assembler: [`normalized_signature`] followed by
/// [`encode_signed`], returning the raw signed transaction. **Never panics.**
pub fn try_assemble_signed(
    tx: &Eip1559Tx,
    affine_point: &[u8],
    scalar: &[u8],
    recovery_id: u8,
) -> Result<Vec<u8>, EvmError> {
    let Signature { r, s, y_parity } = normalized_signature(affine_point, scalar, recovery_id)?;
    Ok(encode_signed(tx, r, s, y_parity))
}

/// The EVM address that signed `hash` to produce `signature`, by the runtime's
/// `ecrecover`: the last 20 bytes of the keccak-256 of the recovered public
/// key. `None` if no public key can be recovered.
///
/// The malleability flag is on, so the host refuses a high `s`, as Ethereum
/// does; [`Signature`] is always low-s. Never panics.
pub fn recover_address(hash: &[u8; 32], signature: &Signature) -> Option<[u8; 20]> {
    let mut r_and_s = [0u8; 64];
    r_and_s[..32].copy_from_slice(&signature.r);
    r_and_s[32..].copy_from_slice(&signature.s);
    let public_key = env::ecrecover(hash, &r_and_s, signature.y_parity, true)?;
    let digest = env::keccak256_array(public_key);
    let mut address = [0u8; 20];
    address.copy_from_slice(&digest[12..]);
    Some(address)
}

/// `1 <= value < n`.
fn is_valid_scalar(value: &[u8; 32]) -> bool {
    *value != [0u8; 32] && *value < SECP256K1_ORDER
}

// ---- Hex and addresses ----

/// Lowercase hex without a prefix.
pub fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// Hex without a prefix, either case. `None` on odd length or a non-hex digit.
pub fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let digits = text.as_bytes();
    if !digits.len().is_multiple_of(2) {
        return None;
    }
    digits
        .chunks(2)
        .map(|pair| Some(hex_digit(pair[0])? << 4 | hex_digit(pair[1])?))
        .collect()
}

fn hex_digit(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

/// `0x` followed by exactly 40 hex digits. The EIP-55 checksum, if the casing
/// carries one, is not verified.
pub fn parse_address(text: &str) -> Option<[u8; 20]> {
    hex_decode(text.strip_prefix("0x")?)?.try_into().ok()
}

/// Lowercase `0x`-prefixed address.
pub fn format_address(address: &[u8; 20]) -> String {
    format!("0x{}", hex_encode(address))
}

/// RLP items shared by the unsigned and signed forms, concatenated.
fn unsigned_fields(tx: &Eip1559Tx) -> Vec<u8> {
    let mut fields = Vec::new();
    fields.extend(rlp_uint(&tx.chain_id.to_be_bytes()));
    fields.extend(rlp_uint(&tx.nonce.to_be_bytes()));
    fields.extend(rlp_uint(&tx.max_priority_fee_per_gas.to_be_bytes()));
    fields.extend(rlp_uint(&tx.max_fee_per_gas.to_be_bytes()));
    fields.extend(rlp_uint(&tx.gas_limit.to_be_bytes()));
    fields.extend(rlp_bytes(&tx.to));
    fields.extend(rlp_uint(&tx.value.to_be_bytes()));
    fields.extend(rlp_bytes(&[])); // data
    fields.extend(rlp_list(&[])); // access list
    fields
}

fn with_type(rlp: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(rlp.len() + 1);
    out.push(TX_TYPE);
    out.extend(rlp);
    out
}

/// `a - b` over 32-byte big-endian integers. Requires `a >= b`.
fn sub_be(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let mut digit = i16::from(a[i]) - i16::from(b[i]) - borrow;
        borrow = 0;
        if digit < 0 {
            digit += 256;
            borrow = 1;
        }
        out[i] = digit as u8;
    }
    debug_assert_eq!(borrow, 0, "a must be >= b");
    out
}

// ---- RLP (only what this structure needs) ----

/// A byte string. A single byte below 0x80 is its own encoding.
fn rlp_bytes(bytes: &[u8]) -> Vec<u8> {
    if let [byte] = bytes {
        if *byte < 0x80 {
            return vec![*byte];
        }
    }
    let mut out = length_prefix(0x80, bytes.len());
    out.extend(bytes);
    out
}

/// An unsigned integer given as big-endian bytes: leading zeros are dropped,
/// so zero encodes as the empty string.
fn rlp_uint(be: &[u8]) -> Vec<u8> {
    let leading_zeros = be.iter().take_while(|&&byte| byte == 0).count();
    rlp_bytes(&be[leading_zeros..])
}

/// A list whose items are already encoded and concatenated in `payload`.
fn rlp_list(payload: &[u8]) -> Vec<u8> {
    let mut out = length_prefix(0xc0, payload.len());
    out.extend(payload);
    out
}

/// Short form `offset + len` up to 55, else `offset + 55 + len_of_len`
/// followed by the length as minimal big-endian.
fn length_prefix(offset: u8, len: usize) -> Vec<u8> {
    if len <= 55 {
        return vec![offset + len as u8];
    }
    let be = (len as u64).to_be_bytes();
    let be = &be[be.iter().take_while(|&&byte| byte == 0).count()..];
    let mut out = vec![offset + 55 + be.len() as u8];
    out.extend(be);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    fn hex20(s: &str) -> [u8; 20] {
        hex(s).try_into().unwrap()
    }

    fn hex32(s: &str) -> [u8; 32] {
        hex(s).try_into().unwrap()
    }

    /// One field set with viem's serialization of it. Generated by
    /// `vectors/gen-evm-vectors.mjs` (viem 2.57.16); do not edit by hand.
    struct Vector {
        name: &'static str,
        tx: Eip1559Tx,
        r: &'static str,
        s: &'static str,
        y_parity: u8,
        unsigned: &'static str,
        signing_hash: &'static str,
        signed: &'static str,
    }

    fn vectors() -> Vec<Vector> {
        vec![
            Vector {
                name: "plain_transfer",
                tx: Eip1559Tx {
                    chain_id: 11155111,
                    nonce: 7,
                    max_priority_fee_per_gas: 1500000000,
                    max_fee_per_gas: 30000000000,
                    gas_limit: 21000,
                    to: hex20("70997970c51812dc3a010c7d01b50e0d17dc79c8"),
                    value: 100000000000000,
                },
                r: "5b2fc0e1f6d4a7c98e3b1d2a4f5c6e7d8a9b0c1d2e3f405162738495a6b7c8d9",
                s: "2a4d6f8091b3d5f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f",
                y_parity: 1,
                unsigned: "02f183aa36a7078459682f008506fc23ac008252089470997970c51812dc3a010c7d01b50e0d17dc79c8865af3107a400080c0",
                signing_hash: "b7ba6bf68e0904a1867d70fb6acff6253f13bcc41ecdde82c6c781e15e361034",
                signed: "02f87483aa36a7078459682f008506fc23ac008252089470997970c51812dc3a010c7d01b50e0d17dc79c8865af3107a400080c001a05b2fc0e1f6d4a7c98e3b1d2a4f5c6e7d8a9b0c1d2e3f405162738495a6b7c8d9a02a4d6f8091b3d5f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f",
            },
            Vector {
                name: "zero_fields",
                tx: Eip1559Tx {
                    chain_id: 1,
                    nonce: 0,
                    max_priority_fee_per_gas: 0,
                    max_fee_per_gas: 7,
                    gas_limit: 21000,
                    to: hex20("70997970c51812dc3a010c7d01b50e0d17dc79c8"),
                    value: 0,
                },
                r: "0000a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e",
                s: "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
                y_parity: 0,
                unsigned: "02df018080078252089470997970c51812dc3a010c7d01b50e0d17dc79c88080c0",
                signing_hash: "712d1c0b97bad74c4b6d9300229ae1c61399e4a3b88daa451e3701c990045420",
                signed: "02f85f018080078252089470997970c51812dc3a010c7d01b50e0d17dc79c88080c0809ea1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e9f112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
            },
            Vector {
                name: "single_byte_boundaries",
                tx: Eip1559Tx {
                    chain_id: 127,
                    nonce: 127,
                    max_priority_fee_per_gas: 128,
                    max_fee_per_gas: 255,
                    gas_limit: 256,
                    to: hex20("70997970c51812dc3a010c7d01b50e0d17dc79c8"),
                    value: 1,
                },
                r: "0000000000000000000000000000000000000000000000000000000000000080",
                s: "0000000000000000000000000000000000000000000000000000000000000001",
                y_parity: 1,
                unsigned: "02e17f7f818081ff8201009470997970c51812dc3a010c7d01b50e0d17dc79c80180c0",
                signing_hash: "8e8949bd66cfa82ed5434f38520ecc2c2712ab8dc5f75878da85c99db85bf63e",
                signed: "02e57f7f818081ff8201009470997970c51812dc3a010c7d01b50e0d17dc79c80180c001818001",
            },
            Vector {
                name: "list_payload_55_bytes",
                tx: Eip1559Tx {
                    chain_id: 1,
                    nonce: 0,
                    max_priority_fee_per_gas: 4328719365,
                    max_fee_per_gas: 1108152157446,
                    gas_limit: 21000,
                    to: hex20("70997970c51812dc3a010c7d01b50e0d17dc79c8"),
                    value: 79850778293499848189627010061,
                },
                r: "00000000000000000000000000000000000000000000000000000000000000ff",
                s: "0000000000000000000000000000000000000000000000000000000000000100",
                y_parity: 0,
                unsigned: "02f70180850102030405860102030405068252089470997970c51812dc3a010c7d01b50e0d17dc79c88d0102030405060708090a0b0c0d80c0",
                signing_hash: "0954baee0536a5be0b54d6a1c0890cf11d1bdebd03cc52492a265b911f9b1078",
                signed: "02f83d0180850102030405860102030405068252089470997970c51812dc3a010c7d01b50e0d17dc79c88d0102030405060708090a0b0c0d80c08081ff820100",
            },
            Vector {
                name: "list_payload_56_bytes",
                tx: Eip1559Tx {
                    chain_id: 1,
                    nonce: 0,
                    max_priority_fee_per_gas: 4328719365,
                    max_fee_per_gas: 1108152157446,
                    gas_limit: 21000,
                    to: hex20("70997970c51812dc3a010c7d01b50e0d17dc79c8"),
                    value: 20441799243135961136544514575630,
                },
                r: "00000000000000000000000000000000000000000000000000000000000000ff",
                s: "0000000000000000000000000000000000000000000000000000000000000100",
                y_parity: 1,
                unsigned: "02f8380180850102030405860102030405068252089470997970c51812dc3a010c7d01b50e0d17dc79c88e0102030405060708090a0b0c0d0e80c0",
                signing_hash: "99c41e0c633e950f8c7057c7a13472f4332a4c50514a7f97daece79c2e0e706d",
                signed: "02f83e0180850102030405860102030405068252089470997970c51812dc3a010c7d01b50e0d17dc79c88e0102030405060708090a0b0c0d0e80c00181ff820100",
            },
            Vector {
                name: "high_top_bytes",
                tx: Eip1559Tx {
                    chain_id: 18446744073709551615,
                    nonce: 18446744073709551615,
                    max_priority_fee_per_gas: 340282366920938463463374607431768211455,
                    max_fee_per_gas: 340282366920938463463374607431768211455,
                    gas_limit: 18446744073709551615,
                    to: hex20("ffffffffffffffffffffffffffffffffffffffff"),
                    value: 340282366920938463463374607431768211455,
                },
                r: "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
                s: "7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0",
                y_parity: 1,
                unsigned: "02f86588ffffffffffffffff88ffffffffffffffff90ffffffffffffffffffffffffffffffff90ffffffffffffffffffffffffffffffff88ffffffffffffffff94ffffffffffffffffffffffffffffffffffffffff90ffffffffffffffffffffffffffffffff80c0",
                signing_hash: "97195fa62dedecdd2c16545da71bc85b6e26b6f8436ac09724e8f3dc704b42d3",
                signed: "02f8a888ffffffffffffffff88ffffffffffffffff90ffffffffffffffffffffffffffffffff90ffffffffffffffffffffffffffffffff88ffffffffffffffff94ffffffffffffffffffffffffffffffffffffffff90ffffffffffffffffffffffffffffffff80c001a0ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffa07fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0",
            },
        ]
    }

    #[test]
    fn unsigned_encoding_matches_viem() {
        for v in vectors() {
            assert_eq!(encode_unsigned(&v.tx), hex(v.unsigned), "{}", v.name);
        }
    }

    #[test]
    fn signing_hash_matches_viem() {
        for v in vectors() {
            assert_eq!(signing_hash(&v.tx), hex32(v.signing_hash), "{}", v.name);
        }
    }

    #[test]
    fn signed_encoding_matches_viem() {
        for v in vectors() {
            let signed = encode_signed(&v.tx, hex32(v.r), hex32(v.s), v.y_parity);
            assert_eq!(signed, hex(v.signed), "{}", v.name);
        }
    }

    #[test]
    fn keccak_matches_known_vectors() {
        // Independent of the transaction code: the empty string and "abc".
        assert_eq!(
            env::keccak256_array(b""),
            hex32("c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470")
        );
        assert_eq!(
            env::keccak256_array(b"abc"),
            hex32("4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45")
        );
    }

    #[test]
    fn vector_set_covers_the_list_header_boundary_and_zero_integers() {
        // Guards the vector set itself: it must keep exercising these edges.
        let by_name = |name: &str| vectors().into_iter().find(|v| v.name == name).unwrap();
        let at_55 = encode_unsigned(&by_name("list_payload_55_bytes").tx);
        assert_eq!(&at_55[..2], &[TX_TYPE, 0xf7], "largest short-form list");
        let at_56 = encode_unsigned(&by_name("list_payload_56_bytes").tx);
        assert_eq!(
            &at_56[..3],
            &[TX_TYPE, 0xf8, 0x38],
            "smallest long-form list"
        );
        let zeros = encode_unsigned(&by_name("zero_fields").tx);
        assert_eq!(
            &zeros[1..5],
            &[0xdf, 0x01, 0x80, 0x80],
            "zero integers are 0x80"
        );
    }

    #[test]
    fn length_prefix_matches_the_rlp_specification() {
        // Lengths this fixed structure never reaches, checked against the
        // published RLP examples.
        assert_eq!(length_prefix(0x80, 0), [0x80]);
        assert_eq!(length_prefix(0x80, 55), [0xb7]);
        assert_eq!(length_prefix(0x80, 56), [0xb8, 0x38]);
        assert_eq!(length_prefix(0x80, 256), [0xb9, 0x01, 0x00]);
        assert_eq!(length_prefix(0xc0, 55), [0xf7]);
        assert_eq!(length_prefix(0xc0, 56), [0xf8, 0x38]);
        assert_eq!(length_prefix(0xc0, 1024), [0xf9, 0x04, 0x00]);
    }

    // ---- normalize_s ----

    const N_MINUS_1: &str = "fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364140";
    const HALF: &str = "7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0";
    const HALF_PLUS_1: &str = "7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a1";
    const ONE: &str = "0000000000000000000000000000000000000000000000000000000000000001";
    // A high s and its mirror n - s, computed independently of this module.
    const HIGH_S: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
    const HIGH_S_FLIPPED: &str = "0123456789abcdef0123456789abcdedbbd2224e38f46e2ac0f5a3f459e20f31";

    #[test]
    fn high_s_is_flipped_and_parity_toggled() {
        assert_eq!(normalize_s(hex32(N_MINUS_1), 0), (hex32(ONE), 1));
        assert_eq!(normalize_s(hex32(N_MINUS_1), 1), (hex32(ONE), 0));
        assert_eq!(normalize_s(hex32(HALF_PLUS_1), 0), (hex32(HALF), 1));
        assert_eq!(normalize_s(hex32(HIGH_S), 1), (hex32(HIGH_S_FLIPPED), 0));
    }

    #[test]
    fn low_s_passes_through_unchanged() {
        assert_eq!(normalize_s(hex32(ONE), 0), (hex32(ONE), 0));
        assert_eq!(normalize_s(hex32(ONE), 1), (hex32(ONE), 1));
        // n / 2 itself is low.
        assert_eq!(normalize_s(hex32(HALF), 1), (hex32(HALF), 1));
    }

    #[test]
    fn normalizing_twice_changes_nothing_more() {
        let (s, y) = normalize_s(hex32(HIGH_S), 0);
        assert_eq!(normalize_s(s, y), (s, y));
    }

    #[test]
    #[should_panic(expected = "s must be in 1..n")]
    fn zero_s_is_rejected() {
        normalize_s([0u8; 32], 0);
    }

    #[test]
    #[should_panic(expected = "s must be in 1..n")]
    fn s_at_or_above_the_order_is_rejected() {
        normalize_s(SECP256K1_ORDER, 0);
    }

    #[test]
    #[should_panic(expected = "recovery id must be 0 or 1")]
    fn recovery_id_other_than_0_or_1_is_rejected() {
        normalize_s(hex32(ONE), 2);
    }

    #[test]
    #[should_panic(expected = "y_parity must be 0 or 1")]
    fn encoding_rejects_a_y_parity_other_than_0_or_1() {
        let v = vectors().remove(0);
        encode_signed(&v.tx, hex32(v.r), hex32(v.s), 27);
    }

    // ---- try_assemble_signed ----

    fn vector(name: &str) -> Vector {
        vectors().into_iter().find(|v| v.name == name).unwrap()
    }

    /// The signer's wire form of a vector's signature: `affine_point` is a
    /// compressed point whose x coordinate is `r`.
    fn affine(prefix: u8, r: &str) -> Vec<u8> {
        let mut point = vec![prefix];
        point.extend(hex(r));
        point
    }

    #[test]
    fn assembling_a_valid_signature_matches_viem() {
        for name in ["plain_transfer", "zero_fields", "single_byte_boundaries"] {
            let v = vector(name);
            for prefix in [0x02, 0x03] {
                let raw = try_assemble_signed(&v.tx, &affine(prefix, v.r), &hex(v.s), v.y_parity)
                    .unwrap();
                assert_eq!(raw, hex(v.signed), "{name} with prefix {prefix:#x}");
            }
        }
    }

    #[test]
    fn assembling_a_high_s_signature_normalizes_it_to_the_viem_encoding() {
        // The mirrored signature (n - s, opposite parity) must come out as the
        // low-s form that viem encoded.
        let v = vector("plain_transfer");
        let high_s = sub_be(&SECP256K1_ORDER, &hex32(v.s));
        let raw = try_assemble_signed(&v.tx, &affine(0x02, v.r), &high_s, v.y_parity ^ 1).unwrap();
        assert_eq!(raw, hex(v.signed));
    }

    #[test]
    fn assembling_rejects_each_invalid_input() {
        let v = vector("plain_transfer");
        let point = affine(0x02, v.r);
        let s = hex(v.s);
        let assemble = |point: &[u8], scalar: &[u8], recovery_id: u8| {
            try_assemble_signed(&v.tx, point, scalar, recovery_id)
        };
        // recovery id
        assert_eq!(assemble(&point, &s, 2), Err(EvmError::RecoveryId));
        assert_eq!(assemble(&point, &s, 255), Err(EvmError::RecoveryId));
        // scalar: zero, the order, above the order, wrong length
        assert_eq!(assemble(&point, &[0u8; 32], 0), Err(EvmError::Scalar));
        assert_eq!(assemble(&point, &SECP256K1_ORDER, 0), Err(EvmError::Scalar));
        assert_eq!(assemble(&point, &[0xff; 32], 0), Err(EvmError::Scalar));
        assert_eq!(assemble(&point, &s[1..], 0), Err(EvmError::Scalar));
        assert_eq!(
            assemble(&point, &[&s[..], &[0u8]].concat(), 0),
            Err(EvmError::Scalar)
        );
        assert_eq!(assemble(&point, &[], 0), Err(EvmError::Scalar));
        // affine point: wrong prefix, wrong length
        for prefix in [0x00, 0x01, 0x04, 0x05, 0xff] {
            assert_eq!(
                assemble(&affine(prefix, v.r), &s, 0),
                Err(EvmError::AffinePoint)
            );
        }
        assert_eq!(assemble(&point[1..], &s, 0), Err(EvmError::AffinePoint));
        assert_eq!(
            assemble(&[&point[..], &[0u8]].concat(), &s, 0),
            Err(EvmError::AffinePoint)
        );
        assert_eq!(assemble(&[], &s, 0), Err(EvmError::AffinePoint));
        // r: zero, the order, above the order
        let with_r = |r: &[u8]| [&[0x02u8][..], r].concat();
        assert_eq!(assemble(&with_r(&[0u8; 32]), &s, 0), Err(EvmError::R));
        assert_eq!(assemble(&with_r(&SECP256K1_ORDER), &s, 0), Err(EvmError::R));
        assert_eq!(assemble(&with_r(&[0xff; 32]), &s, 0), Err(EvmError::R));
    }

    #[test]
    fn assembling_never_panics_on_any_combination_of_edge_inputs() {
        // Exercises the contract that the asserts in normalize_s and
        // encode_signed are unreachable through this function.
        let tx = vector("plain_transfer").tx;
        let scalars: Vec<Vec<u8>> = vec![
            vec![],
            vec![0u8; 32],
            vec![1u8; 31],
            vec![1u8; 32],
            SECP256K1_HALF_ORDER.to_vec(),
            SECP256K1_ORDER.to_vec(),
            vec![0xff; 32],
            vec![0xff; 33],
        ];
        let points: Vec<Vec<u8>> = vec![
            vec![],
            vec![0x02; 33],
            vec![0x03; 33],
            [&[0x02u8][..], &SECP256K1_ORDER[..]].concat(),
            [&[0x02u8][..], &[0u8; 32][..]].concat(),
            vec![0x04; 65],
            vec![0xff; 33],
        ];
        for scalar in &scalars {
            for point in &points {
                for recovery_id in [0u8, 1, 2, 27, 255] {
                    let _ = try_assemble_signed(&tx, point, scalar, recovery_id);
                }
            }
        }
    }

    // ---- hex and addresses ----

    #[test]
    fn hex_round_trips_and_rejects_bad_input() {
        assert_eq!(hex_encode(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
        assert_eq!(hex_decode("000FA0ff"), Some(vec![0x00, 0x0f, 0xa0, 0xff]));
        assert_eq!(hex_decode(""), Some(vec![]));
        assert_eq!(hex_decode("abc"), None, "odd length");
        assert_eq!(hex_decode("zz"), None, "non-hex digit");
        assert_eq!(hex_decode("0x00"), None, "prefix is not accepted here");
    }

    #[test]
    fn addresses_parse_and_format() {
        let text = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
        let address = parse_address(text).unwrap();
        assert_eq!(format_address(&address), text.to_lowercase());
        assert_eq!(parse_address(&text.to_lowercase()), Some(address));
        assert_eq!(parse_address(&text[2..]), None, "needs the 0x prefix");
        assert_eq!(parse_address("0x1234"), None, "too short");
        assert_eq!(parse_address(&format!("{text}00")), None, "too long");
        assert_eq!(parse_address(&format!("0x{}", "g".repeat(40))), None);
    }

    // ---- Authenticity: recovering the signer ----

    /// A real Chain Signatures MPC response on `v1.signer-prod.testnet` (the
    /// yield-resume spike): the signature over this hash, for the account
    /// `yield-spike-530f5512.testnet` and path `yield-spike-1`, whose derived
    /// Ethereum address is 0xec381cf191A500D99E128F4D9BfEf9d5FDc6a58c. Produced
    /// by a signer entirely independent of this repository.
    const MPC_HASH: &str = "c88d01879201e9035c92e56ef2bb8ce3e0e22adaf7c527fb78dfaefa73c09536";
    const MPC_AFFINE_POINT: &str =
        "02c0e71f01267af09d34b9abac10ffe888055a8b9d34d863f084ff17b9f708f3dd";
    const MPC_SCALAR: &str = "72001377c31d0dc2048f8d456d55177f4365744bbd445ed817b877f0cf40f9d7";
    const MPC_RECOVERY_ID: u8 = 1;
    const MPC_ADDRESS: &str = "0xec381cf191a500d99e128f4d9bfef9d5fdc6a58c";

    fn mpc_signature() -> Signature {
        normalized_signature(&hex(MPC_AFFINE_POINT), &hex(MPC_SCALAR), MPC_RECOVERY_ID).unwrap()
    }

    fn mpc_hash() -> [u8; 32] {
        hex(MPC_HASH).try_into().unwrap()
    }

    #[test]
    fn a_real_mpc_signature_recovers_to_the_derived_address() {
        assert_eq!(
            recover_address(&mpc_hash(), &mpc_signature()),
            Some(hex20(&MPC_ADDRESS[2..]))
        );
    }

    #[test]
    fn the_real_signature_does_not_recover_to_that_address_for_anything_else() {
        let address = Some(hex20(&MPC_ADDRESS[2..]));
        // A different hash.
        let mut other_hash = mpc_hash();
        other_hash[31] ^= 1;
        assert_ne!(recover_address(&other_hash, &mpc_signature()), address);
        // The other parity.
        let flipped = Signature {
            y_parity: MPC_RECOVERY_ID ^ 1,
            ..mpc_signature()
        };
        assert_ne!(recover_address(&mpc_hash(), &flipped), address);
        // A different s.
        let mut other_s = mpc_signature();
        other_s.s[31] ^= 1;
        assert_ne!(recover_address(&mpc_hash(), &other_s), address);
    }

    #[test]
    fn recovery_refuses_a_high_s_signature() {
        // (r, n - s, parity ^ 1) is the same signature in high-s form; the
        // host's malleability flag refuses it, so only canonical triples pass.
        let low = mpc_signature();
        let mut high_s = [0u8; 32];
        let mut borrow = 0i16;
        for i in (0..32).rev() {
            let mut digit = i16::from(SECP256K1_ORDER[i]) - i16::from(low.s[i]) - borrow;
            borrow = i16::from(digit < 0);
            if digit < 0 {
                digit += 256;
            }
            high_s[i] = digit as u8;
        }
        let high = Signature {
            r: low.r,
            s: high_s,
            y_parity: low.y_parity ^ 1,
        };
        assert_eq!(recover_address(&mpc_hash(), &high), None);
    }

    #[test]
    fn a_point_off_the_curve_recovers_to_nothing() {
        // x = 5 is not the x coordinate of any secp256k1 point.
        let mut r = [0u8; 32];
        r[31] = 5;
        let signature = Signature {
            r,
            ..mpc_signature()
        };
        assert_eq!(recover_address(&mpc_hash(), &signature), None);
    }

    #[test]
    fn independent_fixture_signatures_recover_to_their_keys() {
        use crate::test_support::{fixture_signature, test_sender, Signer};
        let tx = Eip1559Tx {
            chain_id: 11_155_111,
            nonce: 0,
            max_priority_fee_per_gas: 1_000_000_000,
            max_fee_per_gas: 2_000_000_000,
            gas_limit: 21_000,
            to: hex20("70997970c51812dc3a010c7d01b50e0d17dc79c8"),
            value: 1000,
        };
        let hash = signing_hash(&tx);
        let recovered = |signer| {
            let fixture = fixture_signature(&hash, signer);
            let signature = Signature {
                r: fixture.r,
                s: fixture.s,
                y_parity: fixture.y_parity,
            };
            recover_address(&hash, &signature)
        };
        assert_eq!(recovered(Signer::Sender), Some(test_sender()));
        assert_ne!(recovered(Signer::Other), Some(test_sender()));
        assert!(recovered(Signer::Other).is_some());
    }
}
