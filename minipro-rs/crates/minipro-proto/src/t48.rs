//! The XGecu T48 driver — a fixed-silicon (non-FPGA) programmer.
//!
//! A faithful port of `minipro-t76/src/t48.c` (line numbers cited throughout).
//! The T48 is essentially a re-housed / extended TL866II+: there is **no FPGA
//! and no bitstream** (`get_algorithm` is never called), so [`Programmer::begin`]
//! just sends the shared BEGIN_TRANS header and checks overcurrent — the chip
//! algorithm is selected by the `protocol_id`/`variant` fields inside the
//! programmer's own firmware (`t48.c:246-314`).
//!
//! It differs from the T56 sibling in the wire format in two ways:
//! 1. **Bulk data uses EP02** — `read_block` reads over EP82 IN and
//!    `write_block` writes over EP02 OUT (`read_payload2`/`write_payload2`,
//!    usb_nix.c:334-411), while small commands stay on EP01/EP81. A read
//!    shorter than 64 bytes is padded to a 64-byte transfer to avoid a libusb
//!    overflow (usb_nix.c:397-405).
//! 2. **The clock-enable quirk** — when the chip can adjust its SPI clock the
//!    T48 sets `msg[24]=1` *and* `msg[28]=spi_clock`; the `msg[24]=1` "enable"
//!    byte is required for `msg[28]` to take effect (`t48.c:285-290`). Like the
//!    T56 it does not send `msg[63]` (there is no FPGA algorithm number).
//!
//! Scope of this increment: identity, begin/end, and the MemoryOps core. The
//! T48's large host-side bit-bang / pin-driver subsystem (`PinDriver`), the
//! logic test, fuses, JEDEC rows, TSOP48 unlock, and firmware update are
//! deferred.

use minipro_core::caps::MemoryOps;
use minipro_core::device::{BlockReq, ChipId, Device, EraseKind, MemoryKind, Region};
use minipro_core::error::{Error, FwVersion, Result};
use minipro_core::programmer::{Caps, Programmer, ProgrammerInfo, Session};
use minipro_core::transport::{command, Ep, LinkSpeed, Transport};

use crate::wire::{
    ascii_field, le16, le32, pack_begin64, ChipParams, CMD_END_TRANS, CMD_ERASE, CMD_READID,
    CMD_READ_CODE, CMD_REQUEST_STATUS, CMD_WRITE_CODE,
};

// Endpoints: commands on EP01/EP81, bulk block data on EP82 IN / EP02 OUT
// (usb_nix.c:342-411, non-T76 path).
const EP_MSG_OUT: Ep = Ep(0x01);
const EP_MSG_IN: Ep = Ep(0x81);
const EP_DAT_IN: Ep = Ep(0x82);
const EP_DAT_OUT: Ep = Ep(0x02);

// T48-specific opcodes (t48.c:34-65); shared II+-family opcodes come from
// `crate::wire`.
const MP_T48: u8 = 0x07; // device-type byte in the system-info report (minipro.h:28)
const CMD_WRITE_USER_DATA: u8 = 0x0a; // t48.c:42
const CMD_READ_USER_DATA: u8 = 0x0b; // t48.c:43
const CMD_READ_DATA: u8 = 0x10; // t48.c:48
const CMD_WRITE_DATA: u8 = 0x11; // t48.c:49

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
            link: LinkSpeed::High,
            voltage: 0.0,
        };
        T48 { tx, info }
    }

    /// Query programmer identity (`minipro_get_system_info`, minipro.c:221-232,
    /// T48 case): `[4]`=fw minor, `[5]`=fw major, `[6]`=device type (7 = T48),
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

    /// `0x39` REQUEST_STATUS; overcurrent byte at `resp[12]`
    /// (`t48_get_ovc_status`).
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
        // Fixed-silicon core: memory ops only this increment. The pin-driver /
        // bit-bang subsystem, logic test, fuses, and JEDEC rows are deferred.
        Caps::MEMORY
    }

    /// `t48_begin_transaction` (t48.c:246-314): the shared 64-byte header (no
    /// FPGA, no bitstream), the `msg[24]=1` clock-enable quirk, then the
    /// overcurrent check.
    fn begin(&mut self, dev: &Device) -> Result<Session> {
        let params = ChipParams::from_device(dev);
        let mut msg = pack_begin64(&params);
        // Clock-enable quirk (t48.c:285-290): when the chip can adjust its SPI
        // clock the T48 needs msg[24]=1 for the msg[28] clock byte to take
        // effect. ChipParams doesn't yet carry the decoded `can_adjust_clock`
        // flag; under the DB invariant `spi_clock != 0` iff `can_adjust_clock`,
        // so gating on spi_clock reproduces the C output exactly.
        // TODO(db): key this on a decoded can_adjust_clock flag once Device
        // carries the decoded flag set.
        if params.spi_clock != 0 {
            msg[24] = 1;
        }
        self.send(&msg)?; // t48.c:300 — 64 bytes, no reply

        let ovc = self.ovc_status()?; // t48.c:307
        if ovc != 0 {
            return Err(Error::Overcurrent);
        }
        Ok(Session { device: dev.clone(), emmc_capacity: 0 })
    }

    /// `t48_end_transaction` (t48.c:316-325): a bare END_TRANS, no reply.
    fn end(&mut self, _session: Session) -> Result<()> {
        let mut msg = [0u8; 8];
        msg[0] = CMD_END_TRANS;
        self.send(&msg)
    }

    /// `t48_get_chip_id`: READID, decode per id type (3/4 little-endian, else
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
}

impl MemoryOps for T48 {
    /// `t48_read_block` (t48.c:327-352): 8-byte command on EP01, then the data
    /// on EP82. A read shorter than 64 bytes is padded to a 64-byte transfer
    /// and truncated (usb_nix.c:397-405).
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

    /// `t48_write_block` (t48.c:354-382): 8-byte command on EP01, then the
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
        // TODO(hw): the C sends exactly `device->write_buffer_size` bytes over
        // EP02 (t48.c:378-380); matches when write_buffer_size == page_size.
        self.tx.send(EP_DAT_OUT, data)
    }

    /// `t48_erase` (t48.c): a 15-byte 0x0e (num_fuses/pld zero for a plain
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
            let req = BlockReq { kind: region.kind, address: region.offset + done, len };
            if self.read_block(s, &req)?.iter().any(|&b| b != 0xff) {
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
            package: Package { pin_count: 8, name: "DIP8".into() },
            fw_target: FwVersion(0),
            ..Device::default()
        }
    }

    #[test]
    fn caps_memory_only() {
        let (mut t48, _tx) = t48_with(vec![]);
        assert_eq!(t48.caps().contains(Caps::MEMORY), t48.memory().is_some());
        assert!(t48.calibration().is_none()); // T48 has no calibration op
        assert!(t48.fuses().is_none());
    }

    /// begin() sends only the 64-byte header (no bitstream) then the 0x39
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
        let s = Session { device: dev.clone(), emmc_capacity: 0 };
        let req = BlockReq { kind: MemoryKind::Code, address: 0, len: 0x20 };
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
        let s = Session { device: dev.clone(), emmc_capacity: 0 };
        let req = BlockReq { kind: MemoryKind::Code, address: 0x40, len: 4 };
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
