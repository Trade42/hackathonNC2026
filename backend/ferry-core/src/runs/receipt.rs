//! `POST /v1/runs/{id}/receipt-upload`: where and with which one-time token the runner's app
//! uploads the receipt photo, straight to Supabase Storage with supabase-js's
//! `storage.from(bucket).uploadToSignedUrl(path, token, file)`. The app then sends the path as
//! `receipt_path` to `POST /v1/runs/{id}/purchased`.
//!
//! Only the run's runner, and only while the run is `closed` (after the close, before the
//! purchase). The photo lands in the private `receipts` bucket, whose own limits (5 MiB, images
//! only) Storage enforces on upload.

use rand::RngExt;
use serde::Serialize;

use super::access;
use super::machine::RunStatus;
use crate::error::{ApiError, ApiResult};
use crate::notifications::rfc3339;
use crate::state::AppState;
use crate::storage;

/// Random characters in a receipt's file name: 36^24 is about 2^124, so one runner's paths never
/// collide and cannot be guessed.
pub const RECEIPT_NAME_RANDOM_LEN: usize = 24;

/// `ReceiptUpload` in contract/openapi.yaml. No `Debug`: the token is a credential.
#[derive(Serialize)]
pub struct ReceiptUpload {
    pub bucket: &'static str,
    /// `<run_id>/<random>.jpg`.
    pub path: String,
    pub token: String,
    pub expires_at: String,
}

/// A new object path for a run's receipt: `<run_id>/` plus [`RECEIPT_NAME_RANDOM_LEN`]
/// characters from `[0-9a-z]` and `.jpg`.
pub fn new_receipt_path(run_id: &str) -> String {
    const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::rng();
    let mut path = String::with_capacity(run_id.len() + RECEIPT_NAME_RANDOM_LEN + 5);
    path.push_str(run_id);
    path.push('/');
    for _ in 0..RECEIPT_NAME_RANDOM_LEN {
        path.push(ALPHABET[rng.random_range(0..ALPHABET.len())] as char);
    }
    path.push_str(".jpg");
    path
}

/// `POST /v1/runs/{id}/receipt-upload` (runner only): a fresh path under the run's id and
/// Storage's one-time upload token for it.
///
/// - 404 `not_found`: the user may not see the run (or it does not exist).
/// - 403 `forbidden`: they may see it but are not its runner.
/// - 409 `conflict`: demo history, or the run is not `closed` (`details: {status}`).
/// - 503 `upstream_unavailable`: Storage is down, slow (more than 4 s) or not configured.
pub async fn create_receipt_upload(
    state: &AppState,
    user_id: &str,
    run_id: &str,
) -> ApiResult<ReceiptUpload> {
    let run = access::read(&state.pool, run_id, user_id).await?;
    run.ensure_runner()?;
    run.ensure_real()?;
    let status = run.status()?;
    if status != RunStatus::Closed {
        return Err(ApiError::conflict(format!(
            "A receipt is uploaded after the window closes and before the purchase; this run is {}.",
            status.as_str()
        ))
        .with_details(serde_json::json!({"status": status.as_str()})));
    }

    let path = new_receipt_path(run_id);
    let bucket = storage::RECEIPTS.id;
    let signed = storage::create_signed_upload(&state.http, &state.config, bucket, &path)
        .await
        .map_err(|err| {
            tracing::warn!(run_id, error = %err, "receipt upload token not created");
            ApiError::upstream_unavailable(
                "Receipt uploads are unavailable right now. Try again shortly.",
            )
        })?;
    tracing::info!(run_id, "receipt upload token created");
    Ok(ReceiptUpload {
        bucket,
        path,
        token: signed.token,
        expires_at: rfc3339(signed.expires_at),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_paths_start_with_the_run_and_differ() {
        let a = new_receipt_path("run_demo");
        let b = new_receipt_path("run_demo");
        assert_ne!(a, b);
        let name = a.strip_prefix("run_demo/").unwrap();
        let stem = name.strip_suffix(".jpg").unwrap();
        assert_eq!(stem.len(), RECEIPT_NAME_RANDOM_LEN);
        assert!(
            stem.bytes()
                .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
        );
    }
}
