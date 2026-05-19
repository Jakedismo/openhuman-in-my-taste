//! Direct Google API clients — Gmail (this slice), Calendar + Drive
//! follow in subsequent commits. Each submodule is a thin layer over
//! the relevant Google REST endpoints, using the Bearer token stored
//! in `AuthService` by the native OAuth flow.

pub mod gmail;
