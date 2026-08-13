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

use minipro_core::device::{Device, FuseKind};
use minipro_core::error::{Error, Result};
use minipro_core::transport::{command, Ep, Transport};

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

    // SPI clock (t76.c:551-555 / t56.c:211-214). The C guards this behind
    // `can_adjust_clock`; a device without the capability carries 0 in the
    // field, so writing it unconditionally is equivalent and shared.
    msg[28] = p.spi_clock;

    le32(&mut msg, 40, p.packed_package); // t76.c:557 / t56.c:216
    le16(&mut msg, 44, p.read_buffer_size); // t76.c:559 / t56.c:219
    le32(&mut msg, 56, p.raw_flags); // t76.c:561 / t56.c:221

    // Deliberately NOT written here (verified T76-only against t56.c:181-224):
    //   msg[24] = i2c_address     — T76 sets it (t76.c:549); T56 never does.
    //   msg[63] = variant >> 8    — T76's algorithm number (t76.c:565); on the
    //                               T56 the algorithm is selected purely by the
    //                               uploaded bitstream, so msg[63] stays 0.
    // Each driver layers those on top of this shared subset.
    msg
}

// ---------------------------------------------------------------------------
// Shared firmware-mediated ops (fuses, JEDEC rows, protect, calibration).
//
// These are byte-identical across T48/T56/T76 and all run on the command
// endpoints EP01 OUT / EP81 IN (usb_nix.c:442-459) — no bulk plane, no FPGA
// bitstream. Implemented once here; each driver delegates. (Logic test and SPI
// autodetect are *not* here: on the FPGA drivers they require a utility-algorithm
// bitstream upload, so they stay per-driver.)
// ---------------------------------------------------------------------------
const EP_CMD_OUT: Ep = Ep(0x01);
const EP_CMD_IN: Ep = Ep(0x81);

// Fuse-space opcodes (t48.c/t56.c/t76.c :38-51).
const CMD_READ_USER: u8 = 0x06;
const CMD_WRITE_USER: u8 = 0x07;
const CMD_READ_CFG: u8 = 0x08;
const CMD_WRITE_CFG: u8 = 0x09;
const CMD_WRITE_LOCK: u8 = 0x14;
const CMD_READ_LOCK: u8 = 0x15;
const CMD_READ_CALIBRATION: u8 = 0x16;
const CMD_PROTECT_OFF: u8 = 0x18;
const CMD_PROTECT_ON: u8 = 0x19;
const CMD_READ_JEDEC: u8 = 0x1d;
const CMD_WRITE_JEDEC: u8 = 0x1e;

fn cmd(tx: &mut dyn Transport, pkt: &[u8], resp_len: usize) -> Result<Vec<u8>> {
    command(tx, EP_CMD_OUT, EP_CMD_IN, pkt, resp_len)?.read()
}

/// `*_read_fuses`: `[0]`=space opcode, `[1]`=protocol_id, `[2]`=items_count,
/// `[4..8]`=code_memory_size; send 8, recv 64, return `length` bytes from `[8]`.
pub(crate) fn fuse_read(
    tx: &mut dyn Transport,
    protocol_id: u8,
    code_size: u32,
    kind: FuseKind,
    length: usize,
    items_count: u8,
) -> Result<Vec<u8>> {
    let op = match kind {
        FuseKind::User => CMD_READ_USER,
        FuseKind::Config => CMD_READ_CFG,
        FuseKind::Lock => CMD_READ_LOCK,
    };
    let mut msg = [0u8; 8];
    msg[0] = op;
    msg[1] = protocol_id;
    msg[2] = items_count;
    le32(&mut msg, 4, code_size);
    let resp = cmd(tx, &msg, 64)?;
    if resp.len() < 8 + length {
        return Err(Error::Protocol);
    }
    Ok(resp[8..8 + length].to_vec())
}

/// `*_write_fuses`: as above but the address is `code_memory_size - 0x38` (a
/// preserved firmware-bug workaround) and the payload follows at `[8]`; a single
/// 64-byte send, no reply.
pub(crate) fn fuse_write(
    tx: &mut dyn Transport,
    protocol_id: u8,
    code_size: u32,
    kind: FuseKind,
    items_count: u8,
    data: &[u8],
) -> Result<()> {
    let op = match kind {
        FuseKind::User => CMD_WRITE_USER,
        FuseKind::Config => CMD_WRITE_CFG,
        FuseKind::Lock => CMD_WRITE_LOCK,
    };
    if data.len() > 56 {
        return Err(Error::Format("fuse payload exceeds 56 bytes".into()));
    }
    let mut msg = [0u8; 64];
    msg[0] = op;
    msg[1] = protocol_id;
    msg[2] = items_count;
    le32(&mut msg, 4, code_size.wrapping_sub(0x38)); // 0x38 firmware-bug offset
    msg[8..8 + data.len()].copy_from_slice(data);
    tx.send(EP_CMD_OUT, &msg)
}

/// `*_read_jedec_row`: `[0]`=0x1d, `[1]`=type byte, `[2]`=size, `[4]`=row,
/// `[5]`=flags; send 8, recv 32, return the first `(size + 7) / 8` bytes (from
/// `[0]`, not `[8]`).
pub(crate) fn jedec_read(
    tx: &mut dyn Transport,
    type_byte: u8,
    row: u8,
    flags: u8,
    size: u16,
) -> Result<Vec<u8>> {
    let mut msg = [0u8; 8];
    msg[0] = CMD_READ_JEDEC;
    msg[1] = type_byte;
    msg[2] = size as u8;
    msg[4] = row;
    msg[5] = flags;
    let resp = cmd(tx, &msg, 32)?;
    let nbytes = usize::from(size.div_ceil(8));
    if resp.len() < nbytes {
        return Err(Error::Protocol);
    }
    Ok(resp[..nbytes].to_vec())
}

/// `*_write_jedec_row`: the row payload at `[8]`, a single 64-byte send.
pub(crate) fn jedec_write(
    tx: &mut dyn Transport,
    type_byte: u8,
    row: u8,
    flags: u8,
    size: u16,
    data: &[u8],
) -> Result<()> {
    let nbytes = usize::from(size.div_ceil(8));
    if data.len() < nbytes || nbytes > 56 {
        return Err(Error::Format(format!(
            "JEDEC row needs {nbytes} bytes (size {size} bits), got {}",
            data.len()
        )));
    }
    let mut msg = [0u8; 64];
    msg[0] = CMD_WRITE_JEDEC;
    msg[1] = type_byte;
    msg[2] = size as u8;
    msg[4] = row;
    msg[5] = flags;
    msg[8..8 + nbytes].copy_from_slice(&data[..nbytes]);
    tx.send(EP_CMD_OUT, &msg)
}

/// `*_protect_on` / `*_protect_off`: a bare 0x19 / 0x18, no reply.
pub(crate) fn protect(tx: &mut dyn Transport, on: bool) -> Result<()> {
    let mut msg = [0u8; 8];
    msg[0] = if on { CMD_PROTECT_ON } else { CMD_PROTECT_OFF };
    tx.send(EP_CMD_OUT, &msg)
}

/// `*_read_calibration`: a 64-byte `0x16` header with the length at `[2]`, then
/// `len` bytes on EP81.
pub(crate) fn calibration_read(tx: &mut dyn Transport, len: usize) -> Result<Vec<u8>> {
    let mut msg = [0u8; 64];
    msg[0] = CMD_READ_CALIBRATION;
    le16(&mut msg, 2, len.min(usize::from(u16::MAX)) as u16);
    let data = cmd(tx, &msg, len)?;
    if data.len() < len {
        return Err(Error::Protocol);
    }
    Ok(data)
}

// ---------------------------------------------------------------------------
// Logic-IC test — the vector protocol is byte-identical across T48/T56/T76
// (t48.c:587-642 / t56.c:561-616 / t76.c:1626-…). Only the FPGA bitstream
// upload around it differs per driver, so that stays in each driver; the
// per-pass vector loop and the comparison live here.
// ---------------------------------------------------------------------------
const CMD_LOGIC_IC_TEST_VECTOR: u8 = 0x28;

// Vector state codes (database.h; pst string "01LHCZXGV"). Only L/H/Z are
// checked against the two passes.
const LOGIC_L: u8 = 2;
const LOGIC_H: u8 = 3;
const LOGIC_Z: u8 = 5;

/// One logic-test pass (`do_ic_test`): `pull` = 0 pull-up, 1 pull-down. Sends a
/// 32-byte `0x28` command per vector (VCC + pull in `[1]`, pin count at `[2]`,
/// vector index at `[4]`, the row packed two pins per byte from `[8]`), reads
/// the pin status back, and returns `pin_count * vector_count` unpacked states.
pub(crate) fn logic_pass(
    tx: &mut dyn Transport,
    pin_count: u8,
    vector_count: u16,
    vcc: u8,
    vectors: &[u8],
    pull: u8,
) -> Result<Vec<u8>> {
    let pins = usize::from(pin_count);
    let vecs = usize::from(vector_count);
    if pins == 0 || vecs == 0 || vectors.len() < pins * vecs {
        return Err(Error::Unsupported("device has no logic-test vectors"));
    }
    let mut result = Vec::with_capacity(pins * vecs);
    for n in 0..vecs {
        let mut msg = [0xffu8; 32];
        msg[0] = CMD_LOGIC_IC_TEST_VECTOR;
        msg[1] = vcc | (pull << 7);
        le16(&mut msg, 2, pin_count as u16);
        le32(&mut msg, 4, n.min(u32::MAX as usize) as u32);
        for (i, &v) in vectors[n * pins..(n + 1) * pins].iter().enumerate() {
            if i & 1 == 1 {
                msg[8 + i / 2] |= v << 4;
            } else {
                msg[8 + i / 2] = v;
            }
        }
        let resp = cmd(tx, &msg, 32)?;
        if resp.len() < 8 + pins.div_ceil(2) {
            return Err(Error::Protocol);
        }
        if resp[1] != 0 {
            return Err(Error::Overcurrent);
        }
        for i in 0..pins {
            result.push((resp[8 + i / 2] >> (4 * (i as u8 & 1))) & 0x0f);
        }
    }
    Ok(result)
}

/// Compare the two passes against the vector table: an `L` pin must read 0 in
/// both, `H` must read 1 in both, `Z` must read 1 up / 0 down. Inputs / don't-
/// care / power pins are ignored. `true` iff every element matches.
pub(crate) fn logic_compare(vectors: &[u8], first: &[u8], second: &[u8]) -> bool {
    vectors.iter().enumerate().all(|(n, &state)| {
        let (a, b) = (first[n] != 0, second[n] != 0);
        match state {
            LOGIC_L => !a && !b,
            LOGIC_H => a && b,
            LOGIC_Z => a && !b,
            _ => true,
        }
    })
}
