//! Money math (spec section 4): integer cents (`i64`) only, never a float. Pure functions.

use serde::Serialize;

use crate::config::Config;
use crate::error::ApiError;

/// Escrow covers a receipt above the funded amount by at most this percent of it.
pub const OVERAGE_LIMIT_PERCENT: i64 = 5;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MathError {
    #[error("{0} must not be negative")]
    Negative(&'static str),
    #[error("qty must be at least 1")]
    BadQty,
    #[error("the amount is too large")]
    Overflow,
    #[error("there is nothing to split the amount against")]
    NoWeights,
}

impl From<MathError> for ApiError {
    fn from(err: MathError) -> Self {
        ApiError::validation(err.to_string())
    }
}

/// One priced order line: the item's price plus its choices' deltas, times qty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PricedLine {
    pub unit_price_cents: i64,
    pub qty: i64,
}

/// The contract's `OrderQuote`: what an order with these lines and tip charges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OrderQuote {
    pub subtotal_cents: i64,
    pub tax_cents: i64,
    pub ferry_fee_cents: i64,
    pub tip_cents: i64,
    pub total_cents: i64,
}

/// `subtotal × bps / 10000`, rounded half up, for one order.
pub fn tax_cents(subtotal_cents: i64, tax_rate_bps: i64) -> Result<i64, MathError> {
    if subtotal_cents < 0 {
        return Err(MathError::Negative("subtotal_cents"));
    }
    if tax_rate_bps < 0 {
        return Err(MathError::Negative("tax_rate_bps"));
    }
    let scaled = i128::from(subtotal_cents) * i128::from(tax_rate_bps) + 5_000;
    i64::try_from(scaled / 10_000).map_err(|_| MathError::Overflow)
}

/// An item's price plus the price deltas of the chosen options. Never below zero.
pub fn unit_price_cents(item_price_cents: i64, choice_deltas: &[i64]) -> Result<i64, MathError> {
    let mut unit = item_price_cents;
    for delta in choice_deltas {
        unit = unit.checked_add(*delta).ok_or(MathError::Overflow)?;
    }
    if unit < 0 {
        return Err(MathError::Negative("unit_price_cents"));
    }
    Ok(unit)
}

/// `unit_price_cents × qty`, the `line_total_cents` the orders CHECK enforces.
pub fn line_total_cents(line: PricedLine) -> Result<i64, MathError> {
    if line.unit_price_cents < 0 {
        return Err(MathError::Negative("unit_price_cents"));
    }
    if line.qty < 1 {
        return Err(MathError::BadQty);
    }
    line.unit_price_cents
        .checked_mul(line.qty)
        .ok_or(MathError::Overflow)
}

/// The one pricing function: placing an order and `POST /v1/runs/{id}/quote` both call it.
/// `total = subtotal + tax + ferry fee + tip`.
pub fn quote(
    lines: &[PricedLine],
    tip_cents: i64,
    tax_rate_bps: i64,
    ferry_fee_cents: i64,
) -> Result<OrderQuote, MathError> {
    if tip_cents < 0 {
        return Err(MathError::Negative("tip_cents"));
    }
    if ferry_fee_cents < 0 {
        return Err(MathError::Negative("ferry_fee_cents"));
    }
    let mut subtotal: i64 = 0;
    for line in lines {
        subtotal = subtotal
            .checked_add(line_total_cents(*line)?)
            .ok_or(MathError::Overflow)?;
    }
    let tax = tax_cents(subtotal, tax_rate_bps)?;
    let total = [tax, ferry_fee_cents, tip_cents]
        .into_iter()
        .try_fold(subtotal, i64::checked_add)
        .ok_or(MathError::Overflow)?;
    Ok(OrderQuote {
        subtotal_cents: subtotal,
        tax_cents: tax,
        ferry_fee_cents,
        tip_cents,
        total_cents: total,
    })
}

/// [`quote`] with `TAX_RATE_BPS` and `FERRY_FEE_CENTS` from the config.
pub fn quote_with(
    config: &Config,
    lines: &[PricedLine],
    tip_cents: i64,
) -> Result<OrderQuote, MathError> {
    quote(
        lines,
        tip_cents,
        config.tax_rate_bps,
        config.ferry_fee_cents,
    )
}

/// The most Escrow covers above `funded_cents`: 5% of it, rounded down.
pub fn overage_limit_cents(funded_cents: i64) -> i64 {
    funded_cents.max(0) * OVERAGE_LIMIT_PERCENT / 100
}

/// The highest receipt a run funded with `funded_cents` accepts without customer approval.
pub fn max_receipt_cents(funded_cents: i64) -> i64 {
    funded_cents.max(0) + overage_limit_cents(funded_cents)
}

/// Splits `total_cents` across `weights` (an order id and its subtotal + tax) in proportion,
/// by largest remainder: everyone gets the floor of their exact share, then the leftover cents
/// go one each to the largest remainders, ties broken by order id (ascending). The shares sum
/// to `total_cents` exactly. Returns the shares in the input order.
pub fn split_pro_rata<'a>(
    total_cents: i64,
    weights: &[(&'a str, i64)],
) -> Result<Vec<(&'a str, i64)>, MathError> {
    if total_cents < 0 {
        return Err(MathError::Negative("total_cents"));
    }
    if weights.iter().any(|(_, w)| *w < 0) {
        return Err(MathError::Negative("weight"));
    }
    let sum: i128 = weights.iter().map(|(_, w)| i128::from(*w)).sum();
    if sum == 0 {
        return if total_cents == 0 {
            Ok(weights.iter().map(|(id, _)| (*id, 0)).collect())
        } else {
            Err(MathError::NoWeights)
        };
    }
    let total = i128::from(total_cents);
    let mut shares: Vec<(&'a str, i64)> = Vec::with_capacity(weights.len());
    let mut remainders: Vec<(usize, i128)> = Vec::with_capacity(weights.len());
    let mut given: i128 = 0;
    for (index, (id, weight)) in weights.iter().enumerate() {
        let exact = total * i128::from(*weight);
        let floor = exact / sum;
        given += floor;
        // floor <= total, so it fits in i64.
        shares.push((*id, floor as i64));
        remainders.push((index, exact % sum));
    }
    remainders
        .sort_by(|(a, ra), (b, rb)| rb.cmp(ra).then_with(|| weights[*a].0.cmp(weights[*b].0)));
    let leftover = (total - given) as usize;
    for (index, _) in remainders.into_iter().take(leftover) {
        shares[index].1 += 1;
    }
    Ok(shares)
}

/// A ledger row's amount from the viewer's side: plus when one of `mine` receives it (the
/// debit account), minus when one of `mine` pays it (the credit account).
pub fn signed_amount(
    debit_account: &str,
    credit_account: &str,
    amount_cents: i64,
    mine: &[&str],
) -> i64 {
    let received = if mine.contains(&debit_account) {
        amount_cents
    } else {
        0
    };
    let paid = if mine.contains(&credit_account) {
        amount_cents
    } else {
        0
    };
    received - paid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tax_rounds_half_up() {
        // 750 bps = 7.5%.
        assert_eq!(tax_cents(0, 750), Ok(0));
        assert_eq!(tax_cents(1000, 750), Ok(75));
        assert_eq!(tax_cents(1001, 750), Ok(75)); // 75.075
        assert_eq!(tax_cents(1006, 750), Ok(75)); // 75.45
        assert_eq!(tax_cents(1007, 750), Ok(76)); // 75.525
        assert_eq!(tax_cents(1199, 750), Ok(90)); // 89.925
        // Exactly half a cent rounds up.
        assert_eq!(tax_cents(2, 2500), Ok(1)); // 0.5
        assert_eq!(tax_cents(6, 2500), Ok(2)); // 1.5
        assert_eq!(tax_cents(1, 4999), Ok(0)); // 0.4999
        assert_eq!(tax_cents(1, 5000), Ok(1)); // 0.5
        assert_eq!(tax_cents(1234, 0), Ok(0));
        assert_eq!(
            tax_cents(-1, 750),
            Err(MathError::Negative("subtotal_cents"))
        );
        assert_eq!(tax_cents(i64::MAX, 10_000), Ok(i64::MAX));
        assert_eq!(tax_cents(i64::MAX, 10_001), Err(MathError::Overflow));
    }

    #[test]
    fn unit_and_line_prices() {
        assert_eq!(unit_price_cents(1099, &[0, 150]), Ok(1249));
        assert_eq!(unit_price_cents(500, &[-100]), Ok(400));
        assert_eq!(
            unit_price_cents(100, &[-200]),
            Err(MathError::Negative("unit_price_cents"))
        );
        let line = PricedLine {
            unit_price_cents: 1249,
            qty: 2,
        };
        assert_eq!(line_total_cents(line), Ok(2498));
        assert_eq!(
            line_total_cents(PricedLine { qty: 0, ..line }),
            Err(MathError::BadQty)
        );
        assert_eq!(
            line_total_cents(PricedLine {
                unit_price_cents: i64::MAX,
                qty: 2
            }),
            Err(MathError::Overflow)
        );
    }

    #[test]
    fn quote_totals() {
        let lines = [
            PricedLine {
                unit_price_cents: 1249,
                qty: 1,
            },
            PricedLine {
                unit_price_cents: 350,
                qty: 2,
            },
        ];
        let q = quote(&lines, 200, 750, 150).unwrap();
        assert_eq!(q.subtotal_cents, 1949);
        assert_eq!(q.tax_cents, 146); // 146.175
        assert_eq!(q.ferry_fee_cents, 150);
        assert_eq!(q.tip_cents, 200);
        assert_eq!(q.total_cents, 1949 + 146 + 150 + 200);
        assert_eq!(
            serde_json::to_value(q).unwrap(),
            serde_json::json!({
                "subtotal_cents": 1949, "tax_cents": 146, "ferry_fee_cents": 150,
                "tip_cents": 200, "total_cents": 2445
            })
        );
        // No lines: only the fee and tip.
        assert_eq!(quote(&[], 0, 750, 150).unwrap().total_cents, 150);
        assert_eq!(
            quote(&lines, -1, 750, 150),
            Err(MathError::Negative("tip_cents"))
        );
    }

    #[test]
    fn overage_limit_rounds_down() {
        assert_eq!(overage_limit_cents(0), 0);
        assert_eq!(overage_limit_cents(19), 0); // 0.95
        assert_eq!(overage_limit_cents(20), 1);
        assert_eq!(overage_limit_cents(3999), 199); // 199.95
        assert_eq!(overage_limit_cents(4000), 200);
        assert_eq!(max_receipt_cents(4000), 4200);
        assert_eq!(max_receipt_cents(3999), 4198);
    }

    fn total(shares: &[(&str, i64)]) -> i64 {
        shares.iter().map(|(_, s)| s).sum()
    }

    #[test]
    fn split_sums_exactly_with_largest_remainder() {
        // 100 over three equal weights: 33.33 each; the leftover cent goes to the smallest id.
        let shares = split_pro_rata(100, &[("ord_c", 1), ("ord_a", 1), ("ord_b", 1)]).unwrap();
        assert_eq!(shares, vec![("ord_c", 33), ("ord_a", 34), ("ord_b", 33)]);

        // 10 over 1:2:3 → 1.667, 3.333, 5: floors 1, 3, 5, one cent left for the largest
        // remainder (ord_x, 0.667).
        let shares = split_pro_rata(10, &[("ord_x", 1), ("ord_y", 2), ("ord_z", 3)]).unwrap();
        assert_eq!(shares, vec![("ord_x", 2), ("ord_y", 3), ("ord_z", 5)]);

        // A share can be zero; zero total gives zeros.
        let shares = split_pro_rata(1, &[("ord_a", 1), ("ord_b", 1000)]).unwrap();
        assert_eq!(shares, vec![("ord_a", 0), ("ord_b", 1)]);
        assert_eq!(
            split_pro_rata(0, &[("ord_a", 0)]).unwrap(),
            vec![("ord_a", 0)]
        );
        assert_eq!(
            split_pro_rata(5, &[("ord_a", 0)]),
            Err(MathError::NoWeights)
        );
        assert_eq!(split_pro_rata(5, &[]), Err(MathError::NoWeights));
        assert_eq!(
            split_pro_rata(-5, &[("ord_a", 1)]),
            Err(MathError::Negative("total_cents"))
        );
    }

    #[test]
    fn split_sums_exactly_over_many_inputs() {
        let ids = [
            "ord_1", "ord_2", "ord_3", "ord_4", "ord_5", "ord_6", "ord_7",
        ];
        let mut seed: u64 = 42;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as i64
        };
        for _ in 0..2000 {
            let n = (next() % 7 + 1) as usize;
            let weights: Vec<(&str, i64)> =
                ids[..n].iter().map(|id| (*id, next() % 5000 + 1)).collect();
            let sum: i64 = weights.iter().map(|(_, w)| w).sum();
            let amount = next() % (sum + 1);
            let shares = split_pro_rata(amount, &weights).unwrap();
            assert_eq!(total(&shares), amount, "{weights:?} {amount}");
            for ((id, share), (wid, weight)) in shares.iter().zip(&weights) {
                assert_eq!(id, wid);
                // Each share is the floor or the ceiling of its exact share.
                let exact_floor = amount * weight / sum;
                assert!(*share == exact_floor || *share == exact_floor + 1);
                assert!(*share <= *weight, "a refund never exceeds what was paid");
            }
        }
    }

    #[test]
    fn signed_amounts_from_the_viewers_side() {
        let checking = "user:usr_judge:checking";
        let card = "user:usr_maya:ferry_card";
        let escrow = "platform:ferry:escrow";
        // A hold leaves the customer's checking: minus for them.
        assert_eq!(signed_amount(escrow, checking, 1300, &[checking]), -1300);
        // A refund comes back: plus.
        assert_eq!(signed_amount(checking, escrow, 200, &[checking]), 200);
        // The runner sees card funding as plus and the card purchase as minus.
        let maya = ["user:usr_maya:checking", card];
        assert_eq!(signed_amount(card, escrow, 4000, &maya), 4000);
        assert_eq!(
            signed_amount("restaurant:rst_cosmic:merchant", card, 3900, &maya),
            -3900
        );
        // Someone else's row is zero for this viewer.
        assert_eq!(signed_amount(card, escrow, 4000, &[checking]), 0);
        // A move between two of the viewer's own accounts nets to zero.
        assert_eq!(signed_amount(card, "user:usr_maya:checking", 10, &maya), 0);
    }
}
