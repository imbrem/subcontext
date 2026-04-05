//! Subcontext library crate.
//!
//! Exposes modules that are useful to integration tests. The binary
//! (`src/main.rs`) re-exports these under `crate::` so existing sibling
//! modules (`git`, `overlay`, …) that reference `crate::backend` continue
//! to compile.

pub mod backend;
