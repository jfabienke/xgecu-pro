// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! The USB command channel abstraction, and the must-drain response guard.
//!
//! Real impls live in `minipro-usb` (over `nusb`) and a `MockTransport` that
//! replays capture fixtures. Drivers only ever see `&mut dyn Transport`, which
//! is what makes the protocol unit-testable without hardware.

use crate::error::{Error, Result};

/// A USB endpoint address (e.g. `0x01` OUT, `0x81` IN).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ep(pub u8);

/// Negotiated USB link speed. `link_speed()` surfaces it because on macOS the
/// T76's SuperSpeed bulk path fails and we want to diagnose, not guess.
/// Serializes as the short codes the JSON schema documents: `fs` / `hs` / `ss`.
///
/// `#[non_exhaustive]`: newer USB speeds may be added; consumers should keep a
/// fallback arm rather than match exhaustively.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[non_exhaustive]
pub enum LinkSpeed {
    #[cfg_attr(feature = "serde", serde(rename = "fs"))]
    Full,
    #[cfg_attr(feature = "serde", serde(rename = "hs"))]
    High,
    #[cfg_attr(feature = "serde", serde(rename = "ss"))]
    Super,
}

/// The command channel. Object-safe and `Send`.
pub trait Transport: Send {
    /// Send `data` on `ep` (OUT).
    fn send(&mut self, ep: Ep, data: &[u8]) -> Result<()>;
    /// Receive exactly `len` bytes on `ep` (IN).
    fn recv(&mut self, ep: Ep, len: usize) -> Result<Vec<u8>>;
    /// Receive with an extended deadline, for the few commands the device
    /// answers only once a chip operation has finished (erase, chiefly).
    ///
    /// Separate from [`Transport::recv`] on purpose. The deadline that suits a
    /// chip erase is minutes; applying it to every reply means a device that
    /// stops answering a millisecond-scale command freezes for minutes instead
    /// of failing. Opting in per command keeps the common path fast to fail.
    ///
    /// Defaults to `recv`, so transports with no notion of a deadline (mocks)
    /// need do nothing.
    fn recv_slow(&mut self, ep: Ep, len: usize) -> Result<Vec<u8>> {
        self.recv(ep, len)
    }
    /// Currently negotiated link speed.
    fn link_speed(&self) -> LinkSpeed;
    /// Reset the device / re-arm the endpoints.
    fn reset(&mut self) -> Result<()>;

    /// Receive a payload the device splits across **two** IN endpoints — the
    /// TL866II+ streams reads larger than 64 bytes as alternating 64-byte
    /// blocks whose halves land contiguously on EP2 and EP3, read in parallel
    /// and deinterlaced host-side (the C's `read_payload2` with `limit = 64`).
    /// `len` must be a multiple of 128 — the reference's deinterlace drops the
    /// tail otherwise, and every real page size in the database qualifies.
    ///
    /// Default: unsupported, so single-pipe transports need do nothing.
    fn recv_interlaced(&mut self, _ep_a: Ep, _ep_b: Ep, _len: usize) -> Result<Vec<u8>> {
        Err(Error::Unsupported(
            "this transport has no dual-endpoint interlaced receive",
        ))
    }

    /// Send a payload split across **two** OUT endpoints, first part to
    /// `ep_a`, remainder to `ep_b`, in parallel — the TL866II+ write side
    /// (the C's `write_payload2` with `limit = 64`; the split arithmetic is
    /// the transport's, from XGPro). Contiguous halves, not interlaced —
    /// asymmetric with the read side, faithfully.
    fn send_interlaced(&mut self, _ep_a: Ep, _ep_b: Ep, _data: &[u8]) -> Result<()> {
        Err(Error::Unsupported(
            "this transport has no dual-endpoint interlaced send",
        ))
    }
}

/// A sent command whose response has *not* yet been read.
///
/// The T76 wedges (needs a USB replug) if a command's response is left
/// undrained, so this guard is `#[must_use]` and can only be resolved by
/// consuming it — turning "forgot to read EP81" into a compile-time warning.
#[must_use = "drain this reply: a T76 left holding one stops responding until unplugged"]
pub struct Pending<'t> {
    tx: &'t mut dyn Transport,
    ep: Ep,
    len: usize,
    /// Whether this reply waits on a chip operation; see [`command_slow`].
    slow: bool,
}

impl<'t> Pending<'t> {
    /// Read and return the response payload.
    pub fn read(self) -> Result<Vec<u8>> {
        if self.slow {
            self.tx.recv_slow(self.ep, self.len)
        } else {
            self.tx.recv(self.ep, self.len)
        }
    }
    /// Drain and discard the response (still required — the device must be read).
    pub fn discard(self) -> Result<()> {
        self.read().map(|_| ())
    }
}

/// Issue a command: send `pkt` on `out`, returning a [`Pending`] that must be
/// drained from `r#in`. Every command declares its response length up front,
/// exactly as the vendor protocol does.
pub fn command<'t>(
    tx: &'t mut dyn Transport,
    out: Ep,
    r#in: Ep,
    pkt: &[u8],
    resp_len: usize,
) -> Result<Pending<'t>> {
    tx.send(out, pkt)?;
    Ok(Pending {
        tx,
        ep: r#in,
        len: resp_len,
        slow: false,
    })
}

/// Like [`command`], but for a reply the device sends only after a chip
/// operation completes — a full-chip erase can legitimately take minutes.
///
/// Use this *only* where the wait is inherent to the chip. Everything else
/// should fail fast: measured on a live T76, no ordinary command reply took
/// longer than 200 ms.
pub fn command_slow<'t>(
    tx: &'t mut dyn Transport,
    out: Ep,
    r#in: Ep,
    pkt: &[u8],
    resp_len: usize,
) -> Result<Pending<'t>> {
    tx.send(out, pkt)?;
    Ok(Pending {
        tx,
        ep: r#in,
        len: resp_len,
        slow: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records which receive path a `Pending` resolved through.
    #[derive(Default)]
    struct Spy {
        slow_used: bool,
        fast_used: bool,
    }
    impl Transport for Spy {
        fn send(&mut self, _ep: Ep, _data: &[u8]) -> Result<()> {
            Ok(())
        }
        fn recv(&mut self, _ep: Ep, len: usize) -> Result<Vec<u8>> {
            self.fast_used = true;
            Ok(vec![0; len])
        }
        fn recv_slow(&mut self, _ep: Ep, len: usize) -> Result<Vec<u8>> {
            self.slow_used = true;
            Ok(vec![0; len])
        }
        fn link_speed(&self) -> LinkSpeed {
            LinkSpeed::High
        }
        fn reset(&mut self) -> Result<()> {
            Ok(())
        }
    }

    /// The long deadline is opt-in per command. Getting this backwards is not a
    /// cosmetic slip: routing an ordinary command through the slow path turns a
    /// device that stops answering into a multi-minute freeze instead of a
    /// prompt error, which is exactly the behaviour this split removed.
    #[test]
    fn only_command_slow_takes_the_long_deadline() {
        let mut spy = Spy::default();
        command(&mut spy, Ep(0x01), Ep(0x81), &[0u8; 8], 8)
            .expect("send")
            .discard()
            .expect("drain");
        assert!(spy.fast_used, "command() must use the ordinary deadline");
        assert!(!spy.slow_used);

        let mut spy = Spy::default();
        command_slow(&mut spy, Ep(0x01), Ep(0x81), &[0u8; 8], 8)
            .expect("send")
            .discard()
            .expect("drain");
        assert!(spy.slow_used, "command_slow() must use the long deadline");
        assert!(!spy.fast_used);
    }

    /// `recv_slow` defaults to `recv`, so a transport with no deadlines (mocks,
    /// replay fixtures) keeps working without implementing it.
    #[test]
    fn recv_slow_defaults_to_recv() {
        struct Minimal(bool);
        impl Transport for Minimal {
            fn send(&mut self, _e: Ep, _d: &[u8]) -> Result<()> {
                Ok(())
            }
            fn recv(&mut self, _e: Ep, len: usize) -> Result<Vec<u8>> {
                self.0 = true;
                Ok(vec![0; len])
            }
            fn link_speed(&self) -> LinkSpeed {
                LinkSpeed::High
            }
            fn reset(&mut self) -> Result<()> {
                Ok(())
            }
        }
        let mut m = Minimal(false);
        assert_eq!(m.recv_slow(Ep(0x81), 4).expect("recv"), vec![0u8; 4]);
        assert!(m.0, "the default must fall through to recv");
    }
}
