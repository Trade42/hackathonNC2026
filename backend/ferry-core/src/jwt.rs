//! Ferry's own JWTs (HS256 with `JWT_SECRET`) and the `AuthUser` extractor.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{FromRef, FromRequestParts};
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::RngExt;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::ApiError;

/// How long an issued token stays valid.
pub const TOKEN_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The claims Ferry signs: `sub` is the user id (`usr_...`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub iat: u64,
    pub exp: u64,
}

/// Signing and verifying keys, cheap to clone. Part of `AppState`.
#[derive(Clone)]
pub struct JwtKeys(Arc<Inner>);

struct Inner {
    encoding: EncodingKey,
    decoding: DecodingKey,
    validation: Validation,
}

impl JwtKeys {
    pub fn new(secret: &[u8]) -> Self {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_required_spec_claims(&["exp", "sub"]);
        Self(Arc::new(Inner {
            encoding: EncodingKey::from_secret(secret),
            decoding: DecodingKey::from_secret(secret),
            validation,
        }))
    }

    /// Keys from `JWT_SECRET`. When it is unset, a random secret for this process only, with a
    /// warning: tokens then stop working when the server restarts.
    pub fn from_config(config: &Config) -> Self {
        match &config.jwt_secret {
            Some(secret) => {
                if secret.expose().len() < 32 {
                    tracing::warn!(
                        "JWT_SECRET is shorter than 32 characters; use openssl rand -hex 32"
                    );
                }
                Self::new(secret.expose().as_bytes())
            }
            None => {
                tracing::warn!(
                    "JWT_SECRET is unset: signing with a random per-process secret, so tokens end when the server stops"
                );
                let mut secret = [0u8; 32];
                rand::rng().fill(&mut secret);
                Self::new(&secret)
            }
        }
    }

    /// A token for `user_id`, valid for [`TOKEN_TTL`].
    pub fn issue_token(&self, user_id: &str) -> Result<String, ApiError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(ApiError::internal)?
            .as_secs();
        self.sign(&Claims {
            sub: user_id.to_owned(),
            iat: now,
            exp: now + TOKEN_TTL.as_secs(),
        })
    }

    /// Signs arbitrary claims. Tests use it for expired tokens.
    pub fn sign(&self, claims: &Claims) -> Result<String, ApiError> {
        jsonwebtoken::encode(&Header::new(Algorithm::HS256), claims, &self.0.encoding)
            .map_err(ApiError::internal)
    }

    /// Verifies a token's signature, algorithm and expiry. The WebSocket uses this for `?token=`.
    pub fn verify(&self, token: &str) -> Result<Claims, ApiError> {
        let data = jsonwebtoken::decode::<Claims>(token, &self.0.decoding, &self.0.validation)
            .map_err(|_| ApiError::unauthorized("Invalid or expired token."))?;
        if data.claims.sub.is_empty() {
            return Err(ApiError::unauthorized("Invalid or expired token."));
        }
        Ok(data.claims)
    }
}

/// The signed-in user, from `Authorization: Bearer <jwt>`. Rejects with 401 `unauthorized`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthUser {
    pub user_id: String,
}

impl<S> FromRequestParts<S> for AuthUser
where
    JwtKeys: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let header = parts.headers.get(AUTHORIZATION).ok_or_else(|| {
            ApiError::unauthorized("Sign in first: missing Authorization header.")
        })?;
        let token = header
            .to_str()
            .ok()
            .and_then(|value| value.split_once(' '))
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
            .map(|(_, token)| token.trim())
            .filter(|token| !token.is_empty())
            .ok_or_else(|| ApiError::unauthorized("Expected Authorization: Bearer <token>."))?;
        let claims = JwtKeys::from_ref(state).verify(token)?;
        Ok(AuthUser {
            user_id: claims.sub,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let keys = JwtKeys::new(b"0123456789abcdef0123456789abcdef");
        let token = keys.issue_token("usr_judge").unwrap();
        let claims = keys.verify(&token).unwrap();
        assert_eq!(claims.sub, "usr_judge");
        assert_eq!(claims.exp - claims.iat, TOKEN_TTL.as_secs());
    }

    #[test]
    fn rejects_other_secret_expired_and_garbage() {
        let keys = JwtKeys::new(b"0123456789abcdef0123456789abcdef");
        let other = JwtKeys::new(b"another-secret-another-secret-xx");
        assert!(
            keys.verify(&other.issue_token("usr_judge").unwrap())
                .is_err()
        );

        let expired = keys
            .sign(&Claims {
                sub: "usr_judge".into(),
                iat: 1_000,
                exp: 2_000,
            })
            .unwrap();
        assert!(keys.verify(&expired).is_err());
        assert!(keys.verify("not.a.jwt").is_err());
        assert!(keys.verify("").is_err());
    }

    #[test]
    fn rejects_alg_none() {
        let keys = JwtKeys::new(b"0123456789abcdef0123456789abcdef");
        // {"alg":"none","typ":"JWT"} . {"sub":"usr_judge","iat":1,"exp":99999999999} . (no signature)
        let token = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiJ1c3JfanVkZ2UiLCJpYXQiOjEsImV4cCI6OTk5OTk5OTk5OTl9.";
        assert!(keys.verify(token).is_err());
    }

    #[test]
    fn rejects_empty_sub() {
        let keys = JwtKeys::new(b"0123456789abcdef0123456789abcdef");
        let token = keys
            .sign(&Claims {
                sub: String::new(),
                iat: 1,
                exp: 99_999_999_999,
            })
            .unwrap();
        assert!(keys.verify(&token).is_err());
    }
}
