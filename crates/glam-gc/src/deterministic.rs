//! Private deterministic-test-hook home.
//!
//! C0 has no concurrent state transition to intercept. Later phases may add
//! hook points in this private module; production behavior must not branch on
//! whether a hook is installed.
