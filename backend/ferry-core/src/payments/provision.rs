//! Nessie objects for ledger accounts, created on first use and recorded in `ferry.nessie_links`
//! (owner_kind, owner_id, purpose → nessie_id).
//!
//! - `user:<usr>:checking`: customer "Ferry user" / `<usr>` and a Checking account under it.
//! - `platform:ferry:escrow`: the platform customer "Ferry" / "(demo)" and an account under it.
//! - `user:<usr>:ferry_card`: an account under the platform customer, keyed by the runner.
//! - `restaurant:<rst>:merchant`: a merchant with the restaurant's name, cuisine, address and
//!   geocode.
//!
//! Account nicknames are the ledger account names and user customers' last names are user ids,
//! so a lost response or a second server finds the object by listing instead of creating a twin.
//! No names, phone numbers or street addresses of users go to Nessie.

use sqlx::PgPool;

use super::ledger::accounts::{self, Account};
use super::nessie::{Address, MOCK_ID_PREFIX, Nessie, NessieApi, NessieError, NewMerchant};

/// What a ledger account is in Nessie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Money from outside: a deposit's source. No Nessie object.
    External,
    Account(String),
    Merchant(String),
}

#[derive(Debug, thiserror::Error)]
pub enum ProvisionError {
    #[error(transparent)]
    Nessie(#[from] NessieError),
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
    #[error("{0}")]
    Unsupported(String),
}

impl ProvisionError {
    pub fn label(&self) -> String {
        match self {
            ProvisionError::Nessie(e) => e.label(),
            ProvisionError::Db(_) => "db".into(),
            ProvisionError::Unsupported(_) => "unsupported".into(),
        }
    }

    /// See [`NessieError::may_have_landed`]. A database or mapping error never reached Nessie.
    pub fn may_have_landed(&self) -> bool {
        match self {
            ProvisionError::Nessie(e) => e.may_have_landed(),
            ProvisionError::Db(_) | ProvisionError::Unsupported(_) => false,
        }
    }
}

const PLATFORM_FIRST: &str = "Ferry";
const PLATFORM_LAST: &str = "(demo)";
const USER_FIRST: &str = "Ferry user";

// One provisioning at a time in this process, so two posts that need the same new account do
// not both create it.
static PROVISIONING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The Nessie object behind `account`, creating it (and its customer) when missing.
pub async fn resolve(
    pool: &PgPool,
    nessie: &Nessie,
    account: &str,
) -> Result<Target, ProvisionError> {
    let parsed = accounts::parse(account)
        .ok_or_else(|| ProvisionError::Unsupported(format!("unknown account {account}")))?;
    let (owner_kind, owner_id, purpose) = match &parsed {
        Account::External => return Ok(Target::External),
        Account::Escrow => ("platform", "ferry", "escrow"),
        Account::Checking(user) => ("user", user.as_str(), "checking"),
        Account::FerryCard(user) => ("user", user.as_str(), "ferry_card"),
        Account::Merchant(rst) => ("restaurant", rst.as_str(), "merchant"),
        Account::RestaurantChecking(_) => {
            return Err(ProvisionError::Unsupported(
                "partner restaurant accounts are P2".into(),
            ));
        }
    };
    let wrap = |id: String| {
        if purpose == "merchant" {
            Target::Merchant(id)
        } else {
            Target::Account(id)
        }
    };
    if let Some(id) = link(pool, nessie, owner_kind, owner_id, purpose).await? {
        return Ok(wrap(id));
    }

    let _guard = PROVISIONING.lock().await;
    // Another task may have finished it while this one waited.
    if let Some(id) = link(pool, nessie, owner_kind, owner_id, purpose).await? {
        return Ok(wrap(id));
    }
    let id = match &parsed {
        Account::Escrow => {
            let customer = platform_customer(pool, nessie).await?;
            account_under(nessie, &customer, account).await?
        }
        Account::FerryCard(_) => {
            let customer = platform_customer(pool, nessie).await?;
            account_under(nessie, &customer, account).await?
        }
        Account::Checking(user) => {
            let customer = customer(pool, nessie, "user", user, USER_FIRST, user).await?;
            account_under(nessie, &customer, account).await?
        }
        Account::Merchant(rst) => merchant(pool, nessie, rst).await?,
        Account::External | Account::RestaurantChecking(_) => unreachable!("handled above"),
    };
    let id = store(pool, nessie, owner_kind, owner_id, purpose, &id).await?;
    Ok(wrap(id))
}

/// The stored link, if it belongs to this mode (a mock id is stale in live mode and back).
async fn link(
    pool: &PgPool,
    nessie: &Nessie,
    owner_kind: &str,
    owner_id: &str,
    purpose: &str,
) -> Result<Option<String>, sqlx::Error> {
    let id: Option<(String,)> = sqlx::query_as(
        "SELECT nessie_id FROM ferry.nessie_links \
         WHERE owner_kind = $1 AND owner_id = $2 AND purpose = $3",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(purpose)
    .fetch_optional(pool)
    .await?;
    Ok(id.map(|(id,)| id).filter(|id| nessie.owns_id(id)))
}

/// Stores a link and returns the one that won: a stale link (the other mode's) is replaced, a
/// valid one written meanwhile by another server is kept.
async fn store(
    pool: &PgPool,
    nessie: &Nessie,
    owner_kind: &str,
    owner_id: &str,
    purpose: &str,
    nessie_id: &str,
) -> Result<String, sqlx::Error> {
    let stored: Option<(String,)> = sqlx::query_as(
        "INSERT INTO ferry.nessie_links (owner_kind, owner_id, purpose, nessie_id) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (owner_kind, owner_id, purpose) DO UPDATE \
             SET nessie_id = EXCLUDED.nessie_id, created_at = now() \
             WHERE starts_with(ferry.nessie_links.nessie_id, $5) <> $6 \
         RETURNING nessie_id",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(purpose)
    .bind(nessie_id)
    .bind(MOCK_ID_PREFIX)
    .bind(nessie.is_mock())
    .fetch_optional(pool)
    .await?;
    if let Some((id,)) = stored {
        return Ok(id);
    }
    let (id,): (String,) = sqlx::query_as(
        "SELECT nessie_id FROM ferry.nessie_links \
         WHERE owner_kind = $1 AND owner_id = $2 AND purpose = $3",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(purpose)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

async fn platform_customer(pool: &PgPool, nessie: &Nessie) -> Result<String, ProvisionError> {
    customer(
        pool,
        nessie,
        "platform",
        "ferry",
        PLATFORM_FIRST,
        PLATFORM_LAST,
    )
    .await
}

/// The owner's Nessie customer: the link, else one found by name, else a new one.
async fn customer(
    pool: &PgPool,
    nessie: &Nessie,
    owner_kind: &str,
    owner_id: &str,
    first_name: &str,
    last_name: &str,
) -> Result<String, ProvisionError> {
    if let Some(id) = link(pool, nessie, owner_kind, owner_id, "customer").await? {
        return Ok(id);
    }
    let found =
        nessie.customers().await?.into_iter().find(|c| {
            c.first_name == first_name && c.last_name == last_name && nessie.owns_id(&c.id)
        });
    let id = match found {
        Some(c) => c.id,
        None => nessie.create_customer(first_name, last_name).await?,
    };
    Ok(store(pool, nessie, owner_kind, owner_id, "customer", &id).await?)
}

/// A Checking account nicknamed `nickname` under `customer_id`: found, else created.
async fn account_under(
    nessie: &Nessie,
    customer_id: &str,
    nickname: &str,
) -> Result<String, ProvisionError> {
    let found = nessie
        .customer_accounts(customer_id)
        .await?
        .into_iter()
        .find(|a| a.nickname == nickname);
    match found {
        Some(a) => Ok(a.id),
        None => Ok(nessie.create_account(customer_id, nickname).await?),
    }
}

async fn merchant(
    pool: &PgPool,
    nessie: &Nessie,
    restaurant_id: &str,
) -> Result<String, ProvisionError> {
    let row: Option<(String, Vec<String>, String, f64, f64)> = sqlx::query_as(
        "SELECT name, cuisine, address, lat, lng FROM ferry.restaurants WHERE id = $1",
    )
    .bind(restaurant_id)
    .fetch_optional(pool)
    .await?;
    let (name, cuisine, address, lat, lng) = row.ok_or_else(|| {
        ProvisionError::Unsupported(format!("restaurant {restaurant_id} does not exist"))
    })?;
    let found = nessie
        .merchants()
        .await?
        .into_iter()
        .find(|m| m.name == name && nessie.owns_id(&m.id));
    if let Some(m) = found {
        return Ok(m.id);
    }
    let merchant = NewMerchant {
        name,
        category: cuisine
            .into_iter()
            .next()
            .unwrap_or_else(|| "restaurant".into()),
        address: Address::parse(&address),
        lat,
        lng,
    };
    Ok(nessie.create_merchant(&merchant).await?)
}
