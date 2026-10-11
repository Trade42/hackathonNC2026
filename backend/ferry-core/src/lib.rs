//! ferry-core: the Ferry API server.

// Shared building blocks.
pub mod app;
pub mod config;
pub mod db;
pub mod error;
pub mod extract;
pub mod health;
pub mod ids;
pub mod jwt;
pub mod phone;
pub mod state;
pub mod storage;

// One module per area (spec section 2). Each M1 task fills in its own.
pub mod agent;
pub mod auth;
pub mod catalog;
pub mod circles;
pub mod demo;
pub mod impact;
pub mod invites;
pub mod messaging;
pub mod notifications;
pub mod orders;
pub mod payments;
pub mod profile;
pub mod requests;
pub mod runs;
pub mod tracking;

#[doc(hidden)]
pub mod test_support;

// The unit-test binary gets the same guard as every integration test binary.
#[cfg(test)]
#[ctor::ctor(unsafe)]
fn force_local_test_env() {
    // SAFETY: constructors run before main, while the process has one thread.
    unsafe { test_support::force_local_test_env() }
}
