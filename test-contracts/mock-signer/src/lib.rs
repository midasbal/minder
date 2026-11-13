//! Test double for the Chain Signatures contract (`v1.signer`). It enforces the
//! real signer's preconditions on `sign` (1 yoctoNEAR, at least 15 Tgas,
//! domain 0), records each request, and answers in the shape the real signer
//! returns. It signs nothing: it replays signatures registered per hash by the
//! test (made by an independent signer), so the contract under test receives
//! signatures that genuinely recover, or deliberately do not, for the hash it
//! sent.

use near_sdk::serde_json::{json, Value};
use near_sdk::{
    assert_one_yocto, env, near, require, AccountId, Gas, GasWeight, PanicOnDefault, Promise,
    PromiseError, PromiseOrValue, YieldId,
};

/// Gas given to the callback of a held request, which only builds the answer.
const HELD_ANSWER_GAS: Gas = Gas::from_tgas(10);

/// Padding that pushes an otherwise valid response past 4096 bytes.
const OVERSIZED_PADDING: usize = 5000;

/// Well-formed numbers for the wrong-shape response, which is never checked
/// against a hash.
const R: &str = "5b2fc0e1f6d4a7c98e3b1d2a4f5c6e7d8a9b0c1d2e3f405162738495a6b7c8d9";
const S: &str = "2a4d6f8091b3d5f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f";

#[near(serializers = [json, borsh])]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Answer with the registered signature of the configured sender key.
    Sign,
    /// Answer with the registered signature of a different key: well formed
    /// and genuine for the hash, but not by the configured sender.
    OtherSigner,
    /// Answer with a signature whose recovery id is 2, which cannot sign an
    /// Ethereum transaction.
    BadSignature,
    /// Panic, as the real signer does when it times out.
    Fail,
    /// A successful receipt carrying valid JSON of the wrong shape.
    WrongShape,
    /// A successful receipt carrying bytes that are not JSON.
    Garbage,
    /// A successful receipt carrying a genuine, assemblable signature padded
    /// past any sensible read buffer.
    Oversized,
}

#[near(serializers = [json, borsh])]
#[derive(Clone)]
pub struct Request {
    pub predecessor: AccountId,
    pub path: String,
    /// The 32-byte payload, hex.
    pub hash: String,
    pub domain_id: u64,
}

/// A signature the test registered for a payload, by one of two keys.
#[near(serializers = [json, borsh])]
#[derive(Clone)]
pub struct Registered {
    /// The 32-byte payload, hex without a prefix.
    pub hash: String,
    /// `sender` or `other`.
    pub key: String,
    pub r: String,
    pub s: String,
    pub recovery_id: u8,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct MockSigner {
    mode: Mode,
    /// Extra cross-contract hops before answering, so a request stays in
    /// flight for several blocks.
    slow_hops: u32,
    /// While set, `sign` records the request and then waits, yielded, until
    /// `release_held` resumes it. The wait has no clock of its own, so a test
    /// that holds a request controls exactly how long it stays in flight.
    hold: bool,
    /// The requests waiting on `release_held`, by payload hash.
    held: Vec<(String, YieldId)>,
    requests: Vec<Request>,
    signatures: Vec<Registered>,
}

#[near]
impl MockSigner {
    #[init]
    pub fn new() -> Self {
        Self {
            mode: Mode::Sign,
            slow_hops: 0,
            hold: false,
            held: Vec::new(),
            requests: Vec::new(),
            signatures: Vec::new(),
        }
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// Registers signatures made over payloads by the `sender` or `other` key.
    pub fn register_signatures(&mut self, signatures: Vec<Registered>) {
        for signature in &signatures {
            require!(
                signature.key == "sender" || signature.key == "other",
                "mock signer: key must be sender or other"
            );
        }
        self.signatures.extend(signatures);
    }

    pub fn set_slow_hops(&mut self, hops: u32) {
        self.slow_hops = hops;
    }

    /// Holds every `sign` request that arrives while it is set, until
    /// `release_held`. Unlike `set_slow_hops`, which stretches an answer over a
    /// fixed number of blocks, this does not depend on timing: the request is
    /// in flight exactly until the test releases it (or the runtime's yield
    /// timeout, about 200 blocks, fails it). Turn it off before releasing, so
    /// later requests are answered at once.
    pub fn set_hold(&mut self, hold: bool) {
        self.hold = hold;
    }

    /// Resumes every held request, which then answers as it would have
    /// without the hold (including failing, for a payload with no signature
    /// registered). Returns how many were waiting.
    pub fn release_held(&mut self) -> u32 {
        let held = std::mem::take(&mut self.held);
        let waiting = held.len() as u32;
        for (_, yield_id) in held {
            // A request that already timed out is gone; nothing to resume.
            let _ = yield_id.resume(b"released");
        }
        waiting
    }

    #[payable]
    pub fn sign(&mut self, request: Value) -> PromiseOrValue<Value> {
        assert_one_yocto();
        require!(
            env::prepaid_gas() >= Gas::from_tgas(15),
            "mock signer: sign needs at least 15 Tgas"
        );
        let hash = request["payload_v2"]["Ecdsa"]
            .as_str()
            .unwrap_or_else(|| env::panic_str("mock signer: missing payload_v2.Ecdsa"))
            .to_string();
        require!(hash.len() == 64, "mock signer: payload must be 32 bytes");
        let domain_id = request["domain_id"]
            .as_u64()
            .unwrap_or_else(|| env::panic_str("mock signer: missing domain_id"));
        require!(domain_id == 0, "mock signer: only domain 0 is secp256k1");
        self.requests.push(Request {
            predecessor: env::predecessor_account_id(),
            path: request["path"].as_str().unwrap_or_default().to_string(),
            hash: hash.clone(),
            domain_id,
        });
        if self.hold {
            let (promise, yield_id) = Promise::new_yield(
                "answer_held",
                json!({ "hash": hash }).to_string().into_bytes(),
                HELD_ANSWER_GAS,
                GasWeight(1),
            );
            self.held.push((hash, yield_id));
            return PromiseOrValue::Promise(promise);
        }
        self.answer(self.slow_hops, &hash)
    }

    /// The callback of a held request: runs when `release_held` resumes it, or
    /// fails if the runtime timed the yield out first.
    #[private]
    pub fn answer_held(&self, hash: String) -> PromiseOrValue<Value> {
        if matches!(
            env::promise_result_checked(0, 64),
            Err(PromiseError::Failed)
        ) {
            env::panic_str("mock signer: the held request timed out");
        }
        self.answer(self.slow_hops, &hash)
    }

    #[private]
    pub fn hop(&mut self, remaining: u32, hash: String) -> PromiseOrValue<Value> {
        self.answer(remaining, &hash)
    }

    /// A successful result that is not JSON: borsh-encoded bytes.
    #[private]
    #[result_serializer(borsh)]
    pub fn garbage(&self) -> Vec<u8> {
        b"\xff\xfe not a signature".to_vec()
    }

    pub fn get_requests(&self) -> Vec<Request> {
        self.requests.clone()
    }
}

impl MockSigner {
    fn registered(&self, hash: &str, key: &str) -> &Registered {
        self.signatures
            .iter()
            .find(|signature| signature.hash == hash && signature.key == key)
            .unwrap_or_else(|| env::panic_str("mock signer: no signature registered for the hash"))
    }

    fn answer(&self, remaining: u32, hash: &str) -> PromiseOrValue<Value> {
        if remaining > 0 {
            return PromiseOrValue::Promise(
                Self::ext(env::current_account_id()).hop(remaining - 1, hash.to_string()),
            );
        }
        let (key, recovery_override) = match self.mode {
            Mode::Sign | Mode::Oversized => ("sender", None),
            Mode::OtherSigner => ("other", None),
            Mode::BadSignature => ("sender", Some(2)),
            Mode::Fail => env::panic_str("mock signer: set to fail"),
            Mode::WrongShape => {
                return PromiseOrValue::Value(json!({
                    "big_r": format!("02{R}"),
                    "s": S,
                    "recovery_id": 1,
                }));
            }
            Mode::Garbage => {
                return PromiseOrValue::Promise(Self::ext(env::current_account_id()).garbage());
            }
        };
        let signature = self.registered(hash, key);
        let mut response = json!({
            "scheme": "Secp256k1",
            "big_r": { "affine_point": format!("02{}", signature.r) },
            "s": { "scalar": signature.s },
            "recovery_id": recovery_override.unwrap_or(signature.recovery_id),
        });
        if self.mode == Mode::Oversized {
            response["padding"] = json!("x".repeat(OVERSIZED_PADDING));
        }
        PromiseOrValue::Value(response)
    }
}
