//! In-memory Nessie (`NESSIE_MODE=mock`): the same trait and output shapes as the live API,
//! with no network, so the demo survives a Nessie outage. Ids are UUIDs like Nessie's, under a
//! fixed prefix that marks them as mock ids; every post is `completed`.
//!
//! Tests can make it fail ([`MockNessie::set_failing`]), slow ([`MockNessie::set_delay`]), lose
//! the answer to a post that landed ([`MockNessie::set_losing_responses`]) or list new posts late
//! ([`MockNessie::set_list_lag`]), and count posts per description
//! ([`MockNessie::posts_matching`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rand::RngExt;

use super::{
    AccountRef, CustomerRef, MerchantRef, NessieApi, NessieError, NessieResult, NessieTxn,
    NewMerchant,
};

/// Every mock id starts with this (a UUID's first three groups, all zero but the version).
pub const MOCK_ID_PREFIX: &str = "00000000-0000-4000-8";

pub fn is_mock_id(id: &str) -> bool {
    id.starts_with(MOCK_ID_PREFIX)
}

/// The process-wide mock that `Nessie::from_config` hands out in mock mode, so every request
/// sees the same in-memory bank.
pub fn shared() -> Arc<MockNessie> {
    static SHARED: OnceLock<Arc<MockNessie>> = OnceLock::new();
    SHARED
        .get_or_init(|| Arc::new(MockNessie::new(rand::rng().random::<u16>())))
        .clone()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostKind {
    Deposit,
    Transfer,
    Purchase,
}

#[derive(Debug, Clone)]
pub struct MockPost {
    pub id: String,
    pub kind: PostKind,
    pub account_id: String,
    pub merchant_id: Option<String>,
    pub amount_cents: i64,
    pub description: String,
    pub posted_at: Instant,
}

#[derive(Default)]
struct State {
    next: u64,
    customers: Vec<CustomerRef>,
    /// Account id to (customer id, nickname).
    accounts: Vec<(String, AccountRef)>,
    merchants: Vec<MerchantRef>,
    posts: Vec<MockPost>,
}

pub struct MockNessie {
    /// The 3 hex digits after the prefix's `8`: tells this process's ids from another's, so a
    /// restart never reuses an id still stored in `ferry.nessie_links`.
    instance: u16,
    state: Mutex<State>,
    failing: AtomicBool,
    delay_ms: AtomicU64,
    losing_responses: AtomicBool,
    list_lag_ms: AtomicU64,
}

impl MockNessie {
    pub fn new(instance: u16) -> Self {
        Self {
            instance: instance & 0x0fff,
            state: Mutex::new(State::default()),
            failing: AtomicBool::new(false),
            delay_ms: AtomicU64::new(0),
            losing_responses: AtomicBool::new(false),
            list_lag_ms: AtomicU64::new(0),
        }
    }

    /// While set, every call fails with [`NessieError::MockFailure`].
    pub fn set_failing(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }

    /// Every call first sleeps this long.
    pub fn set_delay(&self, delay: Duration) {
        self.delay_ms
            .store(delay.as_millis() as u64, Ordering::SeqCst);
    }

    /// While set, every post lands and then answers with a timeout, as when the response is lost
    /// on the way back.
    pub fn set_losing_responses(&self, losing: bool) {
        self.losing_responses.store(losing, Ordering::SeqCst);
    }

    /// Lists leave out posts younger than this, the way Nessie, which is eventually consistent,
    /// can list an object some time after accepting it.
    pub fn set_list_lag(&self, lag: Duration) {
        self.list_lag_ms
            .store(lag.as_millis() as u64, Ordering::SeqCst);
    }

    /// Posts (deposits, transfers, purchases) whose description contains `needle`.
    pub fn posts_matching(&self, needle: &str) -> Vec<MockPost> {
        self.lock()
            .posts
            .iter()
            .filter(|p| p.description.contains(needle))
            .cloned()
            .collect()
    }

    pub fn post_count(&self) -> usize {
        self.lock().posts.len()
    }

    pub fn customer_count(&self) -> usize {
        self.lock().customers.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    async fn enter(&self) -> NessieResult<()> {
        let delay = self.delay_ms.load(Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        if self.failing.load(Ordering::SeqCst) {
            return Err(NessieError::MockFailure);
        }
        Ok(())
    }

    fn new_id(&self, state: &mut State) -> String {
        state.next += 1;
        format!("{MOCK_ID_PREFIX}{:03x}-{:012x}", self.instance, state.next)
    }

    fn post(
        &self,
        kind: PostKind,
        account_id: &str,
        merchant_id: Option<&str>,
        amount_cents: i64,
        description: &str,
    ) -> NessieResult<NessieTxn> {
        if amount_cents <= 0 {
            return Err(NessieError::Status {
                status: 400,
                path: "mock".into(),
                body: "amount must be positive".into(),
            });
        }
        let mut state = self.lock();
        let id = self.new_id(&mut state);
        state.posts.push(MockPost {
            id: id.clone(),
            kind,
            account_id: account_id.to_owned(),
            merchant_id: merchant_id.map(str::to_owned),
            amount_cents,
            description: description.to_owned(),
            posted_at: Instant::now(),
        });
        if self.losing_responses.load(Ordering::SeqCst) {
            return Err(NessieError::Timeout {
                path: "mock".into(),
            });
        }
        Ok(NessieTxn {
            id,
            status: Some("completed".into()),
            description: Some(description.to_owned()),
        })
    }

    fn listed(&self, kind: PostKind, account_id: &str) -> Vec<NessieTxn> {
        let lag = Duration::from_millis(self.list_lag_ms.load(Ordering::SeqCst));
        self.lock()
            .posts
            .iter()
            .filter(|p| {
                p.kind == kind && p.account_id == account_id && p.posted_at.elapsed() >= lag
            })
            .map(|p| NessieTxn {
                id: p.id.clone(),
                status: Some("completed".into()),
                description: Some(p.description.clone()),
            })
            .collect()
    }

    /// Mock balances, from the posts: deposits add, transfers and purchases take away.
    /// (Live Nessie keeps `balance` at what the account was opened with.)
    pub fn balances(&self) -> HashMap<String, i64> {
        let mut out = HashMap::new();
        for p in &self.lock().posts {
            let sign = if p.kind == PostKind::Deposit { 1 } else { -1 };
            *out.entry(p.account_id.clone()).or_insert(0) += sign * p.amount_cents;
        }
        out
    }
}

impl NessieApi for MockNessie {
    async fn create_customer(&self, first_name: &str, last_name: &str) -> NessieResult<String> {
        self.enter().await?;
        let mut state = self.lock();
        let id = self.new_id(&mut state);
        state.customers.push(CustomerRef {
            id: id.clone(),
            first_name: first_name.to_owned(),
            last_name: last_name.to_owned(),
        });
        Ok(id)
    }

    async fn customers(&self) -> NessieResult<Vec<CustomerRef>> {
        self.enter().await?;
        Ok(self.lock().customers.clone())
    }

    async fn create_account(&self, customer_id: &str, nickname: &str) -> NessieResult<String> {
        self.enter().await?;
        let mut state = self.lock();
        let id = self.new_id(&mut state);
        state.accounts.push((
            customer_id.to_owned(),
            AccountRef {
                id: id.clone(),
                nickname: nickname.to_owned(),
            },
        ));
        Ok(id)
    }

    async fn customer_accounts(&self, customer_id: &str) -> NessieResult<Vec<AccountRef>> {
        self.enter().await?;
        Ok(self
            .lock()
            .accounts
            .iter()
            .filter(|(c, _)| c == customer_id)
            .map(|(_, a)| a.clone())
            .collect())
    }

    async fn create_merchant(&self, merchant: &NewMerchant) -> NessieResult<String> {
        self.enter().await?;
        let mut state = self.lock();
        let id = self.new_id(&mut state);
        state.merchants.push(MerchantRef {
            id: id.clone(),
            name: merchant.name.clone(),
        });
        Ok(id)
    }

    async fn merchants(&self) -> NessieResult<Vec<MerchantRef>> {
        self.enter().await?;
        Ok(self.lock().merchants.clone())
    }

    async fn deposit(
        &self,
        account_id: &str,
        amount_cents: i64,
        _date: &str,
        description: &str,
    ) -> NessieResult<NessieTxn> {
        self.enter().await?;
        self.post(
            PostKind::Deposit,
            account_id,
            None,
            amount_cents,
            description,
        )
    }

    async fn transfer(
        &self,
        payer_account_id: &str,
        amount_cents: i64,
        _date: &str,
        description: &str,
    ) -> NessieResult<NessieTxn> {
        self.enter().await?;
        self.post(
            PostKind::Transfer,
            payer_account_id,
            None,
            amount_cents,
            description,
        )
    }

    async fn purchase(
        &self,
        account_id: &str,
        merchant_id: &str,
        amount_cents: i64,
        _date: &str,
        description: &str,
    ) -> NessieResult<NessieTxn> {
        self.enter().await?;
        self.post(
            PostKind::Purchase,
            account_id,
            Some(merchant_id),
            amount_cents,
            description,
        )
    }

    async fn deposits(&self, account_id: &str) -> NessieResult<Vec<NessieTxn>> {
        self.enter().await?;
        Ok(self.listed(PostKind::Deposit, account_id))
    }

    async fn transfers(&self, account_id: &str) -> NessieResult<Vec<NessieTxn>> {
        self.enter().await?;
        Ok(self.listed(PostKind::Transfer, account_id))
    }

    async fn purchases(&self, account_id: &str) -> NessieResult<Vec<NessieTxn>> {
        self.enter().await?;
        Ok(self.listed(PostKind::Purchase, account_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ids_are_uuid_shaped_and_marked() {
        let mock = MockNessie::new(0xabc);
        let id = mock.create_customer("Ferry", "(demo)").await.unwrap();
        assert_eq!(id, "00000000-0000-4000-8abc-000000000001");
        assert_eq!(id.len(), 36);
        assert!(is_mock_id(&id));
        assert!(!is_mock_id("17b47cc3-dede-4d9c-a857-2fd8db2b6930"));
        let account = mock
            .create_account(&id, "platform:ferry:escrow")
            .await
            .unwrap();
        let t = mock
            .deposit(&account, 500, "2026-10-11", "seed")
            .await
            .unwrap();
        assert_eq!(t.status.as_deref(), Some("completed"));
        assert_eq!(mock.deposits(&account).await.unwrap().len(), 1);
        assert_eq!(mock.balances()[&account], 500);
    }

    #[tokio::test]
    async fn failing_and_non_positive_amounts() {
        let mock = MockNessie::new(1);
        mock.set_failing(true);
        assert_eq!(mock.customers().await, Err(NessieError::MockFailure));
        mock.set_failing(false);
        assert!(mock.transfer("a", 0, "2026-10-11", "zero").await.is_err());
    }
}
