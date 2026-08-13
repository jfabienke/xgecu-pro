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
pub mod wire;

use minipro_core::error::Error;
use minipro_core::transport::{command, Ep};

/// Detect the attached programmer and return the matching driver: read the
/// system-info report, branch on the device-type byte at `msg[6]`, and bind
/// the matching driver.
pub fn detect(
    mut transport: Box<dyn minipro_core::Transport>,
) -> minipro_core::Result<Box<dyn minipro_core::Programmer>> {
    // One system-info probe to pick the driver. The 5-byte zero request and
    // the msg[6] device-type byte are shared across all XGecu families; each
    // driver's own query_info re-reads and parses its family-specific layout.
    let report = command(transport.as_mut(), Ep(0x01), Ep(0x81), &[0u8; 5], 64)?.read()?;
    if report.len() < 7 {
        return Err(Error::Protocol);
    }
    match report[6] {
        // Device-type byte: 6 = T56, 7 = T48, 8 = T76.
        0x06 => {
            let mut t56 = t56::T56::new(transport);
            t56.query_info()?;
            Ok(Box::new(t56))
        }
        0x07 => {
            let mut t48 = t48::T48::new(transport);
            t48.query_info()?;
            Ok(Box::new(t48))
        }
        0x08 => {
            let mut t76 = t76::T76::new(transport);
            t76.query_info()?;
            Ok(Box::new(t76))
        }
        _ => Err(Error::Unsupported(
            "attached programmer is not a supported model (T48, T56, and T76 are supported)",
        )),
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
