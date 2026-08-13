//! The central [`Programmer`] trait — a thin always-present core plus
//! object-safe accessor upcasts to the optional capability traits in
//! [`crate::caps`]. It is a capability-based vtable: a driver implements only
//! the capabilities its hardware supports.

use crate::caps::{
    Calibration, EmmcOps, FirmwareUpdate, FuseOps, JedecOps, LogicTest, MemoryOps, PinTest,
    Protect, SpiAutodetect,
};
use crate::device::{ChipId, Device};
use crate::error::{FwVersion, Result};
use crate::transport::LinkSpeed;

/// Which optional capabilities a programmer exposes. Must agree with which
/// accessor methods on [`Programmer`] return `Some` (checked in tests).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps(pub u16);

impl Caps {
    pub const MEMORY: Caps = Caps(1 << 0);
    pub const FUSES: Caps = Caps(1 << 1);
    pub const JEDEC: Caps = Caps(1 << 2);
    pub const EMMC: Caps = Caps(1 << 3);
    pub const LOGIC: Caps = Caps(1 << 4);
    pub const PINTEST: Caps = Caps(1 << 5);
    pub const FWUPDATE: Caps = Caps(1 << 6);
    pub const CALIBRATION: Caps = Caps(1 << 7);
    pub const PROTECT: Caps = Caps(1 << 8);
    pub const AUTODETECT: Caps = Caps(1 << 9);

    pub fn contains(self, c: Caps) -> bool {
        self.0 & c.0 == c.0
    }
    pub fn with(self, c: Caps) -> Caps {
        Caps(self.0 | c.0)
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
}

/// Per-transaction state returned by [`Programmer::begin`]: the selected
/// algorithm handle, eMMC capacity, current partition, etc. An opaque token the
/// capability ops borrow; consumed by [`Programmer::end`].
#[derive(Debug)]
pub struct Session {
    pub device: Device,
    pub emmc_capacity: u64,
    // (algorithm handle / partition state live here in the real impl)
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
}

/// RAII transaction guard: ends the transaction on drop (best-effort), so a
/// forgotten `end` can't leave the socket energized. Convenience over the raw
/// `begin`/`end` pair; lives in core, not in the trait, to keep the trait simple.
pub struct Txn<'p> {
    prog: &'p mut dyn Programmer,
    session: Option<Session>,
}

impl<'p> Txn<'p> {
    /// Begin a guarded transaction.
    pub fn begin(prog: &'p mut dyn Programmer, dev: &Device) -> Result<Txn<'p>> {
        let session = prog.begin(dev)?;
        Ok(Txn {
            prog,
            session: Some(session),
        })
    }
    /// Access the programmer and session together for an operation.
    pub fn parts(&mut self) -> (&mut dyn Programmer, &Session) {
        (self.prog, self.session.as_ref().expect("session live"))
    }
}

impl Drop for Txn<'_> {
    fn drop(&mut self) {
        if let Some(s) = self.session.take() {
            // By design: `end()` here is a best-effort de-energize on scope exit,
            // not the operation's result. Any real failure surfaces on the op
            // path (read/write/erase return their own `Result`); a `Drop` can't
            // propagate, and the transaction is ending regardless, so there is
            // nothing actionable to recover from an `end()` error here.
            let _ = self.prog.end(s);
        }
    }
}
