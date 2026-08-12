//! Transport implementations.
//!
//! [`UsbTransport`] talks to the device over `nusb` (pure-Rust USB — the choice
//! that drops the libusb C dependency and the pkg-config build pain). It also
//! centralizes the macOS lesson: expose the link speed so the caller can
//! diagnose the SuperSpeed/U1-U2 failure instead of returning a bare I/O error.
//!
//! [`MockTransport`] replays capture fixtures, turning the T76 protocol into
//! hardware-free unit tests.
#![forbid(unsafe_code)]

use minipro_core::error::{Error, Result};
use minipro_core::transport::{Ep, LinkSpeed, Transport};

/// Real USB transport over `nusb` (stub).
pub struct UsbTransport {
    link: LinkSpeed,
    // device: nusb::Device, iface: nusb::Interface, ...
}

impl UsbTransport {
    /// Open the T76 (`0xA466:0x1A86`) and claim interface 0.
    pub fn open(_vid: u16, _pid: u16) -> Result<Self> {
        // TODO: nusb enumerate + open + claim; read negotiated speed.
        todo!("nusb open + claim interface")
    }

    /// On macOS + SuperSpeed the T76 bulk path fails; surface a clear diagnostic.
    pub fn check_link(&self) -> Result<()> {
        if cfg!(target_os = "macos") && self.link == LinkSpeed::Super {
            return Err(Error::Usb(
                "T76 on macOS SuperSpeed: bulk transfers fail; use a USB 2.0 cable".into(),
            ));
        }
        Ok(())
    }
}

impl Transport for UsbTransport {
    fn send(&mut self, _ep: Ep, _data: &[u8]) -> Result<()> { todo!() }
    fn recv(&mut self, _ep: Ep, _len: usize) -> Result<Vec<u8>> { todo!() }
    fn link_speed(&self) -> LinkSpeed { self.link }
    fn reset(&mut self) -> Result<()> { todo!() }
}

/// A record/replay transport for protocol unit tests. Each `send` must match the
/// next expected packet; each `recv` returns the next canned response.
pub struct MockTransport {
    script: Vec<(Vec<u8>, Vec<u8>)>, // (expected out, canned in)
    pos: usize,
}

impl MockTransport {
    /// Build from a captured `(out, in)` script (e.g. from a vendor USB capture).
    pub fn from_script(script: Vec<(Vec<u8>, Vec<u8>)>) -> Self {
        MockTransport { script, pos: 0 }
    }
}

impl Transport for MockTransport {
    fn send(&mut self, _ep: Ep, data: &[u8]) -> Result<()> {
        match self.script.get(self.pos) {
            Some((expected, _)) if expected == data => Ok(()),
            Some(_) => Err(Error::Protocol),
            None => Err(Error::Protocol),
        }
    }
    fn recv(&mut self, _ep: Ep, _len: usize) -> Result<Vec<u8>> {
        let resp = self.script.get(self.pos).map(|(_, r)| r.clone());
        self.pos += 1;
        resp.ok_or(Error::Protocol)
    }
    fn link_speed(&self) -> LinkSpeed { LinkSpeed::High }
    fn reset(&mut self) -> Result<()> { self.pos = 0; Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minipro_core::transport::{command, Ep};

    // Proves the testability story: a driver's protocol can be exercised with no
    // hardware, and the must-drain `Pending` guard forces the response read.
    #[test]
    fn command_roundtrip_over_mock() {
        let script = vec![(vec![0x3e, 0, 0, 0], vec![0xaa; 32])];
        let mut tx = MockTransport::from_script(script);
        let pending = command(&mut tx, Ep(0x01), Ep(0x81), &[0x3e, 0, 0, 0], 32).unwrap();
        let resp = pending.read().unwrap(); // dropping without read would warn
        assert_eq!(resp.len(), 32);
        assert_eq!(resp[0], 0xaa);
    }

    #[test]
    fn mock_rejects_wrong_packet() {
        let mut tx = MockTransport::from_script(vec![(vec![0x01], vec![0x00])]);
        assert!(tx.send(Ep(0x01), &[0x99]).is_err()); // desync -> Protocol error
    }
}
