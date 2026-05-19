//! Direct Google API clients — Gmail, Calendar, Drive. Each submodule
//! is a thin layer over the relevant Google REST endpoints, using the
//! Bearer token stored in `AuthService` by the native OAuth flow.

pub mod calendar;
pub mod drive;
pub mod gmail;
