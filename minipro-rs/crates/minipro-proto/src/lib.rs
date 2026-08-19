// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! Per-programmer protocol drivers. Each implements the core traits over a
//! `Box<dyn Transport>`; the T76 is the reference driver (see [`t76`]).
//!
//! # Attribution
//!
//! These are independent Rust implementations of the T48/T56/T76 USB wire
//! protocols. The *protocol facts* they implement — opcodes, packet layouts,
//! and the T76's FPGA/bitstream operation sequence — were reverse-engineered by
//! the **minipro community (nmatt0)** in the `t76-improvements` fork
//! (<https://gitlab.com/nmatt0/minipro/-/tree/t76-improvements>). That RE is
//! what made this possible; the code here is original expression of those
//! facts. See the repo-root `NOTICE`.
#![forbid(unsafe_code)]

pub mod t48;
pub mod t56;
pub mod t76;
pub mod tl866ii;
pub mod wire;

use minipro_core::error::Error;
use minipro_core::transport::{command, Ep};

/// Programmer model, as reported in the system-info device-type byte
/// (`msg[6]`).
///
/// `TryFrom<u8>` decodes the wire byte; an unknown byte is a typed
/// [`Error::Unsupported`], and within this crate the [`detect`] dispatch
/// matches this enum exhaustively — adding a model is a compile-time checklist,
/// not a runtime fall-through.
///
/// ```
/// use minipro_proto::DeviceType;
/// assert_eq!(DeviceType::try_from(0x08).unwrap(), DeviceType::T76);
/// assert!(DeviceType::try_from(0x01).is_err());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeviceType {
    Tl866ii,
    T56,
    T48,
    T76,
}

impl TryFrom<u8> for DeviceType {
    type Error = Error;

    fn try_from(byte: u8) -> minipro_core::Result<DeviceType> {
        match byte {
            0x05 => Ok(DeviceType::Tl866ii),
            0x06 => Ok(DeviceType::T56),
            0x07 => Ok(DeviceType::T48),
            0x08 => Ok(DeviceType::T76),
            _ => Err(Error::Unsupported(
                "attached programmer is not a supported model \
                 (TL866II+, T48, T56, and T76 are supported)",
            )),
        }
    }
}

/// Detect the attached programmer and return the matching driver: read the
/// system-info report, decode the device-type byte at `msg[6]` into a
/// [`DeviceType`], and bind the matching driver.
pub fn detect(
    mut transport: Box<dyn minipro_core::Transport>,
) -> minipro_core::Result<Box<dyn minipro_core::Programmer>> {
    // One system-info probe to pick the driver. The 5-byte zero request and
    // the msg[6] device-type byte are shared across all XGecu families; each
    // driver's own query_info re-reads and parses its family-specific layout.
    let report = command(transport.as_mut(), Ep(0x01), Ep(0x81), &[0u8; 5], 64)?.read()?;
    // First contact with an unidentified device: read the type byte totally.
    match DeviceType::try_from(*report.get(6).ok_or(Error::Protocol)?)? {
        DeviceType::Tl866ii => {
            let mut p = tl866ii::Tl866ii::new(transport);
            p.query_info()?;
            Ok(Box::new(p))
        }
        DeviceType::T56 => {
            let mut t56 = t56::T56::new(transport);
            t56.query_info()?;
            Ok(Box::new(t56))
        }
        DeviceType::T48 => {
            let mut t48 = t48::T48::new(transport);
            t48.query_info()?;
            Ok(Box::new(t48))
        }
        DeviceType::T76 => {
            let mut t76 = t76::T76::new(transport);
            t76.query_info()?;
            Ok(Box::new(t76))
        }
    }
}

/// A transport that answers **every** read with the same caller-supplied bytes,
/// whatever length they are. Used by the property tests to drive each driver
/// with hostile/truncated device replies and assert nothing panics.
#[cfg(test)]
pub(crate) mod fuzz_tx {
    use minipro_core::error::Result;
    use minipro_core::transport::{Ep, LinkSpeed, Transport};

    pub(crate) struct FuzzTx {
        pub reply: Vec<u8>,
    }

    impl Transport for FuzzTx {
        fn send(&mut self, _ep: Ep, _data: &[u8]) -> Result<()> {
            Ok(())
        }
        /// Deliberately ignores the requested length: a real device can answer
        /// short, and that must be an error, never a panic.
        fn recv(&mut self, _ep: Ep, _len: usize) -> Result<Vec<u8>> {
            Ok(self.reply.clone())
        }
        fn link_speed(&self) -> LinkSpeed {
            LinkSpeed::High
        }
        fn reset(&mut self) -> Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minipro_core::transport::{LinkSpeed, Transport};
    use minipro_core::Result;
    use std::collections::VecDeque;

    /// Replays queued IN packets; the detect probe and the driver's own
    /// query_info each consume one.
    struct ReplayTx(VecDeque<Vec<u8>>);
    impl Transport for ReplayTx {
        fn send(&mut self, _ep: Ep, _data: &[u8]) -> Result<()> {
            Ok(())
        }
        fn recv(&mut self, _ep: Ep, _len: usize) -> Result<Vec<u8>> {
            self.0.pop_front().ok_or(Error::Protocol)
        }
        fn link_speed(&self) -> LinkSpeed {
            LinkSpeed::High
        }
        fn reset(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn report(device_type: u8) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[6] = device_type;
        r
    }

    #[test]
    fn detect_dispatches_on_device_type() {
        // T56 = 6: one probe report + one query_info report.
        let tx = ReplayTx(vec![report(0x06), report(0x06)].into());
        assert_eq!(detect(Box::new(tx)).unwrap().info().model, "T56");

        // T48 = 7.
        let tx = ReplayTx(vec![report(0x07), report(0x07)].into());
        assert_eq!(detect(Box::new(tx)).unwrap().info().model, "T48");

        // T76 = 8.
        let tx = ReplayTx(vec![report(0x08), report(0x08)].into());
        assert_eq!(detect(Box::new(tx)).unwrap().info().model, "T76");

        // Unknown model is rejected at the probe.
        let tx = ReplayTx(vec![report(0x01)].into());
        match detect(Box::new(tx)) {
            Err(e) => assert_eq!(e.code(), "unsupported"),
            Ok(_) => panic!("expected an unsupported-model error"),
        }
    }
}
