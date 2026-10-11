//! Stream tickets (contract v1.2, `POST /v1/stream/ticket`): random, single use, short-lived,
//! bound to one user and kept in memory, so the long-lived JWT stays out of WebSocket URLs.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rand::RngExt;

/// Live tickets one user may hold. Issuing another drops their oldest.
pub const MAX_TICKETS_PER_USER: usize = 8;

/// Random bytes per ticket (printed as 64 hex characters).
const TICKET_BYTES: usize = 32;

#[derive(Default)]
pub(crate) struct Tickets {
    live: Mutex<HashMap<String, Ticket>>,
}

struct Ticket {
    user_id: String,
    issued: Instant,
    expires: Instant,
}

impl Tickets {
    /// A new ticket for `user_id`, valid for `ttl`. Also forgets expired tickets.
    pub(crate) fn issue(&self, user_id: &str, ttl: Duration) -> String {
        let ticket = random_ticket();
        let now = Instant::now();
        let mut live = self.lock();
        live.retain(|_, t| t.expires > now);
        let mut mine: Vec<(Instant, String)> = live
            .iter()
            .filter(|(_, t)| t.user_id == user_id)
            .map(|(key, t)| (t.issued, key.clone()))
            .collect();
        if mine.len() >= MAX_TICKETS_PER_USER {
            mine.sort();
            for (_, key) in &mine[..=mine.len() - MAX_TICKETS_PER_USER] {
                live.remove(key);
            }
        }
        live.insert(
            ticket.clone(),
            Ticket {
                user_id: user_id.to_owned(),
                issued: now,
                expires: now + ttl,
            },
        );
        ticket
    }

    /// The ticket's user, if the ticket exists and has not expired. Either way the ticket is used
    /// up: a second redeem of the same ticket returns `None`.
    pub(crate) fn redeem(&self, ticket: &str) -> Option<String> {
        let found = self.lock().remove(ticket)?;
        (found.expires > Instant::now()).then_some(found.user_id)
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Ticket>> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }
}

fn random_ticket() -> String {
    let mut bytes = [0u8; TICKET_BYTES];
    rand::rng().fill(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = Duration::from_secs(60);

    #[test]
    fn single_use_and_bound_to_the_user() {
        let tickets = Tickets::default();
        let a = tickets.issue("usr_a", TTL);
        let b = tickets.issue("usr_b", TTL);
        assert_eq!(a.len(), 2 * TICKET_BYTES);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert_eq!(tickets.redeem(&a).as_deref(), Some("usr_a"));
        assert_eq!(tickets.redeem(&a), None);
        assert_eq!(tickets.redeem(&b).as_deref(), Some("usr_b"));
        assert_eq!(tickets.redeem("not-a-ticket"), None);
        assert_eq!(tickets.redeem(""), None);
    }

    #[test]
    fn expired_tickets_fail_and_are_forgotten() {
        let tickets = Tickets::default();
        let old = tickets.issue("usr_a", Duration::ZERO);
        assert_eq!(tickets.redeem(&old), None);

        let stale = tickets.issue("usr_a", Duration::ZERO);
        let _fresh = tickets.issue("usr_b", TTL);
        // Issuing purged the expired one.
        assert_eq!(tickets.len(), 1);
        assert_eq!(tickets.redeem(&stale), None);
    }

    #[test]
    fn a_user_holds_at_most_max_tickets() {
        let tickets = Tickets::default();
        let first = tickets.issue("usr_a", TTL);
        let rest: Vec<String> = (0..MAX_TICKETS_PER_USER)
            .map(|_| tickets.issue("usr_a", TTL))
            .collect();
        let other = tickets.issue("usr_b", TTL);
        assert_eq!(tickets.len(), MAX_TICKETS_PER_USER + 1);
        assert_eq!(tickets.redeem(&first), None, "the oldest was dropped");
        for ticket in &rest {
            assert_eq!(tickets.redeem(ticket).as_deref(), Some("usr_a"));
        }
        assert_eq!(tickets.redeem(&other).as_deref(), Some("usr_b"));
    }
}
