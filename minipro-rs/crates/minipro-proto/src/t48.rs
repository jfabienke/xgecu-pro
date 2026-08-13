//! The XGecu T48 driver — a fixed-silicon (non-FPGA) programmer.
//!
//! An independent Rust implementation of the T48 USB wire protocol — the
//! functional facts (opcodes, packet layouts, operation sequence) reverse-
//! engineered by the minipro community (nmatt0 — see the repo `NOTICE`).
//! The T48 is essentially a re-housed / extended TL866II+: there is **no FPGA
//! and no bitstream** (`get_algorithm` is never called), so [`Programmer::begin`]
//! just sends the shared BEGIN_TRANS header and checks overcurrent — the chip
//! algorithm is selected by the `protocol_id`/`variant` fields inside the
//! programmer's own firmware.
//!
//! It differs from the T56 sibling in the wire format in two ways:
//! 1. **Bulk data uses EP02** — `read_block` reads over EP82 IN and
//!    `write_block` writes over EP02 OUT, while small commands stay on
//!    EP01/EP81. A read shorter than 64 bytes is padded to a 64-byte transfer
//!    to avoid a libusb overflow.
//! 2. **The clock-enable quirk** — when the chip can adjust its SPI clock the
//!    T48 sets `msg[24]=1` *and* `msg[28]=spi_clock`; the `msg[24]=1` "enable"
//!    byte is required for `msg[28]` to take effect. Like the
//!    T56 it does not send `msg[63]` (there is no FPGA algorithm number).
//!
//! Implemented: identity, begin/end, the MemoryOps core, fuses, JEDEC rows,
//! protect, SPI autodetect, and the logic test. Firmware update is deferred.
//!
//! The large host-side bit-bang / pin-driver subsystem (`set_zif_*`,
//! `set_pin_drivers`, `set_voltages`, `hardware_check` — for `custom_protocol`
//! chips and the manufacturing self-test) is **intentionally not implemented**. Its
//! only real payoff is reading vintage parallel PROMs (the `CP_PROM`
//! device class, read-only), which the FPGA programmers (T56/T76) already cover
//! natively by uploading a ROM-class algorithm (`ROM24P`/`ROM28P`/…) — so on
//! fixed-silicon it just re-does in software, over USB pin-by-pin, what the
//! FPGA does with a bitstream. Not worth ~1200 lines of hardware-unverifiable
//! pin-level code for a fixed-silicon-only niche.

use minipro_core::caps::{
    FuseOps, JedecOps, LoadBitstream, LogicTest, MemoryOps, Protect, SpiAutodetect,
};
use minipro_core::device::{BlockReq, ChipId, Device, EraseKind, FuseKind, MemoryKind, Region};
use minipro_core::error::{Error, FwVersion, Result};
use minipro_core::programmer::{Caps, Programmer, ProgrammerInfo, Session};
use minipro_core::transport::{command, Ep, LinkSpeed, Transport};

use crate::wire::{
    self, ascii_field, le16, le32, pack_begin64, ChipParams, CMD_END_TRANS, CMD_ERASE, CMD_READID,
    CMD_READ_CODE, CMD_REQUEST_STATUS, CMD_WRITE_CODE,
};

// Endpoints: commands on EP01/EP81, bulk block data on EP82 IN / EP02 OUT
// (non-T76 path).
const EP_MSG_OUT: Ep = Ep(0x01);
const EP_MSG_IN: Ep = Ep(0x81);
const EP_DAT_IN: Ep = Ep(0x82);
const EP_DAT_OUT: Ep = Ep(0x02);

// T48-specific opcodes; shared II+-family opcodes come from
// `crate::wire`.
const MP_T48: u8 = 0x07; // device-type byte in the system-info report
const CMD_WRITE_USER_DATA: u8 = 0x0a;
const CMD_READ_USER_DATA: u8 = 0x0b;
const CMD_READ_DATA: u8 = 0x10;
const CMD_WRITE_DATA: u8 = 0x11;
const CMD_AUTODETECT: u8 = 0x37;
// Fuse/JEDEC/protect/calibration opcodes + the logic-test vector loop live in
// `crate::wire` (shared).

/// The T48 command channel + cached identity.
pub struct T48 {
    tx: Box<dyn Transport>,
    info: ProgrammerInfo,
}

impl T48 {
    pub fn new(tx: Box<dyn Transport>) -> Self {
        let info = ProgrammerInfo {
            model: "T48".into(),
            firmware: FwVersion(0),
            serial: String::new(),
            mfg_date: String::new(),
            device_code: String::new(),
            link: LinkSpeed::High,
            voltage: 0.0,
        };
        T48 { tx, info }
    }

    /// Query programmer identity from the T48 system-info report:
    /// `[4]`=fw minor, `[5]`=fw major, `[6]`=device type (7 = T48),
    /// `[32..56]`=serial, `[56..60]`=voltage (u32 LE, T48/T56 formula).
    pub fn query_info(&mut self) -> Result<()> {
        let msg = self.cmd(&[0u8; 5], 64)?;
        if msg.len() < 63 {
            return Err(Error::Protocol);
        }
        if msg[6] != MP_T48 {
            return Err(Error::Unsupported("attached programmer is not a T48"));
        }
        let (minor, major) = (msg[4], msg[5]);
        let raw = u32::from_le_bytes([msg[56], msg[57], msg[58], msg[59]]);
        let voltage = (u64::from(raw) * 0xccf6 / 0x27000) as f32 / 100.0;
        self.info = ProgrammerInfo {
            model: "T48".into(),
            firmware: FwVersion((u32::from(major) << 8) | u32::from(minor)),
            serial: ascii_field(&msg[32..56]),
            mfg_date: ascii_field(&msg[8..24]), // mfg date @8, 16 B
            device_code: ascii_field(&msg[24..32]), // device code @24, 8 B
            link: self.tx.link_speed(),
            voltage,
        };
        Ok(())
    }

    /// Send a command and drain `resp_len` bytes from EP81.
    fn cmd(&mut self, pkt: &[u8], resp_len: usize) -> Result<Vec<u8>> {
        command(self.tx.as_mut(), EP_MSG_OUT, EP_MSG_IN, pkt, resp_len)?.read()
    }

    /// Send a command with no reply.
    fn send(&mut self, pkt: &[u8]) -> Result<()> {
        self.tx.send(EP_MSG_OUT, pkt)
    }

    /// `0x39` REQUEST_STATUS; overcurrent byte at `resp[12]`.
    fn ovc_status(&mut self) -> Result<u8> {
        let mut msg = [0u8; 8];
        msg[0] = CMD_REQUEST_STATUS;
        let resp = self.cmd(&msg, 32)?;
        if resp.len() < 13 {
            return Err(Error::Protocol);
        }
        Ok(resp[12])
    }
}

impl Programmer for T48 {
    fn info(&self) -> &ProgrammerInfo {
        &self.info
    }

    fn caps(&self) -> Caps {
        // All firmware-mediated ops. The pin-driver / bit-bang subsystem, the
        // hardware self-test, and firmware update are deferred (see module docs).
        Caps::MEMORY
            .with(Caps::FUSES)
            .with(Caps::JEDEC)
            .with(Caps::PROTECT)
            .with(Caps::AUTODETECT)
            .with(Caps::LOGIC)
    }

    /// Begin a transaction: the shared 64-byte header (no
    /// FPGA, no bitstream), the `msg[24]=1` clock-enable quirk, then the
    /// overcurrent check.
    fn begin(&mut self, dev: &Device) -> Result<Session> {
        let params = ChipParams::from_device(dev);
        let mut msg = pack_begin64(&params);
        // Clock-enable quirk: when the chip can adjust its SPI
        // clock the T48 needs msg[24]=1 for the msg[28] clock byte to take
        // effect. ChipParams doesn't yet carry the decoded `can_adjust_clock`
        // flag; under the DB invariant `spi_clock != 0` iff `can_adjust_clock`,
        // so gating on spi_clock yields the same output.
        // TODO(db): key this on a decoded can_adjust_clock flag once Device
        // carries the decoded flag set.
        if params.spi_clock != 0 {
            msg[24] = 1;
        }
        self.send(&msg)?; // 64 bytes, no reply

        let ovc = self.ovc_status()?;
        if ovc != 0 {
            return Err(Error::Overcurrent);
        }
        Ok(Session {
            device: dev.clone(),
            emmc_capacity: 0,
        })
    }

    /// End the transaction: a bare END_TRANS, no reply.
    fn end(&mut self, _session: Session) -> Result<()> {
        let mut msg = [0u8; 8];
        msg[0] = CMD_END_TRANS;
        self.send(&msg)
    }

    /// Read the chip ID: READID, decode per id type (3/4 little-endian, else
    /// big-endian).
    fn identify(&mut self, s: &Session) -> Result<ChipId> {
        let mut msg = [0u8; 8];
        msg[0] = CMD_READID;
        let resp = self.cmd(&msg, 32)?;
        if resp.len() < 6 {
            return Err(Error::Protocol);
        }
        let id_type = resp[0];
        let id_length = s.device.chip_id_bytes.min(4);
        let bytes = &resp[2..2 + usize::from(id_length)];
        let raw = if id_length == 0 {
            0
        } else if id_type == 0x03 || id_type == 0x04 {
            bytes
                .iter()
                .rev()
                .fold(0u32, |acc, &b| (acc << 8) | u32::from(b))
        } else {
            bytes.iter().fold(0u32, |acc, &b| (acc << 8) | u32::from(b))
        };
        Ok(ChipId {
            raw,
            bytes: id_length,
        })
    }

    fn reset(&mut self) -> Result<()> {
        self.tx.reset()
    }

    fn memory(&mut self) -> Option<&mut dyn MemoryOps> {
        Some(self)
    }
    fn fuses(&mut self) -> Option<&mut dyn FuseOps> {
        Some(self)
    }
    fn jedec(&mut self) -> Option<&mut dyn JedecOps> {
        Some(self)
    }
    fn protect(&mut self) -> Option<&mut dyn Protect> {
        Some(self)
    }
    fn autodetect(&mut self) -> Option<&mut dyn SpiAutodetect> {
        Some(self)
    }
    fn logic(&mut self) -> Option<&mut dyn LogicTest> {
        Some(self)
    }
}

impl FuseOps for T48 {
    /// Read a fuse block via the shared wire helper.
    fn read_fuses(
        &mut self,
        s: &Session,
        kind: FuseKind,
        length: usize,
        items_count: u8,
    ) -> Result<Vec<u8>> {
        let code = s.device.code_size.min(u64::from(u32::MAX)) as u32;
        wire::fuse_read(
            self.tx.as_mut(),
            s.device.protocol_id,
            code,
            kind,
            length,
            items_count,
        )
    }
    /// Write a fuse block via the shared wire helper.
    fn write_fuses(
        &mut self,
        s: &Session,
        kind: FuseKind,
        items_count: u8,
        data: &[u8],
    ) -> Result<()> {
        let code = s.device.code_size.min(u64::from(u32::MAX)) as u32;
        wire::fuse_write(
            self.tx.as_mut(),
            s.device.protocol_id,
            code,
            kind,
            items_count,
            data,
        )
    }
}

impl JedecOps for T48 {
    /// Read a JEDEC row via the shared wire helper.
    fn read_row(&mut self, s: &Session, row: u8, flags: u8, size: u16) -> Result<Vec<u8>> {
        wire::jedec_read(self.tx.as_mut(), s.device.protocol_id, row, flags, size)
    }
    /// Write a JEDEC row via the shared wire helper.
    fn write_row(&mut self, s: &Session, row: u8, flags: u8, size: u16, data: &[u8]) -> Result<()> {
        wire::jedec_write(
            self.tx.as_mut(),
            s.device.protocol_id,
            row,
            flags,
            size,
            data,
        )
    }
}

impl Protect for T48 {
    fn protect_on(&mut self, _s: &Session) -> Result<()> {
        wire::protect(self.tx.as_mut(), true)
    }
    fn protect_off(&mut self, _s: &Session) -> Result<()> {
        wire::protect(self.tx.as_mut(), false)
    }
}

impl SpiAutodetect for T48 {
    /// SPI autodetect: `[0]`=0x37, `[8]`=package flag;
    /// send 10, recv 32, id = 3 bytes big-endian from `[2]`. Fixed-silicon: no
    /// bitstream, so the loader is unused.
    fn spi_autodetect(&mut self, wide: bool, _load: LoadBitstream<'_>) -> Result<u32> {
        let mut msg = [0u8; 10];
        msg[0] = CMD_AUTODETECT;
        msg[8] = u8::from(wide);
        let resp = self.cmd(&msg, 32)?;
        if resp.len() < 5 {
            return Err(Error::Protocol);
        }
        Ok((u32::from(resp[2]) << 16) | (u32::from(resp[3]) << 8) | u32::from(resp[4]))
    }
}

impl LogicTest for T48 {
    /// Logic-IC test: two passes (pull-up then pull-down)
    /// via the shared vector loop, then the L/H/Z comparison. Fixed-silicon: no
    /// FPGA bitstream, so the loader is unused.
    fn run(&mut self, s: &Session, _load: LoadBitstream<'_>) -> Result<bool> {
        let (pc, vc, vcc) = (
            s.device.package.pin_count,
            s.device.vector_count,
            s.device.logic_vcc,
        );
        let first = wire::logic_pass(self.tx.as_mut(), pc, vc, vcc, &s.device.vectors, 0)?;
        let second = wire::logic_pass(self.tx.as_mut(), pc, vc, vcc, &s.device.vectors, 1)?;
        Ok(wire::logic_compare(&s.device.vectors, &first, &second))
    }
}

impl MemoryOps for T48 {
    /// Read a block: 8-byte command on EP01, then the data
    /// on EP82. A read shorter than 64 bytes is padded to a 64-byte transfer
    /// and truncated.
    fn read_block(&mut self, _s: &Session, req: &BlockReq) -> Result<Vec<u8>> {
        let op = match req.kind {
            MemoryKind::Code => CMD_READ_CODE,
            MemoryKind::Data => CMD_READ_DATA,
            MemoryKind::User => CMD_READ_USER_DATA,
            MemoryKind::Data2 | MemoryKind::Config => {
                return Err(Error::Unsupported("T48 read_block: Data2/Config spaces"))
            }
        };
        let mut msg = [0u8; 8];
        msg[0] = op;
        le16(&mut msg, 2, req.len.min(u32::from(u16::MAX)) as u16);
        le32(&mut msg, 4, req.address.min(u64::from(u32::MAX)) as u32);
        let want = (req.len as usize).max(64); // <64 padded to a 64-byte read
        let mut data = command(self.tx.as_mut(), EP_MSG_OUT, EP_DAT_IN, &msg, want)?.read()?;
        if data.len() < req.len as usize {
            return Err(Error::Protocol);
        }
        data.truncate(req.len as usize);
        Ok(data)
    }

    /// Write a block: 8-byte command on EP01, then the
    /// payload on EP02.
    fn write_block(&mut self, _s: &Session, req: &BlockReq, data: &[u8]) -> Result<()> {
        if data.len() != req.len as usize {
            return Err(Error::Format(format!(
                "write_block: req.len {} != data {}",
                req.len,
                data.len()
            )));
        }
        let op = match req.kind {
            MemoryKind::Code => CMD_WRITE_CODE,
            MemoryKind::Data => CMD_WRITE_DATA,
            MemoryKind::User => CMD_WRITE_USER_DATA,
            MemoryKind::Data2 | MemoryKind::Config => {
                return Err(Error::Unsupported("T48 write_block: Data2/Config spaces"))
            }
        };
        let mut msg = [0u8; 8];
        msg[0] = op;
        le16(&mut msg, 2, req.len.min(u32::from(u16::MAX)) as u16);
        le32(&mut msg, 4, req.address.min(u64::from(u32::MAX)) as u32);
        self.send(&msg)?;
        // TODO(hw): send exactly `device->write_buffer_size` bytes over
        // EP02; matches when write_buffer_size == page_size.
        self.tx.send(EP_DAT_OUT, data)
    }

    /// Erase the chip: a 15-byte 0x0e (num_fuses/pld zero for a plain
    /// chip/fuse erase), then drain the 64-byte reply.
    fn erase(&mut self, _s: &Session, kind: EraseKind) -> Result<()> {
        if let EraseKind::Sector { .. } = kind {
            return Err(Error::Unsupported("T48 has no sector erase"));
        }
        let mut msg = [0u8; 15];
        msg[0] = CMD_ERASE;
        command(self.tx.as_mut(), EP_MSG_OUT, EP_MSG_IN, &msg, 64)?.discard()
    }

    /// Blank check by reading the region back and testing for erased flash.
    fn blank_check(&mut self, s: &Session, region: Region) -> Result<bool> {
        let step = u64::from(match s.device.page_size {
            0 => 4096,
            n => n,
        });
        let mut done = 0u64;
        while done < region.len {
            let len = step.min(region.len - done) as u32;
            let req = BlockReq {
                kind: region.kind,
                address: region.offset + done,
                len,
            };
            if self
                .read_block(s, &req)?
                .iter()
                .any(|&b| b != s.device.blank_value)
            {
                return Ok(false);
            }
            done += u64::from(len);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minipro_core::device::Package;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Rec {
        sent: Vec<(u8, Vec<u8>)>,
        replies: VecDeque<Vec<u8>>,
    }

    #[derive(Clone)]
    struct SharedTx(Arc<Mutex<Rec>>);

    impl SharedTx {
        fn new(replies: Vec<Vec<u8>>) -> Self {
            SharedTx(Arc::new(Mutex::new(Rec {
                sent: Vec::new(),
                replies: replies.into(),
            })))
        }
        fn sent(&self) -> Vec<(u8, Vec<u8>)> {
            self.0.lock().unwrap().sent.clone()
        }
    }

    impl Transport for SharedTx {
        fn send(&mut self, ep: Ep, data: &[u8]) -> Result<()> {
            self.0.lock().unwrap().sent.push((ep.0, data.to_vec()));
            Ok(())
        }
        fn recv(&mut self, _ep: Ep, _len: usize) -> Result<Vec<u8>> {
            self.0
                .lock()
                .unwrap()
                .replies
                .pop_front()
                .ok_or(Error::Protocol)
        }
        fn link_speed(&self) -> LinkSpeed {
            LinkSpeed::High
        }
        fn reset(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn t48_with(replies: Vec<Vec<u8>>) -> (T48, SharedTx) {
        let tx = SharedTx::new(replies);
        (T48::new(Box::new(tx.clone())), tx)
    }

    fn device(spi_clock: u8) -> Device {
        Device {
            name: "TEST".into(),
            protocol_id: 0x03,
            variant: 0x2100,
            code_size: 0x8000,
            data_size: 0x100,
            page_size: 0x40,
            chip_id: 0x1234,
            chip_id_bytes: 2,
            spi_clock,
            package: Package {
                pin_count: 8,
                name: "DIP8".into(),
            },
            fw_target: FwVersion(0),
            ..Device::default()
        }
    }

    #[test]
    fn caps_agree_with_accessors() {
        let (mut t48, _tx) = t48_with(vec![]);
        let c = t48.caps();
        assert_eq!(c.contains(Caps::MEMORY), t48.memory().is_some());
        assert_eq!(c.contains(Caps::FUSES), t48.fuses().is_some());
        assert_eq!(c.contains(Caps::JEDEC), t48.jedec().is_some());
        assert_eq!(c.contains(Caps::PROTECT), t48.protect().is_some());
        assert_eq!(c.contains(Caps::AUTODETECT), t48.autodetect().is_some());
        assert_eq!(c.contains(Caps::LOGIC), t48.logic().is_some());
        // T48 has no calibration op.
        assert_eq!(c.contains(Caps::CALIBRATION), t48.calibration().is_some());
        assert!(!c.contains(Caps::CALIBRATION));
    }

    #[test]
    fn read_fuses_packet_and_data() {
        // recv 64: bytes [8..12] are the fuse data.
        let mut reply = vec![0u8; 64];
        reply[8..12].copy_from_slice(&[0xc0, 0xde, 0xba, 0xbe]);
        let (mut t48, tx) = t48_with(vec![reply]);
        let mut dev = device(0x04);
        dev.protocol_id = 0x11;
        dev.code_size = 0x4000;
        let s = Session {
            device: dev,
            emmc_capacity: 0,
        };
        let out = t48
            .fuses()
            .unwrap()
            .read_fuses(&s, FuseKind::Config, 4, 3)
            .unwrap();
        assert_eq!(out, vec![0xc0, 0xde, 0xba, 0xbe]);
        // 8-byte cmd: 0x08 (READ_CFG), protocol 0x11, items 3, code_size LE at [4].
        assert_eq!(tx.sent()[0].1, vec![0x08, 0x11, 0x03, 0, 0x00, 0x40, 0, 0]);
    }

    #[test]
    fn write_fuses_uses_minus_0x38_address() {
        let (mut t48, tx) = t48_with(vec![]);
        let mut dev = device(0x04);
        dev.protocol_id = 0x11;
        dev.code_size = 0x100;
        let s = Session {
            device: dev,
            emmc_capacity: 0,
        };
        t48.fuses()
            .unwrap()
            .write_fuses(&s, FuseKind::User, 2, &[0xaa, 0xbb])
            .unwrap();
        let msg = &tx.sent()[0].1;
        assert_eq!(msg.len(), 64);
        assert_eq!(msg[0], 0x07); // WRITE_USER
        assert_eq!(&msg[4..8], &(0x100u32 - 0x38).to_le_bytes()); // firmware-bug offset
        assert_eq!(&msg[8..10], &[0xaa, 0xbb]);
    }

    #[test]
    fn jedec_row_roundtrip_packets() {
        // Write: 20-bit row -> ceil(20/8) = 3 payload bytes.
        let (mut t48, tx) = t48_with(vec![]);
        let mut dev = device(0x04);
        dev.protocol_id = 0x2a;
        let s = Session {
            device: dev,
            emmc_capacity: 0,
        };
        t48.jedec()
            .unwrap()
            .write_row(&s, 5, 1, 20, &[0x11, 0x22, 0x33])
            .unwrap();
        let msg = &tx.sent()[0].1;
        assert_eq!(msg[0], 0x1e); // WRITE_JEDEC
        assert_eq!(msg[1], 0x2a); // protocol
        assert_eq!(msg[2], 20); // size
        assert_eq!(msg[4], 5); // row
        assert_eq!(msg[5], 1); // flags
        assert_eq!(&msg[8..11], &[0x11, 0x22, 0x33]);
    }

    #[test]
    fn read_jedec_returns_row_from_offset_zero() {
        // The reply carries (size+7)/8 bytes from msg[0], not msg[8].
        let mut reply = vec![0u8; 32];
        reply[0..3].copy_from_slice(&[0xde, 0xad, 0xbe]);
        let (mut t48, _tx) = t48_with(vec![reply]);
        let mut dev = device(0x04);
        dev.protocol_id = 0x2a;
        let s = Session {
            device: dev,
            emmc_capacity: 0,
        };
        let row = t48.jedec().unwrap().read_row(&s, 0, 0, 20).unwrap();
        assert_eq!(row, vec![0xde, 0xad, 0xbe]);
    }

    #[test]
    fn protect_on_off_packets() {
        let (mut t48, tx) = t48_with(vec![]);
        let s = Session {
            device: device(0x04),
            emmc_capacity: 0,
        };
        t48.protect().unwrap().protect_on(&s).unwrap();
        t48.protect().unwrap().protect_off(&s).unwrap();
        assert_eq!(tx.sent()[0].1[0], 0x19);
        assert_eq!(tx.sent()[1].1[0], 0x18);
    }

    #[test]
    fn spi_autodetect_packet_and_id() {
        let mut reply = vec![0u8; 32];
        reply[2..5].copy_from_slice(&[0xef, 0x40, 0x18]); // big-endian id
        let (mut t48, tx) = t48_with(vec![reply]);
        let id = t48
            .autodetect()
            .unwrap()
            .spi_autodetect(true, &mut |_| Ok(vec![]))
            .unwrap();
        assert_eq!(id, 0x00ef_4018);
        let msg = &tx.sent()[0].1;
        assert_eq!(msg.len(), 10);
        assert_eq!(msg[0], 0x37);
        assert_eq!(msg[8], 1); // wide -> 16-pin flag
    }

    #[test]
    fn logic_test_two_pass_pass_and_fail() {
        // 2 pins, 1 vector: pin0 = H (must be 1 in both), pin1 = L (0 in both).
        let mut dev = device(0x04);
        dev.package.pin_count = 2;
        dev.vector_count = 1;
        dev.logic_vcc = 5;
        dev.vectors = vec![3, 2]; // pin0 = H (state 3), pin1 = L (state 2)

        // Passing: pass1 and pass2 both report pin0=1, pin1=0. Packed 2/byte:
        // pins {1,0} -> low nibble 1, high nibble 0 -> resp[8] = 0x01.
        let good = || {
            let mut r = vec![0u8; 32];
            r[8] = 0x01;
            r
        };
        let (mut t48, _tx) = t48_with(vec![good(), good()]);
        let s = Session {
            device: dev.clone(),
            emmc_capacity: 0,
        };
        assert!(t48.logic().unwrap().run(&s, &mut |_| Ok(vec![])).unwrap());

        // Failing: pin0 reads 0 (should be H). resp[8] = 0x00.
        let bad = || vec![0u8; 32];
        let (mut t48, _tx) = t48_with(vec![bad(), bad()]);
        let s = Session {
            device: dev,
            emmc_capacity: 0,
        };
        assert!(!t48.logic().unwrap().run(&s, &mut |_| Ok(vec![])).unwrap());
    }

    /// begin sends only the 64-byte header (no bitstream) then the 0x39
    /// status, and sets the msg[24]=1 clock-enable when spi_clock is present.
    #[test]
    fn begin_no_bitstream_sets_clock_enable() {
        let (mut t48, tx) = t48_with(vec![vec![0u8; 32]]);
        t48.begin(&device(0x04)).unwrap();
        let sent = tx.sent();
        assert_eq!(sent.len(), 2, "no bitstream upload — just BEGIN + status");
        let (ep, begin) = &sent[0];
        assert_eq!(*ep, 0x01);
        assert_eq!(begin.len(), 64);
        assert_eq!(begin[24], 1, "clock-enable byte set when spi_clock present");
        assert_eq!(begin[28], 0x04, "spi_clock in the shared header");
        assert_eq!(begin[63], 0, "no FPGA algorithm number on the T48");
        assert_eq!(sent[1], (0x01, vec![0x39, 0, 0, 0, 0, 0, 0, 0]));
    }

    /// Without an adjustable clock the enable byte stays 0.
    #[test]
    fn begin_no_clock_enable_without_spi_clock() {
        let (mut t48, tx) = t48_with(vec![vec![0u8; 32]]);
        t48.begin(&device(0x00)).unwrap();
        assert_eq!(tx.sent()[0].1[24], 0);
    }

    /// read_block reads bulk over EP82 and pads a sub-64-byte read to 64.
    #[test]
    fn read_block_uses_ep82_and_pads_small() {
        // 0x20 (<64) requested -> firmware sends 64; keep first 0x20.
        let mut reply: Vec<u8> = (0..0x20u8).collect();
        reply.extend_from_slice(&[0xff; 0x20]); // pad to 64
        let (mut t48, tx) = t48_with(vec![reply]);
        let dev = device(0x04);
        let s = Session {
            device: dev.clone(),
            emmc_capacity: 0,
        };
        let req = BlockReq {
            kind: MemoryKind::Code,
            address: 0,
            len: 0x20,
        };
        let out = t48.read_block(&s, &req).unwrap();
        assert_eq!(out, (0..0x20u8).collect::<Vec<u8>>());
        // Command went out on EP01.
        assert_eq!(tx.sent()[0].0, 0x01);
        assert_eq!(&tx.sent()[0].1[..2], &[0x0d, 0x00]);
    }

    /// write_block sends the command on EP01 and the payload on EP02.
    #[test]
    fn write_block_payload_on_ep02() {
        let (mut t48, tx) = t48_with(vec![]);
        let dev = device(0x04);
        let s = Session {
            device: dev.clone(),
            emmc_capacity: 0,
        };
        let req = BlockReq {
            kind: MemoryKind::Code,
            address: 0x40,
            len: 4,
        };
        t48.write_block(&s, &req, &[1, 2, 3, 4]).unwrap();
        let sent = tx.sent();
        assert_eq!(sent[0].0, 0x01); // command
        assert_eq!(sent[0].1[0], 0x0c);
        assert_eq!(sent[1], (0x02, vec![1, 2, 3, 4])); // payload on EP02 OUT
    }

    #[test]
    fn query_info_rejects_non_t48() {
        let mut report = vec![0u8; 64];
        report[6] = 0x06; // T56, not T48
        let (mut t48, _tx) = t48_with(vec![report]);
        assert_eq!(t48.query_info().unwrap_err().code(), "unsupported");
    }
}
