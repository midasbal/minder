//! Shared fixture: builds the contract once with the `test-helpers` feature,
//! deploys it into a fresh sandbox and offers typed call/view helpers.

#![allow(dead_code)] // each test binary uses a different subset

use std::process::Command;

use minder::Decision;
use near_sdk::serde::de::DeserializeOwned;
use near_sdk::serde_json::{json, Value};
use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::{Account, AccountId, Contract, Worker};
use tokio::sync::OnceCell;

pub const SECOND: u64 = 1_000_000_000;
pub const NUM_BUCKETS: u32 = 4;
pub const SLOT_LEN: u64 = 30 * SECOND;
pub const WINDOW_LEN: u64 = SLOT_LEN * NUM_BUCKETS as u64;

pub const PER_TX_CAP: u128 = 50;
pub const WINDOW_LIMIT: u128 = 100;

static WASM: OnceCell<Vec<u8>> = OnceCell::const_new();

async fn contract_wasm() -> &'static Vec<u8> {
    WASM.get_or_init(|| async {
        let out_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/target/test-wasm");
        let status = Command::new("cargo")
            .args(["near", "build", "non-reproducible-wasm", "--locked"])
            .args([
                "--features",
                "test-helpers",
                "--no-abi",
                "--out-dir",
                out_dir,
            ])
            .env(
                "CARGO_TARGET_DIR",
                concat!(env!("CARGO_MANIFEST_DIR"), "/target/test-build"),
            )
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .expect("cargo near must be installed");
        assert!(status.success(), "building the test wasm failed");
        std::fs::read(format!("{out_dir}/minder.wasm")).expect("test wasm missing")
    })
    .await
}

pub fn near() -> Value {
    json!("Near")
}

pub fn nep141() -> Value {
    json!({ "Nep141": "usdc.example.near" })
}

pub struct Fixture {
    pub worker: Worker<Sandbox>,
    pub contract: Contract,
    pub owner: Account,
    pub agent: Account,
    pub outsider: Account,
    pub recipient: Account,
}

impl Fixture {
    /// Deployed but not initialized.
    pub async fn deploy() -> Self {
        let worker = near_workspaces::sandbox().await.unwrap();
        let contract = worker.dev_deploy(contract_wasm().await).await.unwrap();
        Self {
            owner: worker.dev_create_account().await.unwrap(),
            agent: worker.dev_create_account().await.unwrap(),
            outsider: worker.dev_create_account().await.unwrap(),
            recipient: worker.dev_create_account().await.unwrap(),
            worker,
            contract,
        }
    }

    pub fn init_args(&self) -> Value {
        json!({
            "owner": self.owner.id(),
            "window_len": WINDOW_LEN.to_string(),
            "num_buckets": NUM_BUCKETS,
        })
    }

    /// Initialized by the contract account itself, as a deploy-and-init
    /// transaction would do.
    pub async fn new() -> Self {
        let fixture = Self::deploy().await;
        fixture
            .contract
            .call("init")
            .args_json(fixture.init_args())
            .transact()
            .await
            .unwrap()
            .into_result()
            .unwrap();
        fixture
    }

    /// Initialized, with a NEAR policy, an allowed recipient and a day-long
    /// mandate for the agent: every check passes until a test breaks one.
    pub async fn armed() -> Self {
        let fixture = Self::new().await;
        fixture
            .owner_ok(
                "set_asset_policy",
                json!({ "asset": near(), "per_tx_cap": PER_TX_CAP.to_string(), "window_limit": WINDOW_LIMIT.to_string() }),
            )
            .await;
        fixture
            .owner_ok(
                "set_recipient",
                json!({ "recipient": fixture.recipient.id(), "allowed": true }),
            )
            .await;
        let expiry = fixture.now().await + 86_400 * SECOND;
        fixture
            .owner_ok(
                "grant_mandate",
                json!({ "agent": fixture.agent.id(), "expiry": expiry.to_string() }),
            )
            .await;
        fixture
    }

    pub async fn call(&self, who: &Account, method: &str, args: Value) -> ExecutionFinalResult {
        who.call(self.contract.id(), method)
            .args_json(args)
            .transact()
            .await
            .unwrap()
    }

    pub async fn owner_ok(&self, method: &str, args: Value) {
        self.call(&self.owner, method, args)
            .await
            .into_result()
            .unwrap_or_else(|e| panic!("{method} failed: {e:?}"));
    }

    pub async fn view<T: DeserializeOwned>(&self, method: &str, args: Value) -> T {
        self.contract
            .view(method)
            .args_json(args)
            .await
            .unwrap()
            .json()
            .unwrap()
    }

    pub async fn dry_run_as(
        &self,
        caller: &AccountId,
        asset: Value,
        amount: u128,
        recipient: &AccountId,
    ) -> Decision {
        self.view(
            "dry_run",
            json!({ "caller": caller, "asset": asset, "amount": amount.to_string(), "recipient": recipient }),
        )
        .await
    }

    /// As the agent, to the allowed recipient.
    pub async fn dry_run(&self, asset: Value, amount: u128) -> Decision {
        self.dry_run_as(self.agent.id(), asset, amount, self.recipient.id())
            .await
    }

    pub async fn live_spend(&self, asset: Value) -> u128 {
        let spend: String = self.view("get_live_spend", json!({ "asset": asset })).await;
        spend.parse().unwrap()
    }

    pub async fn record_spend(&self, asset: Value, amount: u128) {
        self.owner_ok(
            "test_record_spend",
            json!({ "asset": asset, "amount": amount.to_string() }),
        )
        .await;
    }

    pub async fn now(&self) -> u64 {
        self.worker.view_block().await.unwrap().timestamp()
    }

    /// Fast-forwards until block time is at least `target`. Sandbox time does
    /// not track block counts precisely, so callers must tolerate overshoot.
    pub async fn advance_to(&self, target: u64) {
        while self.now().await < target {
            let remaining = target - self.now().await;
            let blocks = (remaining / (SECOND * 2 / 5)).max(1);
            self.worker.fast_forward(blocks).await.unwrap();
        }
    }
}

/// Asserts the call reverted and its failure mentions `expected`.
pub fn assert_reverts(result: ExecutionFinalResult, expected: &str) {
    let failure = format!(
        "{:?}",
        result.into_result().expect_err("call should have reverted")
    );
    assert!(
        failure.contains(expected),
        "expected `{expected}` in: {failure}"
    );
}

pub fn deny(reason: minder::DenyReason) -> Decision {
    Decision::Deny(reason)
}
