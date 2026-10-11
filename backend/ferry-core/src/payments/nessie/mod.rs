//! Capital One's Nessie sandbox: one trait, a live client and an in-memory mock with the same
//! output shapes (`NESSIE_MODE=live|mock`). Verified paths and shapes: docs/DECISIONS.md, Nessie.
//!
//! Amounts are integer cents: Nessie types every amount and balance as an integer, so Ferry's
//! cents go over the wire as they are and no float ever touches money.

use std::future::Future;
use std::sync::Arc;

use crate::config::{Config, NessieMode};

pub mod live;
pub mod mock;

pub use live::LiveNessie;
pub use mock::{MOCK_ID_PREFIX, MockNessie};

/// The host that answers (docs/DECISIONS.md, Nessie). `NESSIE_BASE_URL` overrides it.
pub const DEFAULT_BASE_URL: &str = "https://prod-api.nessieisreal.com";

/// Every Nessie object Ferry creates sits at this placeholder address: Ferry stores no street
/// addresses for users, and Nessie requires one on a customer.
pub fn placeholder_address() -> Address {
    Address {
        street_number: "250".into(),
        street_name: "E Franklin St".into(),
        city: "Chapel Hill".into(),
        state: "NC".into(),
        zip: "27514".into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Address {
    pub street_number: String,
    pub street_name: String,
    pub city: String,
    pub state: String,
    pub zip: String,
}

impl Address {
    /// Parses a fixture address such as `118 E Franklin St, Chapel Hill, NC 27514`.
    pub fn parse(address: &str) -> Option<Address> {
        let parts: Vec<&str> = address.split(',').map(str::trim).collect();
        let [street, city, state_zip] = parts.as_slice() else {
            return None;
        };
        let (number, name) = street.split_once(' ')?;
        let (state, zip) = state_zip.split_once(' ')?;
        let ok = !number.is_empty()
            && number.bytes().all(|b| b.is_ascii_digit())
            && state.len() == 2
            && zip.len() == 5
            && zip.bytes().all(|b| b.is_ascii_digit());
        ok.then(|| Address {
            street_number: number.into(),
            street_name: name.trim().into(),
            city: (*city).into(),
            state: state.into(),
            zip: zip.into(),
        })
    }
}

/// A merchant to create: one per restaurant.
#[derive(Debug, Clone, PartialEq)]
pub struct NewMerchant {
    pub name: String,
    pub category: String,
    pub address: Option<Address>,
    pub lat: f64,
    pub lng: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomerRef {
    pub id: String,
    pub first_name: String,
    pub last_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRef {
    pub id: String,
    pub nickname: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerchantRef {
    pub id: String,
    pub name: String,
}

/// A deposit, transfer or purchase as Nessie reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NessieTxn {
    pub id: String,
    pub status: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NessieError {
    #[error("Nessie is not configured: {0}")]
    NotConfigured(&'static str),
    #[error("Nessie timed out on {path}")]
    Timeout { path: String },
    #[error("Nessie is unreachable on {path}")]
    Network { path: String },
    #[error("Nessie answered {status} on {path}: {body}")]
    Status {
        status: u16,
        path: String,
        body: String,
    },
    #[error("unexpected Nessie response on {path}: {reason}")]
    Parse { path: String, reason: String },
    #[error("cannot post to Nessie: {0}")]
    Unsupported(String),
    #[error("mock Nessie is set to fail")]
    MockFailure,
}

impl NessieError {
    /// A short label for `nessie_status`: no paths, bodies or keys.
    pub fn label(&self) -> String {
        match self {
            NessieError::NotConfigured(_) => "not_configured".into(),
            NessieError::Timeout { .. } => "timeout".into(),
            NessieError::Network { .. } => "network".into(),
            NessieError::Status { status, .. } => format!("http_{status}"),
            NessieError::Parse { .. } => "bad_response".into(),
            NessieError::Unsupported(_) => "unsupported".into(),
            NessieError::MockFailure => "mock_failure".into(),
        }
    }

    /// True when a POST may have reached Nessie even though Ferry got no answer it could use: a
    /// timeout, a dropped connection, a 5xx (a gateway can time out after the backend saved the
    /// object) or a reply it could not read. The poster then waits before looking the object up
    /// rather than posting again (`poster::RECONCILE_AFTER_SECS`).
    pub fn may_have_landed(&self) -> bool {
        match self {
            NessieError::Timeout { .. }
            | NessieError::Network { .. }
            | NessieError::Parse { .. } => true,
            NessieError::Status { status, .. } => *status >= 500,
            NessieError::NotConfigured(_)
            | NessieError::Unsupported(_)
            | NessieError::MockFailure => false,
        }
    }
}

pub type NessieResult<T> = Result<T, NessieError>;

/// The calls Ferry makes. `amount_cents` is sent as Nessie's integer `amount`; `date` is
/// `YYYY-MM-DD`.
pub trait NessieApi: Send + Sync {
    fn create_customer(
        &self,
        first_name: &str,
        last_name: &str,
    ) -> impl Future<Output = NessieResult<String>> + Send;
    fn customers(&self) -> impl Future<Output = NessieResult<Vec<CustomerRef>>> + Send;
    /// A Checking account under `customer_id`.
    fn create_account(
        &self,
        customer_id: &str,
        nickname: &str,
    ) -> impl Future<Output = NessieResult<String>> + Send;
    fn customer_accounts(
        &self,
        customer_id: &str,
    ) -> impl Future<Output = NessieResult<Vec<AccountRef>>> + Send;
    fn create_merchant(
        &self,
        merchant: &NewMerchant,
    ) -> impl Future<Output = NessieResult<String>> + Send;
    fn merchants(&self) -> impl Future<Output = NessieResult<Vec<MerchantRef>>> + Send;
    fn deposit(
        &self,
        account_id: &str,
        amount_cents: i64,
        date: &str,
        description: &str,
    ) -> impl Future<Output = NessieResult<NessieTxn>> + Send;
    /// Nessie's transfers name no payee (live-verified): the transfer sits on the paying
    /// account, and the description names the receiving one.
    fn transfer(
        &self,
        payer_account_id: &str,
        amount_cents: i64,
        date: &str,
        description: &str,
    ) -> impl Future<Output = NessieResult<NessieTxn>> + Send;
    fn purchase(
        &self,
        account_id: &str,
        merchant_id: &str,
        amount_cents: i64,
        date: &str,
        description: &str,
    ) -> impl Future<Output = NessieResult<NessieTxn>> + Send;
    fn deposits(
        &self,
        account_id: &str,
    ) -> impl Future<Output = NessieResult<Vec<NessieTxn>>> + Send;
    fn transfers(
        &self,
        account_id: &str,
    ) -> impl Future<Output = NessieResult<Vec<NessieTxn>>> + Send;
    fn purchases(
        &self,
        account_id: &str,
    ) -> impl Future<Output = NessieResult<Vec<NessieTxn>>> + Send;
}

/// The client for the configured mode: cheap to clone.
#[derive(Clone)]
pub enum Nessie {
    Live(LiveNessie),
    Mock(Arc<MockNessie>),
}

impl Nessie {
    /// Live when `NESSIE_MODE=live` and `NESSIE_API_KEY` is set; otherwise `mock`.
    pub fn from_config(config: &Config, http: &reqwest::Client) -> Nessie {
        match (config.nessie_mode, &config.nessie_api_key) {
            (NessieMode::Live, Some(key)) => Nessie::Live(LiveNessie::new(
                http.clone(),
                config
                    .nessie_base_url
                    .as_deref()
                    .unwrap_or(DEFAULT_BASE_URL),
                key.clone(),
            )),
            (NessieMode::Live, None) => {
                tracing::warn!("NESSIE_MODE=live but NESSIE_API_KEY is unset: using mock Nessie");
                Nessie::Mock(mock::shared())
            }
            (NessieMode::Mock, _) => Nessie::Mock(mock::shared()),
        }
    }

    pub fn is_mock(&self) -> bool {
        matches!(self, Nessie::Mock(_))
    }

    /// WalletMode: `nessie_live` or `nessie_mock`.
    pub fn wallet_mode(&self) -> &'static str {
        if self.is_mock() {
            "nessie_mock"
        } else {
            "nessie_live"
        }
    }

    /// True when `nessie_id` was made by a client of this mode. Live ids are UUIDs from Nessie;
    /// mock ids are UUIDs with the mock's fixed prefix. A link from the other mode is stale.
    pub fn owns_id(&self, nessie_id: &str) -> bool {
        mock::is_mock_id(nessie_id) == self.is_mock()
    }
}

macro_rules! dispatch {
    ($self:ident, $c:ident => $call:expr) => {
        match $self {
            Nessie::Live($c) => $call.await,
            Nessie::Mock($c) => $call.await,
        }
    };
}

impl NessieApi for Nessie {
    async fn create_customer(&self, first_name: &str, last_name: &str) -> NessieResult<String> {
        dispatch!(self, c => c.create_customer(first_name, last_name))
    }
    async fn customers(&self) -> NessieResult<Vec<CustomerRef>> {
        dispatch!(self, c => c.customers())
    }
    async fn create_account(&self, customer_id: &str, nickname: &str) -> NessieResult<String> {
        dispatch!(self, c => c.create_account(customer_id, nickname))
    }
    async fn customer_accounts(&self, customer_id: &str) -> NessieResult<Vec<AccountRef>> {
        dispatch!(self, c => c.customer_accounts(customer_id))
    }
    async fn create_merchant(&self, merchant: &NewMerchant) -> NessieResult<String> {
        dispatch!(self, c => c.create_merchant(merchant))
    }
    async fn merchants(&self) -> NessieResult<Vec<MerchantRef>> {
        dispatch!(self, c => c.merchants())
    }
    async fn deposit(
        &self,
        account_id: &str,
        amount_cents: i64,
        date: &str,
        description: &str,
    ) -> NessieResult<NessieTxn> {
        dispatch!(self, c => c.deposit(account_id, amount_cents, date, description))
    }
    async fn transfer(
        &self,
        payer_account_id: &str,
        amount_cents: i64,
        date: &str,
        description: &str,
    ) -> NessieResult<NessieTxn> {
        dispatch!(self, c => c.transfer(payer_account_id, amount_cents, date, description))
    }
    async fn purchase(
        &self,
        account_id: &str,
        merchant_id: &str,
        amount_cents: i64,
        date: &str,
        description: &str,
    ) -> NessieResult<NessieTxn> {
        dispatch!(self, c => c.purchase(account_id, merchant_id, amount_cents, date, description))
    }
    async fn deposits(&self, account_id: &str) -> NessieResult<Vec<NessieTxn>> {
        dispatch!(self, c => c.deposits(account_id))
    }
    async fn transfers(&self, account_id: &str) -> NessieResult<Vec<NessieTxn>> {
        dispatch!(self, c => c.transfers(account_id))
    }
    async fn purchases(&self, account_id: &str) -> NessieResult<Vec<NessieTxn>> {
        dispatch!(self, c => c.purchases(account_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fixture_addresses() {
        assert_eq!(
            Address::parse("118 E Franklin St, Chapel Hill, NC 27514"),
            Some(Address {
                street_number: "118".into(),
                street_name: "E Franklin St".into(),
                city: "Chapel Hill".into(),
                state: "NC".into(),
                zip: "27514".into(),
            })
        );
        assert_eq!(
            Address::parse("101 E Weaver St, Carrboro, NC 27510").map(|a| a.city),
            Some("Carrboro".into())
        );
        assert_eq!(Address::parse("Student Union, Chapel Hill, NC 27514"), None);
        assert_eq!(Address::parse("118 E Franklin St"), None);
    }

    #[test]
    fn labels_never_carry_details() {
        let err = NessieError::Status {
            status: 502,
            path: "/accounts/x/transfers".into(),
            body: "boom".into(),
        };
        assert_eq!(err.label(), "http_502");
        assert_eq!(
            NessieError::Timeout {
                path: "/customers".into()
            }
            .label(),
            "timeout"
        );
    }

    #[test]
    fn only_failures_that_may_have_posted_wait_for_a_lookup() {
        let path = || "/accounts/x/deposits".to_owned();
        let status = |status| NessieError::Status {
            status,
            path: path(),
            body: String::new(),
        };
        for err in [
            NessieError::Timeout { path: path() },
            NessieError::Network { path: path() },
            NessieError::Parse {
                path: path(),
                reason: "no id".into(),
            },
            status(500),
            status(504),
        ] {
            assert!(err.may_have_landed(), "{err}");
        }
        for err in [
            status(400),
            status(404),
            status(429),
            NessieError::NotConfigured("no key"),
            NessieError::Unsupported("x".into()),
            NessieError::MockFailure,
        ] {
            assert!(!err.may_have_landed(), "{err}");
        }
    }
}
