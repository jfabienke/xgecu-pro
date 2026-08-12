//! Typed errors with machine-stable codes for the JSON mode.
//!
//! In the real crate this becomes a `thiserror`-derived enum; here it is
//! hand-rolled so `minipro-core` stays std-only. The key contract is
//! [`Error::code`] (never localized — an agent branches on it) and
//! [`Error::hint`] (the hard-won remediations: USB-2.0 cable, contact spray).

use std::fmt;

/// Firmware version, e.g. `00.1.17`. Placed on both the programmer and the chip
/// DB so a mismatch is a typed condition, not a `printf`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FwVersion(pub u32);

impl fmt::Display for FwVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = self.0;
        write!(f, "{:02}.{}.{:02}", v >> 16, (v >> 8) & 0xff, v & 0xff)
    }
}
impl fmt::Debug for FwVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{self}") }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Lower-level USB/transport failure.
    Usb(String),
    /// The command/response stream desynced (e.g. a response was not drained).
    Protocol,
    /// Electronic chip-id did not match the selected device.
    ChipIdMismatch { expected: u32, got: u32, alias: Option<String> },
    /// Pin-contact check reported open pins (advisory — reads may still work).
    BadContact(Vec<u8>),
    /// Device firmware and the bitstream DB target different versions.
    FirmwareMismatch { device: FwVersion, target: FwVersion },
    /// Read-back verification differed from the written image.
    Verify { addr: u32 },
    /// The selected programmer lacks a requested capability.
    Unsupported(&'static str),
    /// File-format parse/emit error.
    Format(String),
    /// I/O error (files, etc.).
    Io(std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Stable, non-localized identifier used as the JSON `code` field.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Usb(_) => "usb",
            Error::Protocol => "protocol",
            Error::ChipIdMismatch { .. } => "chip_id_mismatch",
            Error::BadContact(_) => "bad_contact",
            Error::FirmwareMismatch { .. } => "firmware_mismatch",
            Error::Verify { .. } => "verify_failed",
            Error::Unsupported(_) => "unsupported",
            Error::Format(_) => "format",
            Error::Io(_) => "io",
        }
    }

    /// A short, actionable remediation for the human/JSON layers, when one exists.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Error::Usb(_) => Some("on macOS + T76, connect via a USB 2.0 cable to force High Speed"),
            Error::ChipIdMismatch { .. } => Some("add --force to read anyway"),
            Error::BadContact(_) => Some("reseat/clean the pins, or --skip-pincheck to read anyway"),
            Error::FirmwareMismatch { .. } => Some("regenerate algorithm.xml for the device firmware"),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Usb(s) => write!(f, "usb transport: {s}"),
            Error::Protocol => write!(f, "protocol desync"),
            Error::ChipIdMismatch { expected, got, alias } => {
                write!(f, "chip id mismatch: expected {expected:04x}, got {got:04x}")?;
                if let Some(a) = alias { write!(f, " ({a})")?; }
                Ok(())
            }
            Error::BadContact(pins) => write!(f, "bad contact on pins {pins:?}"),
            Error::FirmwareMismatch { device, target } => {
                write!(f, "firmware mismatch: device {device}, bitstreams target {target}")
            }
            Error::Verify { addr } => write!(f, "verify failed at 0x{addr:06x}"),
            Error::Unsupported(what) => write!(f, "unsupported: {what}"),
            Error::Format(s) => write!(f, "format: {s}"),
            Error::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self { Error::Io(e) => Some(e), _ => None }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self { Error::Io(e) }
}
