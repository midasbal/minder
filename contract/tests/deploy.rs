//! The keyless deployment, proven against the real protocol in a sandbox.
//!
//! Minder is deployed to a contract account that has **no full-access key**,
//! so nobody, the deployer included, can act as the account, redeploy it or
//! change its code, and the contract is the only authority over its own
//! account. The way to get there is one transaction with exactly four actions
//! (CreateAccount, Transfer, DeployContract, and the `init` FunctionCall) and
//! no AddKey. The same sequence is what `deploy/deploy-keyless.sh` sends.
//!
//! Every test here deploys the production wasm (no test feature): the one that
//! ships, not the sandbox build the other binaries use.
//!
//! The only keys the account ever holds after that are the method-restricted
//! function-call keys the contract adds itself in `grant_mandate`, and the test
//! shows both: the account starts with none, gains exactly one restricted key,
//! and is keyless again when it is revoked.

mod common;

use common::*;
use near_sdk::serde_json::{json, Value};
use near_workspaces::network::Sandbox;
use near_workspaces::operations::Function;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{AccessKeyPermission, Gas, KeyType, NearToken, SecretKey};
use near_workspaces::{Account, AccountId, Worker};

/// The only methods the agent's key may call (`AGENT_METHODS` in `lib.rs`).
const AGENT_METHODS: [&str; 3] = [
    "request_local_transfer",
    "request_evm_payment",
    "request_evm_token_payment",
];

/// What the contract account is funded with: storage staking for the wasm and
/// room to pay for the agent key's gas.
const FUNDING: NearToken = NearToken::from_near(10);

/// The account the contract is deployed to: a sub-account of the deployer, as
/// the deploy script's account must be (only a parent can create a sub-account).
fn contract_id(deployer: &Account) -> AccountId {
    format!("minder.{}", deployer.id()).parse().unwrap()
}

fn init_args(owner: &AccountId) -> Value {
    json!({
        "owner": owner,
        "window_len": WINDOW_LEN.to_string(),
        "num_buckets": NUM_BUCKETS,
    })
}

/// The one transaction: CreateAccount, Transfer, DeployContract, `init`. No
/// AddKey. It is signed by `deployer`, which is therefore the caller of `init`
/// and so must be the owner (with no key on the new account, nobody else can
/// be).
async fn deploy_in_one_batch(
    deployer: &Account,
    contract: &AccountId,
    owner: &AccountId,
) -> Result<ExecutionFinalResult, near_workspaces::error::Error> {
    deploy_with_init_args(deployer, contract, init_args(owner)).await
}

/// The same transaction with explicit `init` arguments.
async fn deploy_with_init_args(
    deployer: &Account,
    contract: &AccountId,
    init_args: Value,
) -> Result<ExecutionFinalResult, near_workspaces::error::Error> {
    deployer
        .batch(contract)
        .create_account()
        .transfer(FUNDING)
        .deploy(production_wasm().await)
        .call(
            Function::new("init")
                .args_json(init_args)
                .gas(Gas::from_tgas(30)),
        )
        .transact()
        .await
}

async fn keys(
    worker: &Worker<Sandbox>,
    account: &AccountId,
) -> Vec<(near_workspaces::types::PublicKey, AccessKeyPermission)> {
    worker
        .view_access_keys(account)
        .await
        .unwrap()
        .into_iter()
        .map(|info| (info.public_key, info.access_key.permission))
        .collect()
}

fn full_access_count(all: &[(near_workspaces::types::PublicKey, AccessKeyPermission)]) -> usize {
    all.iter()
        .filter(|(_, permission)| matches!(permission, AccessKeyPermission::FullAccess))
        .count()
}

/// What the runtime says when it refuses a transaction, or what the receipt
/// says if it was accepted and failed.
fn refusal(result: Result<ExecutionFinalResult, near_workspaces::error::Error>) -> String {
    match result {
        Err(error) => format!("{error:?}"),
        Ok(result) => match result.into_result() {
            Err(failure) => format!("EXECUTED AND FAILED: {failure:?}"),
            Ok(_) => "ACCEPTED".to_string(),
        },
    }
}

/// Whether the refusal says the signing key is not on the account: the node's
/// `AccessKeyNotFound`, or the client's own `UnknownAccessKey` when it asks the
/// node for the key's nonce before signing.
fn key_is_unknown(why: &str) -> bool {
    why.contains("AccessKeyNotFound") || why.contains("UnknownAccessKey")
}

/// A sandbox with a funded deployer, which is also the owner.
async fn deployer() -> (Worker<Sandbox>, Account, AccountId) {
    let worker = near_workspaces::sandbox().await.unwrap();
    let deployer = worker.dev_create_account().await.unwrap();
    let contract = contract_id(&deployer);
    (worker, deployer, contract)
}

#[tokio::test]
async fn one_batch_with_no_add_key_deploys_an_initialized_contract_with_no_full_access_key() {
    let (worker, deployer, contract) = deployer().await;

    deploy_in_one_batch(&deployer, &contract, deployer.id())
        .await
        .unwrap()
        .into_result()
        .unwrap();

    // The account holds no key at all, so no full-access key: the batch had no
    // AddKey, and nothing else puts one there.
    let after = keys(&worker, &contract).await;
    assert_eq!(full_access_count(&after), 0);
    assert!(after.is_empty(), "the account holds no key: {after:?}");

    // It is deployed and initialized by that same transaction: the owner is the
    // deployer, the status is active and the contract answers.
    let config: Value = worker
        .view(&contract, "get_config")
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(config["owner"], json!(deployer.id()));
    assert_eq!(config["status"], "Active");
    let account = worker.view_account(&contract).await.unwrap();
    assert!(
        account.balance >= NearToken::from_near(5),
        "funded: {account:?}"
    );
    // The code on the account is the production wasm, byte for byte.
    assert_eq!(
        &worker.view_code(&contract).await.unwrap(),
        production_wasm().await
    );

    // It is the production wasm: the sandbox build's seeding method is absent.
    let why = refusal(
        deployer
            .call(&contract, "test_record_spend")
            .args_json(json!({ "asset": near(), "amount": "1" }))
            .transact()
            .await,
    );
    assert!(why.contains("MethodNotFound"), "{why}");
}

#[tokio::test]
async fn nobody_can_act_as_the_keyless_account_not_even_the_deployer() {
    let (worker, deployer, contract) = deployer().await;
    deploy_in_one_batch(&deployer, &contract, deployer.id())
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let code_before = worker.view_code(&contract).await.unwrap();

    // The deployer's own key signs as the contract account: it is not a key of
    // that account, so the protocol refuses the transaction before anything
    // runs. Redeploying, adding a key and moving funds are all refused alike.
    let impostor =
        Account::from_secret_key(contract.clone(), deployer.secret_key().clone(), &worker);
    let attempts = [
        refusal(
            impostor
                .batch(&contract)
                .deploy(&[0, 97, 115, 109])
                .transact()
                .await,
        ),
        refusal(
            impostor
                .batch(&contract)
                .add_key(
                    SecretKey::from_random(KeyType::ED25519).public_key(),
                    near_workspaces::types::AccessKey::full_access(),
                )
                .transact()
                .await,
        ),
        refusal(
            impostor
                .batch(deployer.id())
                .transfer(NearToken::from_near(1))
                .transact()
                .await,
        ),
    ];
    for why in &attempts {
        assert!(
            key_is_unknown(why),
            "must be refused as an unknown key: {why}"
        );
    }

    // Nothing changed: same code, still no key.
    assert_eq!(worker.view_code(&contract).await.unwrap(), code_before);
    assert!(keys(&worker, &contract).await.is_empty());
}

#[tokio::test]
async fn the_account_only_ever_gains_function_call_keys_restricted_to_the_request_methods() {
    let (worker, deployer, contract) = deployer().await;
    deploy_in_one_batch(&deployer, &contract, deployer.id())
        .await
        .unwrap()
        .into_result()
        .unwrap();
    assert!(keys(&worker, &contract).await.is_empty(), "starts keyless");

    // Positive control: the owner grants a mandate, and the contract adds the
    // agent's key to its own account.
    let agent_secret = SecretKey::from_random(KeyType::ED25519);
    let agent_key = agent_secret.public_key();
    let expiry = worker.view_block().await.unwrap().timestamp() + 3_600 * SECOND;
    deployer
        .call(&contract, "grant_mandate")
        .args_json(json!({ "agent_key": agent_key.to_string(), "expiry": expiry.to_string() }))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();

    let after = keys(&worker, &contract).await;
    assert_eq!(after.len(), 1, "exactly the agent's key: {after:?}");
    assert_eq!(full_access_count(&after), 0, "and it is not full access");
    let (key, permission) = &after[0];
    assert_eq!(key, &agent_key);
    let AccessKeyPermission::FunctionCall(permission) = permission else {
        panic!("the agent key must be a function-call key, not full access");
    };
    assert_eq!(permission.receiver_id, contract.to_string());
    assert_eq!(permission.method_names, AGENT_METHODS);
    assert_eq!(permission.allowance, Some(NearToken::from_millinear(250)));

    // That key cannot become more than it is: it cannot redeploy or add a key.
    let agent = Account::from_secret_key(contract.clone(), agent_secret, &worker);
    let deploy = refusal(
        agent
            .batch(&contract)
            .deploy(&[0, 97, 115, 109])
            .transact()
            .await,
    );
    assert!(deploy.contains("RequiresFullAccess"), "{deploy}");
    let add = refusal(
        agent
            .batch(&contract)
            .add_key(
                SecretKey::from_random(KeyType::ED25519).public_key(),
                near_workspaces::types::AccessKey::full_access(),
            )
            .transact()
            .await,
    );
    assert!(add.contains("RequiresFullAccess"), "{add}");

    // Owner governance never needed a key on the contract account: the owner
    // methods authenticate by predecessor, from the owner's own account.
    for method in ["freeze", "unfreeze"] {
        deployer
            .call(&contract, method)
            .transact()
            .await
            .unwrap()
            .into_result()
            .unwrap();
    }
    deployer
        .call(&contract, "set_asset_policy")
        .args_json(json!({ "asset": near(), "per_tx_cap": "50", "window_limit": "100" }))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    // And a stranger is still refused them.
    let stranger = worker.dev_create_account().await.unwrap();
    let refused = refusal(stranger.call(&contract, "freeze").transact().await);
    assert!(refused.contains("Only the owner"), "{refused}");

    // Revoking deletes the key again: the account is keyless once more.
    deployer
        .call(&contract, "revoke_mandate")
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    assert!(keys(&worker, &contract).await.is_empty(), "keyless again");
}

#[tokio::test]
async fn a_failed_init_in_the_batch_leaves_no_account_behind() {
    let (worker, deployer, contract) = deployer().await;

    // `init` refuses a window that does not divide into its buckets, after it
    // has accepted its caller. The batch is one receipt, so that failure takes
    // the account creation, the funding and the deployment with it: no keyless,
    // uninitialized account is ever left behind.
    let mut bad = init_args(deployer.id());
    bad["num_buckets"] = json!(0);
    let before = deployer.view_account().await.unwrap().balance;
    let why = refusal(deploy_with_init_args(&deployer, &contract, bad).await);
    assert!(
        why.contains("num_buckets must be between 1 and 64"),
        "{why}"
    );
    assert!(
        worker.view_account(&contract).await.is_err(),
        "the account must not exist after a failed batch"
    );
    // The funding came back: only gas was spent.
    let after = deployer.view_account().await.unwrap().balance;
    assert!(
        before.as_yoctonear() - after.as_yoctonear() < NearToken::from_near(1).as_yoctonear(),
        "the transfer must be refunded: {before} -> {after}"
    );

    // And the same names can then be deployed properly.
    deploy_in_one_batch(&deployer, &contract, deployer.id())
        .await
        .unwrap()
        .into_result()
        .unwrap();
    assert!(keys(&worker, &contract).await.is_empty());
}

#[tokio::test]
async fn only_the_batch_signer_can_be_the_owner_so_nobody_can_claim_a_keyless_contract() {
    let (worker, deployer, contract) = deployer().await;
    let stranger = worker.dev_create_account().await.unwrap();

    // Naming someone else as the owner fails: with no key on the new account the
    // caller of `init` is the signer, and `init` requires it to be the owner.
    let why = refusal(deploy_in_one_batch(&deployer, &contract, stranger.id()).await);
    assert!(why.contains("owner or the contract account"), "{why}");
    assert!(worker.view_account(&contract).await.is_err());
}

#[tokio::test]
async fn control_the_same_batch_with_an_add_key_leaves_a_full_access_key_so_the_check_can_fail() {
    // Without this the zero-key assertions above could pass for the wrong
    // reason. The only difference from the keyless batch is one AddKey action,
    // and the account then does hold a full-access key, which the same check
    // sees.
    let (worker, deployer, contract) = deployer().await;
    let key = SecretKey::from_random(KeyType::ED25519).public_key();
    deployer
        .batch(&contract)
        .create_account()
        .transfer(FUNDING)
        .add_key(
            key.clone(),
            near_workspaces::types::AccessKey::full_access(),
        )
        .deploy(production_wasm().await)
        .call(
            Function::new("init")
                .args_json(init_args(deployer.id()))
                .gas(Gas::from_tgas(30)),
        )
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let after = keys(&worker, &contract).await;
    assert_eq!(full_access_count(&after), 1, "{after:?}");
    assert_eq!(after[0].0, key);
}
