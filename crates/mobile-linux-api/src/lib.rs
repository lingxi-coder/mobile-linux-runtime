//! Dependency-free-of-host contracts for portable mobile Linux runtimes.
#![forbid(unsafe_code)]
pub mod execution;
pub mod mobile_linux;
pub use execution::*;
pub use mobile_linux::*;
