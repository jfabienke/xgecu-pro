//! The XGecu T56 driver — the T76's closest sibling.
//!
//! A faithful port of `minipro-t76/src/t56.c` — **Matt Brown's (nmatt0)**
//! reverse-engineered T56 driver (line numbers cited throughout).
//! Like the T76 the T56 is FPGA-based: [`Programmer::begin`] uploads a
//! per-chip Anlogic bitstream before the transaction. It differs from the T76
//! in three ways that matter to the wire format:
//!
//! 1. **Bitstream framing is single-shot** — an 8-byte `0x26` header carrying
//!    the length, then the raw bitstream in one transfer (`t56.c:145-159`), vs
//!    the T76's chunked BEGIN_BS/BS_BLOCK/END_BS. (Logic chips use the two-part
//!    `0x2a` protocol; deferred with the logic-test capability.)
//! 2. **No 128-byte BEGIN extension** — the T56 sends exactly the shared
//!    64-byte header ([`crate::wire::pack_begin64`]) and, unlike the T76, does
//!    *not* write `msg[24]` (I2C address) or `msg[63]` (algorithm number):
//!    the algorithm is selected purely by the uploaded bitstream (`t56.c:181-224`).
//! 3. **One endpoint pair** — commands *and* bulk block data both use EP01 OUT
//!    / EP81 IN (`usb_nix.c:442-459`), so there is no separate bulk plane and
//!    none of the T76's must-drain-or-wedge discipline.
//!
//! Scope of this increment: identity, begin/end, the `0x26` bitstream upload,
//! and the MemoryOps core (read/write/erase/blank-check/identify). Fuses,
//! JEDEC rows, calibration, SPI autodetect, the logic test, and firmware
//! update are deferred (they need cap-trait plumbing the core doesn't carry
//! yet); each is a short, self-contained follow-up.

use minipro_core::caps::{
    Calibration, FuseOps, JedecOps, LoadBitstream, LogicTest, MemoryOps, Protect, SpiAutodetect,
};
use minipro_core::device::{BlockReq, ChipId, Device, EraseKind, FuseKind, MemoryKind, Region};
use minipro_core::error::{Error, FwVersion, Result};
use minipro_core::programmer::{Caps, Programmer, ProgrammerInfo, Session};
use minipro_core::transport::{command, Ep, LinkSpeed, Transport};

use crate::wire::{
    self, ascii_field, le16, le32, pack_begin64, ChipParams, CMD_END_TRANS, CMD_ERASE, CMD_READID,
    CMD_READ_CODE, CMD_REQUEST_STATUS, CMD_WRITE_CODE,
};

// Command + bulk endpoints — the T56 multiplexes everything over EP01/EP81
// (usb_nix.c:442-459; there is no bulk EP for the T56).
const EP_MSG_OUT: Ep = Ep(0x01);
const EP_MSG_IN: Ep = Ep(0x81);

// T56-specific opcodes (t56.c:34-65). The shared II+-family opcodes
// (BEGIN/END/READID/WRITE_CODE/READ_CODE/ERASE/REQUEST_STATUS) come from
// `crate::wire`.
const MP_T56: u8 = 0x06; // device-type byte in the system-info report (minipro.h:27)
const CMD_WRITE_USER_DATA: u8 = 0x0a; // t56.c:41
const CMD_READ_USER_DATA: u8 = 0x0b; // t56.c:42
const CMD_READ_DATA: u8 = 0x10; // t56.c:47
const CMD_WRITE_DATA: u8 = 0x11; // t56.c:48
const CMD_WRITE_BITSTREAM: u8 = 0x26; // t56.c:56 — single-shot bitstream upload
const CMD_WRITE_BITSTREAM2: u8 = 0x2a; // t56.c:58 — multipart (logic TTL1/TTL2)
const CMD_AUTODETECT: u8 = 0x37; // t56.c:59
// Fuse/JEDEC/protect/calibration opcodes + the logic-vector loop live in
// `crate::wire` (shared).

/// The T56 command channel + cached identity.
pub struct T56 {
    tx: Box<dyn Transport>,
    info: ProgrammerInfo,
    /// Name of the FPGA algorithm currently loaded, so `begin` skips the
    /// re-upload within a session. The C uses a plain `static` bool
    /// (`t56.c:88`); keying by name is the T76's fix — switching devices
    /// re-uploads correctly.
    uploaded_algo: Option<String>,
}

impl T56 {
    pub fn new(tx: Box<dyn Transport>) -> Self {
        let info = ProgrammerInfo {
            model: "T56".into(),
            firmware: FwVersion(0),
            serial: String::new(),
            mfg_date: String::new(),
            device_code: String::new(),
            link: LinkSpeed::High,
            voltage: 0.0,
        };
        T56 { tx, info, uploaded_algo: None }
    }

    /// Query programmer identity (`minipro_get_system_info`, minipro.c:234-246,
    /// T56 case): send a 5-byte zero request, read the report. `[4]`=fw minor,
    /// `[5]`=fw major, `[6]`=device type (6 = T56), `[32..56]`=serial,
    /// `[56..60]`=voltage (u32 LE, scaled by the T48/T56 formula). Also verifies
    /// this really is a T56.
    pub fn query_info(&mut self) -> Result<()> {
        let msg = self.cmd(&[0u8; 5], 64)?;
        if msg.len() < 63 {
            return Err(Error::Protocol);
        }
        if msg[6] != MP_T56 {
            return Err(Error::Unsupported("attached programmer is not a T56"));
        }
        let (minor, major) = (msg[4], msg[5]);
        // T48/T56 voltage: (raw * 0xccf6 / 0x27000) / 100.0 (minipro.c:242-243).
        // Integer math first (as in the C), then the float divide.
        let raw = u32::from_le_bytes([msg[56], msg[57], msg[58], msg[59]]);
        let voltage = (u64::from(raw) * 0xccf6 / 0x27000) as f32 / 100.0;
        self.info = ProgrammerInfo {
            model: "T56".into(),
            firmware: FwVersion((u32::from(major) << 8) | u32::from(minor)),
            serial: ascii_field(&msg[32..56]),
            mfg_date: ascii_field(&msg[8..24]),      // mfg date @8, 16 B (minipro.c:238)
            device_code: ascii_field(&msg[24..32]),  // device code @24, 8 B (minipro.c:239)
            link: self.tx.link_speed(),
            voltage,
        };
        Ok(())
    }

    /// Send a command and drain `resp_len` bytes from EP81.
    fn cmd(&mut self, pkt: &[u8], resp_len: usize) -> Result<Vec<u8>> {
        command(self.tx.as_mut(), EP_MSG_OUT, EP_MSG_IN, pkt, resp_len)?.read()
    }

    /// Send a command with no reply (BEGIN/END_TRANS, bitstream transfers).
    fn send(&mut self, pkt: &[u8]) -> Result<()> {
        self.tx.send(EP_MSG_OUT, pkt)
    }

    /// Upload the FPGA bitstream, single-shot (`t56_send_bitstream` normal
    /// path, t56.c:145-163): an 8-byte `0x26` header carrying the length, then
    /// the whole bitstream in one transfer. Neither transfer has a reply.
    fn upload_bitstream(&mut self, dev: &Device) -> Result<()> {
        let algorithm = dev
            .algorithm
            .as_ref()
            .filter(|a| !a.bitstream.is_empty())
            .ok_or(Error::Unsupported("device has no FPGA bitstream loaded"))?;
        self.upload_named(&algorithm.name.clone(), &algorithm.bitstream.clone())
    }

    /// Single-shot `0x26` upload of a named bitstream (chip or utility), skipping
    /// the re-upload if the same algorithm is already loaded this session.
    fn upload_named(&mut self, name: &str, bits: &[u8]) -> Result<()> {
        if self.uploaded_algo.as_deref() == Some(name) {
            return Ok(());
        }
        let mut hdr = [0u8; 8];
        hdr[0] = CMD_WRITE_BITSTREAM;
        le32(&mut hdr, 4, bits.len().min(u32::MAX as usize) as u32); // t56.c:149
        self.send(&hdr)?; // t56.c:151
        self.tx.send(EP_MSG_OUT, bits)?; // t56.c:155 — raw bitstream, one shot
        self.uploaded_algo = Some(name.to_string());
        Ok(())
    }

    /// Upload the two logic-test bitstreams via the multipart `0x2A` protocol
    /// (`t56_send_bitstream` MP_LOGIC path, t56.c:113-134): each is prefixed by
    /// an 8-byte header (`[0]=0x2A`, `[1]`=1 for the first / 0 for the second,
    /// length at `[4]`) and sent in one transfer.
    fn upload_logic_ttl(&mut self, ttl1: &[u8], ttl2: &[u8]) -> Result<()> {
        for (i, bits) in [ttl1, ttl2].into_iter().enumerate() {
            let mut buf = vec![0u8; 8 + bits.len()];
            buf[0] = CMD_WRITE_BITSTREAM2;
            buf[1] = if i == 0 { 1 } else { 0 }; // t56.c:124: `i ? 0 : 1`
            le32(&mut buf, 4, bits.len().min(u32::MAX as usize) as u32);
            buf[8..].copy_from_slice(bits);
            self.send(&buf)?;
        }
        self.uploaded_algo = None; // FPGA now holds TTL; force re-upload next begin
        Ok(())
    }

    /// `0x39` REQUEST_STATUS; returns the overcurrent byte at `resp[12]`
    /// (`t56_get_ovc_status`, t56.c:501-520). The T56 never repacks the chip
    /// header into this command (unlike the T76's NAND/eMMC path).
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

impl Programmer for T56 {
    fn info(&self) -> &ProgrammerInfo {
        &self.info
    }

    fn caps(&self) -> Caps {
        Caps::MEMORY
            .with(Caps::CALIBRATION)
            .with(Caps::FUSES)
            .with(Caps::JEDEC)
            .with(Caps::PROTECT)
            .with(Caps::LOGIC)
            .with(Caps::AUTODETECT)
    }

    /// `t56_begin_transaction` (t56.c:166-240): upload the bitstream, send the
    /// shared 64-byte BEGIN_TRANS header, then the overcurrent check.
    fn begin(&mut self, dev: &Device) -> Result<Session> {
        self.upload_bitstream(dev)?;

        // The shared header — no 0x40.. extension, no msg[24]/msg[63]
        // (t56.c:181-224). `custom_protocol` (bit-bang) chips are not yet
        // supported by this driver.
        let params = ChipParams::from_device(dev);
        let msg = pack_begin64(&params);
        self.send(&msg)?; // t56.c:224 — sends all 64 bytes, no reply

        let ovc = self.ovc_status()?; // t56.c:232
        if ovc != 0 {
            return Err(Error::Overcurrent);
        }
        Ok(Session { device: dev.clone(), emmc_capacity: 0 })
    }

    /// `t56_end_transaction` (t56.c:242-251): a bare END_TRANS, no reply.
    fn end(&mut self, _session: Session) -> Result<()> {
        let mut msg = [0u8; 8];
        msg[0] = CMD_END_TRANS;
        self.send(&msg)
    }

    /// `t56_get_chip_id` (t56.c:393-421): READID, then decode per the reported
    /// id type — types 3/4 little-endian, others big-endian.
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
        // MP_ID_TYPE3 = 3, MP_ID_TYPE4 = 4 (minipro.h) -> little-endian.
        let raw = if id_length == 0 {
            0
        } else if id_type == 0x03 || id_type == 0x04 {
            bytes.iter().rev().fold(0u32, |acc, &b| (acc << 8) | u32::from(b))
        } else {
            bytes.iter().fold(0u32, |acc, &b| (acc << 8) | u32::from(b))
        };
        Ok(ChipId { raw, bytes: id_length })
    }

    fn reset(&mut self) -> Result<()> {
        self.tx.reset()
    }

    fn memory(&mut self) -> Option<&mut dyn MemoryOps> {
        Some(self)
    }

    fn calibration(&mut self) -> Option<&mut dyn Calibration> {
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
    fn logic(&mut self) -> Option<&mut dyn LogicTest> {
        Some(self)
    }
    fn autodetect(&mut self) -> Option<&mut dyn SpiAutodetect> {
        Some(self)
    }
}

impl LogicTest for T56 {
    /// `t56_logic_ic_test` (t56.c:636-…): upload the two TTL bitstreams, then the
    /// shared two-pass vector loop + L/H/Z comparison. The caller's `load`
    /// fetches `TTL1`/`TTL2` from the same `algoT76/` source as chip bitstreams.
    fn run(&mut self, s: &Session, load: LoadBitstream<'_>) -> Result<bool> {
        let ttl1 = load("TTL1")?;
        let ttl2 = load("TTL2")?;
        self.upload_logic_ttl(&ttl1, &ttl2)?;
        let (pc, vc, vcc) = (s.device.package.pin_count, s.device.vector_count, s.device.logic_vcc);
        let first = wire::logic_pass(self.tx.as_mut(), pc, vc, vcc, &s.device.vectors, 0)?;
        let second = wire::logic_pass(self.tx.as_mut(), pc, vc, vcc, &s.device.vectors, 1)?;
        Ok(wire::logic_compare(&s.device.vectors, &first, &second))
    }
}

impl SpiAutodetect for T56 {
    /// `t56_spi_autodetect` (t56.c:423-460): upload the SPI25F autodetect
    /// bitstream, then `0x37` (`[8]`=package flag), send 10, recv 16, id = 3
    /// bytes big-endian from `[2]`.
    fn spi_autodetect(&mut self, wide: bool, load: LoadBitstream<'_>) -> Result<u32> {
        let name = if wide { "SPI25F21" } else { "SPI25F11" };
        let bits = load(name)?;
        self.upload_named(name, &bits)?;
        let mut msg = [0u8; 10];
        msg[0] = CMD_AUTODETECT;
        msg[8] = u8::from(wide);
        let resp = self.cmd(&msg, 16)?;
        if resp.len() < 5 {
            return Err(Error::Protocol);
        }
        Ok((u32::from(resp[2]) << 16) | (u32::from(resp[3]) << 8) | u32::from(resp[4]))
    }
}

impl Calibration for T56 {
    /// `t56_read_calibration` (t56.c:378-391) — the shared implementation.
    fn read_calibration(&mut self, len: usize) -> Result<Vec<u8>> {
        wire::calibration_read(self.tx.as_mut(), len)
    }
}

impl FuseOps for T56 {
    /// `t56_read_fuses` (t56.c:314-345) — the shared implementation.
    fn read_fuses(
        &mut self,
        s: &Session,
        kind: FuseKind,
        length: usize,
        items_count: u8,
    ) -> Result<Vec<u8>> {
        let code = s.device.code_size.min(u64::from(u32::MAX)) as u32;
        wire::fuse_read(self.tx.as_mut(), s.device.protocol_id, code, kind, length, items_count)
    }
    /// `t56_write_fuses` (t56.c:347-376) — the shared implementation.
    fn write_fuses(
        &mut self,
        s: &Session,
        kind: FuseKind,
        items_count: u8,
        data: &[u8],
    ) -> Result<()> {
        let code = s.device.code_size.min(u64::from(u32::MAX)) as u32;
        wire::fuse_write(self.tx.as_mut(), s.device.protocol_id, code, kind, items_count, data)
    }
}

impl JedecOps for T56 {
    /// `t56_read_jedec_row` (t56.c:539-558) — the shared implementation.
    fn read_row(&mut self, s: &Session, row: u8, flags: u8, size: u16) -> Result<Vec<u8>> {
        wire::jedec_read(self.tx.as_mut(), s.device.protocol_id, row, flags, size)
    }
    /// `t56_write_jedec_row` (t56.c:522-537) — the shared implementation.
    fn write_row(
        &mut self,
        s: &Session,
        row: u8,
        flags: u8,
        size: u16,
        data: &[u8],
    ) -> Result<()> {
        wire::jedec_write(self.tx.as_mut(), s.device.protocol_id, row, flags, size, data)
    }
}

impl Protect for T56 {
    fn protect_on(&mut self, _s: &Session) -> Result<()> {
        wire::protect(self.tx.as_mut(), true)
    }
    fn protect_off(&mut self, _s: &Session) -> Result<()> {
        wire::protect(self.tx.as_mut(), false)
    }
}

impl MemoryOps for T56 {
    /// `t56_read_block` (t56.c:253-281): 8-byte command, then the data on EP81.
    /// The T56 firmware returns `size + 16` bytes (an off-by-one bug the C works
    /// around by over-reading, t56.c:277-280); we request that many and keep the
    /// first `size`.
    fn read_block(&mut self, _s: &Session, req: &BlockReq) -> Result<Vec<u8>> {
        let op = match req.kind {
            MemoryKind::Code => CMD_READ_CODE,
            MemoryKind::Data => CMD_READ_DATA,
            MemoryKind::User => CMD_READ_USER_DATA,
            MemoryKind::Data2 | MemoryKind::Config => {
                return Err(Error::Unsupported("T56 read_block: Data2/Config spaces"))
            }
        };
        let mut msg = [0u8; 8];
        msg[0] = op;
        le16(&mut msg, 2, req.len.min(u32::from(u16::MAX)) as u16);
        le32(&mut msg, 4, req.address.min(u64::from(u32::MAX)) as u32);
        let mut data = self.cmd(&msg, req.len as usize + 16)?; // +16 over-read
        if data.len() < req.len as usize {
            return Err(Error::Protocol);
        }
        data.truncate(req.len as usize);
        Ok(data)
    }

    /// `t56_write_block` (t56.c:283-312): 8-byte command, then the payload on
    /// EP01.
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
                return Err(Error::Unsupported("T56 write_block: Data2/Config spaces"))
            }
        };
        let mut msg = [0u8; 8];
        msg[0] = op;
        le16(&mut msg, 2, req.len.min(u32::from(u16::MAX)) as u16);
        le32(&mut msg, 4, req.address.min(u64::from(u32::MAX)) as u32);
        self.send(&msg)?;
        // TODO(hw): the C sends exactly `device->write_buffer_size` bytes
        // (t56.c:308-310); the block loop drives page-sized blocks, so this
        // matches when write_buffer_size == page_size. Reconcile the two
        // granularities when validating writes on hardware.
        self.send(data)
    }

    /// `t56_erase` (t56.c:484-499): a 15-byte 0x0e (num_fuses at [2], pld at
    /// [4]), then drain the 64-byte reply. A plain chip/fuse erase passes zero
    /// for both (the C caller derives them from the fuse-config profile, not the
    /// chip DB entry).
    fn erase(&mut self, _s: &Session, kind: EraseKind) -> Result<()> {
        if let EraseKind::Sector { .. } = kind {
            return Err(Error::Unsupported("T56 has no sector erase"));
        }
        let mut msg = [0u8; 15];
        msg[0] = CMD_ERASE;
        command(self.tx.as_mut(), EP_MSG_OUT, EP_MSG_IN, &msg, 64)?.discard()
    }

    /// Blank check by reading the region back and testing for erased flash
    /// (0xff); the T56 has no dedicated blank-check opcode, same as the T76.
    fn blank_check(&mut self, s: &Session, region: Region) -> Result<bool> {
        let step = u64::from(match s.device.page_size {
            0 => 4096,
            n => n,
        });
        let mut done = 0u64;
        while done < region.len {
            let len = step.min(region.len - done) as u32;
            let req = BlockReq { kind: region.kind, address: region.offset + done, len };
            if self.read_block(s, &req)?.iter().any(|&b| b != s.device.blank_value) {
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
    use minipro_core::device::{Algorithm, Package};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// Records every OUT packet and pops canned IN replies in order.
    #[derive(Default)]
    struct Rec {
        sent: Vec<(u8, Vec<u8>)>,
        replies: VecDeque<Vec<u8>>,
    }

    #[derive(Clone)]
    struct SharedTx(Arc<Mutex<Rec>>);

    impl SharedTx {
        fn new(replies: Vec<Vec<u8>>) -> Self {
            SharedTx(Arc::new(Mutex::new(Rec { sent: Vec::new(), replies: replies.into() })))
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
            self.0.lock().unwrap().replies.pop_front().ok_or(Error::Protocol)
        }
        fn link_speed(&self) -> LinkSpeed {
            LinkSpeed::High
        }
        fn reset(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn t56_with(replies: Vec<Vec<u8>>) -> (T56, SharedTx) {
        let tx = SharedTx::new(replies);
        (T56::new(Box::new(tx.clone())), tx)
    }

    fn device(protocol_id: u8, variant: u16, bitstream: Vec<u8>) -> Device {
        Device {
            name: "TEST".into(),
            protocol_id,
            variant,
            code_size: 0x8000,
            data_size: 0x100,
            page_size: 0x40,
            chip_id: 0x1234,
            chip_id_bytes: 2,
            i2c_address: 0x51, // set on purpose: the T56 must NOT emit it
            spi_clock: 0x04,
            package: Package { pin_count: 8, name: "DIP8".into() },
            algorithm: Some(Algorithm { name: "SPI25F21".into(), bitstream }),
            fw_target: FwVersion(0),
            ..Device::default()
        }
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn caps_agree_with_accessors() {
        let (mut t56, _tx) = t56_with(vec![]);
        let c = t56.caps();
        assert_eq!(c.contains(Caps::MEMORY), t56.memory().is_some());
        assert_eq!(c.contains(Caps::CALIBRATION), t56.calibration().is_some());
        assert_eq!(c.contains(Caps::FUSES), t56.fuses().is_some());
        assert_eq!(c.contains(Caps::JEDEC), t56.jedec().is_some());
        assert_eq!(c.contains(Caps::PROTECT), t56.protect().is_some());
        assert_eq!(c.contains(Caps::LOGIC), t56.logic().is_some());
        assert_eq!(c.contains(Caps::AUTODETECT), t56.autodetect().is_some());
        assert!(c.contains(Caps::LOGIC) && c.contains(Caps::AUTODETECT));
        // Absent (wrong hardware) capabilities.
        assert!(t56.emmc().is_none());
        assert!(t56.pins().is_none());
    }

    #[test]
    fn logic_test_uploads_ttl_then_runs_vectors() {
        // 2 pins, 1 vector: pin0 = H (state 3), pin1 = L (state 2).
        let mut dev = device(0x00, 0x0000, vec![]);
        dev.package.pin_count = 2;
        dev.vector_count = 1;
        dev.logic_vcc = 5;
        dev.vectors = vec![3, 2];
        // Both passes report pin0=1, pin1=0 -> packed resp[8] = 0x01 -> pass.
        let good = || {
            let mut r = vec![0u8; 32];
            r[8] = 0x01;
            r
        };
        let (mut t56, tx) = t56_with(vec![good(), good()]);
        let s = Session { device: dev, emmc_capacity: 0 };
        // Loader returns distinct TTL payloads.
        let mut load = |name: &str| Ok(name.as_bytes().to_vec());
        assert!(t56.logic().unwrap().run(&s, &mut load).unwrap());
        let sent = tx.sent();
        // First two sends are the 0x2A TTL uploads ([1]=1 then [1]=0).
        assert_eq!(&sent[0].1[..2], &[0x2a, 0x01]);
        assert_eq!(&sent[0].1[8..], b"TTL1");
        assert_eq!(&sent[1].1[..2], &[0x2a, 0x00]);
        assert_eq!(&sent[1].1[8..], b"TTL2");
        // Then two 0x28 vector commands.
        assert_eq!(sent[2].1[0], 0x28);
    }

    #[test]
    fn spi_autodetect_uploads_bitstream_then_probes() {
        let mut reply = vec![0u8; 16];
        reply[2..5].copy_from_slice(&[0xef, 0x40, 0x18]);
        let (mut t56, tx) = t56_with(vec![reply]);
        let mut load = |name: &str| Ok(name.as_bytes().to_vec());
        let id = t56.autodetect().unwrap().spi_autodetect(true, &mut load).unwrap();
        assert_eq!(id, 0x00ef_4018);
        let sent = tx.sent();
        // 0x26 header + SPI25F21 bitstream, then the 10-byte 0x37 probe.
        assert_eq!(sent[0].1[0], 0x26);
        assert_eq!(&sent[1].1, b"SPI25F21");
        assert_eq!(sent[2].1.len(), 10);
        assert_eq!(sent[2].1[0], 0x37);
        assert_eq!(sent[2].1[8], 1); // wide
    }

    #[test]
    fn fuse_and_protect_share_wire_ops() {
        // read_fuses: recv 64, data at [8..12].
        let mut reply = vec![0u8; 64];
        reply[8..12].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        let (mut t56, tx) = t56_with(vec![reply]);
        let mut dev = device(0x11, 0x0100, vec![0x01]);
        dev.code_size = 0x4000;
        let s = Session { device: dev, emmc_capacity: 0 };
        let out = t56.fuses().unwrap().read_fuses(&s, FuseKind::User, 4, 2).unwrap();
        assert_eq!(out, vec![0x11, 0x22, 0x33, 0x44]);
        assert_eq!(&tx.sent()[0].1[..3], &[0x06, 0x11, 0x02]); // READ_USER, proto, items
        // protect on/off over the same command endpoint.
        t56.protect().unwrap().protect_on(&s).unwrap();
        assert_eq!(tx.sent()[1].1[0], 0x19);
    }

    #[test]
    fn read_calibration_packet_and_data() {
        let (mut t56, tx) = t56_with(vec![vec![0xa1, 0xa2, 0xa3, 0xa4]]);
        let cal = t56.read_calibration(4).unwrap();
        assert_eq!(cal, vec![0xa1, 0xa2, 0xa3, 0xa4]);
        // 64-byte header: 0x16, length 4 LE at [2].
        let (ep, hdr) = &tx.sent()[0];
        assert_eq!(*ep, 0x01);
        assert_eq!(hdr.len(), 64);
        assert_eq!(&hdr[..4], &[0x16, 0x00, 0x04, 0x00]);
    }

    /// The full begin() wire sequence: single-shot bitstream (8-byte 0x26
    /// header + raw bits), the 64-byte BEGIN, then the 0x39 status request.
    #[test]
    fn begin_uploads_bitstream_then_header() {
        // Only reply needed: the 0x39 status (ovc at [12] = 0).
        let (mut t56, tx) = t56_with(vec![vec![0u8; 32]]);
        let dev = device(0x03, 0x2100, vec![0xde, 0xad, 0xbe, 0xef]);
        t56.begin(&dev).unwrap();
        let sent = tx.sent();
        assert_eq!(sent.len(), 4);
        // Bitstream header: 0x26, length 4 LE at [4].
        assert_eq!(sent[0], (0x01, vec![0x26, 0, 0, 0, 4, 0, 0, 0]));
        // Raw bitstream, one shot.
        assert_eq!(sent[1], (0x01, vec![0xde, 0xad, 0xbe, 0xef]));
        // The 64-byte BEGIN header == the shared subset, and nothing more.
        let (ep, begin) = &sent[2];
        assert_eq!(*ep, 0x01);
        assert_eq!(begin.len(), 64);
        assert_eq!(begin, &pack_begin64(&ChipParams::from_device(&dev)).to_vec());
        // The T56-vs-T76 distinction: algorithm number and I2C address are NOT
        // emitted even though variant>>8 = 0x21 and i2c_address = 0x51.
        assert_eq!(begin[63], 0, "T56 must not send the algorithm number");
        assert_eq!(begin[24], 0, "T56 must not send the I2C address");
        // Status request.
        assert_eq!(sent[3], (0x01, vec![0x39, 0, 0, 0, 0, 0, 0, 0]));
    }

    /// Frozen BEGIN header for a representative T56 SPI device.
    #[test]
    fn begin_header_golden() {
        let dev = device(0x03, 0x2100, vec![0x01]);
        let begin = pack_begin64(&ChipParams::from_device(&dev));
        assert_eq!(
            hex(&begin),
            "03030000000000000001400000000000008000000000000000000000040000000000000000000000000000000000000000000000000000000000000000000000",
        );
    }

    #[test]
    fn begin_aborts_on_overcurrent() {
        let mut ovc = vec![0u8; 32];
        ovc[12] = 1; // overcurrent flagged
        let (mut t56, _tx) = t56_with(vec![ovc]);
        let dev = device(0x03, 0x2100, vec![0x01]);
        assert_eq!(t56.begin(&dev).unwrap_err().code(), "overcurrent");
    }

    /// read_block over-reads by 16 (the firmware bug) and returns exactly size.
    #[test]
    fn read_block_over_reads_and_truncates() {
        // Reply is size(0x40) + 16 = 80 bytes; first 0x40 are the data.
        let mut reply: Vec<u8> = (0..0x40u8).collect();
        reply.extend_from_slice(&[0xff; 16]);
        let (mut t56, tx) = t56_with(vec![reply]);
        let dev = device(0x03, 0x2100, vec![0x01]);
        let s = Session { device: dev.clone(), emmc_capacity: 0 };
        let req = BlockReq { kind: MemoryKind::Code, address: 0x80, len: 0x40 };
        let out = t56.read_block(&s, &req).unwrap();
        assert_eq!(out, (0..0x40u8).collect::<Vec<u8>>());
        // 8-byte command: 0x0d, size 0x40 LE at [2], addr 0x80 LE at [4].
        assert_eq!(tx.sent()[0], (0x01, vec![0x0d, 0, 0x40, 0, 0x80, 0, 0, 0]));
    }

    #[test]
    fn identify_big_endian_default() {
        // id_type 1 (big-endian), 2 id bytes 0x12 0x34 at resp[2..4].
        let mut resp = vec![0u8; 32];
        resp[0] = 1;
        resp[2] = 0x12;
        resp[3] = 0x34;
        let (mut t56, _tx) = t56_with(vec![resp]);
        let dev = device(0x03, 0x2100, vec![0x01]);
        let s = Session { device: dev.clone(), emmc_capacity: 0 };
        let id = t56.identify(&s).unwrap();
        assert_eq!(id.raw, 0x1234);
        assert_eq!(id.bytes, 2);
    }

    #[test]
    fn query_info_rejects_non_t56() {
        let mut report = vec![0u8; 64];
        report[6] = 0x08; // MP_T76, not T56
        let (mut t56, _tx) = t56_with(vec![report]);
        assert_eq!(t56.query_info().unwrap_err().code(), "unsupported");
    }

    #[test]
    fn query_info_parses_t56_report() {
        let mut report = vec![0u8; 64];
        report[4] = 0x49; // fw minor
        report[5] = 0x01; // fw major
        report[6] = MP_T56;
        report[32..37].copy_from_slice(b"ABC12");
        // voltage raw such that (raw * 0xccf6 / 0x27000) / 100 is sensible.
        report[56..60].copy_from_slice(&1500u32.to_le_bytes());
        let (mut t56, _tx) = t56_with(vec![report]);
        t56.query_info().unwrap();
        assert_eq!(t56.info().serial, "ABC12");
        assert_eq!(t56.info().firmware, FwVersion(0x0149));
        // (1500 * 0xccf6 / 0x27000) / 100 = (1500*52470/159744)/100 ~= 4.92 V.
        assert!((t56.info().voltage - 4.926).abs() < 0.05, "got {}", t56.info().voltage);
    }
}
