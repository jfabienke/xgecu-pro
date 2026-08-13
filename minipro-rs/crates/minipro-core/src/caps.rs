//! Optional capability traits. A driver implements the subset its hardware
//! supports and exposes each via the accessor upcasts on
//! [`crate::programmer::Programmer`]. All are object-safe (no generics, no
//! by-value `Self`), block-granular — the
//! loop/progress/verification live in [`crate::ops`], written once.

use crate::device::{BlockReq, EraseKind, FuseKind, MemoryKind, Partition, Region};
use crate::error::Result;
use crate::programmer::Session;

/// Fallback block size when a device declares no page size.
pub const DEFAULT_BLOCK: u32 = 4096;

/// Read/write/erase of a chip's memory spaces.
pub trait MemoryOps {
    fn read_block(&mut self, s: &Session, req: &BlockReq) -> Result<Vec<u8>>;
    fn write_block(&mut self, s: &Session, req: &BlockReq, data: &[u8]) -> Result<()>;
    fn erase(&mut self, s: &Session, kind: EraseKind) -> Result<()>;
    fn blank_check(&mut self, s: &Session, region: Region) -> Result<bool>;

    /// The transfer unit the block loop should step by for `kind`. The default
    /// is the device page size (or [`DEFAULT_BLOCK`]) — correct for byte- and
    /// page-addressed memory. Drivers whose hardware needs an operation-specific
    /// unit override this: e.g. the T76 streams one NAND *erase block*
    /// (page+spare × pages/block) or one eMMC 64 KiB unit per request, and
    /// derives its block index from that size — so feeding it page-sized
    /// requests would mis-address and short-read.
    fn block_size(&self, s: &Session, _kind: MemoryKind) -> u32 {
        match s.device.page_size {
            0 => DEFAULT_BLOCK,
            n => n,
        }
    }
}

/// MCU fuse/config/lock bits. `length` is how many bytes to read back;
/// `items_count` is the fuse-item count the firmware needs (both come from the
/// device's fuse-config profile, not the chip DB entry).
pub trait FuseOps {
    fn read_fuses(
        &mut self,
        s: &Session,
        kind: FuseKind,
        length: usize,
        items_count: u8,
    ) -> Result<Vec<u8>>;
    fn write_fuses(
        &mut self,
        s: &Session,
        kind: FuseKind,
        items_count: u8,
        data: &[u8],
    ) -> Result<()>;
}

/// PLD/GAL JEDEC fuse rows. `size` is the row width in bits (the payload is
/// `(size + 7) / 8` bytes); `flags` selects the row bank (C `jedec_set_t`).
pub trait JedecOps {
    fn read_row(&mut self, s: &Session, row: u8, flags: u8, size: u16) -> Result<Vec<u8>>;
    fn write_row(&mut self, s: &Session, row: u8, flags: u8, size: u16, data: &[u8])
        -> Result<()>;
}

/// Software write-protect toggle (PROTECT_ON/OFF).
pub trait Protect {
    fn protect_on(&mut self, s: &Session) -> Result<()>;
    fn protect_off(&mut self, s: &Session) -> Result<()>;
}

/// SPI 25-series autodetect: probe the socket and return the raw JEDEC id.
/// `wide` selects the 16-pin (vs 8-pin) package profile. FPGA drivers upload the
/// `SPI25F*` bitstream first, so `spi_autodetect` is handed a [`LoadBitstream`].
pub trait SpiAutodetect {
    fn spi_autodetect(&mut self, wide: bool, load: LoadBitstream<'_>) -> Result<u32>;
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

/// Fetches a named FPGA bitstream (decoded bytes) on demand. The CLI backs this
/// with the chip DB ([`crate`]-external `ChipDb::load_algorithm_named`). The FPGA
/// logic-test and autodetect ops call it for their *utility* algorithms
/// (`TestLgcPull`, `TTL1`, `SPI25F11`, …), which are chosen at op time rather
/// than tied to a chip — loaded per op/pass rather than per chip.
/// Fixed-silicon drivers (T48) ignore it.
pub type LoadBitstream<'a> = &'a mut dyn FnMut(&str) -> Result<Vec<u8>>;

/// Logic-IC (74xx/40xx) functional test. On the FPGA programmers each pass needs
/// a utility bitstream uploaded first, so `run` is handed a [`LoadBitstream`].
pub trait LogicTest {
    fn run(&mut self, s: &Session, load: LoadBitstream<'_>) -> Result<bool>;
}

/// Flashing a vendor firmware update (transport only; payload stays opaque).
pub trait FirmwareUpdate {
    fn update(&mut self, image: &[u8]) -> Result<()>;
}

/// Read the programmer's factory calibration bytes (TL866A/II+, T56 — not the
/// T48 or T76). `len` is how many bytes the caller expects back.
pub trait Calibration {
    fn read_calibration(&mut self, len: usize) -> Result<Vec<u8>>;
}
