//! Test double for the Chain Signatures contract (`v1.signer`). It enforces the
//! real signer's preconditions on `sign` (1 yoctoNEAR, at least 15 Tgas,
//! domain 0), records each request, and answers with a fixed signature in the
//! shape the real signer returns. It signs nothing.

use near_sdk::serde_json::{json, Value};
use near_sdk::{
    assert_one_yocto, env, near, require, AccountId, Gas, PanicOnDefault, PromiseOrValue,
};

/// The `r` and `s` of the fixed signature (low `s`), as 32-byte hex.
const R: &str = "5b2fc0e1f6d4a7c98e3b1d2a4f5c6e7d8a9b0c1d2e3f405162738495a6b7c8d9";
const S: &str = "2a4d6f8091b3d5f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5d6e7f";

#[near(serializers = [json, borsh])]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Answer with the fixed valid signature.
    Sign,
    /// Answer with a signature whose recovery id is 2, which cannot sign an
    /// Ethereum transaction.
    BadSignature,
    /// Panic, as the real signer does when it times out.
    Fail,
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

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct MockSigner {
    mode: Mode,
    /// Extra cross-contract hops before answering, so a request stays in
    /// flight for several blocks.
    slow_hops: u32,
    requests: Vec<Request>,
}

#[near]
impl MockSigner {
    #[init]
    pub fn new() -> Self {
        Self {
            mode: Mode::Sign,
            slow_hops: 0,
            requests: Vec::new(),
        }
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    pub fn set_slow_hops(&mut self, hops: u32) {
        self.slow_hops = hops;
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
            hash,
            domain_id,
        });
        self.answer(self.slow_hops)
    }

    #[private]
    pub fn hop(&mut self, remaining: u32) -> PromiseOrValue<Value> {
        self.answer(remaining)
    }

    pub fn get_requests(&self) -> Vec<Request> {
        self.requests.clone()
    }
}

impl MockSigner {
    fn answer(&self, remaining: u32) -> PromiseOrValue<Value> {
        if remaining > 0 {
            return PromiseOrValue::Promise(
                Self::ext(env::current_account_id()).hop(remaining - 1),
            );
        }
        let recovery_id = match self.mode {
            Mode::Sign => 1,
            Mode::BadSignature => 2,
            Mode::Fail => env::panic_str("mock signer: set to fail"),
        };
        PromiseOrValue::Value(json!({
            "scheme": "Secp256k1",
            "big_r": { "affine_point": format!("02{R}") },
            "s": { "scalar": S },
            "recovery_id": recovery_id,
        }))
    }
}
