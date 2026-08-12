//! Optional capability traits. A driver implements the subset its hardware
//! supports and exposes each via the accessor upcasts on
//! [`crate::programmer::Programmer`]. All are object-safe (no generics, no
//! by-value `Self`), block-granular like the C `*_block` functions — the
//! loop/progress/verification live in [`crate::ops`], written once.

use crate::device::{BlockReq, EraseKind, Partition, Region};
use crate::error::Result;
use crate::programmer::Session;

/// Read/write/erase of a chip's memory spaces.
pub trait MemoryOps {
    fn read_block(&mut self, s: &Session, req: &BlockReq) -> Result<Vec<u8>>;
    fn write_block(&mut self, s: &Session, req: &BlockReq, data: &[u8]) -> Result<()>;
    fn erase(&mut self, s: &Session, kind: EraseKind) -> Result<()>;
    fn blank_check(&mut self, s: &Session, region: Region) -> Result<bool>;
}

/// MCU fuse/config/lock bits.
pub trait FuseOps {
    fn read_fuses(&mut self, s: &Session, kind: u8) -> Result<Vec<u8>>;
    fn write_fuses(&mut self, s: &Session, kind: u8, data: &[u8]) -> Result<()>;
}

/// PLD/GAL JEDEC fuse rows.
pub trait JedecOps {
    fn read_row(&mut self, s: &Session, row: u8) -> Result<Vec<u8>>;
    fn write_row(&mut self, s: &Session, row: u8, data: &[u8]) -> Result<()>;
}

/// eMMC-specific operations (T76).
pub trait EmmcOps {
    fn select_partition(&mut self, s: &Session, part: Partition) -> Result<()>;
    /// Real capacity from EXT_CSD SEC_COUNT; 64-bit because eMMC exceeds u32.
    fn capacity(&self) -> u64;
}

/// Socket pin-contact check. Advisory: on oxidized vintage pins this can report
/// bad contact while a read still succeeds (see the AHA-1542CP episode).
pub trait PinTest {
    fn contact_check(&mut self, s: &Session) -> Result<Vec<u8>>; // returns open pins
}

/// Logic-IC (74xx/40xx) functional test.
pub trait LogicTest {
    fn run(&mut self, s: &Session) -> Result<bool>;
}

/// Flashing a vendor firmware update (transport only; payload stays opaque).
pub trait FirmwareUpdate {
    fn update(&mut self, image: &[u8]) -> Result<()>;
}
