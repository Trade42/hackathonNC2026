//! `POST /v1/auth/demo`: sign in as one of the four demo personas, only in demo mode.

use axum::extract::State;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::extract::Json;
use crate::profile::{User, load_user};
use crate::state::AppState;

/// `DemoPersona`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DemoPersona {
    Judge,
    Maya,
    Dana,
    Leo,
}

impl DemoPersona {
    pub const ALL: [DemoPersona; 4] = [
        DemoPersona::Judge,
        DemoPersona::Maya,
        DemoPersona::Dana,
        DemoPersona::Leo,
    ];

    /// The persona named `name`, as `DemoPersona` spells it (`judge`, `maya`, `dana`, `leo`).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }

    pub fn name(self) -> &'static str {
        match self {
            DemoPersona::Judge => "judge",
            DemoPersona::Maya => "maya",
            DemoPersona::Dana => "dana",
            DemoPersona::Leo => "leo",
        }
    }

    /// Persona `p` is the fixture user `usr_p` (docs/DECISIONS.md: personas are not stored).
    pub fn user_id(self) -> &'static str {
        match self {
            DemoPersona::Judge => "usr_judge",
            DemoPersona::Maya => "usr_maya",
            DemoPersona::Dana => "usr_dana",
            DemoPersona::Leo => "usr_leo",
        }
    }
}

/// `DemoSignInRequest`.
#[derive(Debug, Deserialize)]
pub struct DemoSignInRequest {
    pub persona: DemoPersona,
}

/// `AuthSession`.
#[derive(Debug, Serialize)]
pub struct AuthSession {
    pub token: String,
    pub user: User,
}

/// 403 `forbidden` unless DEMO_MODE is true, checked before the body. 404 when the persona's
/// user has not been seeded.
pub async fn sign_in(
    State(state): State<AppState>,
    body: Result<Json<DemoSignInRequest>, ApiError>,
) -> ApiResult<Json<AuthSession>> {
    if !state.config.demo_mode {
        return Err(ApiError::forbidden(
            "Demo sign-in works only when the server runs in demo mode.",
        ));
    }
    let Json(body) = body?;
    let user_id = body.persona.user_id();
    let user = load_user(&state.pool, user_id)
        .await?
        .ok_or_else(|| ApiError::not_found("This demo persona is not seeded yet."))?;
    tracing::info!(user_id, "demo sign-in");
    Ok(Json(AuthSession {
        token: state.jwt.issue_token(user_id)?,
        user,
    }))
}
