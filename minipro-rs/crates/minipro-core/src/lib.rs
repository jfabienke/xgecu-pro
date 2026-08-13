// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! # minipro-core
//!
//! The load-bearing abstractions for the Rust redesign of minipro, per
//! `docs/rust-trait-model.md`. Four trait families:
//!
//! - [`transport::Transport`] — bytes on the wire, with a must-drain response guard.
//! - [`programmer::Programmer`] + the capability traits in [`caps`] — what a
//!   programmer *does*, discovered at runtime via object-safe accessor upcasts.
//! - [`report::Reporter`] — one event stream, rendered by human / JSON / TUI.
//! - [`error::Error`] — typed errors carrying machine-stable JSON codes.
//!
//! Everything used behind `dyn` is object-safe and `Send`. The [`ops`] module
//! carries the real orchestration (block loops, progress, verification with
//! crc32/sha256), written once over `dyn Programmer` + `dyn Reporter` so
//! drivers stay block-granular. With the default-on `serde` feature the
//! reporting/data types serialize to the documented JSON schema.
#![forbid(unsafe_code)]

pub mod caps;
pub mod device;
pub mod error;
pub mod format;
pub mod ops;
pub mod programmer;
pub mod report;
pub mod transport;

pub use error::{Error, Result};
pub use programmer::{Caps, Programmer, ProgrammerInfo, Session};
pub use transport::{Ep, LinkSpeed, Transport};
