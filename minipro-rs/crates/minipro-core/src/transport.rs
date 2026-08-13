// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! The USB command channel abstraction, and the must-drain response guard.
//!
//! Real impls live in `minipro-usb` (over `nusb`) and a `MockTransport` that
//! replays capture fixtures. Drivers only ever see `&mut dyn Transport`, which
//! is what makes the protocol unit-testable without hardware.

use crate::error::Result;

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
    /// Currently negotiated link speed.
    fn link_speed(&self) -> LinkSpeed;
    /// Reset the device / re-arm the endpoints.
    fn reset(&mut self) -> Result<()>;
}

/// A sent command whose response has *not* yet been read.
///
/// The T76 wedges (needs a USB replug) if a command's response is left
/// undrained, so this guard is `#[must_use]` and can only be resolved by
/// consuming it — turning "forgot to read EP81" into a compile-time warning.
#[must_use = "the T76 wedges until USB replug if this response is not drained"]
pub struct Pending<'t> {
    tx: &'t mut dyn Transport,
    ep: Ep,
    len: usize,
}

impl<'t> Pending<'t> {
    /// Read and return the response payload.
    pub fn read(self) -> Result<Vec<u8>> {
        self.tx.recv(self.ep, self.len)
    }
    /// Drain and discard the response (still required — the device must be read).
    pub fn discard(self) -> Result<()> {
        self.tx.recv(self.ep, self.len).map(|_| ())
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
    })
}
