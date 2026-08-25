// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! The central [`Programmer`] trait — a thin always-present core plus
//! object-safe accessor upcasts to the optional capability traits in
//! [`crate::caps`]. It is a capability-based vtable: a driver implements only
//! the capabilities its hardware supports.

use crate::caps::{
    BitstreamLoad, Calibration, EmmcOps, FirmwareUpdate, FuseOps, JedecOps, LogicTest, MemoryOps,
    PinTest, Protect, SpiAutodetect,
};
use crate::device::{ChipId, Device};
use crate::error::{FwVersion, Result};
use crate::transport::LinkSpeed;

bitflags::bitflags! {
    /// Which optional capabilities a programmer exposes. Must agree with which
    /// accessor methods on [`Programmer`] return `Some` (checked in tests).
    ///
    /// ```
    /// use minipro_core::programmer::Caps;
    /// let c = Caps::MEMORY | Caps::FUSES;
    /// assert!(c.contains(Caps::MEMORY));
    /// assert!(!c.contains(Caps::JEDEC));
    /// assert_eq!(Caps::default(), Caps::empty());
    /// ```
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Caps: u16 {
        const MEMORY = 1 << 0;
        const FUSES = 1 << 1;
        const JEDEC = 1 << 2;
        const EMMC = 1 << 3;
        const LOGIC = 1 << 4;
        const PINTEST = 1 << 5;
        const FWUPDATE = 1 << 6;
        const CALIBRATION = 1 << 7;
        const PROTECT = 1 << 8;
        const AUTODETECT = 1 << 9;
    }
}

/// Identity/status of the attached programmer (the queryable identity fields).
#[derive(Clone, Debug)]
pub struct ProgrammerInfo {
    pub model: String,
    pub firmware: FwVersion,
    pub serial: String,
    /// Manufacture date string from the system-info report (C `mfg_date`).
    pub mfg_date: String,
    /// Device code from the system-info report (C `device_code`).
    pub device_code: String,
    pub link: LinkSpeed,
    pub voltage: f32,
    /// The device is running its bootloader rather than normal firmware
    /// (system-info firmware field is zero) — a firmware update was
    /// interrupted or never completed. Normal operations will not work;
    /// `minipro update` can finish the job.
    pub bootloader: bool,
}

/// Per-transaction state returned by [`Programmer::begin`]: the selected
/// algorithm handle, eMMC capacity, current partition, etc. An opaque token the
/// capability ops borrow; consumed by [`Programmer::end`].
///
/// `Default` exists so [`Txn`] can move the session out on drop without
/// modelling an "already taken" state that callers would have to handle.
#[derive(Debug, Default)]
pub struct Session {
    pub device: Device,
    pub emmc_capacity: u64,
}

/// A chip programmer. Object-safe and `Send` (the TUI runs ops off-thread).
pub trait Programmer: Send {
    /// Identity/status of the attached hardware.
    fn info(&self) -> &ProgrammerInfo;
    /// Which optional capabilities this programmer supports.
    fn caps(&self) -> Caps;

    /// Open a transaction for `dev`: upload the bitstream, init the adapter,
    /// configure the socket pin-drivers. On the T76 this is where the FPGA and
    /// firmware-version coupling live.
    fn begin(&mut self, dev: &Device) -> Result<Session>;
    /// Close the transaction and de-energize the socket.
    fn end(&mut self, session: Session) -> Result<()>;
    /// Read the chip's electronic id (autoselect/ONFI/etc.).
    fn identify(&mut self, s: &Session) -> Result<ChipId>;
    /// Hard reset / re-arm.
    fn reset(&mut self) -> Result<()>;

    // ---- Capability upcasts -------------------------------------------------
    // Default `None`; a driver overrides each supported one with `Some(self)`.
    // This is what keeps `dyn Programmer` object-safe while exposing optional,
    // heterogeneous behaviour.
    fn memory(&mut self) -> Option<&mut dyn MemoryOps> {
        None
    }
    fn fuses(&mut self) -> Option<&mut dyn FuseOps> {
        None
    }
    fn jedec(&mut self) -> Option<&mut dyn JedecOps> {
        None
    }
    fn emmc(&mut self) -> Option<&mut dyn EmmcOps> {
        None
    }
    fn logic(&mut self) -> Option<&mut dyn LogicTest> {
        None
    }
    fn pins(&mut self) -> Option<&mut dyn PinTest> {
        None
    }
    fn firmware(&mut self) -> Option<&mut dyn FirmwareUpdate> {
        None
    }
    fn calibration(&mut self) -> Option<&mut dyn Calibration> {
        None
    }
    fn protect(&mut self) -> Option<&mut dyn Protect> {
        None
    }
    fn autodetect(&mut self) -> Option<&mut dyn SpiAutodetect> {
        None
    }
    fn bitstream(&mut self) -> Option<&mut dyn BitstreamLoad> {
        None
    }
}

/// RAII transaction guard: ends the transaction on drop (best-effort), so a
/// forgotten `end` can't leave the socket energized. Convenience over the raw
/// `begin`/`end` pair; lives in core, not in the trait, to keep the trait simple.
pub struct Txn<'p> {
    prog: &'p mut dyn Programmer,
    session: Session,
}

impl<'p> Txn<'p> {
    /// Begin a guarded transaction.
    pub fn begin(prog: &'p mut dyn Programmer, dev: &Device) -> Result<Txn<'p>> {
        let session = prog.begin(dev)?;
        Ok(Txn { prog, session })
    }
    /// Access the programmer and session together for an operation. Total: the
    /// session is owned outright, so there is no "already ended" case.
    pub fn parts(&mut self) -> (&mut dyn Programmer, &Session) {
        (self.prog, &self.session)
    }
}

impl Drop for Txn<'_> {
    fn drop(&mut self) {
        // By design: `end()` here is a best-effort de-energize on scope exit,
        // not the operation's result. Any real failure surfaces on the op
        // path (read/write/erase return their own `Result`); a `Drop` can't
        // propagate, and the transaction is ending regardless, so there is
        // nothing actionable to recover from an `end()` error here.
        let _ = self.prog.end(std::mem::take(&mut self.session));
    }
}
