// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! Typed errors with machine-stable codes for the JSON mode.
//!
//! The key contract is [`Error::code`] (never localized — an agent branches on
//! it) and [`Error::hint`] (the hard-won remediations: USB-2.0 cable, contact
//! spray). Display strings are human-facing and may change; `code()` may not.

use std::fmt;

/// Firmware version, e.g. `00.1.17`. Placed on both the programmer and the chip
/// DB so a mismatch is a typed condition, not a `printf`.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct FwVersion(pub u32);

impl fmt::Display for FwVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = self.0;
        write!(f, "{:02}.{}.{:02}", v >> 16, (v >> 8) & 0xff, v & 0xff)
    }
}
impl fmt::Debug for FwVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

/// Serializes as its display string (`"00.1.17"`) — the JSON schema's shape.
#[cfg(feature = "serde")]
impl serde::Serialize for FwVersion {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

/// Deserializes from the display string (`"00.1.17"` → `FwVersion(0x0111)`),
/// the inverse of `Serialize` — needed by the compiled chip-DB blob.
#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for FwVersion {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        use serde::de::Error as _;
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        let mut parts = s.split('.');
        let mut next = |what| {
            parts
                .next()
                .and_then(|p| p.parse::<u32>().ok())
                .filter(|&v| v <= 0xff)
                .ok_or_else(|| D::Error::custom(format!("bad firmware version {what} in {s:?}")))
        };
        let (major, minor, patch) = (next("major")?, next("minor")?, next("patch")?);
        if parts.next().is_some() {
            return Err(D::Error::custom(format!("bad firmware version {s:?}")));
        }
        Ok(FwVersion((major << 16) | (minor << 8) | patch))
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Lower-level USB/transport failure.
    #[error("usb transport: {0}")]
    Usb(String),
    /// The command/response stream desynced (e.g. a response was not drained).
    #[error("protocol desync")]
    Protocol,
    /// Electronic chip-id did not match the selected device.
    #[error("chip id mismatch: expected {expected:04x}, got {got:04x}{}",
            alias.as_deref().map(|a| format!(" ({a})")).unwrap_or_default())]
    ChipIdMismatch {
        expected: u32,
        got: u32,
        alias: Option<String>,
    },
    /// Pin-contact check reported open pins (advisory — reads may still work).
    #[error("bad contact on pins {0:?}")]
    BadContact(Vec<u8>),
    /// The programmer's overcurrent protection tripped during BEGIN_TRANS.
    #[error("overcurrent protection triggered")]
    Overcurrent,
    /// Device firmware and the bitstream DB target different versions.
    #[error("firmware mismatch: device {device}, bitstreams target {target}")]
    FirmwareMismatch {
        device: FwVersion,
        target: FwVersion,
    },
    /// Read-back verification differed from the written image.
    #[error("verify failed at 0x{addr:06x}")]
    Verify { addr: u32 },
    /// The selected programmer lacks a requested capability.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    /// File-format parse/emit error.
    #[error("format: {0}")]
    Format(String),
    /// I/O error (files, etc.).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
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
            Error::Overcurrent => "overcurrent",
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
            Error::Usb(_) => {
                Some("on macOS + T76, connect via a USB 2.0 cable to force High Speed")
            }
            Error::ChipIdMismatch { .. } => Some("add --force to read anyway"),
            Error::BadContact(_) => {
                Some("reseat/clean the pins, or --skip-pincheck to read anyway")
            }
            Error::Overcurrent => Some("check chip orientation and socket position, then retry"),
            Error::FirmwareMismatch { .. } => {
                Some("regenerate algorithm.xml for the device firmware")
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fw_version_formats_like_vendor() {
        assert_eq!(FwVersion(0x00_01_11).to_string(), "00.1.17");
        assert_eq!(FwVersion(0x00_01_07).to_string(), "00.1.07");
    }

    #[test]
    fn codes_are_stable() {
        // The JSON `code` contract: agents branch on these exact strings.
        let cases: Vec<(Error, &str)> = vec![
            (Error::Usb("x".into()), "usb"),
            (Error::Protocol, "protocol"),
            (
                Error::ChipIdMismatch {
                    expected: 0x208d,
                    got: 0x1e8c,
                    alias: None,
                },
                "chip_id_mismatch",
            ),
            (Error::BadContact(vec![3, 4]), "bad_contact"),
            (Error::Overcurrent, "overcurrent"),
            (
                Error::FirmwareMismatch {
                    device: FwVersion(1),
                    target: FwVersion(2),
                },
                "firmware_mismatch",
            ),
            (Error::Verify { addr: 0x100 }, "verify_failed"),
            (Error::Unsupported("x"), "unsupported"),
            (Error::Format("x".into()), "format"),
            (Error::Io(std::io::Error::other("x")), "io"),
        ];
        for (err, code) in cases {
            assert_eq!(err.code(), code);
        }
    }

    #[test]
    fn chip_id_mismatch_display_includes_alias() {
        let e = Error::ChipIdMismatch {
            expected: 0x208d,
            got: 0x1e8c,
            alias: Some("AT27C256R".into()),
        };
        assert_eq!(
            e.to_string(),
            "chip id mismatch: expected 208d, got 1e8c (AT27C256R)"
        );
        let e = Error::ChipIdMismatch {
            expected: 0x208d,
            got: 0x1e8c,
            alias: None,
        };
        assert_eq!(e.to_string(), "chip id mismatch: expected 208d, got 1e8c");
    }

    #[cfg(feature = "serde")]
    #[test]
    fn fw_version_serde_roundtrips() {
        for v in [FwVersion(0x00_01_11), FwVersion(0x00_01_07), FwVersion(0)] {
            let json = serde_json::to_string(&v).unwrap();
            let back: FwVersion = serde_json::from_str(&json).unwrap();
            assert_eq!(back, v, "{json}");
        }
        assert_eq!(
            serde_json::from_str::<FwVersion>("\"00.1.17\"").unwrap(),
            FwVersion(0x0111)
        );
        for bad in [
            "\"\"",
            "\"1.2\"",
            "\"1.2.3.4\"",
            "\"1.2.999\"",
            "\"x.y.z\"",
            "42",
        ] {
            assert!(serde_json::from_str::<FwVersion>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn io_error_has_source() {
        use std::error::Error as _;
        let e = Error::from(std::io::Error::other("boom"));
        assert!(e.source().is_some());
        assert_eq!(e.to_string(), "io: boom");
    }
}
