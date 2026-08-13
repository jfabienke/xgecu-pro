//! Chip and image data types — the C `device_t`, plus the value types the
//! capability traits pass around. These are plain data, never traits; with the
//! default-on `serde` feature they derive `Serialize` for the JSON mode.

use std::fmt;

use crate::error::FwVersion;

/// Which memory space of a chip an operation targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind { Code, Data, Data2, Config, User }

/// A contiguous span within a memory space.
#[derive(Clone, Copy, Debug)]
pub struct Region {
    pub kind: MemoryKind,
    pub offset: u64,
    pub len: u64,
}

impl Region {
    /// The full code region of a device.
    pub fn code(dev: &Device) -> Region {
        Region { kind: MemoryKind::Code, offset: 0, len: dev.code_size }
    }
}

/// One block-sized request, as the driver-level `*_block` ops consume.
#[derive(Clone, Copy, Debug)]
pub struct BlockReq {
    pub kind: MemoryKind,
    pub address: u64,
    pub len: u32,
}

/// What an erase should target.
#[derive(Clone, Copy, Debug)]
pub enum EraseKind { Chip, Sector { address: u64 }, Fuses }

/// Which fuse/config space a fuse op targets (C `MP_FUSE_USER/CFG/LOCK`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FuseKind { User, Config, Lock }

/// eMMC hardware partition (T76 `--partition`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Partition { User, Boot1, Boot2, Rpmb }

/// A read chip identity (manufacturer + device bytes).
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ChipId {
    pub raw: u32,
    pub bytes: u8,
}

/// The FPGA bitstream for one algorithm (T76). Lazily decompressed at use.
#[derive(Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Algorithm {
    pub name: String,
    /// Inflated bitstream bytes (empty until loaded). Never serialized — the
    /// JSON mode reports metadata, not megabytes of bitstream.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub bitstream: Vec<u8>,
}

/// Compact `Debug` — the bitstream is megabytes; print its length, not bytes.
impl fmt::Debug for Algorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Algorithm")
            .field("name", &self.name)
            .field("bitstream_len", &self.bitstream.len())
            .finish()
    }
}

/// Socket package, e.g. `DIP28`, `SOIC8`.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Package {
    pub pin_count: u8,
    pub name: String,
}

/// A chip descriptor from the database, mirroring the C `device_t` fields the
/// T76 transaction packers consume (`t76.c:503-848`). Plain data.
///
/// Fields absent from a given `infoic.xml` entry stay 0 (the C parser's
/// behaviour); `i2c_address`/`spi_clock`/`icsp` are runtime-adjustable knobs
/// that default to 0 and may be overwritten by CLI options before `begin()`.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Device {
    pub name: String,
    pub protocol_id: u8,
    /// C `variant`: low byte adapter/geometry code, high byte algorithm number.
    pub variant: u16,
    pub code_size: u64,
    pub data_size: u64,
    /// C `data_memory2_size` (second data/user space, e.g. PIC user IDs).
    pub data_memory2_size: u16,
    pub page_size: u32,
    pub chip_id: u32,
    pub chip_id_bytes: u8,
    /// C `voltages.raw_voltages`: packed VDD/VCC/VPP nibbles + option bits.
    pub raw_voltages: u32,
    /// C `chip_info` (voltage-adjust class / PIC word width selector).
    pub chip_info: u8,
    /// C `pin_map` (T48/T56/T76 databases only).
    pub pin_map: u8,
    /// C `pulse_delay` in microseconds.
    pub pulse_delay: u16,
    /// C `read_buffer_size` in bytes.
    pub read_buffer_size: u16,
    /// C `write_buffer_size`: NAND page + spare bytes; write chunk otherwise.
    pub write_buffer_size: u16,
    /// C `pages_per_block` (NAND geometry; flag bits already stripped).
    pub pages_per_block: u16,
    /// C `flags.raw_flags` verbatim (erase/id/data-org… bit soup).
    pub raw_flags: u32,
    /// C `package_details.packed_package` verbatim (pin count in bits 24..30).
    pub packed_package: u32,
    /// C `package_details.icsp` byte (bits 8..16 of `package_details`).
    pub icsp: u8,
    /// Slave address for 24C-class devices (runtime-adjustable, default 0).
    pub i2c_address: u8,
    /// SPI clock tier for 25C-class devices (runtime-adjustable, default 0).
    pub spi_clock: u8,
    pub package: Package,
    pub algorithm: Option<Algorithm>,
    /// The device firmware these bitstreams pair with (see [`FwVersion`]).
    pub fw_target: FwVersion,
    /// Logic-IC test vectors: `vector_count` rows of `package.pin_count` bytes,
    /// each a state code (C `LOGIC_*`: 0/1/L/H/C/Z/X/G/V). Empty for non-logic
    /// devices.
    pub vectors: Vec<u8>,
    /// Number of logic-test vector rows (C `vector_count`).
    pub vector_count: u16,
    /// VCC selector for the logic test (C `voltages.vcc`).
    pub logic_vcc: u8,
}

/// An in-memory dump/image plus the metadata the JSON `Outcome` reports.
#[derive(Clone, Debug, Default)]
pub struct Image {
    pub bytes: Vec<u8>,
}

impl Image {
    pub fn len(&self) -> usize { self.bytes.len() }
    pub fn is_empty(&self) -> bool { self.bytes.is_empty() }
}
