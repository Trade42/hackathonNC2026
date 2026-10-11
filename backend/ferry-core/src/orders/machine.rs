//! The order state machine: the contract's `OrderStatus`.
//!
//! ```text
//! funded ──run purchased──▶ purchased ──run picked up──▶ picked_up ──handoff──▶ delivered ──payout──▶ settled
//!   ├──customer cancels──▶ cancelled
//!   └──runner cancels the run──▶ refunded
//! ```
//!
//! Every other move is illegal and answers 409 `conflict`. Each move writes one
//! `ferry.order_events` row, which is the order's `timeline`.

use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};

/// `OrderStatus` in contract/openapi.yaml.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Funded,
    Purchased,
    PickedUp,
    Delivered,
    Settled,
    Cancelled,
    Refunded,
}

impl OrderStatus {
    pub const ALL: [OrderStatus; 7] = [
        OrderStatus::Funded,
        OrderStatus::Purchased,
        OrderStatus::PickedUp,
        OrderStatus::Delivered,
        OrderStatus::Settled,
        OrderStatus::Cancelled,
        OrderStatus::Refunded,
    ];

    /// An active order: the runner still has it to deliver (contract v1.2, runner-location
    /// privacy).
    pub const ACTIVE: [OrderStatus; 3] = [
        OrderStatus::Funded,
        OrderStatus::Purchased,
        OrderStatus::PickedUp,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            OrderStatus::Funded => "funded",
            OrderStatus::Purchased => "purchased",
            OrderStatus::PickedUp => "picked_up",
            OrderStatus::Delivered => "delivered",
            OrderStatus::Settled => "settled",
            OrderStatus::Cancelled => "cancelled",
            OrderStatus::Refunded => "refunded",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == value)
    }

    /// Parses a status read from `ferry.orders` or `ferry.order_events`, whose CHECKs allow only
    /// the contract's values.
    pub fn from_db(value: &str) -> ApiResult<Self> {
        Self::parse(value)
            .ok_or_else(|| ApiError::internal(format!("unknown order status {value:?}")))
    }

    pub fn is_active(self) -> bool {
        Self::ACTIVE.contains(&self)
    }

    /// Live: not cancelled or refunded. A run's `order_count` counts these.
    pub fn is_live(self) -> bool {
        !matches!(self, OrderStatus::Cancelled | OrderStatus::Refunded)
    }

    /// True when the machine allows `self` → `next`.
    pub fn can_become(self, next: OrderStatus) -> bool {
        use OrderStatus::*;
        matches!(
            (self, next),
            (Funded, Purchased)
                | (Funded, Cancelled)
                | (Funded, Refunded)
                | (Purchased, PickedUp)
                | (PickedUp, Delivered)
                | (Delivered, Settled)
        )
    }

    /// `next` if the machine allows `self` → `next`, else 409 `conflict`.
    pub fn transition(self, next: OrderStatus) -> ApiResult<OrderStatus> {
        if self.can_become(next) {
            Ok(next)
        } else {
            Err(ApiError::conflict(format!(
                "An order that is {} cannot become {}.",
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
    use OrderStatus::*;

    /// Every legal move, and nothing else.
    const LEGAL: [(OrderStatus, OrderStatus); 6] = [
        (Funded, Purchased),
        (Funded, Cancelled),
        (Funded, Refunded),
        (Purchased, PickedUp),
        (PickedUp, Delivered),
        (Delivered, Settled),
    ];

    #[test]
    fn every_pair_is_legal_or_409() {
        let mut legal = 0;
        for from in OrderStatus::ALL {
            for to in OrderStatus::ALL {
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
    fn ends_are_final_and_nothing_skips_a_step() {
        for status in OrderStatus::ALL {
            assert!(!status.can_become(status));
            for end in [Settled, Cancelled, Refunded] {
                assert!(!end.can_become(status), "{end:?} -> {status:?}");
            }
            assert!(!status.can_become(Funded));
        }
        // No cancel after the purchase, and no handoff before the pickup.
        assert!(!Purchased.can_become(Cancelled));
        assert!(!PickedUp.can_become(Cancelled));
        assert!(!Purchased.can_become(Delivered));
        assert!(!Funded.can_become(Delivered));
        assert!(!PickedUp.can_become(Settled));
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
            .skip_while(|l| l.trim() != "OrderStatus:")
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
        let ours: Vec<&str> = OrderStatus::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(listed, ours);
        for status in OrderStatus::ALL {
            assert_eq!(serde_json::to_value(status).unwrap(), status.as_str());
            assert_eq!(OrderStatus::parse(status.as_str()), Some(status));
        }
        assert!(OrderStatus::from_db("nope").is_err());
    }

    #[test]
    fn active_and_live() {
        let active: Vec<OrderStatus> = OrderStatus::ALL
            .into_iter()
            .filter(|s| s.is_active())
            .collect();
        assert_eq!(active, vec![Funded, Purchased, PickedUp]);
        let dead: Vec<OrderStatus> = OrderStatus::ALL
            .into_iter()
            .filter(|s| !s.is_live())
            .collect();
        assert_eq!(dead, vec![Cancelled, Refunded]);
    }
}
