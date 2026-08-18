// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! Chip and image data types, plus the value types the
//! capability traits pass around. These are plain data, never traits; with the
//! default-on `serde` feature they derive `Serialize` for the JSON mode.

use std::fmt;

use crate::error::FwVersion;

/// Which memory space of a chip an operation targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    Code,
    Data,
    Data2,
    Config,
    User,
}

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
        Region {
            kind: MemoryKind::Code,
            offset: 0,
            len: dev.code_size,
        }
    }
}

/// One block-sized request, as the driver-level `*_block` ops consume.
#[derive(Clone, Copy, Debug)]
pub struct BlockReq {
    pub kind: MemoryKind,
    pub address: u64,
    pub len: u32,
    /// First block of the region: the driver announces the whole transfer once,
    /// then streams blocks. The T76 will not program without it — a per-block
    /// init restarts the setup 2048 times and no cell is ever written.
    pub init: bool,
    /// Total blocks in this region, carried on the `init` packet so the device
    /// knows how much is coming. Meaningless when `init` is false.
    pub block_count: u32,
}

impl BlockReq {
    /// A one-block transfer that is its own init — the shape most call sites
    /// (fuses, single-shot reads, tests) actually want.
    pub fn single(kind: MemoryKind, address: u64, len: u32) -> Self {
        BlockReq {
            kind,
            address,
            len,
            init: true,
            block_count: 1,
        }
    }
}

/// Capability bits packed into the C `flags.raw_flags` word.
pub mod flags {
    /// The chip supports an electrical erase. **Clear for one-time-programmable
    /// parts**: an EPROM in a windowless plastic package has no erase path at
    /// all — its cells only return to 1 under UV, through a quartz window it
    /// does not have.
    pub const CAN_ERASE: u32 = 0x0000_0010;
    /// The chip reports an electronic id.
    pub const HAS_CHIP_ID: u32 = 0x0000_0020;
}

/// What an erase should target.
#[derive(Clone, Copy, Debug)]
pub enum EraseKind {
    Chip,
    Sector { address: u64 },
    Fuses,
}

/// Which fuse/config space a fuse op targets (C `MP_FUSE_USER/CFG/LOCK`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FuseKind {
    User,
    Config,
    Lock,
}

/// Device-class codes (C `chip_type`, the XML `type` attribute / DLL
/// `desc[0x08]`), carried on [`Device::chip_type`].
pub mod chip_type {
    pub const MEMORY: u8 = 0x01;
    pub const MCU: u8 = 0x02;
    pub const PLD: u8 = 0x03;
    pub const SRAM: u8 = 0x04;
    pub const LOGIC: u8 = 0x05;
    pub const NAND: u8 = 0x06;
    pub const EMMC: u8 = 0x07;
    /// In-system programming through a display's VGA connector: the monitor's
    /// EDID EEPROM over the DDC/I²C pins, and MStar scaler firmware in an
    /// external SPI flash. A transport of its own, not a socket operation —
    /// and not implemented here (nor in the C tool, which rejects it too).
    pub const VGA: u8 = 0x08;
}

/// eMMC hardware partition (T76 `--partition`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Partition {
    User,
    Boot1,
    Boot2,
    Rpmb,
}

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

/// A chip descriptor from the database — the fields the T76 transaction
/// packers consume. Plain data.
///
/// Fields absent from a given `infoic.xml` entry stay 0;
/// `i2c_address`/`spi_clock`/`icsp` are runtime-adjustable knobs
/// that default to 0 and may be overwritten by CLI options before `begin`.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Device {
    pub name: String,
    pub protocol_id: u8,
    /// Device class (C `chip_type`): [`chip_type::MEMORY`]/`MCU`/`PLD`/`SRAM`/
    /// `LOGIC`. Drives class-specific routing (e.g. logic vs memory).
    pub chip_type: u8,
    /// Erased-cell value (C `blank_value`, default `0xFF`). `0x00` for parts
    /// that erase low; used by blank-check and the write-pad so verify matches
    /// the real erased state, not an assumed `0xFF`.
    pub blank_value: u8,
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

impl Device {
    /// Whether this part can be erased at all — see [`flags::CAN_ERASE`].
    pub fn can_erase(&self) -> bool {
        self.raw_flags & flags::CAN_ERASE != 0
    }
    /// Whether this part reports an electronic id — see [`flags::HAS_CHIP_ID`].
    pub fn has_chip_id(&self) -> bool {
        self.raw_flags & flags::HAS_CHIP_ID != 0
    }
}

/// An in-memory dump/image plus the metadata the JSON `Outcome` reports.
#[derive(Clone, Debug, Default)]
pub struct Image {
    pub bytes: Vec<u8>,
}

impl Image {
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

#[cfg(test)]
mod flag_tests {
    use super::*;

    /// Real `raw_flags` from the vendor database. Attempting an erase on a
    /// one-time-programmable part is not merely futile — it energizes the
    /// socket and runs an algorithm the chip was never meant to see.
    #[test]
    fn otp_parts_are_not_erasable() {
        let otp = |f| Device {
            raw_flags: f,
            ..Default::default()
        };
        // MX27C2000@DIP32 and AT27C256R@DIP28 both read 0x68 - bit 4 clear.
        assert!(
            !otp(0x0000_0068).can_erase(),
            "OTP EPROM must not be erasable"
        );
        // ...but they do report an electronic id (bit 5 set), which is how the
        // MX27C2000 identifies as 0xc220.
        assert!(otp(0x0000_0068).has_chip_id());
        // W25Q128BV@SOIC8 reads 0x504278 - bit 4 set.
        assert!(otp(0x0050_4278).can_erase(), "flash must stay erasable");
    }
}
