//! Native OAuth 2.0 PKCE flow for direct provider authorization.
//!
//! Replaces the Composio-via-backend OAuth aggregator with first-party OAuth
//! handshakes against each provider (Google, GitHub, …). Tokens land in
//! [`crate::openhuman::credentials::AuthService`] (encrypted-at-rest) so they
//! never traverse any third party. See `tasks/todo.md` Phase 2 for the
//! full deletion plan.

pub mod loopback;
pub mod ops;
pub mod persistence;
pub mod pkce;
pub mod providers;

#[cfg(test)]
mod loopback_tests;
#[cfg(test)]
mod ops_tests;
#[cfg(test)]
mod persistence_tests;
#[cfg(test)]
mod pkce_tests;
