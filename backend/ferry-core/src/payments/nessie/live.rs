//! The live Nessie client over HTTPS. The key goes in `?key=`; nothing here logs or returns a
//! URL that carries it: errors name the path only, and response text is scrubbed of the key.

use std::time::Duration;

use reqwest::Method;
use serde_json::{Value, json};

use super::{
    AccountRef, CustomerRef, MerchantRef, NessieApi, NessieError, NessieResult, NessieTxn,
    NewMerchant, placeholder_address,
};
use crate::config::Secret;

/// Per-request timeout. The poster runs off the request path, so a slow Nessie never holds a
/// user response (that wait is capped separately, see `poster::USER_WAIT`).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Clone)]
pub struct LiveNessie {
    http: reqwest::Client,
    base: String,
    key: Secret,
    timeout: Duration,
}

impl std::fmt::Debug for LiveNessie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveNessie")
            .field("base", &self.base)
            .field("key", &self.key)
            .finish()
    }
}

impl LiveNessie {
    pub fn new(http: reqwest::Client, base_url: &str, key: Secret) -> Self {
        Self {
            http,
            base: base_url.trim_end_matches('/').to_owned(),
            key,
            timeout: REQUEST_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn scrub(&self, text: &str) -> String {
        let key = self.key.expose();
        let text = if key.is_empty() {
            text.to_owned()
        } else {
            text.replace(key, "<key>")
        };
        text.chars().take(300).collect()
    }

    /// One call. Returns the status and the body (JSON, or the text as a JSON string).
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> NessieResult<(u16, Value)> {
        let mut url = url::Url::parse(&format!("{}{path}", self.base))
            .map_err(|_| NessieError::NotConfigured("NESSIE_BASE_URL is not a valid base URL"))?;
        url.query_pairs_mut().append_pair("key", self.key.expose());
        let mut request = self.http.request(method.clone(), url).timeout(self.timeout);
        if let Some(body) = &body {
            request = request.json(body);
        }
        let started = std::time::Instant::now();
        let response = request
            .send()
            .await
            .map_err(|e| self.transport_error(e, path))?;
        let status = response.status().as_u16();
        let text = response
            .text()
            .await
            .map_err(|e| self.transport_error(e, path))?;
        tracing::debug!(%method, path, status, ms = started.elapsed().as_millis() as u64, "nessie");
        let value = serde_json::from_str(&text).unwrap_or(Value::String(self.scrub(&text)));
        Ok((status, value))
    }

    fn transport_error(&self, err: reqwest::Error, path: &str) -> NessieError {
        // `without_url` drops the URL, which carries the key.
        let err = err.without_url();
        if err.is_timeout() {
            NessieError::Timeout { path: path.into() }
        } else {
            tracing::debug!(error = %err, path, "nessie transport error");
            NessieError::Network { path: path.into() }
        }
    }

    fn status_error(&self, status: u16, path: &str, body: &Value) -> NessieError {
        let body = match body {
            Value::String(s) => self.scrub(s),
            other => self.scrub(&other.to_string()),
        };
        NessieError::Status {
            status,
            path: path.into(),
            body,
        }
    }

    /// POST that creates an object: 201 `{"code", "message", "objectCreated": {..., "_id"}}`.
    async fn create(&self, path: &str, body: Value) -> NessieResult<Value> {
        let (status, value) = self.send(Method::POST, path, Some(body)).await?;
        if !(200..300).contains(&status) {
            return Err(self.status_error(status, path, &value));
        }
        let created = value.get("objectCreated").cloned().unwrap_or(value);
        if object_id(&created).is_none() {
            return Err(NessieError::Parse {
                path: path.into(),
                reason: "no _id in the created object".into(),
            });
        }
        Ok(created)
    }

    /// GET a list. A 404 on a list ("No transfers found for this account") is an empty list.
    async fn list(&self, path: &str) -> NessieResult<Vec<Value>> {
        let (status, value) = self.send(Method::GET, path, None).await?;
        match (status, value) {
            (200, Value::Array(items)) => Ok(items),
            (404, Value::String(_)) => Ok(Vec::new()),
            (200, _) => Err(NessieError::Parse {
                path: path.into(),
                reason: "expected a JSON array".into(),
            }),
            (status, value) => Err(self.status_error(status, path, &value)),
        }
    }
}

/// `_id`, or `id` (transfer listings use `id`).
fn object_id(value: &Value) -> Option<String> {
    value
        .get("_id")
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn text(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn txn(value: &Value) -> Option<NessieTxn> {
    Some(NessieTxn {
        id: object_id(value)?,
        status: value
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned),
        description: value
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn created_txn(path: &str, value: &Value) -> NessieResult<NessieTxn> {
    txn(value).ok_or_else(|| NessieError::Parse {
        path: path.into(),
        reason: "no _id in the created object".into(),
    })
}

impl NessieApi for LiveNessie {
    async fn create_customer(&self, first_name: &str, last_name: &str) -> NessieResult<String> {
        let created = self
            .create(
                "/customers",
                json!({"first_name": first_name, "last_name": last_name, "address": placeholder_address()}),
            )
            .await?;
        Ok(object_id(&created).unwrap_or_default())
    }

    async fn customers(&self) -> NessieResult<Vec<CustomerRef>> {
        Ok(self
            .list("/customers")
            .await?
            .iter()
            .filter_map(|c| {
                Some(CustomerRef {
                    id: object_id(c)?,
                    first_name: text(c, "first_name"),
                    last_name: text(c, "last_name"),
                })
            })
            .collect())
    }

    async fn create_account(&self, customer_id: &str, nickname: &str) -> NessieResult<String> {
        let path = format!("/customers/{customer_id}/accounts");
        let created = self
            .create(
                &path,
                json!({"type": "Checking", "nickname": nickname, "rewards": 0, "balance": 0}),
            )
            .await?;
        Ok(object_id(&created).unwrap_or_default())
    }

    async fn customer_accounts(&self, customer_id: &str) -> NessieResult<Vec<AccountRef>> {
        Ok(self
            .list(&format!("/customers/{customer_id}/accounts"))
            .await?
            .iter()
            .filter_map(|a| {
                Some(AccountRef {
                    id: object_id(a)?,
                    nickname: text(a, "nickname"),
                })
            })
            .collect())
    }

    async fn create_merchant(&self, merchant: &NewMerchant) -> NessieResult<String> {
        let mut body = json!({
            "name": merchant.name,
            "category": merchant.category,
            "geocode": {"lat": merchant.lat, "lng": merchant.lng},
        });
        if let Some(address) = &merchant.address {
            body["address"] = json!(address);
        }
        let created = self.create("/merchants", body).await?;
        Ok(object_id(&created).unwrap_or_default())
    }

    async fn merchants(&self) -> NessieResult<Vec<MerchantRef>> {
        Ok(self
            .list("/merchants")
            .await?
            .iter()
            .filter_map(|m| {
                Some(MerchantRef {
                    id: object_id(m)?,
                    name: text(m, "name"),
                })
            })
            .collect())
    }

    async fn deposit(
        &self,
        account_id: &str,
        amount_cents: i64,
        date: &str,
        description: &str,
    ) -> NessieResult<NessieTxn> {
        let path = format!("/accounts/{account_id}/deposits");
        let created = self
            .create(
                &path,
                json!({
                    "medium": "balance", "transaction_date": date, "status": "completed",
                    "amount": amount_cents, "description": description,
                }),
            )
            .await?;
        created_txn(&path, &created)
    }

    async fn transfer(
        &self,
        payer_account_id: &str,
        amount_cents: i64,
        date: &str,
        description: &str,
    ) -> NessieResult<NessieTxn> {
        // TransferCreate takes exactly these four fields; `medium` and `payee_id` are rejected
        // as extra fields (live-verified).
        let path = format!("/accounts/{payer_account_id}/transfers");
        let created = self
            .create(
                &path,
                json!({
                    "transaction_date": date, "status": "completed",
                    "amount": amount_cents, "description": description,
                }),
            )
            .await?;
        created_txn(&path, &created)
    }

    async fn purchase(
        &self,
        account_id: &str,
        merchant_id: &str,
        amount_cents: i64,
        date: &str,
        description: &str,
    ) -> NessieResult<NessieTxn> {
        let path = format!("/accounts/{account_id}/purchases");
        let created = self
            .create(
                &path,
                json!({
                    "merchant_id": merchant_id, "medium": "balance", "purchase_date": date,
                    "amount": amount_cents, "status": "completed", "description": description,
                }),
            )
            .await?;
        created_txn(&path, &created)
    }

    async fn deposits(&self, account_id: &str) -> NessieResult<Vec<NessieTxn>> {
        Ok(self
            .list(&format!("/accounts/{account_id}/deposits"))
            .await?
            .iter()
            .filter_map(txn)
            .collect())
    }

    async fn transfers(&self, account_id: &str) -> NessieResult<Vec<NessieTxn>> {
        Ok(self
            .list(&format!("/accounts/{account_id}/transfers"))
            .await?
            .iter()
            .filter_map(txn)
            .collect())
    }

    async fn purchases(&self, account_id: &str) -> NessieResult<Vec<NessieTxn>> {
        Ok(self
            .list(&format!("/accounts/{account_id}/purchases"))
            .await?
            .iter()
            .filter_map(txn)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_live_shapes() {
        // From the live API on 2026-10-11 (docs/DECISIONS.md, Nessie).
        let created: Value = serde_json::from_str(
            r#"{"code": 201, "message": "Deposit created", "objectCreated": {"medium": "balance",
            "transaction_date": "2026-10-11", "status": "completed", "amount": 2599,
            "description": "d", "_id": "eefbdb5c-5bc9-4083-b4b8-f20e48aa3f45"}}"#,
        )
        .unwrap();
        let t = txn(&created["objectCreated"]).unwrap();
        assert_eq!(t.id, "eefbdb5c-5bc9-4083-b4b8-f20e48aa3f45");
        assert_eq!(t.status.as_deref(), Some("completed"));
        // Transfer listings say `id`, not `_id`.
        let listed: Value = serde_json::from_str(
            r#"{"id": "9589d6b3", "status": "completed", "description": "x"}"#,
        )
        .unwrap();
        assert_eq!(txn(&listed).unwrap().id, "9589d6b3");
    }

    #[test]
    fn debug_and_errors_never_show_the_key() {
        let client = LiveNessie::new(
            reqwest::Client::new(),
            "https://nessie.invalid",
            Secret::new("super-secret-key"),
        );
        assert!(!format!("{client:?}").contains("super-secret-key"));
        let err = client.status_error(
            400,
            "/customers",
            &Value::String("bad key super-secret-key".into()),
        );
        assert!(!err.to_string().contains("super-secret-key"), "{err}");
    }
}
