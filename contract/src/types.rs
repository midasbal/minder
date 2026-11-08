//! Data types shared by state, evaluation and the public interface.

use near_sdk::json_types::{U128, U64};
use near_sdk::{near, AccountId};

/// Identifies an asset a policy can govern. Extended with an EVM variant when
/// the cross-chain surface is added; borsh enum tags are append-only.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AssetId {
    /// The native NEAR token.
    Near,
    /// A NEP-141 fungible token, identified by its contract account.
    Nep141(AccountId),
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
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mandate {
    pub agent: AccountId,
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
