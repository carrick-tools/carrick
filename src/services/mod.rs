//! Service modules for external process management.
//!
//! This module contains services that manage external processes and communication
//! with sidecar applications.

pub mod sidecar_pool;
pub mod type_sidecar;

pub use type_sidecar::TypeSidecar;
