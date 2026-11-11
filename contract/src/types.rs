//! Data types shared by state, evaluation and the public interface.

use near_sdk::json_types::{U128, U64};
use near_sdk::{near, AccountId, PublicKey};

/// Identifies an asset a policy can govern. Borsh enum tags are append-only.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AssetId {
    /// The native NEAR token.
    Near,
    /// A NEP-141 fungible token, identified by its contract account.
    Nep141(AccountId),
    /// Native ETH on the EVM chain with this chain id. Paid out by signing a
    /// transaction, not by a NEAR transfer, so it has its own entry point.
    Evm(u64),
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Active,
    Frozen,
}

/// Immutable-after-init contract configuration.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub owner: AccountId,
    pub status: Status,
    /// Length of the sliding spend window, in nanoseconds.
    pub window_len: U64,
    /// Number of sub-buckets the window is divided into. `1` is a fixed window.
    pub num_buckets: u32,
}

/// Limits for one asset. Having an entry is what allowlists the asset.
#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssetPolicy {
    pub per_tx_cap: U128,
    pub window_limit: U128,
}

/// The agent's authority. A grant always carries a fresh, strictly larger
/// epoch, so a revoked or expired mandate can never be mistaken for a new one.
///
/// The agent is identified by a public key, not an account: the key is
/// provisioned on this contract's own account as a function-call access key
/// limited to the agent methods, and a call is the agent's only if it is signed
/// by exactly this key.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mandate {
    pub agent_key: PublicKey,
    /// Block timestamp (ns) at and after which the mandate is expired.
    pub expiry: U64,
    pub epoch: u64,
    pub revoked: bool,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenyReason {
    Frozen,
    NoMandate,
    MandateRevoked,
    MandateExpired,
    NotAuthorized,
    AssetNotAllowed,
    RecipientNotAllowed,
    PerTxCapExceeded,
    WindowLimitExceeded,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(DenyReason),
}

/// Owner-set parameters for EVM payments. The agent supplies none of these:
/// the owner fixes the signer, key path and fees, and the contract assigns the
/// nonce.
#[near(serializers = [borsh])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvmConfig {
    /// The Chain Signatures contract to ask for signatures. Fixed once set.
    pub signer_id: AccountId,
    /// Derivation path of the signing key. Fixed once set.
    pub derivation_path: String,
    /// This contract's own EVM address: the address the signer derives for this
    /// contract account and `derivation_path`. A signature is only accepted
    /// (its transaction emitted) if it recovers to this address. Fixed once set.
    pub sender: [u8; 20],
    pub chain_id: u64,
    /// Wei per gas, upper bound. Also what fees are accounted at.
    pub max_fee_per_gas: U128,
    pub max_priority_fee_per_gas: U128,
    pub gas_limit: u64,
}

/// The JSON form of [`EvmConfig`], with the sender as a `0x` address rather
/// than a list of twenty numbers.
#[near(serializers = [json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvmConfigView {
    pub signer_id: AccountId,
    pub derivation_path: String,
    pub sender: String,
    pub chain_id: u64,
    pub max_fee_per_gas: U128,
    pub max_priority_fee_per_gas: U128,
    pub gas_limit: u64,
}

impl From<&EvmConfig> for EvmConfigView {
    fn from(config: &EvmConfig) -> Self {
        Self {
            signer_id: config.signer_id.clone(),
            derivation_path: config.derivation_path.clone(),
            sender: crate::evm::format_address(&config.sender),
            chain_id: config.chain_id,
            max_fee_per_gas: config.max_fee_per_gas,
            max_priority_fee_per_gas: config.max_priority_fee_per_gas,
            gas_limit: config.gas_limit,
        }
    }
}

impl EvmConfig {
    /// Worst-case wei the transaction can spend on fees. Saturates; `set_evm_config`
    /// rejects configurations where it would.
    pub fn max_fee_cost(&self) -> u128 {
        self.max_fee_per_gas
            .0
            .saturating_mul(u128::from(self.gas_limit))
    }

    /// What a payment of `value` wei can cost the signing address in total:
    /// the value plus the worst-case fee. This is what policy limits count.
    pub fn outlay(&self, value: u128) -> u128 {
        value.saturating_add(self.max_fee_cost())
    }
}
