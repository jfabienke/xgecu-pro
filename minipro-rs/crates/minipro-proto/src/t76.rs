//! The XGecu T76 driver — reference implementation of the core traits.
//!
//! A faithful port of `minipro-t76/src/t76.c` (line numbers cited throughout)
//! over `dyn Transport`. The T76 is FPGA-based: [`Programmer::begin`] uploads a
//! per-operation Anlogic bitstream, inits the socket adapter, and configures
//! pin-drivers before any chip op.
//!
//! Every command that produces a response goes through
//! [`minipro_core::transport::command`], whose `#[must_use]` [`Pending`] guard
//! preserves the C code's load-bearing invariant: *an undrained EP81/EP82
//! response wedges the device until a USB replug* (t76.c:192-196).
//!
//! Endpoint map (usb_nix.c:442-460, 341-346, 385-390):
//! - commands OUT on EP 0x01 (`msg_send`), replies IN on EP 0x81 (`msg_recv`)
//! - bulk read payloads IN on EP 0x82 (`read_payload`)
//! - bulk write payloads OUT on EP 0x05 (`write_payload`)

use minipro_core::caps::{EmmcOps, FirmwareUpdate, MemoryOps, PinTest};
use minipro_core::device::{BlockReq, ChipId, Device, EraseKind, MemoryKind, Partition, Region};
use minipro_core::error::{Error, FwVersion, Result};
use minipro_core::programmer::{Caps, Programmer, ProgrammerInfo, Session};
use minipro_core::transport::{command, Ep, LinkSpeed, Transport};

// ---------------------------------------------------------------------------
// Endpoints (usb_nix.c: msg_send EP01 OUT / msg_recv EP81 IN /
// read_payload EP82 IN / write_payload EP05 OUT).
// ---------------------------------------------------------------------------
const EP_MSG_OUT: Ep = Ep(0x01);
const EP_MSG_IN: Ep = Ep(0x81);
const EP_DAT_IN: Ep = Ep(0x82);
const EP_DAT_OUT: Ep = Ep(0x05);

// ---------------------------------------------------------------------------
// Opcodes (t76.c:34-93). Only the subset this driver's capabilities use is
// defined; fuse/JEDEC/logic opcodes (0x06-0x09, 0x14-0x16, 0x1d/0x1e, 0x28,
// 0x37/0x38) come with those capability traits.
// ---------------------------------------------------------------------------
const CMD_BEGIN_TRANS_LOGIC: u8 = 0x02; // t76.c:34 — 64-byte NAND FPGA-setup prelude
const CMD_BEGIN_TRANS: u8 = 0x03; // t76.c:35
const CMD_END_TRANS: u8 = 0x04; // t76.c:36
const CMD_READID: u8 = 0x05; // t76.c:37
const CMD_READ_CFG: u8 = 0x08; // t76.c:40 — doubles as the eMMC EXT_CSD read
const CMD_WRITE_USER_DATA: u8 = 0x0a; // t76.c:42
const CMD_READ_USER_DATA: u8 = 0x0b; // t76.c:43
const CMD_WRITE_CODE: u8 = 0x0c; // t76.c:44
const CMD_READ_CODE: u8 = 0x0d; // t76.c:45
const CMD_ERASE: u8 = 0x0e; // t76.c:46
const CMD_READ_DATA: u8 = 0x10; // t76.c:48
const CMD_WRITE_DATA: u8 = 0x11; // t76.c:49
const CMD_NAND_PROGRAM: u8 = 0x1f; // t76.c:57
const CMD_FPGA_REG_IO: u8 = 0x24; // t76.c:72
const CMD_WRITE_BITSTREAM: u8 = 0x26; // t76.c:58
const CMD_EMMC_SEND_CMD: u8 = 0x27; // t76.c:73
const CMD_REQUEST_STATUS: u8 = 0x39; // t76.c:62
const CMD_NAND_BAD_BLOCK_CHECK: u8 = 0x3a; // t76.c:63
const CMD_BOOTLOADER_WRITE: u8 = 0x3b; // t76.c:64
const CMD_BOOTLOADER_ERASE: u8 = 0x3c; // t76.c:65
const CMD_SWITCH: u8 = 0x3d; // t76.c:66
const CMD_PIN_DETECTION: u8 = 0x3e; // t76.c:67
const CMD_RESET: u8 = 0x3f; // minipro.c:36 XGECU_RESET

// Protocol ids this driver special-cases (database.h:29-75).
const ALG_SPI25F_1: u8 = 0x03;
const ALG_SPI25F_2: u8 = 0x0f;
const ALG_T48: u8 = 0x12;
const ALG_T40B: u8 = 0x14;
const ALG_NAND: u8 = 0x2d;
const ALG_EMMC: u8 = 0x31;

// SPI algorithm-number high bytes (t76.c:99-100).
const SPI_DEVICE_16P: u8 = 0x21;

// Bitstream sub-commands + framing (t76.c:104-113).
const BS_BEGIN: u8 = 0x00;
const BS_BLOCK: u8 = 0x01;
const BS_END: u8 = 0x02;
const BS_RESET_FPGA: u8 = 0xaf; // t76.c:109
const FPGA_MAGIC: u32 = 0xaa55_ddee; // t76.c:110
const BS_PACKET_SIZE: usize = 0x200; // t76.c:113
const BS_PAYLOAD: usize = BS_PACKET_SIZE - 8; // 504-byte payload per chunk

// eMMC 0x27 op codes and CMD6 SWITCH partition args (t76.c:77-86).
const EMMC_OP_SWITCH: u8 = 0x46;
const EMMC_PART_USER: u32 = 0x02b3_0700;
const EMMC_PART_BOOT1: u32 = 0x01b3_0100;
const EMMC_PART_BOOT2: u32 = 0x01b3_0200;
const EMMC_PART_RPMB: u32 = 0x01b3_0300;

// Firmware update file framing (t76.c:89-92, 1785-1786).
const UPDATE_FILE_VERS_MASK: u32 = 0xffff_0000;
const UPDATE_FILE_VERSION: u32 = 0xf076_0000;
const BTLDR_MAGIC: u32 = 0x0004_9000;
const LAST_BLOCK_ADDR: u32 = 0x0004_9f00;
const LAST_BLOCK_CRC: u32 = 0xcdef_8668;

// Chip-id byte-order types (minipro.h:52-56).
const ID_TYPE3: u8 = 0x03;
const ID_TYPE4: u8 = 0x04;

/// Little-endian store, the Rust `format_int(.., MP_LITTLE_ENDIAN)`.
fn le16(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
}
fn le32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

/// The C `device_t` fields consumed by the T76 packet packers (t76.c:503-848).
///
/// `from_device` copies them 1:1 from the core `Device`, which now carries the
/// full field set the packers need (sizes clamped to the wire widths).
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
    fn nand_geometry(&self) -> Result<(u16, u16)> {
        if self.write_buffer_size == 0 || self.pages_per_block == 0 {
            return Err(Error::Unsupported(
                "NAND geometry (write_buffer_size/pages_per_block) missing from the chip DB entry",
            ));
        }
        Ok((self.write_buffer_size, self.pages_per_block))
    }
}

/// Pack the BEGIN_TRANS packet (t76.c:516-822). Returns the 128-byte buffer
/// and the number of bytes actually sent (64, or 128 when a chip-class
/// extension block applies).
pub(crate) fn pack_begin_trans(p: &ChipParams) -> ([u8; 128], usize) {
    let mut msg = [0u8; 128];
    let mut msglen = 64usize;

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

    // SPI 25-series NOR read-setup extension (t76.c:584-600). All four values
    // are individually load-bearing; dropping any one reads all zeros.
    if p.protocol_id == ALG_SPI25F_1 || p.protocol_id == ALG_SPI25F_2 {
        if (p.variant >> 8) as u8 == SPI_DEVICE_16P {
            le32(&mut msg, 0x40, 0x0002_0000); // t76.c:587
            le32(&mut msg, 0x50, 0x0200_0000); // t76.c:589
        } else {
            le32(&mut msg, 0x40, 0x0800_0000); // t76.c:592
            le32(&mut msg, 0x50, 0x0080_0000); // t76.c:594
        }
        le32(&mut msg, 0x60, 0x0f05_172f); // t76.c:597
        msg[0x65] = 0x03; // t76.c:598
        msglen = 128;
    }

    // Parallel NOR x16 family extension (t76.c:616-649).
    if p.protocol_id == ALG_T48 || p.protocol_id == ALG_T40B {
        let family = p.packed_package as u8;
        let adapter = (p.variant & 0xf0) as u8;
        let geom = (p.variant & 0x0f) as u8;
        if family == 0x0b && geom >= 8 {
            le32(&mut msg, 0x40, 0x0100_0000); // t76.c:624
            le32(&mut msg, 0x44, 0x0000_0040); // t76.c:626
            le32(&mut msg, 0x50, 0x1000_0000); // t76.c:628
            le32(&mut msg, 0x54, 0x0000_8000); // t76.c:630
            let b48: u32 = match adapter {
                0x10 => 0x0200,
                0x20 => 0x1200,
                0x30 => 0x0a00,
                0x40 => 0x1000,
                0x50 => 0x0800,
                0x60 => 0x1800,
                0x70 => 0x0400,
                _ => 0x0800,
            }; // t76.c:632-641
            le32(&mut msg, 0x48, b48); // t76.c:642
            le32(&mut msg, 0x60, 0x0f05_172f); // t76.c:644
            msg[0x65] = 0x03; // t76.c:646
            msglen = 128;
        }
    }

    // NAND BEGIN adjustments (t76.c:659-703). The 0x02 prelude that precedes
    // this packet is packed separately in `pack_nand_prelude`.
    if p.protocol_id == ALG_NAND {
        if p.pages_per_block != 0 {
            // Per-block transfer size (data + spare), NOT the chip size, at
            // msg[0x10] so the FPGA streams exactly one block per 0x0d.
            le32(
                &mut msg,
                16,
                u32::from(p.write_buffer_size) * u32::from(p.pages_per_block),
            ); // t76.c:666
        }
        le32(&mut msg, 56, p.raw_flags | 0x800); // t76.c:670 — NAND flag bit
        le32(&mut msg, 0x60, 0x0b09_272f); // t76.c:673 — lower clock tier
        msg[0x65] = 0x03; // t76.c:675
        // Forced bytes matching the vendor read5 capture (t76.c:683-702).
        msg[0x0e] = 0x20;
        msg[0x14] = 0x00;
        msg[0x18] = 0x03;
        msg[0x1c] = 0x03;
        if (p.variant & 0x70) == 0 {
            le32(&mut msg, 0x28, 0xe200_0000); // parallel NAND pin/family dword
        }
        msg[0x30] = 0x40;
        msglen = 128;
    }

    // eMMC: 128-byte BEGIN, no 0x40..0x7f packer; msg[0x0c] carries the
    // bus-mode/CSD byte (t76.c:817-820).
    if p.protocol_id == ALG_EMMC {
        msg[0x0c] = (p.variant >> 8) as u8;
        msglen = 128;
    }

    (msg, msglen)
}

/// Pack the 64-byte opcode-0x02 NAND FPGA-setup prelude sent immediately
/// before BEGIN_TRANS for NAND chips (t76.c:740-801). Without it the FPGA
/// never clocks the NAND (READID 00 FF FF, 0x0d timeout).
pub(crate) fn pack_nand_prelude(p: &ChipParams) -> Result<[u8; 64]> {
    let (wbuf, ppb) = p.nand_geometry()?;
    let page_or_blocks = p.page_size; // desc[0x54]; == page for parallel NAND

    // Real page = largest power of two <= write_buffer_size (t76.c:751-753).
    let mut real_page: u16 = 1;
    while u32::from(real_page) << 1 <= u32::from(wbuf) {
        real_page <<= 1;
    }
    // Page-size code (t76.c:754-758).
    let ps_code: u32 = if real_page < 0x800 {
        4
    } else if real_page == 0x800 {
        8
    } else if real_page == 0x1000 {
        4
    } else if real_page == 0x4000 {
        1
    } else {
        2
    };
    // Bus/width code and clock/adapter selection (t76.c:765-780).
    let big = u32::from(ppb) * u32::from(page_or_blocks) > 0x10000;
    let busw: u32 = if real_page >= 0x800 {
        if big { 3 } else { 1 }
    } else if big {
        2
    } else {
        0
    };
    let serial = (p.variant & 0x70) != 0;
    // Conservative low-speed bus clock entries (serial table[0], parallel
    // table[3]); the C honors a T76_NAND_CLOCK env override, which the Rust
    // driver deliberately drops (config belongs to the caller, not getenv).
    let clock: u32 = if serial { 0x0808_230e } else { 0x2715_4f3b };
    let adapter: u32 = if serial { 0x0001_0001 } else { 0x0001_0000 };
    let spare = wbuf - real_page;

    let mut pre = [0u8; 64];
    pre[0] = CMD_BEGIN_TRANS_LOGIC; // t76.c:786
    le16(&mut pre, 0x08, spare); // t76.c:787
    le16(&mut pre, 0x0a, real_page); // t76.c:788
    le16(&mut pre, 0x0c, page_or_blocks); // t76.c:790
    le16(&mut pre, 0x0e, ppb); // t76.c:792
    le16(&mut pre, 0x10, 1); // t76.c:793 — plane count
    le16(&mut pre, 0x12, 1); // t76.c:794 — LUN count
    le32(&mut pre, 0x14, busw); // t76.c:795
    le32(&mut pre, 0x18, ps_code); // t76.c:796
    // pre[0x1c] = 0 (t76.c:728)
    le32(&mut pre, 0x20, adapter); // t76.c:797
    le32(&mut pre, 0x24, clock); // t76.c:798
    Ok(pre)
}

/// The 40-byte eMMC 0x0d (read) / 0x1f (program) region init (t76.c:395-409).
/// The firmware then streams/accepts `blocks` x 64 KiB on EP82/EP05.
pub(crate) fn pack_emmc_io_init(opcode: u8, lba: u32, blocks: u32) -> [u8; 40] {
    let mut init = [0u8; 40];
    init[0] = opcode;
    init[1] = 0x01;
    le32(&mut init, 4, lba); // start LBA (512-byte sectors)
    le32(&mut init, 8, 0x200); // sector size
    le32(&mut init, 12, 0x20);
    le32(&mut init, 16, blocks); // 64 KiB-block count
    le32(&mut init, 20, 0x80);
    le32(&mut init, 24, 0x20);
    le32(&mut init, 28, 0x04);
    le32(&mut init, 32, 0x01);
    init
}

/// The fixed 16-byte 0x27/op00 eMMC timing command that wraps every 0x0d read
/// / 0x1f program (t76.c:373-389). `post` selects the POST variant; byte [9]
/// is the JEDEC bus-width code (0=1-bit, 1=4-bit, 2=8-bit).
pub(crate) fn pack_emmc_timing(post: bool, variant: u16) -> [u8; 16] {
    let mut pkt: [u8; 16] = if post {
        [
            0x27, 0x00, 0xff, 0x00, 0x3b, 0x2c, 0x10, 0x0b, 0x00, 0x02, 0xb7, 0x03, 0x00, 0x01,
            0xb9, 0x03,
        ]
    } else {
        [
            0x27, 0x00, 0xff, 0x00, 0x3b, 0x0e, 0x05, 0x02, 0x00, 0x02, 0xb7, 0x03, 0x00, 0x12,
            0xb9, 0x03,
        ]
    };
    pkt[9] = match (variant >> 8) as u8 {
        0x51 => 0, // 1-bit
        0x54 => 1, // 4-bit
        _ => 2,    // 8-bit (0x53 and the un-mapped default, t76.c:380-385)
    };
    pkt
}

/// eMMC geometry captured from EXT_CSD at session bring-up (t76.c:462-495).
#[derive(Clone, Copy, Debug, Default)]
struct EmmcGeometry {
    sec_count: u32, // EXT_CSD[212..216] SEC_COUNT
    boot_mult: u8,  // EXT_CSD[226] BOOT_SIZE_MULT
    rpmb_mult: u8,  // EXT_CSD[168] RPMB_SIZE_MULT
}

impl EmmcGeometry {
    /// Partition capacity in bytes (t76.c:483-491).
    fn capacity(&self, part: Partition) -> u64 {
        match part {
            Partition::Boot1 | Partition::Boot2 => u64::from(self.boot_mult) * 128 * 1024,
            Partition::Rpmb => u64::from(self.rpmb_mult) * 128 * 1024,
            Partition::User => u64::from(self.sec_count) * 512,
        }
    }
}

fn partition_config(part: Partition) -> u32 {
    match part {
        Partition::User => EMMC_PART_USER,
        Partition::Boot1 => EMMC_PART_BOOT1,
        Partition::Boot2 => EMMC_PART_BOOT2,
        Partition::Rpmb => EMMC_PART_RPMB,
    }
}

/// The T76 command channel + cached identity.
pub struct T76 {
    tx: Box<dyn Transport>,
    info: ProgrammerInfo,
    /// Name of the FPGA algorithm currently loaded, so `begin` skips the
    /// re-upload within a session (the C `bitstream_uploaded`, t76.c:283-285,
    /// keyed by name so switching devices re-uploads).
    uploaded_algo: Option<String>,
    emmc_geom: EmmcGeometry,
    emmc_capacity: u64,
}

impl T76 {
    pub fn new(tx: Box<dyn Transport>) -> Self {
        let info = ProgrammerInfo {
            model: "T76".into(),
            firmware: FwVersion(0),
            serial: String::new(),
            link: LinkSpeed::High,
            voltage: 0.0,
        };
        // TODO(hw): populate firmware/serial/voltage via the report request in
        // minipro.c `minipro_get_system_info` (not part of t76.c) once the
        // detect() identity query is ported.
        T76 { tx, info, uploaded_algo: None, emmc_geom: EmmcGeometry::default(), emmc_capacity: 0 }
    }

    /// Send an 8-byte command and drain `resp_len` bytes from EP81.
    fn cmd(&mut self, pkt: &[u8], resp_len: usize) -> Result<Vec<u8>> {
        if std::env::var_os("MINIPRO_TRACE").is_some() {
            eprintln!("[t76] cmd op={:02x}/{:02x} out={} want={resp_len}", pkt[0], pkt.get(1).copied().unwrap_or(0), pkt.len());
        }
        let r = command(self.tx.as_mut(), EP_MSG_OUT, EP_MSG_IN, pkt, resp_len)?.read();
        if std::env::var_os("MINIPRO_TRACE").is_some() {
            match &r { Ok(v) => eprintln!("[t76]   -> {} bytes: {:02x?}", v.len(), &v[..v.len().min(8)]), Err(e) => eprintln!("[t76]   -> ERR {e}") }
        }
        r
    }

    /// Send a command with no reply (the T76 has genuinely reply-less
    /// commands: BEGIN/END_TRANS, bitstream blocks, timing writes).
    fn send(&mut self, pkt: &[u8]) -> Result<()> {
        self.tx.send(EP_MSG_OUT, pkt)
    }

    /// One 0x24 FPGA-register-I/O command. The command word carries the
    /// response length in msg[2..3]; that many bytes MUST be drained from
    /// EP81 or the next transfer desyncs — a 0xf0 power-down left undrained
    /// wedges the device until a USB replug (t76.c:192-211).
    fn cmd_24(&mut self, pkt: &[u8; 8]) -> Result<()> {
        let mut resp_len = usize::from(u16::from_le_bytes([pkt[2], pkt[3]]));
        if resp_len > 64 {
            resp_len = 64; // t76.c:205-206
        }
        if resp_len > 0 {
            self.cmd(pkt, resp_len).map(|_| ())
        } else {
            self.send(pkt)
        }
    }

    /// One 16-byte 0x3e pin-detection command; drains the 32-byte bad-pin
    /// bitmask (t76.c:238-245).
    fn pin_detection16(&mut self) -> Result<Vec<u8>> {
        let mut pd = [0u8; 16];
        pd[0] = CMD_PIN_DETECTION;
        self.cmd(&pd, 32)
    }

    /// One-time NAND socket-adapter power/init at session start
    /// (t76_adapter_init, t76.c:221-247): 0x24 power-down (drains 8),
    /// read-adapter-ID (drains 0x30), power-up, then pin detection run twice.
    fn adapter_init(&mut self) -> Result<()> {
        let pwr_down: [u8; 8] = [CMD_FPGA_REG_IO, 0xf0, 0x08, 0x00, 0x01, 0x00, 0x00, 0x00]; // t76.c:223
        let read_id: [u8; 8] = [CMD_FPGA_REG_IO, 0xe4, 0x30, 0x00, 0x11, 0x01, 0x08, 0x00]; // t76.c:225
        let pwr_up: [u8; 8] = [CMD_FPGA_REG_IO, 0xf1, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]; // t76.c:227
        self.cmd_24(&pwr_down)?;
        self.cmd_24(&read_id)?;
        self.cmd_24(&pwr_up)?;
        // TODO(hw): validate the returned adapter ID (t76.c:218-220).
        for _ in 0..2 {
            self.pin_detection16()?; // configure socket pin drivers, drain mask
        }
        Ok(())
    }

    /// eMMC socket-adapter power/init (t76_emmc_adapter_init, t76.c:257-279),
    /// byte-exact from the XGPro eMMC READ capture: 0x24 f0 power-down,
    /// 12-byte 0x24 e0 init (recv 0x28), 0x24 f1 power-up, ONE pin-detect.
    fn emmc_adapter_init(&mut self) -> Result<()> {
        let pwr_down: [u8; 8] = [CMD_FPGA_REG_IO, 0xf0, 0x08, 0x00, 0x01, 0x00, 0x00, 0x00]; // t76.c:259
        let e0_init: [u8; 12] = [CMD_FPGA_REG_IO, 0xe0, 0x28, 0x00, 0, 0, 0, 0, 0, 0, 0, 0]; // t76.c:261
        let pwr_up: [u8; 8] = [CMD_FPGA_REG_IO, 0xf1, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]; // t76.c:263
        self.cmd_24(&pwr_down)?;
        self.cmd(&e0_init, 0x28)?;
        self.cmd_24(&pwr_up)?;
        self.pin_detection16()?;
        Ok(())
    }

    /// Upload the FPGA bitstream: BEGIN_BS, 512-byte chunks (8-byte header +
    /// 504-byte payload), END_BS — including the NAND last-block-size fix
    /// (t76_write_bitstream, t76.c:116-189).
    fn write_bitstream(&mut self, dev: &Device) -> Result<()> {
        let algorithm = dev
            .algorithm
            .as_ref()
            .filter(|a| !a.bitstream.is_empty())
            .ok_or(Error::Unsupported("device has no FPGA bitstream loaded"))?;
        if self.uploaded_algo.as_deref() == Some(algorithm.name.as_str()) {
            return Ok(()); // same session, same algorithm (t76.c:283-285)
        }
        let bits = &algorithm.bitstream;
        let len = bits.len();

        // BEGIN_BS: packet size + total bitstream size (t76.c:122-136).
        let mut msg = [0u8; 8];
        msg[0] = CMD_WRITE_BITSTREAM;
        msg[1] = BS_BEGIN;
        le16(&mut msg, 2, BS_PACKET_SIZE as u16);
        le32(&mut msg, 4, len.min(u32::MAX as usize) as u32);
        let resp = self.cmd(&msg, 8)?;
        if resp.len() < 2 || resp[1] != 0 {
            return Err(Error::Protocol);
        }

        // 504-byte payload chunks. The C reuses one 512-byte buffer without
        // re-zeroing, so a short final chunk carries the previous chunk's
        // tail bytes past `block_size`; replicated for byte-fidelity
        // (t76.c:142-159).
        let mut pkt = [0u8; BS_PACKET_SIZE];
        for chunk in bits.chunks(BS_PAYLOAD) {
            pkt[0] = CMD_WRITE_BITSTREAM;
            pkt[1] = BS_BLOCK;
            le16(&mut pkt, 2, chunk.len() as u16);
            pkt[4..8].fill(0);
            pkt[8..8 + chunk.len()].copy_from_slice(chunk);
            self.send(&pkt)?; // no reply for BS_BLOCK
        }

        // END_BS. The vendor puts the final (partial) block size in msg[2..3]
        // so the FPGA finalizes the last config word; minipro sent 0, which
        // left a NAND FPGA mis-finalized (READID/read 0xFF). Carried for NAND
        // (t76.c:161-179).
        let mut end = [0u8; 8];
        end[0] = CMD_WRITE_BITSTREAM;
        end[1] = BS_END;
        if dev.protocol_id == ALG_NAND {
            let mut last_block = len % BS_PAYLOAD;
            if last_block == 0 {
                last_block = BS_PAYLOAD;
            }
            le16(&mut end, 2, last_block as u16);
        }
        let resp = self.cmd(&end, 8)?;
        if resp.len() < 2 || resp[1] != 0 {
            return Err(Error::Protocol);
        }

        self.uploaded_algo = Some(algorithm.name.clone());
        Ok(())
    }

    /// Reset the FPGA (t76_reset_fpga, t76.c:861-872).
    pub fn reset_fpga(&mut self) -> Result<()> {
        let mut msg = [0u8; 8];
        msg[0] = CMD_WRITE_BITSTREAM;
        msg[1] = BS_RESET_FPGA;
        le32(&mut msg, 4, FPGA_MAGIC);
        let resp = self.cmd(&msg, 8)?;
        if resp.len() < 2 || resp[1] != 0 {
            return Err(Error::Protocol);
        }
        Ok(())
    }

    /// 0x39 REQUEST_STATUS; returns the OVC byte (t76_get_ovc_status,
    /// t76.c:1491-1525). For NAND/eMMC the chip-parameter header is repacked
    /// into msg[1..8] — a zeroed 0x39 leaves the NAND deselected.
    fn ovc_status(&mut self, p: &ChipParams) -> Result<u8> {
        let mut msg = [0u8; 8];
        msg[0] = CMD_REQUEST_STATUS;
        if p.protocol_id == ALG_NAND || p.protocol_id == ALG_EMMC {
            msg[1] = p.protocol_id; // t76.c:1504
            msg[2] = p.variant as u8;
            msg[3] = p.icsp;
            le16(&mut msg, 4, p.raw_voltages as u16);
            msg[6] = p.chip_info;
            msg[7] = p.pin_map;
        }
        let resp = self.cmd(&msg, 32)?;
        if resp.len() < 13 {
            return Err(Error::Protocol);
        }
        Ok(resp[12]) // t76.c:1523
    }

    /// 0x27 Form A: simple eMMC command, no payload; send 8 / recv 8;
    /// resp[1] != 0 is an error (t76_emmc_cmd27, t76.c:339-355).
    fn emmc_cmd27(&mut self, op: u8, arg: u32) -> Result<Vec<u8>> {
        let mut msg = [0u8; 8];
        msg[0] = CMD_EMMC_SEND_CMD;
        msg[1] = op;
        le32(&mut msg, 4, arg);
        let resp = self.cmd(&msg, 8)?;
        if resp.len() < 2 || resp[1] != 0 {
            return Err(Error::Protocol);
        }
        Ok(resp)
    }

    /// eMMC session bring-up (t76_emmc_bring_up, t76.c:439-501): ID queries,
    /// EXT_CSD capacity read, then CMD6 SWITCH to the USER partition.
    /// Returns the USER-partition capacity.
    fn emmc_bring_up(&mut self) -> Result<u64> {
        // Query sequence; replies are drained, not matched (t76.c:441-452).
        for (op, resp_len) in [(0x21u8, 32usize), (0x05, 32), (0x06, 24)] {
            let mut cmd = [0u8; 8];
            cmd[0] = op;
            self.cmd(&cmd, resp_len)?;
        }

        // EXT_CSD via opcode 0x08. The device returns exactly 520 bytes (a
        // 512-byte full packet + an 8-byte short packet) on EP82: an 8-byte
        // header + EXT_CSD, so EXT_CSD[N] is at buf[8+N] (t76.c:469-494).
        let cmd: [u8; 8] = [CMD_READ_CFG, 0x48, 0x00, 0x02, 0, 0, 0, 0];
        let ext = command(self.tx.as_mut(), EP_MSG_OUT, EP_DAT_IN, &cmd, 520)?.read()?;
        if ext.len() < 235 {
            return Err(Error::Protocol);
        }
        self.emmc_geom = EmmcGeometry {
            sec_count: u32::from_le_bytes([ext[220], ext[221], ext[222], ext[223]]),
            boot_mult: ext[234], // EXT_CSD[226]
            rpmb_mult: ext[176], // EXT_CSD[168]
        };
        let capacity = self.emmc_geom.capacity(Partition::User);

        // Select the default USER partition (t76.c:497-499).
        self.emmc_cmd27(EMMC_OP_SWITCH, EMMC_PART_USER)?;
        Ok(capacity)
    }

    /// eMMC region read: PRE timing + 40-byte 0x0d init, then the 64 KiB
    /// blocks stream on EP82 (t76.c:907-935).
    ///
    /// TODO(hw): the vendor issues ONE init for the whole region and streams
    /// `region/64KiB` blocks; `BlockReq` is per-block, so each call opens a
    /// one-shot stream (`blocks = ceil(len/64KiB)`) — same packet layout,
    /// per-call granularity. Validate against hardware, then add stream state
    /// if the per-call init measurably hurts throughput.
    fn emmc_read(&mut self, p: &ChipParams, req: &BlockReq) -> Result<Vec<u8>> {
        self.send(&pack_emmc_timing(false, p.variant))?; // PRE, no reply
        let lba = (req.address / 512).min(u64::from(u32::MAX)) as u32;
        let blocks = (u64::from(req.len)).div_ceil(0x10000) as u32;
        let init = pack_emmc_io_init(CMD_READ_CODE, lba, blocks);
        command(self.tx.as_mut(), EP_MSG_OUT, EP_DAT_IN, &init, req.len as usize)?.read()
    }

    /// eMMC block program (t76.c:1023-1063): 0x27 op0x50 setup, PRE timing,
    /// 40-byte 0x1f init, data on EP05, then the 0x39 / POST-timing / 0x39
    /// commit. Per-call granularity as with `emmc_read` (TODO(hw) above).
    fn emmc_write(&mut self, p: &ChipParams, req: &BlockReq, data: &[u8]) -> Result<()> {
        // 0x27 op 0x50 program-setup, ARG 0x20000; reply drained unchecked
        // (t76.c:1027-1033).
        let mut op50 = [0u8; 8];
        op50[0] = CMD_EMMC_SEND_CMD;
        op50[1] = 0x50;
        le32(&mut op50, 4, 0x0002_0000);
        self.cmd(&op50, 8)?;

        self.send(&pack_emmc_timing(false, p.variant))?; // PRE
        let lba = (req.address / 512).min(u64::from(u32::MAX)) as u32;
        let blocks = (u64::from(req.len)).div_ceil(0x10000) as u32;
        self.send(&pack_emmc_io_init(CMD_NAND_PROGRAM, lba, blocks))?;
        self.tx.send(EP_DAT_OUT, data)?;

        // Commit: 0x39 -> POST-timing -> 0x39 (t76.c:1047-1061).
        let mut st = [0u8; 8];
        st[0] = CMD_REQUEST_STATUS;
        self.cmd(&st, 32)?;
        self.send(&pack_emmc_timing(true, p.variant))?; // POST
        self.cmd(&st, 32)?;
        Ok(())
    }

    /// NAND block read: one erase-block (data + spare) per 0x0d with the
    /// 16-bit block index in msg[2..3] and the fixed read-parameter header in
    /// msg[4..0xf]; the block streams raw on EP82 (t76.c:891-905).
    fn nand_read(&mut self, req: &BlockReq) -> Result<Vec<u8>> {
        const NAND_READ_HDR: [u8; 12] = [
            0x10, 0x00, 0x04, 0x00, // msg[4..7]  (t76.c:893-897)
            0x08, 0x00, 0x08, 0x00, // msg[8..b]
            0x69, 0x01, 0x00, 0x00, // msg[c..f]
        ];
        let block_index = if req.len != 0 { req.address / u64::from(req.len) } else { 0 };
        let mut msg = [0u8; 16];
        msg[0] = CMD_READ_CODE;
        le16(&mut msg, 2, block_index.min(u64::from(u16::MAX)) as u16);
        msg[4..16].copy_from_slice(&NAND_READ_HDR);
        command(self.tx.as_mut(), EP_MSG_OUT, EP_DAT_IN, &msg, req.len as usize)?.read()
    }

    /// NAND block program (t76.c:1092-1133): 16-byte 0x1f init, then each
    /// page (page + spare) as a separate EP05 packet prefixed by a 16-byte
    /// header, then a plain 0x39 REQUEST_STATUS commit. The firmware
    /// cache-programs, so without the 0x39 the block's last page reads back
    /// erased.
    fn nand_write(&mut self, p: &ChipParams, req: &BlockReq, data: &[u8]) -> Result<()> {
        let (page_full, ppb) = p.nand_geometry()?;
        let block_size = u32::from(page_full) * u32::from(ppb);
        if data.len() != block_size as usize {
            return Err(Error::Format(format!(
                "NAND write needs one full block ({} bytes = {} pages x {} bytes), got {}",
                block_size,
                ppb,
                page_full,
                data.len()
            )));
        }
        let block_index = if req.len != 0 { req.address / u64::from(req.len) } else { 0 };

        let mut init = [0u8; 16];
        init[0] = CMD_NAND_PROGRAM;
        le16(&mut init, 2, page_full); // t76.c:1100
        le32(&mut init, 4, block_index.min(u64::from(u32::MAX)) as u32); // t76.c:1101
        le32(&mut init, 8, u32::from(ppb)); // t76.c:1102
        le32(&mut init, 12, u32::from(page_full)); // t76.c:1103
        self.send(&init)?;

        // Per page: [16-byte header (hdr[0]=0x1f) | page+spare] -> EP05.
        let mut pkt = vec![0u8; 16 + usize::from(page_full)];
        for page in data.chunks(usize::from(page_full)) {
            pkt[..16].fill(0);
            pkt[0] = CMD_NAND_PROGRAM;
            pkt[16..].copy_from_slice(page);
            self.tx.send(EP_DAT_OUT, &pkt)?;
        }

        // Commit the block (t76.c:1125-1131).
        let mut st = [0u8; 8];
        st[0] = CMD_REQUEST_STATUS;
        self.cmd(&st, 32)?;
        Ok(())
    }

    /// NAND full-chip erase with factory-bad-block skip (t76_nand_erase,
    /// t76.c:1364-1408): per block, 0x3a probes the bad-block marker (skip so
    /// the marker survives), then 0x0e erases by 16-bit block index.
    fn nand_erase(&mut self, p: &ChipParams, code_size: u64) -> Result<()> {
        let (wbuf, ppb) = p.nand_geometry()?;
        let block_size = u64::from(wbuf) * u64::from(ppb);
        let block_count = code_size / block_size;
        for blk in 0..block_count {
            if self.nand_erase_block(blk as u32)?.is_none() {
                // Factory-marked bad block skipped; marker preserved.
                continue;
            }
        }
        Ok(())
    }

    /// Erase one NAND block. Returns `Ok(None)` when the block is factory-
    /// marked bad and was skipped (the C counts and preserves these).
    fn nand_erase_block(&mut self, blk: u32) -> Result<Option<()>> {
        // 0x3a bad-block check: send 8 / recv 8; resp[1] != 0 => bad
        // (t76.c:1382-1391).
        let mut chk = [0u8; 8];
        chk[0] = CMD_NAND_BAD_BLOCK_CHECK;
        le16(&mut chk, 2, blk.min(u32::from(u16::MAX)) as u16);
        let resp = self.cmd(&chk, 8)?;
        if resp.len() < 2 || resp[1] != 0 {
            return Ok(None);
        }
        // 0x0e erase: send 16 / recv 8 (t76.c:1394-1401).
        let mut msg = [0u8; 16];
        msg[0] = CMD_ERASE;
        le16(&mut msg, 2, blk.min(u32::from(u16::MAX)) as u16);
        let resp = command(self.tx.as_mut(), EP_MSG_OUT, EP_MSG_IN, &msg, 8)?.read()?;
        if resp.len() < 2 || resp[1] != 0 {
            return Ok(None); // erase-failed block, counted as bad in the C
        }
        Ok(Some(()))
    }

    /// eMMC erase (t76_emmc_erase, t76.c:1419-1465): per 0x20000-sector
    /// group, a 16-byte 0x0e with start/end LBA, then poll 0x27 op 0x4d until
    /// the card returns to ready (resp[5] != 0x0e busy).
    fn emmc_erase(&mut self, code_size: u64) -> Result<()> {
        const POLL: [u8; 8] = [0x27, 0x4d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00]; // t76.c:1422
        let total = if self.emmc_capacity != 0 {
            self.emmc_capacity / 512
        } else {
            code_size / 512
        };
        if total == 0 {
            return Ok(());
        }
        let mut start = 0u64;
        while start < total {
            let end = (start + 0x1ffff).min(total - 1);
            let mut cmd = [0u8; 16];
            cmd[0] = CMD_ERASE;
            le32(&mut cmd, 4, start.min(u64::from(u32::MAX)) as u32);
            le32(&mut cmd, 8, end.min(u64::from(u32::MAX)) as u32);
            command(self.tx.as_mut(), EP_MSG_OUT, EP_MSG_IN, &cmd, 8)?.read()?;

            // Poll until complete (resp[5] back from 0x0e busy), same bound
            // as the C (t76.c:1447-1462).
            let mut done = false;
            for _ in 0..2_000_000 {
                let resp = self.cmd(&POLL, 8)?;
                if resp.len() < 6 {
                    return Err(Error::Protocol);
                }
                if resp[5] != 0x0e {
                    done = true;
                    break;
                }
            }
            if !done {
                return Err(Error::Usb("eMMC erase timed out".into()));
            }
            start += 0x20000;
        }
        Ok(())
    }

    /// Reboot the device (XGECU_RESET 0x3f, minipro.c:516), then re-arm the
    /// transport.
    fn reboot(&mut self) -> Result<()> {
        let mut msg = [0u8; 8];
        msg[0] = CMD_RESET;
        self.send(&msg)?; // no reply; the device drops off the bus
        self.tx.reset()
    }
}

impl Programmer for T76 {
    fn info(&self) -> &ProgrammerInfo {
        &self.info
    }

    fn caps(&self) -> Caps {
        Caps::MEMORY.with(Caps::EMMC).with(Caps::PINTEST).with(Caps::FWUPDATE)
    }

    /// `t76_begin_transaction` (t76.c:503-848): adapter init (NAND/eMMC),
    /// bitstream upload, NAND 0x02 prelude, the 64/128-byte BEGIN_TRANS, the
    /// 0x39 overcurrent check, and the eMMC bring-up.
    fn begin(&mut self, dev: &Device) -> Result<Session> {
        let params = ChipParams::from_device(dev);

        // Socket-adapter energize/init before the first bitstream
        // (t76_send_bitstream, t76.c:292-303).
        match dev.protocol_id {
            ALG_NAND => self.adapter_init()?,
            ALG_EMMC => self.emmc_adapter_init()?,
            _ => {}
        }
        self.write_bitstream(dev)?;

        // NAND: the 64-byte opcode-0x02 FPGA-setup prelude immediately BEFORE
        // BEGIN_TRANS (t76.c:705-801).
        if dev.protocol_id == ALG_NAND {
            let pre = pack_nand_prelude(&params)?;
            self.send(&pre)?;
        }

        // BEGIN_TRANS itself has no reply (t76.c:822).
        let (pkt, msglen) = pack_begin_trans(&params);
        self.send(&pkt[..msglen])?;

        // Overcurrent check (t76.c:830-835).
        let ovc = self.ovc_status(&params)?;
        if ovc != 0 {
            return Err(Error::Overcurrent);
        }

        // eMMC host-controller bring-up + capacity + partition select
        // (t76.c:839-845).
        let mut emmc_capacity = 0u64;
        if dev.protocol_id == ALG_EMMC {
            emmc_capacity = self.emmc_bring_up()?;
            self.emmc_capacity = emmc_capacity;
        }

        Ok(Session { device: dev.clone(), emmc_capacity })
    }

    /// `t76_end_transaction` (t76.c:850-858): a bare END_TRANS, no reply.
    fn end(&mut self, _session: Session) -> Result<()> {
        let mut msg = [0u8; 8];
        msg[0] = CMD_END_TRANS;
        self.send(&msg)
    }

    /// `t76_get_chip_id` (t76.c:1263-1294): READID, then decode per the
    /// reported id type — types 3/4 little-endian, others big-endian.
    fn identify(&mut self, s: &Session) -> Result<ChipId> {
        let mut msg = [0u8; 8];
        msg[0] = CMD_READID;
        // msg[1..7] are don't-care (XGPro sends stack garbage, t76.c:1271-1274).
        let resp = self.cmd(&msg, 32)?;
        if resp.len() < 6 {
            return Err(Error::Protocol);
        }
        let id_type = resp[0];
        let id_length = s.device.chip_id_bytes.min(4);
        let bytes = &resp[2..2 + usize::from(id_length)];
        let raw = if id_length == 0 {
            0
        } else if id_type == ID_TYPE3 || id_type == ID_TYPE4 {
            bytes.iter().rev().fold(0u32, |acc, &b| (acc << 8) | u32::from(b))
        } else {
            bytes.iter().fold(0u32, |acc, &b| (acc << 8) | u32::from(b))
        };
        Ok(ChipId { raw, bytes: id_length })
    }

    fn reset(&mut self) -> Result<()> {
        self.tx.reset()
    }

    // Capability upcasts — must agree with caps() (checked in tests).
    fn memory(&mut self) -> Option<&mut dyn MemoryOps> {
        Some(self)
    }
    fn emmc(&mut self) -> Option<&mut dyn EmmcOps> {
        Some(self)
    }
    fn pins(&mut self) -> Option<&mut dyn PinTest> {
        Some(self)
    }
    fn firmware(&mut self) -> Option<&mut dyn FirmwareUpdate> {
        Some(self)
    }
}

impl MemoryOps for T76 {
    /// `t76_read_block` (t76.c:875-1002).
    fn read_block(&mut self, s: &Session, req: &BlockReq) -> Result<Vec<u8>> {
        let params = ChipParams::from_device(&s.device);
        match (s.device.protocol_id, req.kind) {
            (ALG_NAND, MemoryKind::Code) => return self.nand_read(req),
            (ALG_EMMC, MemoryKind::Code) => return self.emmc_read(&params, req),
            _ => {}
        }

        match req.kind {
            // MP_CODE (t76.c:942-953): 16-byte 0x0d init, data on EP82.
            // TODO(hw): the C sends the init once per region with the total
            // block count and then drains one block per call; BlockReq is
            // per-block, so each call opens a one-block stream (same layout,
            // block_count = 1).
            MemoryKind::Code => {
                let mut msg = [0u8; 16];
                msg[0] = CMD_READ_CODE;
                le16(&mut msg, 2, req.len.min(u32::from(u16::MAX)) as u16);
                le32(&mut msg, 4, req.address.min(u64::from(u32::MAX)) as u32);
                le32(&mut msg, 8, 1); // block_count
                command(self.tx.as_mut(), EP_MSG_OUT, EP_DAT_IN, &msg, req.len as usize)?.read()
            }
            // MP_DATA (t76.c:956-974): 0x10, payload is 16 header bytes +
            // data on EP82.
            MemoryKind::Data => {
                let mut msg = [0u8; 16];
                msg[0] = CMD_READ_DATA;
                le16(&mut msg, 2, req.len.min(u32::from(u16::MAX)) as u16);
                le32(&mut msg, 4, req.address.min(u64::from(u32::MAX)) as u32);
                let raw = command(
                    self.tx.as_mut(),
                    EP_MSG_OUT,
                    EP_DAT_IN,
                    &msg,
                    req.len as usize + 16,
                )?
                .read()?;
                if raw.len() < 16 {
                    return Err(Error::Protocol);
                }
                Ok(raw[16..].to_vec())
            }
            // MP_USER (t76.c:977-998): 0x0b, reply (16 header bytes + data)
            // comes back on EP81, not the bulk EP82.
            MemoryKind::User => {
                let mut msg = [0u8; 16];
                msg[0] = CMD_READ_USER_DATA;
                le16(&mut msg, 2, req.len.min(u32::from(u16::MAX)) as u16);
                le32(&mut msg, 4, req.address.min(u64::from(u32::MAX)) as u32);
                let raw = self.cmd(&msg, req.len as usize + 16)?;
                if raw.len() < 16 {
                    return Err(Error::Protocol);
                }
                Ok(raw[16..].to_vec())
            }
            MemoryKind::Data2 | MemoryKind::Config => {
                Err(Error::Unsupported("T76 read_block: Data2/Config spaces"))
            }
        }
    }

    /// `t76_write_block` (t76.c:1005-1186).
    fn write_block(&mut self, s: &Session, req: &BlockReq, data: &[u8]) -> Result<()> {
        if data.len() != req.len as usize {
            return Err(Error::Format(format!(
                "write_block: req.len {} != data {}",
                req.len,
                data.len()
            )));
        }
        let params = ChipParams::from_device(&s.device);
        match (s.device.protocol_id, req.kind) {
            (ALG_EMMC, MemoryKind::Code) => return self.emmc_write(&params, req, data),
            (ALG_NAND, MemoryKind::Code) => return self.nand_write(&params, req, data),
            _ => {}
        }

        match req.kind {
            // MP_CODE (t76.c:1141-1165): 16-byte 0x0c init (block_count at
            // [8..11]), then [16-byte header with [8..11] zeroed | data] on
            // EP05. Per-call one-block stream, as with read.
            MemoryKind::Code => {
                let mut msg = [0u8; 16];
                msg[0] = CMD_WRITE_CODE;
                le16(&mut msg, 2, req.len.min(u32::from(u16::MAX)) as u16);
                le32(&mut msg, 4, req.address.min(u64::from(u32::MAX)) as u32);
                le32(&mut msg, 12, req.len);
                let mut init = msg;
                le32(&mut init, 8, 1); // block_count, init only
                self.send(&init)?;

                let mut pkt = Vec::with_capacity(16 + data.len());
                pkt.extend_from_slice(&msg); // header with [8..11] zeroed
                pkt.extend_from_slice(data);
                self.tx.send(EP_DAT_OUT, &pkt)
            }
            // MP_DATA (t76.c:1168-1175): 16-byte 0x11 init, raw data on EP05.
            MemoryKind::Data => {
                let mut msg = [0u8; 16];
                msg[0] = CMD_WRITE_DATA;
                le16(&mut msg, 2, req.len.min(u32::from(u16::MAX)) as u16);
                le32(&mut msg, 4, req.address.min(u64::from(u32::MAX)) as u32);
                le32(&mut msg, 12, req.len);
                self.send(&msg)?;
                self.tx.send(EP_DAT_OUT, data)
            }
            // MP_USER (t76.c:1178-1181): single 16+len packet on EP01.
            MemoryKind::User => {
                let mut msg = vec![0u8; 16 + data.len()];
                msg[0] = CMD_WRITE_USER_DATA;
                le16(&mut msg, 2, req.len.min(u32::from(u16::MAX)) as u16);
                le32(&mut msg, 4, req.address.min(u64::from(u32::MAX)) as u32);
                le32(&mut msg, 12, req.len);
                msg[16..].copy_from_slice(data);
                self.send(&msg)
            }
            MemoryKind::Data2 | MemoryKind::Config => {
                Err(Error::Unsupported("T76 write_block: Data2/Config spaces"))
            }
        }
    }

    /// `t76_erase` (t76.c:1467-1489) plus the NAND/eMMC loops.
    fn erase(&mut self, s: &Session, kind: EraseKind) -> Result<()> {
        let params = ChipParams::from_device(&s.device);
        match kind {
            EraseKind::Chip => match s.device.protocol_id {
                ALG_NAND => self.nand_erase(&params, s.device.code_size),
                ALG_EMMC => self.emmc_erase(s.device.code_size),
                _ => {
                    // Generic: 16-byte 0x0e (num_fuses/pld zero — the C
                    // caller passes the device's fuse count, which comes from
                    // the fuse-config profile, not the chip DB entry), then
                    // drain the 64-byte reply (t76.c:1479-1488).
                    let mut msg = [0u8; 16];
                    msg[0] = CMD_ERASE;
                    command(self.tx.as_mut(), EP_MSG_OUT, EP_MSG_IN, &msg, 64)?.discard()
                }
            },
            EraseKind::Sector { address } => {
                if s.device.protocol_id == ALG_NAND {
                    let (wbuf, ppb) = params.nand_geometry()?;
                    let block_size = u64::from(wbuf) * u64::from(ppb);
                    let blk = (address / block_size).min(u64::from(u32::MAX)) as u32;
                    self.nand_erase_block(blk).map(|_| ())
                } else {
                    Err(Error::Unsupported("T76 sector erase is NAND-only"))
                }
            }
            EraseKind::Fuses => {
                let mut msg = [0u8; 16];
                msg[0] = CMD_ERASE;
                command(self.tx.as_mut(), EP_MSG_OUT, EP_MSG_IN, &msg, 64)?.discard()
            }
        }
    }

    /// Blank check by reading back the region and testing for erased flash
    /// (0xff). The T76 has no dedicated blank-check opcode; the C tool reads
    /// and compares host-side too.
    fn blank_check(&mut self, s: &Session, region: Region) -> Result<bool> {
        let step = u64::from(match s.device.page_size {
            0 => 4096,
            n => n,
        });
        let mut done = 0u64;
        while done < region.len {
            let len = step.min(region.len - done) as u32;
            let req = BlockReq { kind: region.kind, address: region.offset + done, len };
            let block = self.read_block(s, &req)?;
            if block.iter().any(|&b| b != 0xff) {
                return Ok(false);
            }
            done += u64::from(len);
        }
        Ok(true)
    }
}

impl EmmcOps for T76 {
    /// CMD6 SWITCH to a hardware partition (t76_emmc_switch_partition,
    /// t76.c:360-363), updating the reported capacity from the EXT_CSD
    /// geometry captured at bring-up.
    fn select_partition(&mut self, _s: &Session, part: Partition) -> Result<()> {
        self.emmc_cmd27(EMMC_OP_SWITCH, partition_config(part))?;
        self.emmc_capacity = self.emmc_geom.capacity(part);
        Ok(())
    }

    /// Real capacity from EXT_CSD SEC_COUNT (t76.c:479-494), per the
    /// currently selected partition.
    fn capacity(&self) -> u64 {
        self.emmc_capacity
    }
}

impl PinTest for T76 {
    /// `t76_pin_test` (t76.c:1563-1591): the 8-byte 0x3e form, draining the
    /// 32-byte result. Unlike the C this does NOT end the transaction — the
    /// session token stays live and `end()` remains the caller's job.
    ///
    /// TODO(hw): the C never actually decodes the 32-byte reply (its `value`
    /// stays 0 — a long-standing bug), and the vendor calls it a bad-pin
    /// bitmask (t76.c:237). Decoded here as an LSB-first bitmask over 40
    /// socket positions, mapped to package pins with the C's mask rule;
    /// validate set-bit polarity on hardware before trusting the pin list.
    fn contact_check(&mut self, s: &Session) -> Result<Vec<u8>> {
        let mut msg = [0u8; 8];
        msg[0] = CMD_PIN_DETECTION;
        let resp = self.cmd(&msg, 32)?;

        let pin_count = u16::from(s.device.package.pin_count);
        let mut open = Vec::new();
        for pos in 0..40u16 {
            let (byte, bit) = (usize::from(pos / 8), pos % 8);
            if byte < resp.len() && resp[byte] & (1 << bit) != 0 {
                let logical = pos + 1;
                // The C pin mapping (t76.c:1579-1581): low half direct, high
                // half counted back from pin 40.
                let pin = if logical <= pin_count / 2 {
                    logical
                } else if pin_count + logical >= 40 {
                    pin_count + logical - 40
                } else {
                    continue;
                };
                if pin >= 1 && pin <= pin_count {
                    open.push(pin as u8);
                }
            }
        }
        Ok(open)
    }
}

impl FirmwareUpdate for T76 {
    /// `t76_firmware_update` (t76.c:1789-2056), transport only: the caller
    /// supplies the raw `updateT76.dat` bytes and has already confirmed the
    /// update (no interactive prompt here).
    ///
    /// File layout (t76.c:1773-1783): 16-byte header — version, CRC,
    /// unknown, block count — then `blocks` x 284-byte (0x114) blocks.
    fn update(&mut self, image: &[u8]) -> Result<()> {
        // Size / version / block-count / CRC validation (t76.c:1800-1853).
        if image.len() < 16 || image.len() > 1_048_576 {
            return Err(Error::Format("updateT76.dat: bad file size".into()));
        }
        let version = u32::from_le_bytes([image[0], image[1], image[2], image[3]]);
        if version & UPDATE_FILE_VERS_MASK != UPDATE_FILE_VERSION {
            return Err(Error::Format("updateT76.dat: bad file version".into()));
        }
        let blocks = u32::from_le_bytes([image[12], image[13], image[14], image[15]]) as usize;
        if blocks * 0x114 + 16 != image.len() {
            return Err(Error::Format("updateT76.dat: block count/size mismatch".into()));
        }
        // The C `crc_32` (minipro.c:68-78) is CRC-32/IEEE with initial
        // 0xffffffff and NO final complement, i.e. `!crc32fast::hash`.
        let crc = !crc32fast::hash(&image[16..]);
        let stored = u32::from_le_bytes([image[4], image[5], image[6], image[7]]);
        if crc != stored {
            return Err(Error::Format("updateT76.dat: CRC mismatch".into()));
        }

        // Switch to the bootloader (t76.c:1874-1898).
        // TODO(hw): the C only switches when the device reports
        // MP_STATUS_NORMAL and then re-opens the re-enumerated device; the
        // bootloader status query lives in minipro.c, outside this driver.
        // Here the switch is unconditional and the transport's reset() must
        // re-arm the same device — validate the re-enumeration path on
        // hardware.
        let mut msg = [0u8; 8];
        msg[0] = CMD_SWITCH;
        msg[1] = 0xaa;
        le32(&mut msg, 4, BTLDR_MAGIC);
        let resp = self.cmd(&msg, 8)?;
        if resp.len() < 2 || resp[1] != 0 {
            return Err(Error::Protocol);
        }
        self.reboot()?;

        // Erase (t76.c:1928-1948).
        let mut msg = [0u8; 8];
        msg[0] = CMD_BOOTLOADER_ERASE;
        msg[1] = 0xaa;
        let resp = self.cmd(&msg, 8)?;
        if resp.len() < 2 || resp[1] != 0 {
            return Err(Error::Protocol);
        }

        // Begin write — no reply (t76.c:1957-1963).
        let mut msg = [0u8; 8];
        msg[0] = CMD_BOOTLOADER_WRITE;
        msg[1] = 0xaa;
        self.send(&msg)?;

        // 284-byte blocks in 0x11c-byte packets, 8-byte status each
        // (t76.c:1965-1999).
        let mut address = 0u32;
        for i in 0..blocks {
            let mut pkt = [0u8; 0x11c];
            pkt[0] = CMD_BOOTLOADER_WRITE;
            pkt[2] = 0x00; // data length LSB
            pkt[3] = 0x01; // data length MSB (0x100)
            le32(&mut pkt, 4, address);
            let start = 16 + i * 0x114;
            pkt[8..8 + 0x114].copy_from_slice(&image[start..start + 0x114]);
            let resp = self.cmd(&pkt, 8)?;
            if resp.len() < 2 || resp[1] != 0 {
                return Err(Error::Protocol);
            }
            address += 256;
        }

        // Last block: fixed address + CRC in a 0x108-byte packet
        // (t76.c:2002-2029).
        let mut pkt = [0u8; 0x108];
        pkt[0] = CMD_BOOTLOADER_WRITE;
        pkt[1] = 0x03;
        pkt[2] = 0x00;
        pkt[3] = 0x01;
        le32(&mut pkt, 4, LAST_BLOCK_ADDR);
        le32(&mut pkt, 8, LAST_BLOCK_CRC);
        let resp = self.cmd(&pkt, 8)?;
        if resp.len() < 2 || resp[1] != 0 {
            return Err(Error::Protocol);
        }

        // Back to normal mode (t76.c:2034-2047).
        self.reboot()
    }
}

// ===========================================================================
// Tests — MockTransport for single command/response exchanges, plus a local
// scripted transport for the multi-packet sequences (MockTransport advances
// its cursor only on recv, so reply-less packets — bitstream blocks,
// BEGIN/END_TRANS, timing writes — can't be scripted with it).
// ===========================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use minipro_core::device::{Algorithm, Package};
    use minipro_usb::MockTransport;
    use std::collections::VecDeque;

    /// Records every OUT packet (with its endpoint) and pops canned IN
    /// responses in order, logging the requested (ep, len).
    struct ScriptedTx {
        sent: Vec<(u8, Vec<u8>)>,
        replies: VecDeque<Vec<u8>>,
        recv_log: Vec<(u8, usize)>,
    }

    impl ScriptedTx {
        fn new(replies: Vec<Vec<u8>>) -> Self {
            ScriptedTx { sent: Vec::new(), replies: replies.into(), recv_log: Vec::new() }
        }
    }

    impl Transport for ScriptedTx {
        fn send(&mut self, ep: Ep, data: &[u8]) -> Result<()> {
            self.sent.push((ep.0, data.to_vec()));
            Ok(())
        }
        fn recv(&mut self, ep: Ep, len: usize) -> Result<Vec<u8>> {
            self.recv_log.push((ep.0, len));
            self.replies.pop_front().ok_or(Error::Protocol)
        }
        fn link_speed(&self) -> LinkSpeed {
            LinkSpeed::High
        }
        fn reset(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn device(protocol_id: u8, bitstream: Vec<u8>) -> Device {
        Device {
            name: "TEST".into(),
            protocol_id,
            code_size: 0x8000,
            data_size: 0x100,
            page_size: 0x40,
            chip_id: 0x1234,
            chip_id_bytes: 2,
            package: Package { pin_count: 8, name: "DIP8".into() },
            algorithm: Some(Algorithm { name: "TESTALG".into(), bitstream }),
            fw_target: FwVersion(0),
            ..Device::default()
        }
    }

    fn session(dev: &Device) -> Session {
        Session { device: dev.clone(), emmc_capacity: 0 }
    }

    /// A `Send`-able handle to a shared `ScriptedTx`, so tests can hand the
    /// transport to `T76::new(Box<dyn Transport>)` and still inspect the
    /// recorded traffic afterwards.
    #[derive(Clone)]
    struct SharedTx(std::sync::Arc<std::sync::Mutex<ScriptedTx>>);

    impl SharedTx {
        fn new(replies: Vec<Vec<u8>>) -> Self {
            SharedTx(std::sync::Arc::new(std::sync::Mutex::new(ScriptedTx::new(replies))))
        }
        fn sent(&self) -> Vec<(u8, Vec<u8>)> {
            self.0.lock().unwrap().sent.clone()
        }
        fn recv_log(&self) -> Vec<(u8, usize)> {
            self.0.lock().unwrap().recv_log.clone()
        }
    }

    impl Transport for SharedTx {
        fn send(&mut self, ep: Ep, data: &[u8]) -> Result<()> {
            self.0.lock().unwrap().send(ep, data)
        }
        fn recv(&mut self, ep: Ep, len: usize) -> Result<Vec<u8>> {
            self.0.lock().unwrap().recv(ep, len)
        }
        fn link_speed(&self) -> LinkSpeed {
            LinkSpeed::High
        }
        fn reset(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn t76_with(replies: Vec<Vec<u8>>) -> (T76, SharedTx) {
        let tx = SharedTx::new(replies);
        (T76::new(Box::new(tx.clone())), tx)
    }

    // -----------------------------------------------------------------------
    // Capability wiring
    // -----------------------------------------------------------------------

    #[test]
    fn caps_agree_with_accessors() {
        let (mut t76, _tx) = t76_with(vec![]);
        let caps = t76.caps();
        assert_eq!(caps.contains(Caps::MEMORY), t76.memory().is_some());
        assert_eq!(caps.contains(Caps::EMMC), t76.emmc().is_some());
        assert_eq!(caps.contains(Caps::PINTEST), t76.pins().is_some());
        assert_eq!(caps.contains(Caps::FWUPDATE), t76.firmware().is_some());
        assert_eq!(caps.contains(Caps::FUSES), t76.fuses().is_some());
        assert_eq!(caps.contains(Caps::JEDEC), t76.jedec().is_some());
        assert_eq!(caps.contains(Caps::LOGIC), t76.logic().is_some());
    }

    // -----------------------------------------------------------------------
    // Bitstream upload (t76.c:116-189)
    // -----------------------------------------------------------------------

    #[test]
    fn write_bitstream_chunks_and_end() {
        // 1000 bytes -> one full 504-byte chunk + one 496-byte chunk.
        let bits: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        let dev = device(0x01, bits.clone());
        let (mut t76, tx) = t76_with(vec![
            vec![0x26, 0, 0, 0, 0, 0, 0, 0], // BEGIN_BS ok
            vec![0x26, 0, 0, 0, 0, 0, 0, 0], // END_BS ok
        ]);
        t76.write_bitstream(&dev).unwrap();

        let sent = tx.sent();
        assert_eq!(sent.len(), 4);
        // BEGIN_BS: opcode, sub, packet size 0x200 LE, total length LE32
        // (t76.c:125-128).
        assert_eq!(sent[0].0, 0x01);
        assert_eq!(sent[0].1, vec![0x26, 0x00, 0x00, 0x02, 0xe8, 0x03, 0x00, 0x00]);
        // Chunk 1: full 504-byte payload.
        assert_eq!(sent[1].1.len(), 512);
        assert_eq!(&sent[1].1[..8], &[0x26, 0x01, 0xf8, 0x01, 0, 0, 0, 0]);
        assert_eq!(&sent[1].1[8..], &bits[..504]);
        // Chunk 2: 496 bytes of payload; the buffer tail past block_size
        // still holds chunk 1's bytes 496..504 (the C reuses the buffer
        // without re-zeroing, t76.c:143-158).
        assert_eq!(sent[2].1.len(), 512);
        assert_eq!(&sent[2].1[..8], &[0x26, 0x01, 0xf0, 0x01, 0, 0, 0, 0]);
        assert_eq!(&sent[2].1[8..8 + 496], &bits[504..1000]);
        assert_eq!(&sent[2].1[504..], &bits[496..504]);
        // END_BS: non-NAND leaves msg[2..3] zero (t76.c:167-175).
        assert_eq!(sent[3].1, vec![0x26, 0x02, 0x00, 0x00, 0, 0, 0, 0]);
        // Both replies drained from EP81, 8 bytes each.
        assert_eq!(tx.recv_log(), vec![(0x81, 8), (0x81, 8)]);
    }

    #[test]
    fn write_bitstream_nand_end_carries_last_block_size() {
        let bits = vec![0xa5u8; 1000]; // last block = 1000 % 504 = 496 = 0x1f0
        let dev = device(ALG_NAND, bits);
        let (mut t76, tx) = t76_with(vec![
            vec![0x26, 0, 0, 0, 0, 0, 0, 0],
            vec![0x26, 0, 0, 0, 0, 0, 0, 0],
        ]);
        t76.write_bitstream(&dev).unwrap();
        let sent = tx.sent();
        // END_BS carries the final partial block size for NAND (t76.c:170-175).
        assert_eq!(sent[3].1, vec![0x26, 0x02, 0xf0, 0x01, 0, 0, 0, 0]);
    }

    #[test]
    fn write_bitstream_skips_reupload_of_same_algorithm() {
        let dev = device(0x01, vec![0u8; 504]);
        let (mut t76, tx) = t76_with(vec![
            vec![0x26, 0, 0, 0, 0, 0, 0, 0],
            vec![0x26, 0, 0, 0, 0, 0, 0, 0],
        ]);
        t76.write_bitstream(&dev).unwrap();
        let count = tx.sent().len();
        t76.write_bitstream(&dev).unwrap(); // same algorithm -> no traffic
        assert_eq!(tx.sent().len(), count);
    }

    #[test]
    fn write_bitstream_error_status_is_protocol_error() {
        let dev = device(0x01, vec![0u8; 10]);
        // BEGIN_BS reply has msg[1] != 0 -> not okay to begin (t76.c:135-136).
        let (mut t76, _tx) = t76_with(vec![vec![0x26, 0x01, 0, 0, 0, 0, 0, 0]]);
        let err = t76.write_bitstream(&dev).unwrap_err();
        assert_eq!(err.code(), "protocol");
    }

    // -----------------------------------------------------------------------
    // Adapter init (t76.c:221-279)
    // -----------------------------------------------------------------------

    #[test]
    fn adapter_init_sequence() {
        let (mut t76, tx) = t76_with(vec![
            vec![0u8; 8],    // 0x24 f0 power-down reply
            vec![0u8; 0x30], // 0x24 e4 adapter-id reply
            vec![0u8; 32],   // pin detection 1
            vec![0u8; 32],   // pin detection 2
        ]);
        t76.adapter_init().unwrap();

        let sent = tx.sent();
        assert_eq!(sent.len(), 5);
        assert_eq!(sent[0].1, vec![0x24, 0xf0, 0x08, 0x00, 0x01, 0x00, 0x00, 0x00]); // t76.c:223
        assert_eq!(sent[1].1, vec![0x24, 0xe4, 0x30, 0x00, 0x11, 0x01, 0x08, 0x00]); // t76.c:225
        assert_eq!(sent[2].1, vec![0x24, 0xf1, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // t76.c:227
        // Pin detection: 16-byte form, run twice (t76.c:238-245).
        let mut pd = vec![0u8; 16];
        pd[0] = 0x3e;
        assert_eq!(sent[3].1, pd);
        assert_eq!(sent[4].1, pd);
        // Drain lengths: 8 for power-down, 0x30 for the id, none for
        // power-up (recv_len 0), 32 per pin detection.
        assert_eq!(tx.recv_log(), vec![(0x81, 8), (0x81, 0x30), (0x81, 32), (0x81, 32)]);
    }

    #[test]
    fn emmc_adapter_init_sequence() {
        let (mut t76, tx) = t76_with(vec![
            vec![0u8; 8],    // power-down reply
            vec![0u8; 0x28], // e0 init reply
            vec![0u8; 32],   // single pin detection
        ]);
        t76.emmc_adapter_init().unwrap();
        let sent = tx.sent();
        assert_eq!(sent.len(), 4);
        assert_eq!(sent[1].1, vec![0x24, 0xe0, 0x28, 0x00, 0, 0, 0, 0, 0, 0, 0, 0]); // t76.c:261
        assert_eq!(tx.recv_log(), vec![(0x81, 8), (0x81, 0x28), (0x81, 32)]);
    }

    // -----------------------------------------------------------------------
    // BEGIN_TRANS packing (t76.c:503-848)
    // -----------------------------------------------------------------------

    #[test]
    fn begin_trans_common_header() {
        let p = ChipParams {
            protocol_id: 0x07,
            variant: 0x1102,
            icsp: 1,
            raw_voltages: 0x0021_43f0,
            chip_info: 0xaa,
            pin_map: 0xbb,
            data_memory_size: 0x1234,
            page_size: 0x0040,
            pulse_delay: 0x5678,
            data_memory2_size: 0x9abc,
            code_memory_size: 0x0008_0000,
            i2c_address: 0xa0,
            spi_clock: 3,
            packed_package: 0xdead_beef,
            read_buffer_size: 0x200,
            raw_flags: 0x1122_3344,
            ..ChipParams::default()
        };
        let (msg, len) = pack_begin_trans(&p);
        assert_eq!(len, 64); // no chip-class extension for protocol 0x07
        assert_eq!(msg[0], 0x03);
        assert_eq!(msg[1], 0x07);
        assert_eq!(msg[2], 0x02); // variant low byte
        assert_eq!(msg[3], 1); // icsp
        assert_eq!(&msg[4..6], &[0xf0, 0x43]); // raw_voltages LE16
        assert_eq!(msg[6], 0xaa);
        assert_eq!(msg[7], 0xbb);
        assert_eq!(&msg[8..10], &[0x34, 0x12]);
        assert_eq!(&msg[10..12], &[0x40, 0x00]);
        assert_eq!(&msg[12..14], &[0x78, 0x56]);
        assert_eq!(&msg[14..16], &[0xbc, 0x9a]);
        assert_eq!(&msg[16..20], &[0x00, 0x00, 0x08, 0x00]);
        // Voltage bytes (t76.c:536-546): (raw>>16)=0x21 at [20]; low nibble
        // split since (raw & 0xf0) == 0xf0 takes the [22]=raw branch.
        assert_eq!(msg[20], 0x21);
        assert_eq!(msg[22], 0xf0);
        assert_eq!(msg[24], 0xa0); // i2c address
        assert_eq!(msg[28], 3); // spi clock
        assert_eq!(&msg[40..44], &[0xef, 0xbe, 0xad, 0xde]);
        assert_eq!(&msg[44..46], &[0x00, 0x02]);
        assert_eq!(&msg[56..60], &[0x44, 0x33, 0x22, 0x11]);
        assert_eq!(msg[63], 0x11); // algorithm number = variant >> 8
    }

    #[test]
    fn begin_trans_spi_extension_8p_and_16p() {
        // 8-pin (variant high byte 0x11, t76.c:591-595).
        let p8 = ChipParams { protocol_id: ALG_SPI25F_1, variant: 0x1100, ..Default::default() };
        let (msg, len) = pack_begin_trans(&p8);
        assert_eq!(len, 128);
        assert_eq!(&msg[0x40..0x44], &[0x00, 0x00, 0x00, 0x08]); // 0x08000000 LE
        assert_eq!(&msg[0x50..0x54], &[0x00, 0x00, 0x80, 0x00]); // 0x00800000 LE
        assert_eq!(&msg[0x60..0x64], &[0x2f, 0x17, 0x05, 0x0f]); // 0x0f05172f LE
        assert_eq!(msg[0x65], 0x03);

        // 16-pin (variant high byte 0x21, t76.c:586-590).
        let p16 = ChipParams { protocol_id: ALG_SPI25F_2, variant: 0x2100, ..Default::default() };
        let (msg, len) = pack_begin_trans(&p16);
        assert_eq!(len, 128);
        assert_eq!(&msg[0x40..0x44], &[0x00, 0x00, 0x02, 0x00]); // 0x00020000 LE
        assert_eq!(&msg[0x50..0x54], &[0x00, 0x00, 0x00, 0x02]); // 0x02000000 LE
    }

    #[test]
    fn begin_trans_parallel_nor_x16_extension() {
        // S29GL512N-style: family byte 0x0b, adapter nibble 0x50, geometry 8
        // -> msg[0x48] = 0x800 (t76.c:616-648).
        let p = ChipParams {
            protocol_id: ALG_T48,
            variant: 0x0058,
            packed_package: 0x0000_000b,
            ..Default::default()
        };
        let (msg, len) = pack_begin_trans(&p);
        assert_eq!(len, 128);
        assert_eq!(&msg[0x40..0x44], &[0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&msg[0x44..0x48], &[0x40, 0x00, 0x00, 0x00]);
        assert_eq!(&msg[0x48..0x4c], &[0x00, 0x08, 0x00, 0x00]); // 0x800 LE
        assert_eq!(&msg[0x50..0x54], &[0x00, 0x00, 0x00, 0x10]);
        assert_eq!(&msg[0x54..0x58], &[0x00, 0x80, 0x00, 0x00]);
        assert_eq!(&msg[0x60..0x64], &[0x2f, 0x17, 0x05, 0x0f]);
        assert_eq!(msg[0x65], 0x03);
    }

    /// W29N02GZ geometry: 2048-byte page + 64 spare, 64 pages/block —
    /// the hardware-validated capture in t76.c:731-739.
    fn w29n02gz() -> ChipParams {
        ChipParams {
            protocol_id: ALG_NAND,
            variant: 0x0000, // parallel: variant & 0x70 == 0
            page_size: 2048,
            write_buffer_size: 2112,
            pages_per_block: 64,
            code_memory_size: 0x1080_0000,
            ..Default::default()
        }
    }

    #[test]
    fn begin_trans_nand_adjustments() {
        let (msg, len) = pack_begin_trans(&w29n02gz());
        assert_eq!(len, 128);
        // msg[0x10] = per-block transfer size 2112 * 64 = 0x21000 (t76.c:665-669).
        assert_eq!(&msg[16..20], &[0x00, 0x10, 0x02, 0x00]);
        // NAND flag bit 0x800 (t76.c:670-672).
        assert_eq!(&msg[56..60], &[0x00, 0x08, 0x00, 0x00]);
        // Clock tier + cfg (t76.c:673-675).
        assert_eq!(&msg[0x60..0x64], &[0x2f, 0x27, 0x09, 0x0b]); // 0x0b09272f LE
        assert_eq!(msg[0x65], 0x03);
        // Forced capture-matched bytes (t76.c:683-702).
        assert_eq!(msg[0x0e], 0x20);
        assert_eq!(msg[0x18], 0x03);
        assert_eq!(msg[0x1c], 0x03);
        assert_eq!(&msg[0x28..0x2c], &[0x00, 0x00, 0x00, 0xe2]); // 0xe2000000 LE
        assert_eq!(msg[0x30], 0x40);
    }

    #[test]
    fn nand_prelude_matches_w29n02gz_capture() {
        // Expected values per the vendor packer field map (t76.c:717-739):
        // real_page 2048, spare 64, ps_code 8 (page == 0x800), busw 3
        // (page >= 0x800 and ppb*page > 0x10000), parallel clock/adapter.
        let pre = pack_nand_prelude(&w29n02gz()).unwrap();
        assert_eq!(pre[0], 0x02);
        assert_eq!(&pre[0x08..0x0a], &[64, 0]); // spare
        assert_eq!(&pre[0x0a..0x0c], &[0x00, 0x08]); // real page 2048
        assert_eq!(&pre[0x0c..0x0e], &[0x00, 0x08]); // page_or_blocks
        assert_eq!(&pre[0x0e..0x10], &[64, 0]); // pages per block
        assert_eq!(&pre[0x10..0x12], &[1, 0]); // planes
        assert_eq!(&pre[0x12..0x14], &[1, 0]); // LUNs
        assert_eq!(&pre[0x14..0x18], &[3, 0, 0, 0]); // bus/width code
        assert_eq!(&pre[0x18..0x1c], &[8, 0, 0, 0]); // page-size code
        assert_eq!(&pre[0x1c..0x20], &[0, 0, 0, 0]);
        assert_eq!(&pre[0x20..0x24], &[0x00, 0x00, 0x01, 0x00]); // adapter 0x00010000
        assert_eq!(&pre[0x24..0x28], &[0x3b, 0x4f, 0x15, 0x27]); // clock 0x27154f3b
    }

    #[test]
    fn nand_prelude_requires_geometry() {
        let p = ChipParams { protocol_id: ALG_NAND, ..Default::default() };
        assert_eq!(pack_nand_prelude(&p).unwrap_err().code(), "unsupported");
    }

    #[test]
    fn begin_trans_emmc_bus_mode_byte() {
        let p = ChipParams { protocol_id: ALG_EMMC, variant: 0x5300, ..Default::default() };
        let (msg, len) = pack_begin_trans(&p);
        assert_eq!(len, 128);
        assert_eq!(msg[0x0c], 0x53); // t76.c:818
    }

    // -----------------------------------------------------------------------
    // Full begin() over the wire (generic EPROM-style device)
    // -----------------------------------------------------------------------

    #[test]
    fn begin_generic_device_sequence() {
        let dev = device(0x07, vec![0x11u8; 504]);
        let (mut t76, tx) = t76_with(vec![
            vec![0x26, 0, 0, 0, 0, 0, 0, 0], // BEGIN_BS ok
            vec![0x26, 0, 0, 0, 0, 0, 0, 0], // END_BS ok
            vec![0u8; 32],                   // 0x39 status: msg[12] = 0, no OVC
        ]);
        let s = t76.begin(&dev).unwrap();
        assert_eq!(s.emmc_capacity, 0);

        let sent = tx.sent();
        assert_eq!(sent.len(), 5); // BEGIN_BS, chunk, END_BS, BEGIN_TRANS, 0x39
        // BEGIN_TRANS: 64 bytes for a plain device, header from the Device
        // fields the core carries.
        let bt = &sent[3].1;
        assert_eq!(bt.len(), 64);
        assert_eq!(bt[0], 0x03);
        assert_eq!(bt[1], 0x07);
        assert_eq!(&bt[8..10], &[0x00, 0x01]); // data_size 0x100
        assert_eq!(&bt[10..12], &[0x40, 0x00]); // page_size 0x40
        assert_eq!(&bt[16..20], &[0x00, 0x80, 0x00, 0x00]); // code_size 0x8000
        // Plain 0x39 (non-NAND/eMMC leaves the header zeroed, t76.c:1494-1511).
        assert_eq!(sent[4].1, vec![0x39, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn begin_reports_overcurrent() {
        let dev = device(0x07, vec![0x11u8; 504]);
        let mut ovc = vec![0u8; 32];
        ovc[12] = 1; // OVC flag set (t76.c:1523)
        let (mut t76, _tx) = t76_with(vec![
            vec![0x26, 0, 0, 0, 0, 0, 0, 0],
            vec![0x26, 0, 0, 0, 0, 0, 0, 0],
            ovc,
        ]);
        let err = match t76.begin(&dev) {
            Err(e) => e,
            Ok(_) => panic!("begin should fail on overcurrent"),
        };
        assert_eq!(err.code(), "overcurrent");
        assert!(err.to_string().contains("overcurrent"));
    }

    #[test]
    fn end_sends_bare_end_trans() {
        let dev = device(0x07, vec![]);
        let (mut t76, tx) = t76_with(vec![]);
        t76.end(session(&dev)).unwrap();
        assert_eq!(tx.sent(), vec![(0x01, vec![0x04, 0, 0, 0, 0, 0, 0, 0])]);
    }

    // -----------------------------------------------------------------------
    // Chip id (t76.c:1263-1294)
    // -----------------------------------------------------------------------

    #[test]
    fn identify_big_endian_type1() {
        // MockTransport works for a strict command/response pair.
        let mut resp = vec![0u8; 32];
        resp[0] = 0x01; // MP_ID_TYPE1 -> big-endian
        resp[2] = 0xc2;
        resp[3] = 0x34;
        let script = vec![(vec![0x05, 0, 0, 0, 0, 0, 0, 0], resp)];
        let mut t76 = T76::new(Box::new(MockTransport::from_script(script)));
        let dev = device(0x07, vec![]);
        let id = t76.identify(&session(&dev)).unwrap();
        assert_eq!(id.raw, 0xc234);
        assert_eq!(id.bytes, 2);
    }

    #[test]
    fn identify_little_endian_type3() {
        let mut resp = vec![0u8; 32];
        resp[0] = ID_TYPE3;
        resp[2] = 0xc2;
        resp[3] = 0x34;
        let script = vec![(vec![0x05, 0, 0, 0, 0, 0, 0, 0], resp)];
        let mut t76 = T76::new(Box::new(MockTransport::from_script(script)));
        let dev = device(0x07, vec![]);
        let id = t76.identify(&session(&dev)).unwrap();
        assert_eq!(id.raw, 0x34c2); // LE load of c2 34
    }

    // -----------------------------------------------------------------------
    // Generic read/write block (t76.c:937-1186)
    // -----------------------------------------------------------------------

    #[test]
    fn read_block_code_packet_and_payload() {
        let dev = device(0x07, vec![]);
        let payload = vec![0x5au8; 0x40];
        let (mut t76, tx) = t76_with(vec![payload.clone()]);
        let req = BlockReq { kind: MemoryKind::Code, address: 0x1000, len: 0x40 };
        let got = t76.read_block(&session(&dev), &req).unwrap();
        assert_eq!(got, payload);
        // 16-byte 0x0d: size LE16 at [2], address LE32 at [4], block count at
        // [8] (t76.c:938-944).
        assert_eq!(
            tx.sent(),
            vec![(
                0x01,
                vec![0x0d, 0, 0x40, 0x00, 0x00, 0x10, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0]
            )]
        );
        // Payload drained from bulk EP82.
        assert_eq!(tx.recv_log(), vec![(0x82, 0x40)]);
    }

    #[test]
    fn read_block_data_strips_16_byte_header() {
        let dev = device(0x07, vec![]);
        let mut raw = vec![0xeeu8; 16]; // header junk
        raw.extend(vec![0x77u8; 8]);
        let (mut t76, tx) = t76_with(vec![raw]);
        let req = BlockReq { kind: MemoryKind::Data, address: 4, len: 8 };
        let got = t76.read_block(&session(&dev), &req).unwrap();
        assert_eq!(got, vec![0x77u8; 8]);
        let sent = tx.sent();
        assert_eq!(
            sent[0].1,
            vec![0x10, 0, 0x08, 0x00, 0x04, 0x00, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(tx.recv_log(), vec![(0x82, 24)]); // size + 16 on EP82
    }

    #[test]
    fn read_block_user_uses_msg_endpoint() {
        let dev = device(0x07, vec![]);
        let mut raw = vec![0u8; 16];
        raw.extend([1, 2, 3, 4]);
        let (mut t76, tx) = t76_with(vec![raw]);
        let req = BlockReq { kind: MemoryKind::User, address: 0, len: 4 };
        let got = t76.read_block(&session(&dev), &req).unwrap();
        assert_eq!(got, vec![1, 2, 3, 4]);
        // MP_USER reply comes back on EP81 (msg_recv), not EP82 (t76.c:992).
        assert_eq!(tx.recv_log(), vec![(0x81, 20)]);
    }

    #[test]
    fn write_block_code_init_then_payload() {
        let dev = device(0x07, vec![]);
        let data = vec![0xabu8; 0x40];
        let (mut t76, tx) = t76_with(vec![]);
        let req = BlockReq { kind: MemoryKind::Code, address: 0x200, len: 0x40 };
        t76.write_block(&session(&dev), &req, &data).unwrap();

        let sent = tx.sent();
        assert_eq!(sent.len(), 2);
        // Init on EP01: 0x0c with size/address/block_count/size (t76.c:1136-1151).
        assert_eq!(sent[0].0, 0x01);
        assert_eq!(
            sent[0].1,
            vec![0x0c, 0, 0x40, 0x00, 0x00, 0x02, 0x00, 0x00, 0x01, 0, 0, 0, 0x40, 0, 0, 0]
        );
        // Data packet on EP05: same header with block_count zeroed + data
        // (t76.c:1153-1163).
        assert_eq!(sent[1].0, 0x05);
        assert_eq!(
            &sent[1].1[..16],
            &[0x0c, 0, 0x40, 0x00, 0x00, 0x02, 0x00, 0x00, 0, 0, 0, 0, 0x40, 0, 0, 0]
        );
        assert_eq!(&sent[1].1[16..], &data[..]);
    }

    #[test]
    fn write_block_user_single_packet() {
        let dev = device(0x07, vec![]);
        let data = [9u8, 8, 7, 6];
        let (mut t76, tx) = t76_with(vec![]);
        let req = BlockReq { kind: MemoryKind::User, address: 0, len: 4 };
        t76.write_block(&session(&dev), &req, &data).unwrap();
        let sent = tx.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, 0x01); // EP01, not the bulk pipe (t76.c:1181)
        assert_eq!(sent[0].1.len(), 20);
        assert_eq!(&sent[0].1[16..], &data);
    }

    // -----------------------------------------------------------------------
    // NAND read/write (t76.c:891-905, 1092-1133)
    // -----------------------------------------------------------------------

    #[test]
    fn nand_read_block_packet() {
        let mut dev = device(ALG_NAND, vec![]);
        dev.code_size = 0x1080_0000;
        let block = vec![0xffu8; 0x21000];
        let (mut t76, tx) = t76_with(vec![block.clone()]);
        // Block 2: address = 2 * 0x21000.
        let req = BlockReq { kind: MemoryKind::Code, address: 2 * 0x21000, len: 0x21000 };
        let got = t76.read_block(&session(&dev), &req).unwrap();
        assert_eq!(got.len(), block.len());
        // 16-byte 0x0d with block index at [2..3] + fixed NAND header
        // (t76.c:892-902).
        assert_eq!(
            tx.sent(),
            vec![(
                0x01,
                vec![
                    0x0d, 0x00, 0x02, 0x00, // block index 2
                    0x10, 0x00, 0x04, 0x00, 0x08, 0x00, 0x08, 0x00, 0x69, 0x01, 0x00, 0x00,
                ]
            )]
        );
        assert_eq!(tx.recv_log(), vec![(0x82, 0x21000)]);
    }

    #[test]
    fn nand_write_block_pages_and_commit() {
        let mut dev = device(ALG_NAND, vec![]);
        dev.page_size = 2048;
        let s = session(&dev);
        // Geometry comes from ChipParams; the core Device can't carry it yet,
        // so exercise nand_write directly with the full params.
        let p = w29n02gz();
        let block_len = 2112u32 * 64;
        let data: Vec<u8> = (0..block_len).map(|i| i as u8).collect();
        let (mut t76, tx) = t76_with(vec![vec![0u8; 32]]); // 0x39 commit reply
        let req = BlockReq { kind: MemoryKind::Code, address: 0, len: block_len };
        t76.nand_write(&p, &req, &data).unwrap();
        let _ = s;

        let sent = tx.sent();
        // 1 init + 64 pages + 1 commit.
        assert_eq!(sent.len(), 66);
        // Init (t76.c:1099-1104): page+spare 2112 = 0x840 at [2..3] and
        // [12..15], block 0 at [4..7], ppb 64 at [8..11].
        assert_eq!(
            sent[0].1,
            vec![0x1f, 0, 0x40, 0x08, 0, 0, 0, 0, 64, 0, 0, 0, 0x40, 0x08, 0, 0]
        );
        // Every page packet: EP05, 16-byte header + 2112 bytes of page data.
        for (i, (ep, pkt)) in sent[1..65].iter().enumerate() {
            assert_eq!(*ep, 0x05);
            assert_eq!(pkt.len(), 16 + 2112);
            assert_eq!(pkt[0], 0x1f);
            assert_eq!(&pkt[16..], &data[i * 2112..(i + 1) * 2112]);
        }
        // Commit: plain 0x39, 32-byte status drained (t76.c:1127-1131).
        assert_eq!(sent[65].1, vec![0x39, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(tx.recv_log(), vec![(0x81, 32)]);
    }

    #[test]
    fn nand_erase_skips_bad_blocks() {
        // Two blocks: first factory-bad (0x3a resp[1] != 0, skipped — no 0x0e),
        // second good (checked then erased).
        let p = w29n02gz();
        let code_size = 2 * 2112 * 64; // exactly two blocks
        let (mut t76, tx) = t76_with(vec![
            vec![0x3a, 0x01, 0, 0, 0, 0, 0, 0], // block 0: bad
            vec![0x3a, 0x00, 0, 0, 0, 0, 0, 0], // block 1: good
            vec![0x0e, 0x00, 0, 0, 0, 0, 0, 0], // erase ok
        ]);
        t76.nand_erase(&p, code_size).unwrap();
        let sent = tx.sent();
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[0].1, vec![0x3a, 0, 0, 0, 0, 0, 0, 0]); // check blk 0
        assert_eq!(sent[1].1, vec![0x3a, 0, 1, 0, 0, 0, 0, 0]); // check blk 1
        // Erase is the 16-byte form (t76.c:1394-1397).
        let mut erase = vec![0u8; 16];
        erase[0] = 0x0e;
        erase[2] = 1;
        assert_eq!(sent[2].1, erase);
    }

    // -----------------------------------------------------------------------
    // eMMC (t76.c:339-501, 907-935, 1023-1063, 1419-1465)
    // -----------------------------------------------------------------------

    #[test]
    fn emmc_read_timing_and_init() {
        let dev = device(ALG_EMMC, vec![]);
        let payload = vec![0x42u8; 0x10000];
        let (mut t76, tx) = t76_with(vec![payload.clone()]);
        // 64 KiB block at byte offset 0x200000 -> LBA 0x1000.
        let req = BlockReq { kind: MemoryKind::Code, address: 0x20_0000, len: 0x10000 };
        let got = t76.read_block(&session(&dev), &req).unwrap();
        assert_eq!(got, payload);

        let sent = tx.sent();
        assert_eq!(sent.len(), 2);
        // PRE timing, default 8-bit width code 2 at [9] (t76.c:375-388).
        assert_eq!(
            sent[0].1,
            vec![
                0x27, 0x00, 0xff, 0x00, 0x3b, 0x0e, 0x05, 0x02, 0x00, 0x02, 0xb7, 0x03, 0x00,
                0x12, 0xb9, 0x03
            ]
        );
        // 40-byte 0x0d region init (t76.c:395-409): LBA 0x1000, one 64 KiB
        // block, geometry constants.
        let mut init = vec![0u8; 40];
        init[0] = 0x0d;
        init[1] = 0x01;
        init[4..8].copy_from_slice(&0x1000u32.to_le_bytes());
        init[8..12].copy_from_slice(&0x200u32.to_le_bytes());
        init[12..16].copy_from_slice(&0x20u32.to_le_bytes());
        init[16..20].copy_from_slice(&1u32.to_le_bytes());
        init[20..24].copy_from_slice(&0x80u32.to_le_bytes());
        init[24..28].copy_from_slice(&0x20u32.to_le_bytes());
        init[28..32].copy_from_slice(&0x04u32.to_le_bytes());
        init[32..36].copy_from_slice(&0x01u32.to_le_bytes());
        assert_eq!(sent[1].1, init);
        assert_eq!(tx.recv_log(), vec![(0x82, 0x10000)]);
    }

    #[test]
    fn emmc_write_setup_data_commit() {
        let dev = device(ALG_EMMC, vec![]);
        let data = vec![0x99u8; 0x10000];
        let (mut t76, tx) = t76_with(vec![
            vec![0u8; 8],  // op50 reply
            vec![0u8; 32], // first 0x39
            vec![0u8; 32], // second 0x39
        ]);
        let req = BlockReq { kind: MemoryKind::Code, address: 0, len: 0x10000 };
        t76.write_block(&session(&dev), &req, &data).unwrap();

        let sent = tx.sent();
        assert_eq!(sent.len(), 7);
        // 0x27 op 0x50 setup, ARG 0x20000 (t76.c:1029-1030).
        assert_eq!(sent[0].1, vec![0x27, 0x50, 0, 0, 0x00, 0x00, 0x02, 0x00]);
        // PRE timing, 0x1f init, then the raw 64 KiB block on EP05.
        assert_eq!(sent[1].1[0], 0x27);
        assert_eq!(sent[2].1[0], 0x1f);
        assert_eq!(sent[2].1.len(), 40);
        assert_eq!(sent[3].0, 0x05);
        assert_eq!(sent[3].1, data);
        // Commit: 0x39, POST timing (byte [5] = 0x2c variant), 0x39
        // (t76.c:1047-1061).
        assert_eq!(sent[4].1[0], 0x39);
        assert_eq!(sent[5].1[0], 0x27);
        assert_eq!(sent[5].1[5], 0x2c);
        assert_eq!(sent[6].1[0], 0x39);
    }

    #[test]
    fn emmc_bring_up_reads_ext_csd_and_selects_user() {
        // EXT_CSD reply: 8-byte header + 512 bytes; SEC_COUNT[212] at
        // buf[220..224], BOOT_SIZE_MULT[226] at buf[234], RPMB[168] at
        // buf[176] (t76.c:469-491).
        let mut ext = vec![0u8; 520];
        ext[220..224].copy_from_slice(&0x00e9_0000u32.to_le_bytes()); // KLM8G1GEAC
        ext[234] = 0x20;
        ext[176] = 0x02;
        let (mut t76, tx) = t76_with(vec![
            vec![0u8; 32], // 0x21 device id
            vec![0u8; 32], // 0x05 readid
            vec![0u8; 24], // 0x06
            ext,
            vec![0u8; 8], // CMD6 SWITCH ok
        ]);
        let cap = t76.emmc_bring_up().unwrap();
        assert_eq!(cap, 0x00e9_0000u64 * 512); // 7.28 GiB

        let sent = tx.sent();
        assert_eq!(sent.len(), 5);
        assert_eq!(sent[0].1[0], 0x21);
        assert_eq!(sent[1].1[0], 0x05);
        assert_eq!(sent[2].1[0], 0x06);
        // EXT_CSD read command (t76.c:469) and its 520-byte EP82 drain.
        assert_eq!(sent[3].1, vec![0x08, 0x48, 0x00, 0x02, 0, 0, 0, 0]);
        assert_eq!(tx.recv_log()[3], (0x82, 520));
        // CMD6 SWITCH to USER: ARG 0x02B30700 LE (t76.c:83, 497-499).
        assert_eq!(sent[4].1, vec![0x27, 0x46, 0, 0, 0x00, 0x07, 0xb3, 0x02]);
    }

    #[test]
    fn emmc_select_partition_updates_capacity() {
        let dev = device(ALG_EMMC, vec![]);
        let (mut t76, tx) = t76_with(vec![vec![0u8; 8]]);
        t76.emmc_geom = EmmcGeometry { sec_count: 0x00e9_0000, boot_mult: 0x20, rpmb_mult: 2 };
        t76.select_partition(&session(&dev), Partition::Boot1).unwrap();
        // BOOT1 arg 0x01B30100 LE (t76.c:84).
        assert_eq!(tx.sent()[0].1, vec![0x27, 0x46, 0, 0, 0x00, 0x01, 0xb3, 0x01]);
        // BOOT capacity = BOOT_SIZE_MULT * 128 KiB = 4 MiB (t76.c:483-486).
        assert_eq!(t76.capacity(), 0x20 * 128 * 1024);
    }

    #[test]
    fn emmc_erase_groups_and_polls() {
        let (mut t76, tx) = t76_with(vec![
            vec![0u8; 8],                                  // erase group ack
            vec![0x27, 0, 0, 0, 0, 0x09, 0, 0],            // poll: ready
        ]);
        t76.emmc_capacity = 0x20000 * 512; // exactly one erase group
        t76.emmc_erase(0).unwrap();
        let sent = tx.sent();
        assert_eq!(sent.len(), 2);
        // 16-byte 0x0e with start LBA 0 and end LBA 0x1ffff (t76.c:1438-1441).
        let mut cmd = vec![0u8; 16];
        cmd[0] = 0x0e;
        cmd[8..12].copy_from_slice(&0x1ffffu32.to_le_bytes());
        assert_eq!(sent[0].1, cmd);
        // Status poll 0x27 op 0x4d (t76.c:1422).
        assert_eq!(sent[1].1, vec![0x27, 0x4d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00]);
    }

    // -----------------------------------------------------------------------
    // Erase / blank check / pins
    // -----------------------------------------------------------------------

    #[test]
    fn generic_chip_erase_drains_64() {
        let dev = device(0x07, vec![]);
        let (mut t76, tx) = t76_with(vec![vec![0u8; 64]]);
        t76.erase(&session(&dev), EraseKind::Chip).unwrap();
        let mut msg = vec![0u8; 16];
        msg[0] = 0x0e;
        assert_eq!(tx.sent(), vec![(0x01, msg)]);
        assert_eq!(tx.recv_log(), vec![(0x81, 64)]); // t76.c:1487-1488
    }

    #[test]
    fn blank_check_reads_blocks() {
        let mut dev = device(0x07, vec![]);
        dev.code_size = 0x80;
        dev.page_size = 0x40;
        let (mut t76, _tx) = t76_with(vec![vec![0xffu8; 0x40], vec![0xffu8; 0x40]]);
        let region = Region::code(&dev);
        assert!(t76.blank_check(&session(&dev), region).unwrap());

        let (mut t76, _tx) = t76_with(vec![vec![0xffu8; 0x40], {
            let mut b = vec![0xffu8; 0x40];
            b[7] = 0x00;
            b
        }]);
        assert!(!t76.blank_check(&session(&dev), region).unwrap());
    }

    #[test]
    fn contact_check_reports_open_pins() {
        let dev = device(0x07, vec![]);
        // Bit 0 (socket position 1) set -> package pin 1 for a DIP8.
        let mut resp = vec![0u8; 32];
        resp[0] = 0x01;
        let (mut t76, tx) = t76_with(vec![resp]);
        let open = t76.contact_check(&session(&dev)).unwrap();
        assert_eq!(open, vec![1]);
        // The 8-byte 0x3e form with a 32-byte drain (t76.c:1565-1570).
        assert_eq!(tx.sent(), vec![(0x01, vec![0x3e, 0, 0, 0, 0, 0, 0, 0])]);
        assert_eq!(tx.recv_log(), vec![(0x81, 32)]);
    }

    // -----------------------------------------------------------------------
    // Firmware update (t76.c:1789-2056)
    // -----------------------------------------------------------------------

    /// Build a minimal valid updateT76.dat: 16-byte header + one 284-byte block.
    fn update_image() -> Vec<u8> {
        let block = vec![0x5au8; 0x114];
        let mut img = Vec::new();
        img.extend((UPDATE_FILE_VERSION | 0x0111).to_le_bytes()); // version
        img.extend((!crc32fast::hash(&block)).to_le_bytes()); // crc (C crc_32)
        img.extend(0u32.to_le_bytes()); // unknown
        img.extend(1u32.to_le_bytes()); // block count
        img.extend(&block);
        img
    }

    #[test]
    fn firmware_update_sequence() {
        let img = update_image();
        let (mut t76, tx) = t76_with(vec![
            vec![0x3d, 0, 0, 0, 0, 0, 0, 0], // switch ok
            vec![0x3c, 0, 0, 0, 0, 0, 0, 0], // erase ok
            vec![0x3b, 0, 0, 0, 0, 0, 0, 0], // block ok
            vec![0x3b, 0, 0, 0, 0, 0, 0, 0], // last block ok
        ]);
        t76.update(&img).unwrap();

        let sent = tx.sent();
        assert_eq!(sent.len(), 7);
        // Switch to bootloader: 0x3d aa + magic 0x049000 LE (t76.c:1879-1881).
        assert_eq!(sent[0].1, vec![0x3d, 0xaa, 0, 0, 0x00, 0x90, 0x04, 0x00]);
        // Reboot (XGECU_RESET 0x3f, minipro.c:516).
        assert_eq!(sent[1].1, vec![0x3f, 0, 0, 0, 0, 0, 0, 0]);
        // Erase: 0x3c aa (t76.c:1929-1930).
        assert_eq!(sent[2].1, vec![0x3c, 0xaa, 0, 0, 0, 0, 0, 0]);
        // Begin write: 0x3b aa, no reply (t76.c:1957-1958).
        assert_eq!(sent[3].1, vec![0x3b, 0xaa, 0, 0, 0, 0, 0, 0]);
        // Block 0: 0x11c bytes, length 0x100, address 0, then the payload
        // (t76.c:1967-1973).
        assert_eq!(sent[4].1.len(), 0x11c);
        assert_eq!(&sent[4].1[..8], &[0x3b, 0x00, 0x00, 0x01, 0, 0, 0, 0]);
        assert_eq!(&sent[4].1[8..], &img[16..16 + 0x114]);
        // Last block: 0x108 bytes with the fixed address + CRC
        // (t76.c:2002-2008).
        assert_eq!(sent[5].1.len(), 0x108);
        assert_eq!(&sent[5].1[..12], {
            let mut hdr = vec![0x3b, 0x03, 0x00, 0x01];
            hdr.extend(LAST_BLOCK_ADDR.to_le_bytes());
            hdr.extend(LAST_BLOCK_CRC.to_le_bytes());
            &hdr.clone()[..]
        });
        // Final reboot.
        assert_eq!(sent[6].1, vec![0x3f, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn firmware_update_rejects_bad_images() {
        let (mut t76, _tx) = t76_with(vec![]);
        // Too short.
        assert_eq!(t76.update(&[0u8; 4]).unwrap_err().code(), "format");
        // Bad version magic.
        let mut img = update_image();
        img[3] = 0x00;
        assert_eq!(t76.update(&img).unwrap_err().code(), "format");
        // Corrupt payload -> CRC mismatch.
        let mut img = update_image();
        img[100] ^= 0xff;
        assert_eq!(t76.update(&img).unwrap_err().code(), "format");
        // Block count mismatch.
        let mut img = update_image();
        img[12] = 2;
        assert_eq!(t76.update(&img).unwrap_err().code(), "format");
    }

    #[test]
    fn reset_fpga_packet() {
        let (mut t76, tx) = t76_with(vec![vec![0x26, 0, 0, 0, 0, 0, 0, 0]]);
        t76.reset_fpga().unwrap();
        // 0x26 af + magic 0xaa55ddee LE (t76.c:864-867).
        assert_eq!(tx.sent(), vec![(0x01, vec![0x26, 0xaf, 0, 0, 0xee, 0xdd, 0x55, 0xaa])]);
    }
}
