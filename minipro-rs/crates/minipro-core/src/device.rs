//! Chip and image data types — the C `device_t`, plus the value types the
//! capability traits pass around. These are plain data (`#[derive(Deserialize)]`
//! in the real crate), never traits.

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

/// eMMC hardware partition (T76 `--partition`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Partition { User, Boot1, Boot2, Rpmb }

/// A read chip identity (manufacturer + device bytes).
#[derive(Clone, Copy, Debug)]
pub struct ChipId {
    pub raw: u32,
    pub bytes: u8,
}

/// The FPGA bitstream for one algorithm (T76). Lazily decompressed at use.
#[derive(Clone)]
pub struct Algorithm {
    pub name: String,
    /// Inflated bitstream bytes (empty until loaded).
    pub bitstream: Vec<u8>,
}

/// Socket package, e.g. `DIP28`, `SOIC8`.
#[derive(Clone, Debug)]
pub struct Package {
    pub pin_count: u8,
    pub name: String,
}

/// A chip descriptor from the database (subset; the full struct mirrors the C
/// `device_t`). Plain data.
#[derive(Clone)]
pub struct Device {
    pub name: String,
    pub protocol_id: u8,
    pub code_size: u64,
    pub data_size: u64,
    pub page_size: u32,
    pub chip_id: u32,
    pub chip_id_bytes: u8,
    pub package: Package,
    pub algorithm: Option<Algorithm>,
    /// The device firmware these bitstreams pair with (see [`FwVersion`]).
    pub fw_target: FwVersion,
}

/// An in-memory dump/image plus the metadata the JSON `Outcome` reports.
#[derive(Clone, Default)]
pub struct Image {
    pub bytes: Vec<u8>,
}

impl Image {
    pub fn len(&self) -> usize { self.bytes.len() }
    pub fn is_empty(&self) -> bool { self.bytes.is_empty() }
}
