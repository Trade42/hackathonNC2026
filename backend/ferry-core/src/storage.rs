//! Supabase Storage through its REST API, with the server's secret key.
//!
//! A new secret key (`sb_secret_...`) is not a JWT, so it goes only in the `apikey` header; a
//! legacy `service_role` JWT goes in both `apikey` and `Authorization: Bearer`. Verified paths are
//! in docs/DECISIONS.md, Supabase.
//!
//! - [`ensure_buckets`]: the buckets, at startup.
//! - [`create_signed_upload`]: a one-time upload token for `uploadToSignedUrl` (receipt-upload).
//! - [`signed_receipt_urls`]: signed download links to receipt photos (`Order.receipt_url`),
//!   cached, and never an error.
//!
//! Upload tokens and signed links are credentials: nothing here logs them, and the URLs this
//! module requests never contain one.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::config::Config;

/// A bucket ferry-core creates at startup when it is missing.
#[derive(Debug, Clone, Serialize)]
pub struct BucketSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub public: bool,
    /// Bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size_limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_mime_types: Option<&'static [&'static str]>,
}

/// Runner receipt photos: private, images only, 5 MB (5 MiB) each.
pub const RECEIPTS: BucketSpec = BucketSpec {
    id: "receipts",
    name: "receipts",
    public: false,
    file_size_limit: Some(5 * 1024 * 1024),
    allowed_mime_types: Some(&["image/*"]),
};

/// Restaurant and menu photos the team takes: public.
pub const MEDIA: BucketSpec = BucketSpec {
    id: "media",
    name: "media",
    public: true,
    file_size_limit: None,
    allowed_mime_types: None,
};

pub const BUCKETS: [BucketSpec; 2] = [RECEIPTS, MEDIA];

/// Which header scheme a key needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// `sb_secret_...`: `apikey` only.
    Secret,
    /// A legacy `service_role` JWT: `apikey` and `Authorization: Bearer`.
    LegacyServiceRole,
}

impl KeyKind {
    pub fn of(key: &str) -> Result<Self, StorageError> {
        if key.starts_with("sb_secret_") {
            Ok(KeyKind::Secret)
        } else if key.starts_with("sb_publishable_") {
            Err(StorageError::PublishableKey)
        } else {
            Ok(KeyKind::LegacyServiceRole)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("Storage is not configured: {0}")]
    NotConfigured(&'static str),
    #[error("SUPABASE_SECRET_KEY holds a publishable key (sb_publishable_...), not a secret key")]
    PublishableKey,
    #[error("SUPABASE_SECRET_KEY contains characters that cannot go in a header")]
    BadKey,
    #[error("SUPABASE_URL cannot take a Storage path")]
    BadUrl,
    #[error("Storage's answer to {0} has no token")]
    NoToken(&'static str),
    #[error("Storage request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Storage answered {status} to {what}: {body}")]
    Status {
        what: &'static str,
        status: u16,
        body: String,
    },
}

/// The headers that authenticate a server request with `key`.
pub fn auth_headers(key: &str) -> Result<HeaderMap, StorageError> {
    let kind = KeyKind::of(key)?;
    let mut headers = HeaderMap::new();
    let mut apikey = HeaderValue::from_str(key).map_err(|_| StorageError::BadKey)?;
    apikey.set_sensitive(true);
    headers.insert("apikey", apikey);
    if kind == KeyKind::LegacyServiceRole {
        let mut bearer =
            HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| StorageError::BadKey)?;
        bearer.set_sensitive(true);
        headers.insert(AUTHORIZATION, bearer);
    }
    Ok(headers)
}

#[derive(Debug, Deserialize)]
struct Bucket {
    id: String,
    #[serde(default)]
    public: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// Nothing was called; the reason says which setting is missing.
    Skipped(&'static str),
    Done {
        created: Vec<&'static str>,
        existing: Vec<&'static str>,
    },
}

/// Creates the missing buckets in [`BUCKETS`]. Skips when `SUPABASE_URL` or
/// `SUPABASE_SECRET_KEY` is unset.
pub async fn ensure_buckets(
    http: &reqwest::Client,
    config: &Config,
) -> Result<EnsureOutcome, StorageError> {
    let (Some(base), Some(key)) = (&config.supabase_url, &config.supabase_secret_key) else {
        return Ok(EnsureOutcome::Skipped(
            if config.supabase_secret_key.is_none() {
                "SUPABASE_SECRET_KEY is unset"
            } else {
                "SUPABASE_URL is unset"
            },
        ));
    };
    let headers = auth_headers(key.expose())?;
    let url = format!("{base}/storage/v1/bucket");

    let response = http.get(&url).headers(headers.clone()).send().await?;
    let present: Vec<Bucket> = check(response, "GET /storage/v1/bucket")
        .await?
        .json()
        .await?;

    let mut outcome = (Vec::new(), Vec::new());
    for spec in &BUCKETS {
        match present.iter().find(|b| b.id == spec.id) {
            Some(bucket) => {
                if bucket.public != spec.public {
                    tracing::warn!(
                        bucket = spec.id,
                        public = bucket.public,
                        "storage bucket exists with the wrong visibility; fix it in the Supabase dashboard"
                    );
                }
                outcome.1.push(spec.id);
            }
            None => {
                let response = http
                    .post(&url)
                    .headers(headers.clone())
                    .json(spec)
                    .send()
                    .await?;
                check(response, "POST /storage/v1/bucket").await?;
                outcome.0.push(spec.id);
            }
        }
    }
    Ok(EnsureOutcome::Done {
        created: outcome.0,
        existing: outcome.1,
    })
}

/// Startup: ensures the buckets and logs the outcome. Never fails startup.
pub async fn ensure_buckets_at_startup(http: &reqwest::Client, config: &Config) {
    match ensure_buckets(http, config).await {
        Ok(EnsureOutcome::Skipped(reason)) => {
            tracing::warn!("storage buckets skipped: {reason}");
        }
        Ok(EnsureOutcome::Done { created, existing }) => {
            tracing::info!(?created, ?existing, "storage buckets ready");
        }
        Err(err) => tracing::error!("storage buckets not ensured: {err}"),
    }
}

/// A signed download link to a receipt photo is valid this long (`Order.receipt_url`: 1 hour).
pub const RECEIPT_URL_SECS: u64 = 3600;
/// A signed receipt link is reused for this long, so every link handed out has at least 10
/// minutes left.
pub const RECEIPT_URL_REUSE: Duration = Duration::from_secs(50 * 60);
/// A receipt Storage could not sign (no such object yet) is asked for again after this long.
pub const RECEIPT_MISSING_RETRY: Duration = Duration::from_secs(60);
/// After a failed signing call, reads leave Storage alone this long and show no receipt.
pub const READ_BACKOFF: Duration = Duration::from_secs(30);
/// An upload token request gives up after this long (receipt-upload answers 503).
pub const UPLOAD_SIGN_TIMEOUT: Duration = Duration::from_secs(4);
/// A signing call on an order read gives up after this long (the order shows no receipt).
pub const READ_SIGN_TIMEOUT: Duration = Duration::from_secs(2);
/// Storage's own default upload token lifetime (`UPLOAD_SIGNED_URL_EXPIRATION_TIME`, seconds),
/// assumed when a token carries no `exp`.
pub const UPLOAD_TOKEN_FALLBACK_SECS: u64 = 60;
/// The cache drops everything once it holds this many links and none has expired.
const READ_CACHE_MAX: usize = 4096;

/// What `storage.from(bucket).uploadToSignedUrl(path, token, file)` needs. No `Debug`: the token
/// is a credential.
pub struct SignedUpload {
    pub token: String,
    /// When Storage stops accepting the token: its `exp` claim.
    pub expires_at: SystemTime,
}

#[derive(Deserialize)]
struct UploadSigned {
    /// `/object/upload/sign/{bucket}/{path}?token=...`
    url: String,
    #[serde(default)]
    token: Option<String>,
}

/// `POST /storage/v1/object/upload/sign/{bucket}/{path}`: a one-time token to upload one new
/// object at `path` (no upsert), as supabase-js's `createSignedUploadUrl` asks for it. Storage
/// signs it without looking at the file; the bucket's own size and type limits apply when the
/// client uploads. Gives up after [`UPLOAD_SIGN_TIMEOUT`].
pub async fn create_signed_upload(
    http: &reqwest::Client,
    config: &Config,
    bucket: &str,
    path: &str,
) -> Result<SignedUpload, StorageError> {
    const WHAT: &str = "POST /storage/v1/object/upload/sign";
    let (base, key) = settings(config)?;
    let url = object_url(base, &["upload", "sign", bucket], path)?;
    let response = http
        .post(url)
        .headers(auth_headers(key)?)
        .timeout(UPLOAD_SIGN_TIMEOUT)
        .send()
        .await?;
    let signed: UploadSigned = check(response, WHAT).await?.json().await?;
    // supabase-js reads the token from the URL's query; the separate field is newer.
    let token = token_param(&signed.url)
        .or(signed.token)
        .filter(|t| !t.is_empty())
        .ok_or(StorageError::NoToken(WHAT))?;
    let expires_at = token_expiry(&token).unwrap_or_else(|| {
        tracing::warn!("storage upload token has no readable exp; assuming Storage's default");
        SystemTime::now() + Duration::from_secs(UPLOAD_TOKEN_FALLBACK_SECS)
    });
    Ok(SignedUpload { token, expires_at })
}

/// Signed download links, valid [`RECEIPT_URL_SECS`], to these paths in the `receipts` bucket,
/// keyed by path. Never an error: a path that cannot be signed (no such object, Storage down or
/// not configured) is left out, and the reader shows no receipt.
///
/// Links are reused for [`RECEIPT_URL_REUSE`] and a missing object is asked for again after
/// [`RECEIPT_MISSING_RETRY`], so order lists rarely call Storage. The rest are signed in one call
/// (`POST /storage/v1/object/sign/receipts`), which gives up after [`READ_SIGN_TIMEOUT`]; after a
/// failed call, reads leave Storage alone for [`READ_BACKOFF`].
pub async fn signed_receipt_urls(
    http: &reqwest::Client,
    config: &Config,
    paths: &[String],
) -> HashMap<String, String> {
    let mut links = HashMap::new();
    if paths.is_empty() {
        return links;
    }
    let Ok((base, key)) = settings(config) else {
        return links;
    };
    let bucket = RECEIPTS.id;
    let mut misses: Vec<String> = Vec::new();
    {
        let cache = READ_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        for path in paths {
            match cache.links.get(&cache_key(base, bucket, path)) {
                Some((Some(link), until)) if *until > now => {
                    links.insert(path.clone(), link.clone());
                }
                // Storage had no such object a moment ago: still none.
                Some((None, until)) if *until > now => {}
                _ if !misses.contains(path) => misses.push(path.clone()),
                _ => {}
            }
        }
        if cache.down_until.get(base).is_some_and(|until| *until > now) {
            misses.clear();
        }
    }
    if misses.is_empty() {
        return links;
    }

    let signed = sign_paths(http, base, key, bucket, &misses).await;
    let mut cache = READ_CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    let now = Instant::now();
    match signed {
        Ok(signed) => {
            if cache.links.len() + signed.len() > READ_CACHE_MAX {
                cache.links.retain(|_, (_, until)| *until > now);
                if cache.links.len() + signed.len() > READ_CACHE_MAX {
                    cache.links.clear();
                }
            }
            for (path, link) in signed {
                let reuse = if link.is_some() {
                    RECEIPT_URL_REUSE
                } else {
                    RECEIPT_MISSING_RETRY
                };
                if let Some(link) = &link {
                    links.insert(path.clone(), link.clone());
                }
                cache
                    .links
                    .insert(cache_key(base, bucket, &path), (link, now + reuse));
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "receipt links not signed; orders show no receipt for now");
            cache.down_until.insert(base.to_owned(), now + READ_BACKOFF);
        }
    }
    links
}

#[derive(Default)]
struct ReadCache {
    /// By [`cache_key`]: the signed link (none when Storage has no such object) and until when
    /// it is reused.
    links: HashMap<String, (Option<String>, Instant)>,
    /// By Storage base URL: reads skip Storage until then.
    down_until: HashMap<String, Instant>,
}

/// Keyed by Storage's base URL too, so tests' stubs in one process never share a link.
static READ_CACHE: LazyLock<Mutex<ReadCache>> = LazyLock::new(Mutex::default);

fn cache_key(base: &str, bucket: &str, path: &str) -> String {
    format!("{base} {bucket}/{path}")
}

#[derive(Deserialize)]
struct SignedObject {
    path: String,
    /// `/object/sign/{bucket}/{path}?token=...`, or null with `error` set when the object does
    /// not exist.
    #[serde(rename = "signedURL", default)]
    signed_url: Option<String>,
}

/// One `POST /storage/v1/object/sign/{bucket}` with `{expiresIn, paths}`: each path with its
/// absolute signed link, or `None` when Storage did not sign it.
async fn sign_paths(
    http: &reqwest::Client,
    base: &str,
    key: &str,
    bucket: &str,
    paths: &[String],
) -> Result<Vec<(String, Option<String>)>, StorageError> {
    let url = storage_url(base, &["object", "sign", bucket])?;
    let response = http
        .post(url)
        .headers(auth_headers(key)?)
        .timeout(READ_SIGN_TIMEOUT)
        .json(&serde_json::json!({"expiresIn": RECEIPT_URL_SECS, "paths": paths}))
        .send()
        .await?;
    let signed: Vec<SignedObject> = check(response, "POST /storage/v1/object/sign/{bucket}")
        .await?
        .json()
        .await?;
    Ok(paths
        .iter()
        .map(|path| {
            let link = signed
                .iter()
                .find(|s| &s.path == path)
                .and_then(|s| s.signed_url.as_deref())
                .and_then(token_param)
                .and_then(|token| download_url(base, bucket, path, &token).ok());
            (path.clone(), link)
        })
        .collect())
}

/// `{base}/storage/v1/object/sign/{bucket}/{path}?token=...`, every path segment percent-encoded:
/// Storage's own `signedURL` carries the path raw, and a stored path may hold a space.
fn download_url(base: &str, bucket: &str, path: &str, token: &str) -> Result<String, StorageError> {
    let mut url = object_url(base, &["sign", bucket], path)?;
    url.query_pairs_mut().append_pair("token", token);
    Ok(url.into())
}

/// `SUPABASE_URL` and the secret key, or why Storage cannot be called.
fn settings(config: &Config) -> Result<(&str, &str), StorageError> {
    match (&config.supabase_url, &config.supabase_secret_key) {
        (Some(base), Some(key)) => Ok((base, key.expose())),
        (_, None) => Err(StorageError::NotConfigured("SUPABASE_SECRET_KEY is unset")),
        (None, _) => Err(StorageError::NotConfigured("SUPABASE_URL is unset")),
    }
}

/// `{base}/storage/v1/{segments...}`, each segment percent-encoded.
fn storage_url(base: &str, segments: &[&str]) -> Result<Url, StorageError> {
    let mut url = Url::parse(base).map_err(|_| StorageError::BadUrl)?;
    url.path_segments_mut()
        .map_err(|_| StorageError::BadUrl)?
        .pop_if_empty()
        .extend(["storage", "v1"])
        .extend(segments);
    Ok(url)
}

/// `{base}/storage/v1/object/{action...}/{path}`, the object path split on `/`.
fn object_url(base: &str, action: &[&str], path: &str) -> Result<Url, StorageError> {
    let mut url = storage_url(base, &["object"])?;
    url.path_segments_mut()
        .map_err(|_| StorageError::BadUrl)?
        .extend(action)
        .extend(path.split('/'));
    Ok(url)
}

/// The `token` query parameter of a URL Storage returned (relative or absolute).
fn token_param(url: &str) -> Option<String> {
    let (_, query) = url.rsplit_once('?')?;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(name, _)| name == "token")
        .map(|(_, token)| token.into_owned())
        .filter(|token| !token.is_empty())
}

#[derive(Deserialize)]
struct Expiry {
    exp: f64,
}

/// The `exp` claim of a Storage token. Read without checking the signature: Storage checks it
/// on upload, and the server only reports the time.
fn token_expiry(token: &str) -> Option<SystemTime> {
    let Expiry { exp } = jsonwebtoken::dangerous::insecure_decode_claims(token).ok()?;
    UNIX_EPOCH.checked_add(Duration::try_from_secs_f64(exp).ok()?)
}

async fn check(
    response: reqwest::Response,
    what: &'static str,
) -> Result<reqwest::Response, StorageError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let mut body = response.text().await.unwrap_or_default();
    // The body is untrusted text (a proxy's HTML page, or bytes reqwest replaced with U+FFFD), so
    // cut on a char boundary: `truncate` panics inside a multibyte character.
    body.truncate(body.floor_char_boundary(300));
    Err(StorageError::Status {
        what,
        status: status.as_u16(),
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_key_goes_only_in_apikey() {
        let headers = auth_headers("sb_secret_abc").unwrap();
        assert_eq!(headers.get("apikey").unwrap(), "sb_secret_abc");
        assert!(headers.get(AUTHORIZATION).is_none());
    }

    #[test]
    fn legacy_jwt_goes_in_both() {
        let headers = auth_headers("eyJhbGciOiJIUzI1NiJ9.e30.sig").unwrap();
        assert_eq!(
            headers.get("apikey").unwrap(),
            "eyJhbGciOiJIUzI1NiJ9.e30.sig"
        );
        assert_eq!(
            headers.get(AUTHORIZATION).unwrap(),
            "Bearer eyJhbGciOiJIUzI1NiJ9.e30.sig"
        );
    }

    #[test]
    fn publishable_key_is_refused() {
        assert!(matches!(
            auth_headers("sb_publishable_abc"),
            Err(StorageError::PublishableKey)
        ));
    }

    #[test]
    fn download_links_encode_each_path_segment() {
        assert_eq!(
            download_url(
                "https://abc.supabase.co",
                "receipts",
                "run_x/r.jpg",
                "a.b-c_d"
            )
            .unwrap(),
            "https://abc.supabase.co/storage/v1/object/sign/receipts/run_x/r.jpg?token=a.b-c_d"
        );
        assert_eq!(
            download_url(
                "http://127.0.0.1:9",
                "receipts",
                "run_x/my receipt#1?.jpg",
                "t"
            )
            .unwrap(),
            "http://127.0.0.1:9/storage/v1/object/sign/receipts/run_x/my%20receipt%231%3F.jpg?token=t"
        );
        assert!(matches!(
            download_url("not a url", "receipts", "run_x/r.jpg", "t"),
            Err(StorageError::BadUrl)
        ));
    }

    #[test]
    fn upload_sign_url() {
        assert_eq!(
            object_url(
                "https://abc.supabase.co",
                &["upload", "sign", "receipts"],
                "run_x/r.jpg"
            )
            .unwrap()
            .as_str(),
            "https://abc.supabase.co/storage/v1/object/upload/sign/receipts/run_x/r.jpg"
        );
    }

    #[test]
    fn token_from_a_signed_url() {
        assert_eq!(
            token_param("/object/upload/sign/receipts/run_x/r.jpg?token=eyJ.eyJ.sig").as_deref(),
            Some("eyJ.eyJ.sig")
        );
        assert_eq!(
            token_param("/object/sign/receipts/run_x/a?b.jpg?token=t1").as_deref(),
            Some("t1")
        );
        assert_eq!(token_param("/object/sign/receipts/run_x/r.jpg"), None);
        assert_eq!(
            token_param("/object/sign/receipts/run_x/r.jpg?token="),
            None
        );
    }

    #[test]
    fn token_expiry_reads_exp() {
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &serde_json::json!({"url": "receipts/run_x/r.jpg", "exp": 2_000_000_000}),
            &jsonwebtoken::EncodingKey::from_secret(b"stub"),
        )
        .unwrap();
        assert_eq!(
            token_expiry(&token),
            Some(UNIX_EPOCH + Duration::from_secs(2_000_000_000))
        );
        assert_eq!(token_expiry("not-a-jwt"), None);
        let no_exp = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &serde_json::json!({"url": "receipts/run_x/r.jpg"}),
            &jsonwebtoken::EncodingKey::from_secret(b"stub"),
        )
        .unwrap();
        assert_eq!(token_expiry(&no_exp), None);
    }

    #[test]
    fn bucket_bodies() {
        assert_eq!(
            serde_json::to_value(&RECEIPTS).unwrap(),
            serde_json::json!({
                "id": "receipts", "name": "receipts", "public": false,
                "file_size_limit": 5_242_880, "allowed_mime_types": ["image/*"]
            })
        );
        assert_eq!(
            serde_json::to_value(&MEDIA).unwrap(),
            serde_json::json!({"id": "media", "name": "media", "public": true})
        );
    }
}
