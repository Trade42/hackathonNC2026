//! Gemini's circuit breaker (spec section 8, Resilience: "Nessie, Gemini, OSRM and Photon each
//! have a timeout, a retry and a fallback"). Without it, a Gemini that stops answering makes
//! every message wait out the turn's 25 s budget before the fallback answers.
//!
//! - **Opens** after [`FAILURES_TO_OPEN`] consecutive failed calls. A call is one
//!   [`Gemini::generate_counted`](super::gemini::Gemini::generate_counted), its retry included,
//!   and it fails when it ends in a timeout, a connection error or a 5xx. Any other outcome is an
//!   answer from Gemini (a reply, a 4xx including 429, an unusable 200), so it resets the count.
//! - **While open**, every call returns [`GeminiError::Open`] at once, without a request: the
//!   tool loop and the trip classifier both fall back through their usual error paths.
//! - **Probes:** while open, a background task sends ONE attempt every [`PROBE_INTERVAL`]. It
//!   sends the agent's real request shape ([`probe_request`]: every declaration, the system
//!   instruction, fixed fixture text, sealed), so a trivial request can't close the breaker
//!   while real ones still hang. A probe that answers closes it; one that fails keeps it open.
//! - One INFO line per state change, with the failure count only: no user data.
//! - The state is per app ([`crate::state::AppState::gemini_breaker`]), so tests never share it.
//!   The probe task holds only a weak reference and is aborted when the breaker goes away.

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tokio::task::JoinHandle;

use super::gemini::{self, Gemini, GeminiError, ToolMode};
use super::privacy::{Redactor, Sealed};
use super::tools;
use super::turn::system_instruction;

/// Consecutive failed calls that open the breaker.
pub const FAILURES_TO_OPEN: u32 = 2;

/// How often an open breaker probes Gemini.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(60);

/// The probe's output cap, as a turn's.
const PROBE_MAX_OUTPUT_TOKENS: u32 = 2048;

/// The breaker. Cheap to clone: clones share one state.
#[derive(Clone)]
pub struct Breaker {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<State>,
    probe_interval: Duration,
}

#[derive(Default)]
struct State {
    /// Consecutive failed calls.
    failures: u32,
    open: bool,
    /// The probe task while open.
    probe: Option<JoinHandle<()>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(state) = self.state.get_mut()
            && let Some(probe) = state.probe.take()
        {
            probe.abort();
        }
    }
}

impl Default for Breaker {
    fn default() -> Self {
        Self::new(PROBE_INTERVAL)
    }
}

impl std::fmt::Debug for Breaker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Breaker")
            .field("open", &self.is_open())
            .finish_non_exhaustive()
    }
}

/// What one call's outcome means to the breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Gemini answered (any reply, a 4xx, an unusable 200).
    Answered,
    /// A timeout, a connection error or a 5xx.
    Failed,
    /// The breaker itself refused the call: nothing was sent.
    Skipped,
}

fn outcome<T>(result: &Result<T, GeminiError>) -> Outcome {
    match result {
        Ok(_) => Outcome::Answered,
        Err(GeminiError::Timeout | GeminiError::Network) => Outcome::Failed,
        Err(GeminiError::Status(code)) if (500..=599).contains(code) => Outcome::Failed,
        Err(GeminiError::Open) => Outcome::Skipped,
        Err(GeminiError::Status(_) | GeminiError::Empty(_)) => Outcome::Answered,
    }
}

impl Breaker {
    /// A closed breaker whose probe, once open, runs every `probe_interval` ([`PROBE_INTERVAL`] in
    /// production; tests pass milliseconds).
    pub fn new(probe_interval: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                probe_interval,
            }),
        }
    }

    pub fn is_open(&self) -> bool {
        self.lock().open
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A panic while holding this lock leaves plain counters: keep using them.
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records one call's result. `gemini` is the client the probe will use if this opens the
    /// breaker.
    pub(crate) fn record<T>(&self, result: &Result<T, GeminiError>, gemini: &Gemini) {
        match outcome(result) {
            Outcome::Skipped => {}
            Outcome::Answered => self.close("a call answered"),
            Outcome::Failed => {
                let mut state = self.lock();
                state.failures = state.failures.saturating_add(1);
                if state.open || state.failures < FAILURES_TO_OPEN {
                    return;
                }
                state.open = true;
                tracing::info!(
                    failures = state.failures,
                    "gemini breaker opened; turns use the fallback until a probe answers"
                );
                let weak = Arc::downgrade(&self.inner);
                state.probe = Some(tokio::spawn(probe_loop(
                    weak,
                    gemini.without_breaker(),
                    self.inner.probe_interval,
                )));
            }
        }
    }

    /// Back to closed with no failures; logs and stops the probe when it was open.
    fn close(&self, why: &'static str) {
        let mut state = self.lock();
        state.failures = 0;
        if !state.open {
            return;
        }
        state.open = false;
        if let Some(probe) = state.probe.take() {
            // The probe closing the breaker ends its own loop instead (see `probe_loop`).
            if why != PROBE_ANSWERED {
                probe.abort();
            }
        }
        tracing::info!(reason = why, "gemini breaker closed");
    }
}

const PROBE_ANSWERED: &str = "a probe answered";

/// While the breaker lives and is open: wait `interval`, send one probe attempt, close on an
/// answer.
async fn probe_loop(breaker: Weak<Inner>, gemini: Gemini, interval: Duration) {
    let request = probe_request();
    loop {
        tokio::time::sleep(interval).await;
        let Some(inner) = breaker.upgrade() else {
            return;
        };
        let breaker = Breaker { inner };
        if !breaker.is_open() {
            return;
        }
        // Drop the strong reference while waiting on Gemini, so an app going away is not held.
        let weak = Arc::downgrade(&breaker.inner);
        drop(breaker);
        let result = gemini.probe(&request).await;
        let Some(inner) = weak.upgrade() else {
            return;
        };
        let breaker = Breaker { inner };
        match outcome(&result) {
            Outcome::Answered => {
                breaker.close(PROBE_ANSWERED);
                return;
            }
            _ => {
                let error = result.err().map_or("none", |e| e.label());
                tracing::debug!(error, "gemini probe failed; breaker stays open");
            }
        }
    }
}

/// The probe's request: the tool loop's real shape (the system instruction, every declaration,
/// the turn's output cap) around fixed fixture text, sealed like any request.
pub fn probe_request() -> Sealed {
    let system = system_instruction(
        false,
        "Sat, Oct 10, 7:42 PM",
        "The user's saved defaults (use them for suggestions): default drop-off: {dropoff}; \
         favorite restaurants: rst_cosmic; default tip: $2.00; walking pace: brisk.",
    );
    let contents = [gemini::user_text("how long until my food gets here?")];
    let body = gemini::build_request(
        &system,
        &contents,
        &tools::declarations(),
        ToolMode::Auto,
        PROBE_MAX_OUTPUT_TOKENS,
    );
    Redactor::new()
        .seal(body)
        .expect("the probe's fixed text holds no name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_counts_as_a_failure() {
        let failed: [Result<(), GeminiError>; 4] = [
            Err(GeminiError::Timeout),
            Err(GeminiError::Network),
            Err(GeminiError::Status(500)),
            Err(GeminiError::Status(503)),
        ];
        for result in &failed {
            assert_eq!(outcome(result), Outcome::Failed, "{result:?}");
        }
        let answered: [Result<(), GeminiError>; 5] = [
            Ok(()),
            Err(GeminiError::Status(400)),
            Err(GeminiError::Status(404)),
            Err(GeminiError::Status(429)),
            Err(GeminiError::Empty("no candidates".into())),
        ];
        for result in &answered {
            assert_eq!(outcome(result), Outcome::Answered, "{result:?}");
        }
        assert_eq!(outcome::<()>(&Err(GeminiError::Open)), Outcome::Skipped);
    }

    #[test]
    fn the_probe_request_is_the_real_shape_and_sealed() {
        let request = probe_request();
        let body = request.as_value();
        let declared = body["tools"][0]["functionDeclarations"]
            .as_array()
            .unwrap()
            .len();
        assert_eq!(declared, tools::Tool::ALL.len());
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
        assert!(body["systemInstruction"].is_object());
    }

    fn client() -> Gemini {
        let mut vars = std::collections::HashMap::from([
            (
                "DATABASE_URL".to_owned(),
                "postgres://u:p@127.0.0.1/db".to_owned(),
            ),
            ("GEMINI_API_KEY".to_owned(), "test-key-not-real".to_owned()),
            ("GEMINI_MODEL".to_owned(), "gemini-test".to_owned()),
        ]);
        vars.insert(
            "GEMINI_BASE_URL".to_owned(),
            "http://127.0.0.1:1".to_owned(),
        );
        let config = crate::config::Config::from_vars(&vars).unwrap();
        Gemini::from_config(&reqwest::Client::new(), &config).unwrap()
    }

    #[tokio::test]
    async fn the_probe_holds_no_strong_reference_and_stops_with_the_breaker() {
        let breaker = Breaker::new(Duration::from_secs(3600));
        let gemini = client();
        for _ in 0..FAILURES_TO_OPEN {
            breaker.record::<()>(&Err(GeminiError::Timeout), &gemini);
        }
        assert!(breaker.is_open());
        assert_eq!(
            Arc::strong_count(&breaker.inner),
            1,
            "only the breaker itself"
        );
        let weak = Arc::downgrade(&breaker.inner);
        drop(breaker);
        assert!(
            weak.upgrade().is_none(),
            "dropping the app's breaker frees it"
        );
    }

    #[tokio::test]
    async fn opens_after_two_failures_and_any_answer_resets_the_count() {
        let gemini = client();
        let breaker = Breaker::new(Duration::from_secs(3600));
        breaker.record::<()>(&Err(GeminiError::Timeout), &gemini);
        assert!(!breaker.is_open());
        // An answer between failures resets the count.
        breaker.record::<()>(&Err(GeminiError::Status(400)), &gemini);
        breaker.record::<()>(&Err(GeminiError::Status(503)), &gemini);
        assert!(!breaker.is_open());
        // A skipped call changes nothing.
        breaker.record::<()>(&Err(GeminiError::Open), &gemini);
        assert!(!breaker.is_open());
        breaker.record::<()>(&Err(GeminiError::Network), &gemini);
        assert!(breaker.is_open());
        assert!(breaker.lock().probe.is_some());
        // A real call that answers (one already in flight when it opened) closes it.
        breaker.record(&Ok(()), &gemini);
        assert!(!breaker.is_open());
        assert!(breaker.lock().probe.is_none());
        assert_eq!(breaker.lock().failures, 0);
    }
}
