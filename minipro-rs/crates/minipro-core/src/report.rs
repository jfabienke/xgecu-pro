//! One event stream, three renderers. The core never prints — it emits
//! [`Event`]s during an op and a terminal [`Outcome`]. `Reporter` impls live in
//! `minipro-cli` (human / JSON / TUI). In the real crate `Event`/`Outcome`
//! derive `serde::Serialize`, which *is* the JSON mode.

use crate::transport::LinkSpeed;
use std::borrow::Cow;

/// A non-terminal progress/diagnostic event.
pub enum Event {
    Progress { done: u64, total: u64 },
    Warn(Warning),
    Note(Cow<'static, str>),
}

/// Structured warnings (so JSON can render a stable shape, not free text).
pub enum Warning {
    FirmwareMismatch,
    BadContact(Vec<u8>),
    ChipIdMismatch { expected: u32, got: u32 },
}

/// The terminal result of an operation. `#[derive(Serialize)]` in the real crate;
/// the JSON reporter emits this as the final NDJSON line.
pub enum Outcome {
    /// A completed read, carrying the built-in verification fields.
    Read {
        device: String,
        bytes: u64,
        crc32: u32,
        sha256: [u8; 32],
        reads: u8,
        stable: bool,
        link: LinkSpeed,
    },
    /// Programmer/chip status (`minipro info`).
    Info {
        model: String,
        firmware: String,
        firmware_expected: String,
        link: LinkSpeed,
        vcc: f32,
    },
    /// A generic success with no payload (erase, write).
    Ok { op: &'static str },
}

/// Renders the event stream. Object-safe and `Send` so the TUI can receive it
/// over a channel from a worker thread.
pub trait Reporter: Send {
    fn event(&mut self, ev: &Event);
    fn finish(&mut self, out: &Outcome);
}

/// A no-op reporter, useful in tests and non-interactive paths.
pub struct NullReporter;
impl Reporter for NullReporter {
    fn event(&mut self, _ev: &Event) {}
    fn finish(&mut self, _out: &Outcome) {}
}
