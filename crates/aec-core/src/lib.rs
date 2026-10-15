//! Agent Egress Controller core.
//!
//! Vocabulary follows `CONTEXT.md`; behaviour follows `docs/SPEC.md`. Everything that touches the
//! machine sits behind the traits in [`probe`], so the rest of the crate is testable with
//! [`testing::FakeHost`] and never changes system state by accident.

pub mod agent;
pub mod checkpage;
pub mod checks;
pub mod cli;
pub mod controller;
pub mod core;
pub mod credentials;
#[cfg(windows)]
pub mod crypto;
pub mod dashboard;
pub mod desktop;
pub mod diagnostics;
pub mod entries;
pub mod fixed;
pub mod http;
pub mod import;
#[cfg(windows)]
pub mod live;
pub mod log;
pub mod model;
pub mod paths;
pub mod probe;
pub mod profile;
pub mod prompts;
pub mod providers;
pub mod relay;
pub mod route_switch;
pub mod setup;
pub mod snapshot;
pub mod status;
pub mod store;
pub mod testing;
pub mod tunnel;
pub mod ui;
pub mod writers;

pub mod brand;
#[cfg(windows)]
pub mod windows;
