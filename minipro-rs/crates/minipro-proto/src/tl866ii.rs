// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! The TL866II+ driver — the oldest member of the shared-protocol family.
//!
//! An independent Rust implementation of the TL866II+ USB wire protocol — the
//! functional facts (opcodes, packet layouts, operation sequence) reverse-
//! engineered by the minipro community (nmatt0 — see the repo `NOTICE`).
//! Like the T48 it is fixed-silicon: no FPGA, no bitstream, the chip algorithm
//! is chosen by `protocol_id`/`variant` inside the firmware. It shares the
//! family opcode table, and differs from the T48 on the wire in four ways:
//!
//! 1. **No SPI clock-adjust extension** — the `msg[24]`/`msg[28]` bytes of
//!    BEGIN_TRANS stay zero (the hardware predates the feature).
//! 2. **Interlaced bulk transfers** — payloads larger than 64 bytes stream
//!    across *two* endpoint pairs (EP2+EP3), read in parallel and deinterlaced
//!    as alternating 64-byte blocks; writes split contiguously per XGPro's
//!    arithmetic. See [`Transport::recv_interlaced`] and [`Transport::send_interlaced`].
//! 3. **Small writes ride the command pipe** — a block shorter than 57 bytes
//!    is sent as one EP1 message (8-byte header + data), no bulk pipe at all.
//! 4. **User-space reads ride the command pipe** — `MP_USER` data always
//!    returns over EP81.
//!
//! The system-info report is also its own shape: device code at `[8..16]`,
//! serial at `[16..36]`, a hardware-revision byte at `[40]` that becomes the
//! leading field of the displayed version (`04.2.132`), and no voltage.
//!
//! Implemented: identity, begin/end, MemoryOps, fuses, JEDEC rows, protect,
//! SPI autodetect, and the logic test (the shared vector loop). Deferred:
//! firmware update (a 300-line XOR-scrambled container of its own), the
//! bit-bang/custom-protocol subsystem, and the hardware self-test — the same
//! boundaries as the T48 driver, for the same reasons.
//!
//! **Reference-only**: no TL866II+ has ever been attached to this codebase.
//! Every packet below is pinned against nmatt0's C, not against silicon — and
//! this project's record says first hardware contact finds a bug.

use minipro_core::caps::{
    FuseOps, JedecOps, LoadBitstream, LogicTest, MemoryOps, Protect, SpiAutodetect, TransferDir,
};
use minipro_core::device::{BlockReq, ChipId, Device, EraseKind, FuseKind, MemoryKind, Region};
use minipro_core::error::{Error, FwVersion, Result};
use minipro_core::programmer::{Caps, Programmer, ProgrammerInfo, Session};
use minipro_core::transport::{command, Ep, LinkSpeed, Transport};

use crate::wire::{
    self, ascii_field, le16, le32, pack_begin64, ChipParams, CMD_END_TRANS, CMD_ERASE, CMD_READID,
    CMD_READ_CODE, CMD_REQUEST_STATUS, CMD_WRITE_CODE,
};

// Commands on EP01/EP81; bulk data on EP82/EP02, with EP83/EP03 as the second
// half of an interlaced transfer.
const EP_MSG_OUT: Ep = Ep(0x01);
const EP_MSG_IN: Ep = Ep(0x81);
const EP_DAT_IN: Ep = Ep(0x82);
const EP_DAT_IN2: Ep = Ep(0x83);
const EP_DAT_OUT: Ep = Ep(0x02);
const EP_DAT_OUT2: Ep = Ep(0x03);

const MP_TL866II: u8 = 0x05; // device-type byte in the system-info report
const CMD_WRITE_USER_DATA: u8 = 0x0a;
const CMD_READ_USER_DATA: u8 = 0x0b;
const CMD_READ_DATA: u8 = 0x10;
const CMD_WRITE_DATA: u8 = 0x11;
const CMD_AUTODETECT: u8 = 0x37;

/// Payloads at or under this ride a single bulk endpoint; above it they
/// interlace across the pair (the C's `limit = 64`).
const SINGLE_PIPE_MAX: usize = 64;
/// A write this short is folded into the EP1 command itself (8 + data ≤ 64).
const INLINE_WRITE_MAX: usize = 56;

/// The TL866II+ command channel + cached identity.
pub struct Tl866ii {
    tx: Box<dyn Transport>,
    info: ProgrammerInfo,
}

impl Tl866ii {
    pub fn new(tx: Box<dyn Transport>) -> Self {
        let info = ProgrammerInfo {
            model: "TL866II+".into(),
            firmware: FwVersion(0),
            serial: String::new(),
            mfg_date: String::new(),
            device_code: String::new(),
            link: LinkSpeed::High,
            voltage: 0.0,
            bootloader: false,
        };
        Tl866ii { tx, info }
    }

    /// Query identity from the TL866II+ system-info report — its own layout:
    /// `[4]`=fw minor, `[5]`=fw major, `[6]`=device type (5), `[8..16]`=device
    /// code, `[16..36]`=serial, `[40]`=hardware revision. No voltage field.
    /// The display version leads with the hardware revision (`04.2.132`).
    pub fn query_info(&mut self) -> Result<()> {
        let msg = self.cmd(&[0u8; 5], 64)?;
        if msg.len() < 41 {
            return Err(Error::Protocol);
        }
        let (minor, major) = (msg[4], msg[5]);
        let bootloader = minor == 0 && major == 0;
        if !bootloader && msg[6] != MP_TL866II {
            return Err(Error::Unsupported("attached programmer is not a TL866II+"));
        }
        self.info = ProgrammerInfo {
            model: "TL866II+".into(),
            firmware: FwVersion(
                (u32::from(msg[40]) << 16) | (u32::from(major) << 8) | u32::from(minor),
            ),
            serial: ascii_field(&msg[16..36]),
            mfg_date: String::new(), // the report carries none
            device_code: ascii_field(&msg[8..16]),
            link: self.tx.link_speed(),
            voltage: 0.0, // the report carries none
            bootloader,
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

    /// Bulk receive per the family rule: ≤64 bytes on EP82 alone (short reads
    /// padded to one 64-byte transfer), larger interlaced across EP82+EP83.
    fn recv_bulk(&mut self, len: usize) -> Result<Vec<u8>> {
        if len <= SINGLE_PIPE_MAX {
            let mut data = self.tx.recv(EP_DAT_IN, len.max(64))?;
            if data.len() < len {
                return Err(Error::Protocol);
            }
            data.truncate(len);
            Ok(data)
        } else {
            self.tx.recv_interlaced(EP_DAT_IN, EP_DAT_IN2, len)
        }
    }

    /// Bulk send per the family rule: ≤64 bytes on EP02 alone, larger split
    /// across EP02+EP03 by the XGPro arithmetic in the transport.
    fn send_bulk(&mut self, data: &[u8]) -> Result<()> {
        if data.len() <= SINGLE_PIPE_MAX {
            self.tx.send(EP_DAT_OUT, data)
        } else {
            self.tx.send_interlaced(EP_DAT_OUT, EP_DAT_OUT2, data)
        }
    }
}

impl Programmer for Tl866ii {
    fn info(&self) -> &ProgrammerInfo {
        &self.info
    }

    fn caps(&self) -> Caps {
        Caps::MEMORY | Caps::FUSES | Caps::JEDEC | Caps::PROTECT | Caps::AUTODETECT | Caps::LOGIC
    }

    /// Begin a transaction: the shared 64-byte header. Unlike the T48 there is
    /// no clock-enable extension — `msg[24]`/`msg[28]` stay zero.
    fn begin(&mut self, dev: &Device) -> Result<Session> {
        let params = ChipParams::from_device(dev);
        let mut msg = pack_begin64(&params);
        // The shared header writes msg[28] = spi_clock for the newer family
        // members; the TL866II+ predates the feature and its BEGIN carries no
        // clock byte at all — the C never touches msg[24]/msg[28] here.
        msg[24] = 0;
        msg[28] = 0;
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

    /// Read the chip ID: READID, a 6-byte reply (`[0]`=id type, `[1]`=length,
    /// bytes from `[2]`), decoded per id type as in the rest of the family.
    fn identify(&mut self, s: &Session) -> Result<ChipId> {
        let mut msg = [0u8; 8];
        msg[0] = CMD_READID;
        let resp = self.cmd(&msg, 6)?;
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

impl FuseOps for Tl866ii {
    /// Read a fuse block via the shared wire helper (identical to the T48).
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
    /// Write a fuse block via the shared wire helper (identical to the T48).
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

impl JedecOps for Tl866ii {
    fn read_row(&mut self, s: &Session, row: u8, flags: u8, size: u16) -> Result<Vec<u8>> {
        wire::jedec_read(self.tx.as_mut(), s.device.protocol_id, row, flags, size)
    }
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

impl Protect for Tl866ii {
    fn protect_on(&mut self, _s: &Session) -> Result<()> {
        wire::protect(self.tx.as_mut(), true)
    }
    fn protect_off(&mut self, _s: &Session) -> Result<()> {
        wire::protect(self.tx.as_mut(), false)
    }
}

impl SpiAutodetect for Tl866ii {
    /// SPI autodetect: `[0]`=0x37, `[8]`=package flag; send 10, recv **16**
    /// (the T48 reads 32 here), id = 3 bytes big-endian from `[2]`.
    fn spi_autodetect(&mut self, wide: bool, _load: LoadBitstream<'_>) -> Result<u32> {
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

impl LogicTest for Tl866ii {
    /// Logic-IC test: the same shared vector loop the whole family uses.
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

impl MemoryOps for Tl866ii {
    /// Read a block: 8-byte command on EP01. `MP_USER` data comes back on the
    /// command pipe itself; everything else on the bulk pipe(s).
    fn read_block(&mut self, _s: &Session, req: &BlockReq) -> Result<Vec<u8>> {
        let op = match req.kind {
            MemoryKind::Code => CMD_READ_CODE,
            MemoryKind::Data => CMD_READ_DATA,
            MemoryKind::User => CMD_READ_USER_DATA,
            MemoryKind::Data2 | MemoryKind::Config => {
                return Err(Error::Unsupported(
                    "TL866II+ read_block: Data2/Config spaces",
                ))
            }
        };
        let mut msg = [0u8; 8];
        msg[0] = op;
        le16(&mut msg, 2, req.len.min(u32::from(u16::MAX)) as u16);
        le32(&mut msg, 4, req.address.min(u64::from(u32::MAX)) as u32);
        if req.kind == MemoryKind::User {
            // User pages always ride the command pipe.
            return self.cmd(&msg, req.len as usize);
        }
        self.send(&msg)?;
        self.recv_bulk(req.len as usize)
    }

    /// Write a block. Short blocks are folded into the EP1 command itself
    /// (8-byte header + data); larger ones send the header on EP1 and the
    /// payload on the bulk pipe(s).
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
                return Err(Error::Unsupported(
                    "TL866II+ write_block: Data2/Config spaces",
                ))
            }
        };
        let mut msg = [0u8; 8];
        msg[0] = op;
        le16(&mut msg, 2, req.len.min(u32::from(u16::MAX)) as u16);
        le32(&mut msg, 4, req.address.min(u64::from(u32::MAX)) as u32);
        if data.len() <= INLINE_WRITE_MAX {
            let mut pkt = Vec::with_capacity(8 + data.len());
            pkt.extend_from_slice(&msg);
            pkt.extend_from_slice(data);
            return self.send(&pkt);
        }
        self.send(&msg)?;
        // TODO(hw): the C sends exactly `device->write_buffer_size` bytes;
        // matches when write_buffer_size == page_size (same note as the T48).
        self.send_bulk(data)
    }

    /// Erase the chip: a 15-byte 0x0e, then drain the 64-byte reply.
    fn erase(&mut self, _s: &Session, kind: EraseKind) -> Result<()> {
        if let EraseKind::Sector { .. } = kind {
            return Err(Error::Unsupported("TL866II+ has no sector erase"));
        }
        let mut msg = [0u8; 15];
        msg[0] = CMD_ERASE;
        command(self.tx.as_mut(), EP_MSG_OUT, EP_MSG_IN, &msg, 64)?.discard()
    }

    /// Fixed-silicon chunking, per the C: reads step by `read_buffer_size`,
    /// writes by `write_buffer_size` — two different numbers on many parts
    /// (page size stands in when the catalog carries no buffer size).
    fn block_size(&self, s: &Session, _kind: MemoryKind, dir: TransferDir) -> u32 {
        let fallback = match s.device.page_size {
            0 => 4096,
            n => n,
        };
        let buf = match dir {
            TransferDir::Read => u32::from(s.device.read_buffer_size),
            TransferDir::Write => u32::from(s.device.write_buffer_size),
        };
        if buf == 0 {
            fallback
        } else {
            buf
        }
    }

    /// Blank check by reading the region back.
    fn blank_check(&mut self, s: &Session, region: Region) -> Result<bool> {
        let step = u64::from(self.block_size(s, region.kind, TransferDir::Read));
        for req in region.blocks(step) {
            if self
                .read_block(s, &req)?
                .iter()
                .any(|&b| b != s.device.blank_value)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Records every transfer — including the interlaced pair calls, which the
    /// stock mock does not implement — and pops canned replies in order.
    struct PairTx {
        sent: Vec<(u8, Vec<u8>)>,
        /// (ep_a, ep_b, first_len) per send_interlaced call.
        sent_pairs: Vec<(u8, u8, usize)>,
        /// (ep_a, ep_b, len) per recv_interlaced call.
        recv_pairs: Vec<(u8, u8, usize)>,
        replies: VecDeque<Vec<u8>>,
    }

    impl PairTx {
        fn new(replies: Vec<Vec<u8>>) -> Self {
            PairTx {
                sent: Vec::new(),
                sent_pairs: Vec::new(),
                recv_pairs: Vec::new(),
                replies: replies.into(),
            }
        }
    }

    impl Transport for PairTx {
        fn send(&mut self, ep: Ep, data: &[u8]) -> Result<()> {
            self.sent.push((ep.0, data.to_vec()));
            Ok(())
        }
        fn recv(&mut self, _ep: Ep, _len: usize) -> Result<Vec<u8>> {
            self.replies.pop_front().ok_or(Error::Protocol)
        }
        fn recv_interlaced(&mut self, a: Ep, b: Ep, len: usize) -> Result<Vec<u8>> {
            self.recv_pairs.push((a.0, b.0, len));
            self.replies.pop_front().ok_or(Error::Protocol)
        }
        fn send_interlaced(&mut self, a: Ep, b: Ep, data: &[u8]) -> Result<()> {
            self.sent_pairs.push((a.0, b.0, data.len()));
            Ok(())
        }
        fn link_speed(&self) -> LinkSpeed {
            LinkSpeed::High
        }
        fn reset(&mut self) -> Result<()> {
            Ok(())
        }
    }

    /// A cloneable handle onto a shared `PairTx` (same pattern as the other
    /// drivers' scripted transports): the driver owns one clone, the test
    /// inspects through the other.
    #[derive(Clone)]
    struct SharedTx(std::sync::Arc<std::sync::Mutex<PairTx>>);

    impl Transport for SharedTx {
        fn send(&mut self, ep: Ep, data: &[u8]) -> Result<()> {
            self.0.lock().unwrap().send(ep, data)
        }
        fn recv(&mut self, ep: Ep, len: usize) -> Result<Vec<u8>> {
            self.0.lock().unwrap().recv(ep, len)
        }
        fn recv_interlaced(&mut self, a: Ep, b: Ep, len: usize) -> Result<Vec<u8>> {
            self.0.lock().unwrap().recv_interlaced(a, b, len)
        }
        fn send_interlaced(&mut self, a: Ep, b: Ep, data: &[u8]) -> Result<()> {
            self.0.lock().unwrap().send_interlaced(a, b, data)
        }
        fn link_speed(&self) -> LinkSpeed {
            LinkSpeed::High
        }
        fn reset(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn with_tx(replies: Vec<Vec<u8>>) -> (Tl866ii, SharedTx) {
        let tx = SharedTx(std::sync::Arc::new(std::sync::Mutex::new(PairTx::new(
            replies,
        ))));
        (Tl866ii::new(Box::new(tx.clone())), tx)
    }

    fn device() -> Device {
        Device {
            name: "X".into(),
            protocol_id: 0x07,
            chip_id_bytes: 2,
            ..Default::default()
        }
    }
    fn session(dev: &Device) -> Session {
        Session {
            device: dev.clone(),
            emmc_capacity: 0,
        }
    }

    #[test]
    fn query_info_parses_the_iiplus_layout() {
        let mut report = vec![0u8; 64];
        report[4] = 132; // fw minor
        report[5] = 2; // fw major
        report[6] = MP_TL866II;
        report[8..16].copy_from_slice(b"DEVCODE\0");
        report[16..26].copy_from_slice(b"SERIAL1234");
        report[40] = 4; // hardware revision -> leading version field
        let (mut p, _tx) = with_tx(vec![report]);
        p.query_info().unwrap();
        let info = p.info();
        assert_eq!(info.model, "TL866II+");
        // 04.2.132, the vendor's own display shape.
        assert_eq!(info.firmware.to_string(), "04.2.132");
        assert_eq!(info.serial, "SERIAL1234");
        assert_eq!(info.device_code, "DEVCODE");
        assert!(info.mfg_date.is_empty(), "the report carries no mfg date");
        assert!(!info.bootloader);
    }

    #[test]
    fn begin_has_no_clock_extension() {
        let mut dev = device();
        dev.spi_clock = 3; // would set msg[24]/[28] on a T48
        let (mut p, tx) = with_tx(vec![vec![0u8; 32]]); // 0x39 status reply
        p.begin(&dev).unwrap();
        let guard = tx.0.lock().unwrap();
        let sent = &guard.sent;
        assert_eq!(sent[0].1.len(), 64);
        assert_eq!(sent[0].1[24], 0, "no clock-enable byte on a TL866II+");
        assert_eq!(sent[0].1[28], 0, "no clock byte on a TL866II+");
    }

    #[test]
    fn identify_reads_a_six_byte_reply() {
        let dev = device();
        let (mut p, _tx) = with_tx(vec![vec![0x00, 0x02, 0xc2, 0x20, 0, 0]]);
        let id = p.identify(&session(&dev)).unwrap();
        assert_eq!(id.raw, 0xc220);
        assert_eq!(id.bytes, 2);
    }

    #[test]
    fn small_write_rides_the_command_pipe() {
        let dev = device();
        let data = vec![0xabu8; 16]; // <= INLINE_WRITE_MAX
        let (mut p, tx) = with_tx(vec![]);
        let req = BlockReq::single(MemoryKind::Code, 0x200, 16);
        p.write_block(&session(&dev), &req, &data).unwrap();
        let t = tx.0.lock().unwrap();
        assert!(t.sent_pairs.is_empty(), "no bulk pipe for a small write");
        assert_eq!(t.sent.len(), 1, "one EP1 message, header + data folded");
        let (ep, pkt) = &t.sent[0];
        assert_eq!(*ep, 0x01);
        assert_eq!(pkt.len(), 8 + 16);
        assert_eq!(pkt[0], CMD_WRITE_CODE);
        assert_eq!(&pkt[2..4], &[16, 0]); // length LE16
        assert_eq!(&pkt[4..8], &[0x00, 0x02, 0, 0]); // address LE32
        assert_eq!(&pkt[8..], &data[..]);
    }

    #[test]
    fn large_write_splits_across_the_endpoint_pair() {
        let dev = device();
        let data = vec![0x55u8; 256];
        let (mut p, tx) = with_tx(vec![]);
        let req = BlockReq::single(MemoryKind::Code, 0, 256);
        p.write_block(&session(&dev), &req, &data).unwrap();
        let t = tx.0.lock().unwrap();
        assert_eq!(t.sent.len(), 1, "header only on EP1");
        assert_eq!(t.sent[0].1.len(), 8);
        assert_eq!(t.sent_pairs, vec![(0x02, 0x03, 256)]);
    }

    #[test]
    fn large_read_interlaces_and_user_read_rides_ep1() {
        let dev = device();
        let (mut p, tx) = with_tx(vec![vec![0x99u8; 256], vec![0x77u8; 32]]);
        // Code read > 64: interlaced.
        let req = BlockReq::single(MemoryKind::Code, 0, 256);
        let out = p.read_block(&session(&dev), &req).unwrap();
        assert_eq!(out.len(), 256);
        assert_eq!(tx.0.lock().unwrap().recv_pairs, vec![(0x82, 0x83, 256)]);
        // User read: EP1 command round-trip, no bulk involvement.
        let req = BlockReq::single(MemoryKind::User, 0, 32);
        let out = p.read_block(&session(&dev), &req).unwrap();
        assert_eq!(out.len(), 32);
        assert_eq!(
            tx.0.lock().unwrap().recv_pairs.len(),
            1,
            "user read used no pair"
        );
    }

    #[test]
    fn autodetect_reads_sixteen_bytes_and_decodes_the_id() {
        let mut reply = vec![0u8; 16];
        reply[2..5].copy_from_slice(&[0xef, 0x40, 0x17]);
        let (mut p, _tx) = with_tx(vec![reply]);
        let id = p.spi_autodetect(false, &mut |_| Ok(vec![])).unwrap();
        assert_eq!(id, 0xef4017);
    }
}
