//! The run state machine: the contract's `RunStatus`.
//!
//! ```text
//! open ──close──▶ closed ──purchased──▶ purchased ──picked-up──▶ delivering ──last handoff──▶ delivered
//!   │               │
//!   └────cancel─────┴──▶ cancelled
//! ```
//!
//! Every other move is illegal and answers 409 `conflict`.

use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};

/// `RunStatus` in contract/openapi.yaml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Open,
    Closed,
    Purchased,
    Delivering,
    Delivered,
    Cancelled,
}

impl RunStatus {
    pub const ALL: [RunStatus; 6] = [
        RunStatus::Open,
        RunStatus::Closed,
        RunStatus::Purchased,
        RunStatus::Delivering,
        RunStatus::Delivered,
        RunStatus::Cancelled,
    ];

    /// The statuses in which the runner is still on the way (`GET /v1/runs?mine=true`, tracking).
    pub const ACTIVE: [RunStatus; 4] = [
        RunStatus::Open,
        RunStatus::Closed,
        RunStatus::Purchased,
        RunStatus::Delivering,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Open => "open",
            RunStatus::Closed => "closed",
            RunStatus::Purchased => "purchased",
            RunStatus::Delivering => "delivering",
            RunStatus::Delivered => "delivered",
            RunStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == value)
    }

    /// Parses a status read from `ferry.runs`, whose CHECK allows only the contract's values.
    pub fn from_db(value: &str) -> ApiResult<Self> {
        Self::parse(value)
            .ok_or_else(|| ApiError::internal(format!("unknown run status {value:?}")))
    }

    pub fn is_active(self) -> bool {
        Self::ACTIVE.contains(&self)
    }

    /// True when the machine allows `self` → `next`.
    pub fn can_become(self, next: RunStatus) -> bool {
        use RunStatus::*;
        matches!(
            (self, next),
            (Open, Closed)
                | (Open, Cancelled)
                | (Closed, Purchased)
                | (Closed, Cancelled)
                | (Purchased, Delivering)
                | (Delivering, Delivered)
        )
    }

    /// `next` if the machine allows `self` → `next`, else 409 `conflict`.
    pub fn transition(self, next: RunStatus) -> ApiResult<RunStatus> {
        if self.can_become(next) {
            Ok(next)
        } else {
            Err(ApiError::conflict(format!(
                "A run that is {} cannot become {}.",
                self.as_str(),
                next.as_str()
            ))
            .with_details(serde_json::json!({"status": self.as_str()})))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use RunStatus::*;

    /// Every legal move, and nothing else.
    const LEGAL: [(RunStatus, RunStatus); 6] = [
        (Open, Closed),
        (Open, Cancelled),
        (Closed, Purchased),
        (Closed, Cancelled),
        (Purchased, Delivering),
        (Delivering, Delivered),
    ];

    #[test]
    fn every_pair_is_legal_or_409() {
        let mut legal = 0;
        for from in RunStatus::ALL {
            for to in RunStatus::ALL {
                let expected = LEGAL.contains(&(from, to));
                assert_eq!(from.can_become(to), expected, "{from:?} -> {to:?}");
                match from.transition(to) {
                    Ok(next) => {
                        assert!(expected, "{from:?} -> {to:?} must be illegal");
                        assert_eq!(next, to);
                        legal += 1;
                    }
                    Err(err) => {
                        assert!(!expected, "{from:?} -> {to:?} must be legal");
                        assert_eq!(err.code, ErrorCode::Conflict);
                        assert_eq!(err.status.as_u16(), 409);
                        assert_eq!(err.details.unwrap()["status"], from.as_str());
                    }
                }
            }
        }
        assert_eq!(legal, LEGAL.len());
    }

    #[test]
    fn no_status_moves_to_itself_and_the_ends_are_final() {
        for status in RunStatus::ALL {
            assert!(!status.can_become(status));
            assert!(!Delivered.can_become(status));
            assert!(!Cancelled.can_become(status));
            // Nothing goes back to open.
            assert!(!status.can_become(Open));
        }
    }

    #[test]
    fn names_match_the_contract() {
        let yaml = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contract/openapi.yaml"
        ))
        .unwrap();
        let line = yaml
            .lines()
            .skip_while(|l| l.trim() != "RunStatus:")
            .find(|l| l.trim_start().starts_with("enum:"))
            .unwrap();
        let listed: Vec<&str> = line
            .trim()
            .trim_start_matches("enum:")
            .trim()
            .trim_matches(['[', ']'])
            .split(',')
            .map(str::trim)
            .collect();
        let ours: Vec<&str> = RunStatus::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(listed, ours);
        for status in RunStatus::ALL {
            assert_eq!(serde_json::to_value(status).unwrap(), status.as_str());
            assert_eq!(RunStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(RunStatus::parse("nope"), None);
        assert!(RunStatus::from_db("nope").is_err());
    }

    #[test]
    fn active_statuses() {
        let active: Vec<RunStatus> = RunStatus::ALL
            .into_iter()
            .filter(|s| s.is_active())
            .collect();
        assert_eq!(active, vec![Open, Closed, Purchased, Delivering]);
    }
}
