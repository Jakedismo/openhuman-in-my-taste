//! Direct Google API clients — Gmail, Calendar (this slice), Drive
//! follows next. Each submodule is a thin layer over the relevant
//! Google REST endpoints, using the Bearer token stored in
//! `AuthService` by the native OAuth flow.

pub mod calendar;
pub mod gmail;
