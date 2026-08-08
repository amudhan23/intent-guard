use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rand::RngCore;
use rsa::Oaep;
use rsa::RsaPublicKey;
use rsa::pkcs8::DecodePublicKey;
use serde_json::Value;
use sha1::Sha1;

use crate::intent_check::ApprovedAction;

const RAIN_BASE_URL: &str = "https://api-dev.raincards.xyz/v1";

/// Rain's published sandbox RSA public key. Public by design — not a secret.
const SANDBOX_PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQCAP192809jZyaw62g/eTzJ3P9H
+RmT88sXUYjQ0K8Bx+rJ83f22+9isKx+lo5UuV8tvOlKwvdDS/pVbzpG7D7NO45c
0zkLOXwDHZkou8fuj8xhDO5Tq3GzcrabNLRLVz3dkx0znfzGOhnY4lkOMIdKxlQb
LuVM/dGDC9UpulF+UwIDAQAB
-----END PUBLIC KEY-----";

// ---------------------------------------------------------------- errors ----

#[derive(Debug)]
pub enum Error {
    MissingEnv(&'static str),
    Http(reqwest::Error),
    /// Rain answered with a non-2xx status.
    Api { endpoint: String, status: reqwest::StatusCode, body: String },
    /// Rain answered 2xx but the payload was not shaped how we expected.
    MalformedResponse { endpoint: String, detail: String },
    Crypto(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Only the variable name is printed, never the value.
            Error::MissingEnv(var) => write!(f, "missing environment variable {var}"),
            Error::Http(e) => write!(f, "http transport error: {e}"),
            Error::Api { endpoint, status, body } => {
                write!(f, "rain api {endpoint} returned {status}: {body}")
            }
            Error::MalformedResponse { endpoint, detail } => {
                write!(f, "rain api {endpoint} returned an unexpected payload: {detail}")
            }
            Error::Crypto(detail) => write!(f, "session id generation failed: {detail}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e)
    }
}

// ---------------------------------------------------------------- config ----

/// Rain credentials. Loaded from the environment, never logged.
///
/// `Debug` is implemented by hand so an accidental `{:?}` cannot leak a key.
pub struct RainConfig {
    api_key: String,
    user_id: String,
    /// Not required by the four endpoints this demo calls; loaded because the
    /// spec provisions it and other Rain endpoints scope by team.
    #[allow(dead_code)]
    team_id: String,
    contract_id: String,
}

impl std::fmt::Debug for RainConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RainConfig")
            .field("api_key", &"<redacted>")
            .field("user_id", &"<redacted>")
            .field("team_id", &"<redacted>")
            .field("contract_id", &"<redacted>")
            .finish()
    }
}

impl RainConfig {
    pub fn from_env() -> Result<Self, Error> {
        fn var(name: &'static str) -> Result<String, Error> {
            match std::env::var(name) {
                Ok(v) if !v.trim().is_empty() => Ok(v),
                _ => Err(Error::MissingEnv(name)),
            }
        }

        Ok(Self {
            api_key: var("RAIN_API_KEY")?,
            user_id: var("RAIN_USER_ID")?,
            team_id: var("RAIN_TEAM_ID")?,
            contract_id: var("RAIN_CONTRACT_ID")?,
        })
    }
}

// ------------------------------------------------------------ session id ----

/// A freshly minted session: the plaintext secret and its encrypted form.
pub struct Session {
    /// 32-char hex secret. Rain uses it to encrypt card material we never read.
    #[allow(dead_code)]
    pub secret_key: String,
    /// Base64 of the RSA-OAEP ciphertext — the `sessionid` header value.
    pub session_id: String,
}

/// Port of Rain's Node `generateSessionId`, with a random 32-char hex secret.
pub fn generate_session_id() -> Result<Session, Error> {
    // crypto.randomUUID().replace(/-/g, "") is 32 hex chars == 16 random bytes.
    let mut raw = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut raw);
    session_from_secret(&hex::encode(raw))
}

/// The transformation chain, matching the reference implementation exactly:
///
/// ```text
/// Buffer.from(secretKey, "hex")      -> decode the hex string to 16 bytes
///   .toString("base64")              -> base64 of those bytes
/// Buffer.from(that, "utf-8")         -> the ASCII bytes of the base64 text
/// publicEncrypt(RSA-OAEP, sha1)      -> ciphertext
///   .toString("base64")              -> the sessionid header value
/// ```
fn session_from_secret(secret_key: &str) -> Result<Session, Error> {
    let secret_bytes = hex::decode(secret_key)
        .map_err(|e| Error::Crypto(format!("secret key is not valid hex: {e}")))?;
    let secret_key_base64 = BASE64.encode(&secret_bytes);

    let public_key = RsaPublicKey::from_public_key_pem(SANDBOX_PUBLIC_KEY_PEM)
        .map_err(|e| Error::Crypto(format!("could not parse sandbox public key: {e}")))?;

    let encrypted = public_key
        .encrypt(&mut rand::thread_rng(), Oaep::new::<Sha1>(), secret_key_base64.as_bytes())
        .map_err(|e| Error::Crypto(format!("rsa-oaep encryption failed: {e}")))?;

    Ok(Session { secret_key: secret_key.to_string(), session_id: BASE64.encode(encrypted) })
}

// ------------------------------------------------------------- api calls ----

/// Reads a 2xx JSON body, or turns a non-2xx into `Error::Api`.
async fn json_or_api_error(endpoint: &str, response: reqwest::Response) -> Result<Value, Error> {
    let status = response.status();
    let body = response.text().await?;

    if !status.is_success() {
        return Err(Error::Api { endpoint: endpoint.to_string(), status, body });
    }

    serde_json::from_str(&body).map_err(|e| Error::MalformedResponse {
        endpoint: endpoint.to_string(),
        detail: format!("body was not json ({e}): {body}"),
    })
}

/// Pulls a required string out of a response, trying each candidate key at the
/// top level and then inside a `data` envelope. Rain is not uniform about this:
/// card issuance answers with `id`, authorization with `transactionId`.
fn string_field(endpoint: &str, body: &Value, candidates: &[&str]) -> Result<String, Error> {
    candidates
        .iter()
        .find_map(|field| {
            body.get(field)
                .or_else(|| body.get("data").and_then(|data| data.get(field)))
                .and_then(Value::as_str)
        })
        .map(str::to_string)
        .ok_or_else(|| Error::MalformedResponse {
            endpoint: endpoint.to_string(),
            detail: format!("none of {candidates:?} present as a string in {body}"),
        })
}

/// Tops up sandbox collateral. Call once at startup, not per request.
pub async fn fund_collateral(config: &RainConfig) -> Result<(), Error> {
    let endpoint = "POST /simulate/collateral/fund";
    let response = reqwest::Client::new()
        .post(format!("{RAIN_BASE_URL}/simulate/collateral/fund"))
        .header("Api-Key", &config.api_key)
        .json(&serde_json::json!({
            "contractId": config.contract_id,
            "currency": "rusd",
            "amount": 100000,
        }))
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        return Err(Error::Api {
            endpoint: endpoint.to_string(),
            status,
            body: response.text().await?,
        });
    }
    Ok(())
}

/// Issues a single-use card scoped to `amount_cents`. Returns the card id.
///
/// The response also carries `encryptedPan` / `encryptedCvc`; this flow never
/// needs the raw card number, so those fields are deliberately ignored.
pub async fn issue_scoped_card(
    config: &RainConfig,
    amount_cents: u64,
    session_id: &str,
) -> Result<String, Error> {
    let endpoint = "POST /issuing/users/{userId}/cards/scoped";
    let response = reqwest::Client::new()
        .post(format!("{RAIN_BASE_URL}/issuing/users/{}/cards/scoped", config.user_id))
        .header("Api-Key", &config.api_key)
        .header("sessionid", session_id)
        .json(&serde_json::json!({ "amountInUSDCents": amount_cents }))
        .send()
        .await?;

    let body = json_or_api_error(endpoint, response).await?;
    string_field(endpoint, &body, &["id", "cardId"])
}

/// Simulates a merchant authorization against an issued card.
pub async fn authorize_transaction(
    config: &RainConfig,
    card_id: &str,
    amount_cents: u64,
    merchant_name: &str,
    mcc: &str,
) -> Result<String, Error> {
    let endpoint = "POST /simulate/transactions/authorize";
    let response = reqwest::Client::new()
        .post(format!("{RAIN_BASE_URL}/simulate/transactions/authorize"))
        .header("Api-Key", &config.api_key)
        .json(&serde_json::json!({
            "cardId": card_id,
            "amount": amount_cents,
            "currency": "usd",
            "merchantName": merchant_name,
            "merchantCategoryCode": mcc,
        }))
        .send()
        .await?;

    let body = json_or_api_error(endpoint, response).await?;
    string_field(endpoint, &body, &["transactionId", "id"])
}

/// Settles a previously authorized transaction.
///
/// Rain's validator requires an `amount` in the body — settling for the full
/// authorized amount means passing the same figure used to authorize.
pub async fn settle_transaction(
    config: &RainConfig,
    transaction_id: &str,
    amount_cents: u64,
) -> Result<(), Error> {
    let endpoint = "POST /simulate/transactions/{id}/settle";
    let response = reqwest::Client::new()
        .post(format!("{RAIN_BASE_URL}/simulate/transactions/{transaction_id}/settle"))
        .header("Api-Key", &config.api_key)
        .json(&serde_json::json!({ "amount": amount_cents }))
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        return Err(Error::Api {
            endpoint: endpoint.to_string(),
            status,
            body: response.text().await?,
        });
    }
    Ok(())
}

#[derive(Debug)]
pub struct RainResult {
    pub card_id: String,
    pub transaction_id: String,
    pub status: String,
}

/// Default MCC: 4511, airlines/air carriers.
pub const DEFAULT_MCC: &str = "4511";

/// Runs the full Rain lifecycle: session -> scoped card -> authorize -> settle.
///
/// Takes an [`ApprovedAction`], not a `ProposedAction`. A blocked or escalated
/// action cannot be turned into one, so this function is unreachable from any
/// path except the approve branch.
pub async fn execute_rain_flow(
    approved: &ApprovedAction<'_>,
    config: &RainConfig,
    mcc: &str,
) -> Result<RainResult, Error> {
    let action = approved.action();
    let amount_cents = (action.amount * 100.0) as u64;

    let session = generate_session_id()?;
    let card_id = issue_scoped_card(config, amount_cents, &session.session_id).await?;
    let transaction_id =
        authorize_transaction(config, &card_id, amount_cents, &action.description, mcc).await?;
    settle_transaction(config, &transaction_id, amount_cents).await?;

    Ok(RainResult { card_id, transaction_id, status: "settled".to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_a_non_empty_base64_session_id() {
        let session = generate_session_id().expect("session generation");
        assert!(!session.session_id.is_empty());
        let decoded = BASE64.decode(&session.session_id).expect("valid base64");
        // 1024-bit RSA modulus => 128 byte ciphertext.
        assert_eq!(decoded.len(), 128);
    }

    #[test]
    fn generates_a_32_char_hex_secret() {
        let session = generate_session_id().expect("session generation");
        assert_eq!(session.secret_key.len(), 32);
        assert!(session.secret_key.chars().all(|c| c.is_ascii_hexdigit()));
        hex::decode(&session.secret_key).expect("secret is valid hex");
    }

    #[test]
    fn session_ids_are_unique_per_call() {
        let a = generate_session_id().expect("session generation");
        let b = generate_session_id().expect("session generation");
        assert_ne!(a.secret_key, b.secret_key);
        assert_ne!(a.session_id, b.session_id);
    }

    /// Locks the pre-encryption transform to the Node reference implementation.
    ///
    /// `Buffer.from("0123...ef", "hex").toString("base64")` in Node prints
    /// `ASNFZ4mrze8BI0VniavN7w==`; that exact string's UTF-8 bytes are the
    /// RSA-OAEP plaintext. Encryption itself is randomized, so this asserts the
    /// deterministic half.
    #[test]
    fn matches_node_reference_transform() {
        let secret = "0123456789abcdef0123456789abcdef";
        let encoded = BASE64.encode(hex::decode(secret).unwrap());
        assert_eq!(encoded, "ASNFZ4mrze8BI0VniavN7w==");

        let session = session_from_secret(secret).expect("session generation");
        assert_eq!(session.secret_key, secret);
        assert_eq!(BASE64.decode(&session.session_id).unwrap().len(), 128);
    }

    #[test]
    fn rejects_a_non_hex_secret() {
        assert!(matches!(session_from_secret("not-hex!!"), Err(Error::Crypto(_))));
    }

    #[test]
    fn config_debug_never_prints_credentials() {
        let config = RainConfig {
            api_key: "sk_live_supersecret".to_string(),
            user_id: "user-123".to_string(),
            team_id: "team-123".to_string(),
            contract_id: "contract-123".to_string(),
        };
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("supersecret"));
        assert!(!rendered.contains("user-123"));
    }
}
