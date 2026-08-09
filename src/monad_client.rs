//! An immutable audit trail for IntentGuard decisions, on Monad testnet.
//!
//! # Why this exists
//!
//! Rain sees only payment data. IntentGuard sees the task. But without this
//! module, the fact that a decision was made — and on what grounds — exists
//! only in this program's own printed output. A third party auditing the
//! system after the fact has nothing to check: the same program that made the
//! decision is the only witness that it happened.
//!
//! Anchoring each decision to a public chain makes "this exact decision was
//! made, at this time" provable without trusting IntentGuard's own logs.
//!
//! Only a hash goes on chain, never the task itself. Destination, budget, and
//! purpose are exactly the private context that makes IntentGuard work, and
//! publishing them would defeat the point. Anyone holding the original task can
//! recompute the hash and prove it matches; the chain alone reveals nothing.
//!
//! # Best-effort by construction
//!
//! This is an audit trail, not a control. The decision has already been made
//! and the Rain flow has already run by the time anything here is called, so a
//! testnet hiccup must never change an outcome or stop the program. Every entry
//! point returns a `Result` the caller is expected to report and move past.

use std::time::Duration;

use alloy::network::EthereumWallet;
use alloy::primitives::{Address, B256, keccak256};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use alloy::transports::http::reqwest::Url;
use serde::Serialize;

use crate::types::{Decision, ProposedAction, Task};

/// Testnet can be slower than a centralized API, so this is looser than the
/// Anthropic timeout — but still bounded, because the demo cannot hang.
const TIMEOUT: Duration = Duration::from_secs(15);

sol! {
    #[sol(rpc)]
    contract IntentGuardAuditLog {
        function logDecision(bytes32 decisionHash, string calldata status) external;
        function getRecord(uint256 index) external view returns (bytes32, string memory, uint256);
        function recordCount() external view returns (uint256);
    }
}

// ---------------------------------------------------------------- errors ----

#[derive(Debug)]
pub enum Error {
    MissingEnv(&'static str),
    /// A malformed address or key in the environment.
    BadConfig(String),
    /// Anything the chain or the transport rejected.
    Chain(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::MissingEnv(var) => write!(f, "missing environment variable {var}"),
            Error::BadConfig(detail) => write!(f, "bad monad config: {detail}"),
            Error::Chain(detail) => write!(f, "monad call failed: {detail}"),
        }
    }
}

impl std::error::Error for Error {}

// ---------------------------------------------------------------- config ----

/// Monad testnet connection details. Loaded from the environment, never logged.
pub struct MonadConfig {
    rpc_url: String,
    address: Address,
    contract: Address,
    /// One provider for the whole run, built once here.
    ///
    /// This is not just an optimization. Alloy's default nonce filler caches the
    /// account's next nonce *per provider* and increments it locally as each
    /// transaction is sent. A provider built fresh per call starts with an empty
    /// cache, so it asks the node for the nonce — and Monad answers with a count
    /// that does not yet include the transaction we sent a moment ago and that
    /// has not been mined. Two decisions logged in quick succession then get the
    /// same nonce, and the node rejects the second one:
    ///
    /// ```text
    /// error code -32603: An existing transaction had higher priority
    /// ```
    ///
    /// Sharing one provider keeps that nonce cache alive across every write in a
    /// run, so the decisions are numbered in the order they were made.
    provider: DynProvider,
}

/// Hand-written so an accidental `{:?}` cannot leak the signing key, which the
/// provider's wallet still holds.
impl std::fmt::Debug for MonadConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MonadConfig")
            .field("rpc_url", &self.rpc_url)
            .field("address", &self.address)
            .field("contract", &self.contract)
            .field("provider", &"<redacted>")
            .finish()
    }
}

impl MonadConfig {
    pub fn from_env() -> Result<Self, Error> {
        fn var(name: &'static str) -> Result<String, Error> {
            match std::env::var(name) {
                Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
                _ => Err(Error::MissingEnv(name)),
            }
        }

        let signer: PrivateKeySigner = var("MONAD_PRIVATE_KEY")?
            .parse()
            .map_err(|e| Error::BadConfig(format!("MONAD_PRIVATE_KEY is not a valid key: {e}")))?;

        let contract: Address = var("MONAD_CONTRACT_ADDRESS")?.parse().map_err(|e| {
            Error::BadConfig(format!("MONAD_CONTRACT_ADDRESS is not an address: {e}"))
        })?;

        let rpc_url = var("MONAD_RPC_URL")?;
        let url: Url = rpc_url
            .parse()
            .map_err(|e| Error::BadConfig(format!("MONAD_RPC_URL is not a valid URL: {e}")))?;

        let address = signer.address();

        // `connect_http` does no network I/O, so a config that builds here can
        // still fail at the first call — which is what the callers expect.
        let provider = ProviderBuilder::new()
            .wallet(EthereumWallet::from(signer))
            .connect_http(url)
            .erased();

        Ok(Self {
            rpc_url,
            address,
            contract,
            provider,
        })
    }

    /// The address decisions are logged from — printed so a verifier knows
    /// whose transactions to look at.
    pub fn address(&self) -> Address {
        self.address
    }
}

// ----------------------------------------------------------------- hash -----

/// Exactly what gets hashed. Named and ordered so the digest is reproducible
/// by anyone holding the same task, action, and decision.
#[derive(Serialize)]
struct DecisionCommitment<'a> {
    task: &'a Task,
    action: &'a ProposedAction,
    status: &'a str,
    reason: &'a str,
}

/// Commits a decision to a single 32-byte digest.
///
/// Synchronous and offline — the hash is computed whether or not the chain is
/// reachable, so a verifier can always recompute it from the original inputs.
pub fn hash_decision(task: &Task, action: &ProposedAction, decision: &Decision) -> [u8; 32] {
    let commitment = DecisionCommitment {
        task,
        action,
        status: &decision.status,
        reason: &decision.reason,
    };

    // serde_json emits struct fields in declaration order, so this serialization
    // is stable across runs for a given set of inputs.
    let encoded = serde_json::to_vec(&commitment)
        .expect("DecisionCommitment is plain data and cannot fail to serialize");

    keccak256(encoded).into()
}

// ------------------------------------------------------------ chain calls ---

/// Writes a decision hash to the audit log. Returns the transaction hash.
///
/// Callers are expected to report a failure and carry on: nothing about the
/// decision or the Rain flow depends on this succeeding.
pub async fn log_decision_to_monad(
    config: &MonadConfig,
    decision_hash: [u8; 32],
    status: &str,
) -> Result<String, Error> {
    let contract = IntentGuardAuditLog::new(config.contract, config.provider.clone());

    let pending = tokio::time::timeout(
        TIMEOUT,
        contract
            .logDecision(B256::from(decision_hash), status.to_string())
            .send(),
    )
    .await
    .map_err(|_| Error::Chain(format!("timed out after {}s", TIMEOUT.as_secs())))?
    .map_err(|e| Error::Chain(format!("transaction rejected: {e}")))?;

    Ok(format!("{:#x}", pending.tx_hash()))
}

/// Reads a record back out of the contract.
///
/// This is the independent-verification path: it goes to the chain, not to
/// anything this program remembers.
pub async fn read_decision_from_monad(
    config: &MonadConfig,
    index: u64,
) -> Result<(String, String, u64), Error> {
    let contract = IntentGuardAuditLog::new(config.contract, config.provider.clone());

    let record = tokio::time::timeout(
        TIMEOUT,
        contract
            .getRecord(alloy::primitives::U256::from(index))
            .call(),
    )
    .await
    .map_err(|_| Error::Chain(format!("timed out after {}s", TIMEOUT.as_secs())))?
    .map_err(|e| Error::Chain(format!("could not read record {index}: {e}")))?;

    Ok((
        format!("{:#x}", record._0),
        record._1,
        record._2.to::<u64>(),
    ))
}

/// How many decisions the contract holds. Used to pick a record to verify.
pub async fn record_count(config: &MonadConfig) -> Result<u64, Error> {
    let contract = IntentGuardAuditLog::new(config.contract, config.provider.clone());

    let count = tokio::time::timeout(TIMEOUT, contract.recordCount().call())
        .await
        .map_err(|_| Error::Chain(format!("timed out after {}s", TIMEOUT.as_secs())))?
        .map_err(|e| Error::Chain(format!("could not read record count: {e}")))?;

    Ok(count.to::<u64>())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test here is offline — `hash_decision` never touches the network,
    /// which is exactly why it can be tested and a chain write cannot.
    fn task() -> Task {
        Task {
            destination: "NYC".to_string(),
            max_budget: 500.0,
            purpose: "hackathon attendance".to_string(),
        }
    }

    fn action() -> ProposedAction {
        ProposedAction {
            task_id: "t-1".to_string(),
            destination: "NYC".to_string(),
            amount: 480.0,
            description: "Delta flight to NYC".to_string(),
        }
    }

    #[test]
    fn is_deterministic() {
        let decision = Decision::approved("looks fine");
        let first = hash_decision(&task(), &action(), &decision);
        let second = hash_decision(&task(), &action(), &decision);
        assert_eq!(first, second);
    }

    #[test]
    fn a_different_task_changes_the_hash() {
        let decision = Decision::approved("looks fine");
        let baseline = hash_decision(&task(), &action(), &decision);

        let mut elsewhere = task();
        elsewhere.destination = "Miami".to_string();
        assert_ne!(baseline, hash_decision(&elsewhere, &action(), &decision));

        let mut richer = task();
        richer.max_budget = 900.0;
        assert_ne!(baseline, hash_decision(&richer, &action(), &decision));
    }

    #[test]
    fn a_different_action_changes_the_hash() {
        let decision = Decision::approved("looks fine");
        let baseline = hash_decision(&task(), &action(), &decision);

        let mut pricier = action();
        pricier.amount = 481.0;
        assert_ne!(baseline, hash_decision(&task(), &pricier, &decision));
    }

    /// The status and the reason are the audit-relevant part: the same task and
    /// action decided differently must not share a digest.
    #[test]
    fn a_different_decision_changes_the_hash() {
        let approved = hash_decision(&task(), &action(), &Decision::approved("same words"));
        let blocked = hash_decision(&task(), &action(), &Decision::blocked("same words"));
        let escalated = hash_decision(&task(), &action(), &Decision::escalated("same words"));

        assert_ne!(approved, blocked);
        assert_ne!(approved, escalated);
        assert_ne!(blocked, escalated);
    }

    #[test]
    fn a_different_reason_changes_the_hash() {
        let first = hash_decision(&task(), &action(), &Decision::blocked("wrong city"));
        let second = hash_decision(&task(), &action(), &Decision::blocked("over budget"));
        assert_ne!(first, second);
    }

    #[test]
    fn produces_a_full_32_byte_digest() {
        let hash = hash_decision(&task(), &action(), &Decision::approved("x"));
        assert_eq!(hash.len(), 32);
        assert!(
            hash.iter().any(|&b| b != 0),
            "digest should not be all zeros"
        );
    }
}
