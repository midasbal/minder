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
    /// What this mandate may do beyond the global policy: nothing, or less.
    pub scope: MandateScope,
}

/// The most entries each list of a [`MandateScope`] may hold. Bounds what is
/// stored and the work of one evaluation.
pub const MAX_SCOPE_ENTRIES: usize = 64;

/// Least-privilege limits on a mandate, set at grant time. The authority a
/// mandate grants is always the **intersection** of the global policy and this
/// scope: the global checks run first and a scope can only subtract from them,
/// never add.
///
/// A field left `None` inherits: it adds no restriction beyond the global
/// policy, and an all-`None` scope (the default) behaves exactly as a mandate
/// did before scopes existed. A field that is `Some` restricts, and `Some` of
/// an empty list permits nothing of that kind.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MandateScope {
    /// The only assets the agent may move.
    pub allowed_assets: Option<Vec<AssetId>>,
    /// The only NEAR recipients (for NEAR and NEP-141 transfers).
    pub allowed_near_recipients: Option<Vec<AccountId>>,
    /// The only EVM recipients, as lowercase `0x` addresses. Entered in any
    /// letter case; stored normalized.
    pub allowed_evm_recipients: Option<Vec<String>>,
    /// A per-transaction cap in addition to the global one: the effective cap
    /// is the smaller of the two. It is in each asset's own units (for EVM, the
    /// outlay in wei), so it is only meaningful together with `allowed_assets`
    /// naming assets that share a unit.
    pub per_tx_cap: Option<U128>,
}

impl MandateScope {
    /// Checks the scope and puts it in its stored form (EVM addresses
    /// normalized to lowercase), or says what is wrong with it.
    pub fn validated(mut self) -> Result<Self, &'static str> {
        let too_long = |len: Option<usize>| len.is_some_and(|len| len > MAX_SCOPE_ENTRIES);
        if too_long(self.allowed_assets.as_ref().map(Vec::len))
            || too_long(self.allowed_near_recipients.as_ref().map(Vec::len))
            || too_long(self.allowed_evm_recipients.as_ref().map(Vec::len))
        {
            return Err("A scope list may hold at most 64 entries");
        }
        if let Some(addresses) = &mut self.allowed_evm_recipients {
            for text in addresses.iter_mut() {
                let address = crate::evm::parse_address(text)
                    .ok_or("Invalid EVM address in the scope: expected 0x and 40 hex digits")?;
                *text = crate::evm::format_address(&address);
            }
        }
        Ok(self)
    }
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
    // Appended, not inserted: enum tags are part of the borsh form. Checked
    // after every global check; see `policy::evaluate`.
    /// The mandate's scope does not include this asset.
    AssetOutOfScope,
    /// The mandate's scope does not include this recipient.
    RecipientOutOfScope,
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
    /// The transaction for a transfer of `value` wei to `to` at `nonce`, on the
    /// configured chain and at the configured fees. The only place a transaction
    /// to be signed is assembled, for the agent path and the owner path alike.
    pub fn build_tx(&self, nonce: u64, to: [u8; 20], value: u128) -> crate::evm::Eip1559Tx {
        crate::evm::Eip1559Tx {
            chain_id: self.chain_id,
            nonce,
            max_priority_fee_per_gas: self.max_priority_fee_per_gas.0,
            max_fee_per_gas: self.max_fee_per_gas.0,
            gas_limit: self.gas_limit,
            to,
            value,
        }
    }

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
