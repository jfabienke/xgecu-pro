//! Shared protocol primitives for the "II+-class" XGecu programmers — the
//! TL866II+, T48, T56, and T76 all speak the same command generation: the same
//! opcode space (BEGIN_TRANS `0x03`, END `0x04`, READID `0x05`, WRITE/READ CODE
//! `0x0c`/`0x0d`, ERASE `0x0e`, REQUEST_STATUS `0x39`), and a **byte-identical
//! 64-byte BEGIN_TRANS header** (`t48.c:254-298`, `t56.c:181-222`,
//! `t76.c:517-565`). The T76 additionally appends a chip-class extension block
//! in bytes `0x40..0x7f`; that stays in the driver, on top of [`pack_begin64`].
//!
//! Extracted from the T76 driver so the T56/T48/TL866II+ ports reuse it rather
//! than re-deriving the packing. The single source of the "magic length per
//! opcode" and field-offset knowledge lives here.

use minipro_core::device::Device;
use minipro_core::error::{Error, Result};

// ---------------------------------------------------------------------------
// II+-family opcodes shared across T48/T56/T76 (t76.c:34-93 and siblings).
// ---------------------------------------------------------------------------
/// BEGIN_TRANS — load the chip profile / open a transaction.
pub const CMD_BEGIN_TRANS: u8 = 0x03;
/// END_TRANS — close the transaction, de-energize the socket.
pub const CMD_END_TRANS: u8 = 0x04;
/// READID — read the chip's electronic id.
pub const CMD_READID: u8 = 0x05;
/// WRITE_CODE — program a code-memory block.
pub const CMD_WRITE_CODE: u8 = 0x0c;
/// READ_CODE — read a code-memory block.
pub const CMD_READ_CODE: u8 = 0x0d;
/// ERASE — erase (args are family-specific).
pub const CMD_ERASE: u8 = 0x0e;
/// REQUEST_STATUS — read the overcurrent / status packet.
pub const CMD_REQUEST_STATUS: u8 = 0x39;

// ---------------------------------------------------------------------------
// Little-endian field writers — the Rust form of the C
// `format_int(.., MP_LITTLE_ENDIAN)`.
// ---------------------------------------------------------------------------

/// Store a u16 little-endian at `off`.
pub(crate) fn le16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

/// Store a u32 little-endian at `off`.
pub(crate) fn le32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

/// Decode a fixed-width ASCII report field (NUL/space padded) into a String.
pub(crate) fn ascii_field(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

/// The `device_t` fields consumed by the II+-family packet packers
/// (`t76.c:503-848` and the T48/T56 equivalents). `from_device` copies them 1:1
/// from the core [`Device`], with sizes clamped to the wire widths.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChipParams {
    pub protocol_id: u8,
    pub variant: u16,
    pub icsp: u8,
    pub raw_voltages: u32,
    pub chip_info: u8,
    pub pin_map: u8,
    pub data_memory_size: u16,
    pub page_size: u16,
    pub pulse_delay: u16,
    pub data_memory2_size: u16,
    pub code_memory_size: u32,
    pub i2c_address: u8,
    pub spi_clock: u8,
    pub packed_package: u32,
    pub read_buffer_size: u16,
    pub raw_flags: u32,
    /// NAND page + spare bytes (C `write_buffer_size`).
    pub write_buffer_size: u16,
    pub pages_per_block: u16,
}

impl ChipParams {
    pub fn from_device(dev: &Device) -> ChipParams {
        ChipParams {
            protocol_id: dev.protocol_id,
            variant: dev.variant,
            icsp: dev.icsp,
            raw_voltages: dev.raw_voltages,
            chip_info: dev.chip_info,
            pin_map: dev.pin_map,
            data_memory_size: dev.data_size.min(u64::from(u16::MAX)) as u16,
            page_size: dev.page_size.min(u32::from(u16::MAX)) as u16,
            pulse_delay: dev.pulse_delay,
            data_memory2_size: dev.data_memory2_size,
            code_memory_size: dev.code_size.min(u64::from(u32::MAX)) as u32,
            i2c_address: dev.i2c_address,
            spi_clock: dev.spi_clock,
            packed_package: dev.packed_package,
            read_buffer_size: dev.read_buffer_size,
            raw_flags: dev.raw_flags,
            write_buffer_size: dev.write_buffer_size,
            pages_per_block: dev.pages_per_block,
        }
    }

    /// NAND geometry is required for the 0x02 prelude and the 0x1f program
    /// path; a database entry without it cannot be driven.
    pub(crate) fn nand_geometry(&self) -> Result<(u16, u16)> {
        if self.write_buffer_size == 0 || self.pages_per_block == 0 {
            return Err(Error::Unsupported(
                "NAND geometry (write_buffer_size/pages_per_block) missing from the chip DB entry",
            ));
        }
        Ok((self.write_buffer_size, self.pages_per_block))
    }
}

/// Pack the shared 64-byte BEGIN_TRANS header (`t76.c:516-565`, byte-identical
/// on T48/T56). The T76 driver copies this into a 128-byte buffer and appends
/// its chip-class extension in `0x40..0x7f`; a fixed-silicon sibling (T48,
/// TL866II+) sends exactly these 64 bytes.
pub(crate) fn pack_begin64(p: &ChipParams) -> [u8; 64] {
    let mut msg = [0u8; 64];

    msg[0] = CMD_BEGIN_TRANS; // t76.c:517
    msg[1] = p.protocol_id; // t76.c:518
    msg[2] = p.variant as u8; // t76.c:519
    msg[3] = p.icsp; // t76.c:520
    le16(&mut msg, 4, p.raw_voltages as u16); // t76.c:522
    msg[6] = p.chip_info; // t76.c:524
    msg[7] = p.pin_map; // t76.c:525
    le16(&mut msg, 8, p.data_memory_size); // t76.c:526
    le16(&mut msg, 10, p.page_size); // t76.c:528
    le16(&mut msg, 12, p.pulse_delay); // t76.c:529
    le16(&mut msg, 14, p.data_memory2_size); // t76.c:531
    le32(&mut msg, 16, p.code_memory_size); // t76.c:533

    msg[20] = (p.raw_voltages >> 16) as u8; // t76.c:536
    if (p.raw_voltages & 0xf0) == 0xf0 {
        msg[22] = p.raw_voltages as u8; // t76.c:539
    } else {
        msg[21] = p.raw_voltages as u8 & 0x0f; // t76.c:541
        msg[22] = p.raw_voltages as u8 & 0xf0; // t76.c:542
    }
    if p.raw_voltages & 0x8000_0000 != 0 {
        msg[22] = ((p.raw_voltages >> 16) & 0x0f) as u8; // t76.c:545
    }

    // I2C address / SPI clock (t76.c:548-555). The C guards these behind
    // `can_adjust_address` / `can_adjust_clock` flags; a device without the
    // capability carries 0 in the field, so copying is equivalent.
    msg[24] = p.i2c_address;
    msg[28] = p.spi_clock;

    le32(&mut msg, 40, p.packed_package); // t76.c:557
    le16(&mut msg, 44, p.read_buffer_size); // t76.c:559
    le32(&mut msg, 56, p.raw_flags); // t76.c:561
    msg[63] = (p.variant >> 8) as u8; // t76.c:565 — algorithm number

    msg
}
