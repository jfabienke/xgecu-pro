// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! Orchestration: the block loop, progress reporting, and built-in
//! verification — written once here, generic over any `dyn Programmer` and
//! `dyn Reporter`, so drivers stay small (they only implement `*_block`).

use crate::device::{BlockReq, Image, Region};
use crate::error::{Error, Result};
use crate::programmer::{Programmer, Session};
use crate::report::{Event, Outcome, Reporter};
use crate::transport::LinkSpeed;

use sha2::Digest;

/// Read a whole region, looping block requests and reporting progress. This is
/// where a driver's block-granular [`crate::caps::MemoryOps`] becomes a
/// user-level "read the chip".
pub fn read_region(
    prog: &mut dyn Programmer,
    s: &Session,
    region: Region,
    rep: &mut dyn Reporter,
) -> Result<Image> {
    let mem = prog.memory().ok_or(Error::Unsupported("memory ops"))?;
    let total = region.len;
    let step = u64::from(mem.block_size(s, region.kind));
    let mut bytes = Vec::with_capacity(total as usize);
    rep.event(&Event::Progress { done: 0, total });
    let mut done = 0u64;
    while done < total {
        let len = step.min(total - done) as u32;
        let req = BlockReq {
            kind: region.kind,
            address: region.offset + done,
            len,
        };
        let block = mem.read_block(s, &req)?;
        if block.len() != len as usize {
            // A short/long response means the command stream desynced.
            return Err(Error::Protocol);
        }
        bytes.extend_from_slice(&block);
        done += u64::from(len);
        rep.event(&Event::Progress { done, total });
    }
    Ok(Image { bytes })
}

/// The result of [`read_verified`]: the dump plus the trust evidence the JSON
/// `Outcome::Read` reports (stability across re-reads, crc32, sha256).
pub struct VerifiedRead {
    pub image: Image,
    /// `true` when every read returned byte-identical data.
    pub stable: bool,
    /// How many full reads were performed.
    pub reads: u8,
    /// CRC-32 (IEEE) of the first read's bytes.
    pub crc32: u32,
    /// SHA-256 of the first read's bytes.
    pub sha256: [u8; 32],
}

impl VerifiedRead {
    /// Build the terminal [`Outcome::Read`] this verification evidences.
    pub fn outcome(&self, device: &str, link: LinkSpeed) -> Outcome {
        Outcome::Read {
            device: device.to_owned(),
            bytes: self.image.len() as u64,
            crc32: self.crc32,
            sha256: self.sha256,
            reads: self.reads,
            stable: self.stable,
            link,
        }
    }
}

/// Read twice and confirm byte-identical — the "is this dump trustworthy?"
/// check, promoted to a first-class operation (our AHA-1542CP lesson). Also
/// computes the crc32/sha256 the JSON outcome carries, so an agent can trust
/// the dump in one call. An unstable re-read is reported as a note, not an
/// error: the caller decides whether an unstable dump is acceptable.
pub fn read_verified(
    prog: &mut dyn Programmer,
    s: &Session,
    region: Region,
    rep: &mut dyn Reporter,
) -> Result<VerifiedRead> {
    let a = read_region(prog, s, region, rep)?;
    let b = read_region(prog, s, region, rep)?;
    let stable = a.bytes == b.bytes;
    if !stable {
        rep.event(&Event::Note(
            "re-read differs from first read: dump is unstable".into(),
        ));
    }
    let crc32 = crc32fast::hash(&a.bytes);
    let sha256: [u8; 32] = sha2::Sha256::digest(&a.bytes).into();
    Ok(VerifiedRead {
        image: a,
        stable,
        reads: 2,
        crc32,
        sha256,
    })
}

/// Write a region and verify by read-back. The write loop reports progress
/// over `region.len`; the verify pass then re-reads the region (with its own
/// progress) and fails with [`Error::Verify`] at the first differing address.
pub fn write_region(
    prog: &mut dyn Programmer,
    s: &Session,
    region: Region,
    image: &Image,
    rep: &mut dyn Reporter,
) -> Result<()> {
    if image.bytes.len() as u64 != region.len {
        return Err(Error::Format(format!(
            "image is {} bytes but region is {} bytes",
            image.bytes.len(),
            region.len
        )));
    }

    // Write loop. Scoped so the `MemoryOps` borrow ends before the read-back
    // pass borrows the programmer again.
    {
        let mem = prog.memory().ok_or(Error::Unsupported("memory ops"))?;
        let total = region.len;
        let step = u64::from(mem.block_size(s, region.kind));
        rep.event(&Event::Progress { done: 0, total });
        let mut done = 0u64;
        while done < total {
            let len = step.min(total - done) as u32;
            let req = BlockReq {
                kind: region.kind,
                address: region.offset + done,
                len,
            };
            let start = done as usize;
            mem.write_block(s, &req, &image.bytes[start..start + len as usize])?;
            done += u64::from(len);
            rep.event(&Event::Progress { done, total });
        }
    }

    // Read-back verify.
    let back = read_region(prog, s, region, rep)?;
    if let Some(i) = back
        .bytes
        .iter()
        .zip(&image.bytes)
        .position(|(a, b)| a != b)
    {
        return Err(Error::Verify {
            addr: (region.offset + i as u64) as u32,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{Algorithm, ChipId, Device, EraseKind, MemoryKind, Package};
    use crate::error::FwVersion;
    use crate::programmer::{Caps, ProgrammerInfo};

    /// A reporter that records progress ticks and notes.
    #[derive(Default)]
    struct Collect {
        progress: Vec<(u64, u64)>,
        notes: Vec<String>,
    }
    impl Reporter for Collect {
        fn event(&mut self, ev: &Event) {
            match ev {
                Event::Progress { done, total } => self.progress.push((*done, *total)),
                Event::Note(n) => self.notes.push(n.to_string()),
                Event::Warn(_) => {}
            }
        }
        fn finish(&mut self, _out: &Outcome) {}
    }

    /// An in-memory fake programmer: a flat byte array behind `MemoryOps`.
    /// `flaky` makes every second read_block call return flipped bytes, to
    /// exercise the stability check.
    struct FakeProg {
        info: ProgrammerInfo,
        mem: Vec<u8>,
        flaky: bool,
        reads: usize,
        /// If set, write_block silently corrupts this offset (verify test).
        corrupt_at: Option<usize>,
    }

    impl FakeProg {
        fn new(mem: Vec<u8>) -> Self {
            FakeProg {
                info: ProgrammerInfo {
                    model: "FAKE".into(),
                    firmware: FwVersion(0x00_01_11),
                    serial: "0".into(),
                    mfg_date: String::new(),
                    device_code: String::new(),
                    link: LinkSpeed::High,
                    voltage: 5.0,
                },
                mem,
                flaky: false,
                reads: 0,
                corrupt_at: None,
            }
        }
    }

    impl Programmer for FakeProg {
        fn info(&self) -> &ProgrammerInfo {
            &self.info
        }
        fn caps(&self) -> Caps {
            Caps::MEMORY
        }
        fn begin(&mut self, dev: &Device) -> Result<Session> {
            Ok(Session {
                device: dev.clone(),
                emmc_capacity: 0,
            })
        }
        fn end(&mut self, _session: Session) -> Result<()> {
            Ok(())
        }
        fn identify(&mut self, s: &Session) -> Result<ChipId> {
            Ok(ChipId {
                raw: s.device.chip_id,
                bytes: s.device.chip_id_bytes,
            })
        }
        fn reset(&mut self) -> Result<()> {
            Ok(())
        }
        fn memory(&mut self) -> Option<&mut dyn crate::caps::MemoryOps> {
            Some(self)
        }
    }

    impl crate::caps::MemoryOps for FakeProg {
        fn read_block(&mut self, _s: &Session, req: &BlockReq) -> Result<Vec<u8>> {
            self.reads += 1;
            let start = req.address as usize;
            let mut out = self.mem[start..start + req.len as usize].to_vec();
            // Flip bits on the second full pass to simulate an unstable chip.
            if self.flaky && self.reads > 1 {
                for b in &mut out {
                    *b = !*b;
                }
            }
            Ok(out)
        }
        fn write_block(&mut self, _s: &Session, req: &BlockReq, data: &[u8]) -> Result<()> {
            let start = req.address as usize;
            self.mem[start..start + data.len()].copy_from_slice(data);
            if let Some(at) = self.corrupt_at {
                if (start..start + data.len()).contains(&at) {
                    self.mem[at] ^= 0xff;
                }
            }
            Ok(())
        }
        fn erase(&mut self, _s: &Session, _kind: EraseKind) -> Result<()> {
            self.mem.fill(0xff);
            Ok(())
        }
        fn blank_check(&mut self, _s: &Session, _region: Region) -> Result<bool> {
            Ok(self.mem.iter().all(|&b| b == 0xff))
        }
    }

    fn test_device(code_size: u64, page_size: u32) -> Device {
        Device {
            name: "TEST@DIP8".into(),
            protocol_id: 1,
            code_size,
            data_size: 0,
            page_size,
            chip_id: 0x1234,
            chip_id_bytes: 2,
            package: Package {
                pin_count: 8,
                name: "DIP8".into(),
            },
            algorithm: Some(Algorithm {
                name: "test".into(),
                bitstream: Vec::new(),
            }),
            fw_target: FwVersion(0x00_01_11),
            ..Device::default()
        }
    }

    fn session_for(dev: &Device) -> Session {
        Session {
            device: dev.clone(),
            emmc_capacity: 0,
        }
    }

    #[test]
    fn crc32_known_vector() {
        // The classic CRC-32 (IEEE) check value.
        assert_eq!(crc32fast::hash(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn sha256_known_vector() {
        let d: [u8; 32] = sha2::Sha256::digest(b"").into();
        let hex: String = d.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn read_region_assembles_blocks_and_reports_progress() {
        let data: Vec<u8> = (0u8..10).collect();
        let dev = test_device(10, 4);
        let s = session_for(&dev);
        let mut prog = FakeProg::new(data.clone());
        let mut rep = Collect::default();
        let img = read_region(&mut prog, &s, Region::code(&dev), &mut rep).unwrap();
        assert_eq!(img.bytes, data);
        // Blocks of 4, 4, 2 — with a leading 0/10 tick.
        assert_eq!(rep.progress, vec![(0, 10), (4, 10), (8, 10), (10, 10)]);
    }

    #[test]
    fn read_region_uses_default_block_when_page_size_zero() {
        let data = vec![0x5a; 100];
        let dev = test_device(100, 0);
        let s = session_for(&dev);
        let mut prog = FakeProg::new(data.clone());
        let mut rep = Collect::default();
        let img = read_region(&mut prog, &s, Region::code(&dev), &mut rep).unwrap();
        assert_eq!(img.bytes, data);
        assert_eq!(prog.reads, 1); // 100 bytes < DEFAULT_BLOCK -> one block
    }

    #[test]
    fn read_region_respects_region_offset() {
        let data: Vec<u8> = (0u8..16).collect();
        let dev = test_device(16, 8);
        let s = session_for(&dev);
        let mut prog = FakeProg::new(data);
        let mut rep = Collect::default();
        let region = Region {
            kind: MemoryKind::Code,
            offset: 8,
            len: 8,
        };
        let img = read_region(&mut prog, &s, region, &mut rep).unwrap();
        assert_eq!(img.bytes, (8u8..16).collect::<Vec<u8>>());
    }

    #[test]
    fn read_verified_stable_computes_hashes() {
        let data = b"123456789".to_vec();
        let dev = test_device(9, 4);
        let s = session_for(&dev);
        let mut prog = FakeProg::new(data.clone());
        let mut rep = Collect::default();
        let v = read_verified(&mut prog, &s, Region::code(&dev), &mut rep).unwrap();
        assert!(v.stable);
        assert_eq!(v.reads, 2);
        assert_eq!(v.image.bytes, data);
        assert_eq!(v.crc32, 0xcbf4_3926);
        let expect: [u8; 32] = sha2::Sha256::digest(&data).into();
        assert_eq!(v.sha256, expect);
        assert!(rep.notes.is_empty());

        // And the Outcome built from it carries the same evidence.
        let out = v.outcome(&dev.name, LinkSpeed::High);
        match out {
            Outcome::Read {
                device,
                bytes,
                crc32,
                reads,
                stable,
                ..
            } => {
                assert_eq!(device, "TEST@DIP8");
                assert_eq!(bytes, 9);
                assert_eq!(crc32, 0xcbf4_3926);
                assert_eq!(reads, 2);
                assert!(stable);
            }
            _ => panic!("expected Outcome::Read"),
        }
    }

    #[test]
    fn read_verified_flags_unstable_reads() {
        let dev = test_device(8, 8); // one block per pass -> second pass flips
        let s = session_for(&dev);
        let mut prog = FakeProg::new(vec![0xa5; 8]);
        prog.flaky = true;
        let mut rep = Collect::default();
        let v = read_verified(&mut prog, &s, Region::code(&dev), &mut rep).unwrap();
        assert!(!v.stable);
        assert_eq!(rep.notes.len(), 1);
        // The image is the *first* read; hashes must match it.
        assert_eq!(v.image.bytes, vec![0xa5; 8]);
        assert_eq!(v.crc32, crc32fast::hash(&[0xa5; 8]));
    }

    #[test]
    fn write_region_roundtrips_with_readback_verify() {
        let dev = test_device(10, 4);
        let s = session_for(&dev);
        let mut prog = FakeProg::new(vec![0xff; 10]);
        let mut rep = Collect::default();
        let image = Image {
            bytes: (0u8..10).collect(),
        };
        write_region(&mut prog, &s, Region::code(&dev), &image, &mut rep).unwrap();
        assert_eq!(prog.mem, image.bytes);
        // Write pass + verify pass each report a full progress ramp.
        assert_eq!(rep.progress.len(), 8);
    }

    #[test]
    fn write_region_detects_verify_failure_at_address() {
        let dev = test_device(10, 4);
        let s = session_for(&dev);
        let mut prog = FakeProg::new(vec![0xff; 10]);
        prog.corrupt_at = Some(6);
        let mut rep = Collect::default();
        let image = Image {
            bytes: vec![0x00; 10],
        };
        let err = write_region(&mut prog, &s, Region::code(&dev), &image, &mut rep).unwrap_err();
        match err {
            Error::Verify { addr } => assert_eq!(addr, 6),
            other => panic!("expected Verify, got {other:?}"),
        }
    }

    #[test]
    fn write_region_rejects_length_mismatch() {
        let dev = test_device(10, 4);
        let s = session_for(&dev);
        let mut prog = FakeProg::new(vec![0xff; 10]);
        let mut rep = Collect::default();
        let image = Image {
            bytes: vec![0x00; 4],
        };
        let err = write_region(&mut prog, &s, Region::code(&dev), &image, &mut rep).unwrap_err();
        assert_eq!(err.code(), "format");
    }

    #[test]
    fn missing_memory_ops_is_unsupported() {
        /// A programmer with no capabilities at all.
        struct NoCaps(ProgrammerInfo);
        impl Programmer for NoCaps {
            fn info(&self) -> &ProgrammerInfo {
                &self.0
            }
            fn caps(&self) -> Caps {
                Caps::default()
            }
            fn begin(&mut self, dev: &Device) -> Result<Session> {
                Ok(Session {
                    device: dev.clone(),
                    emmc_capacity: 0,
                })
            }
            fn end(&mut self, _session: Session) -> Result<()> {
                Ok(())
            }
            fn identify(&mut self, _s: &Session) -> Result<ChipId> {
                Ok(ChipId { raw: 0, bytes: 0 })
            }
            fn reset(&mut self) -> Result<()> {
                Ok(())
            }
        }
        let dev = test_device(10, 4);
        let s = session_for(&dev);
        let mut prog = NoCaps(ProgrammerInfo {
            model: "NONE".into(),
            firmware: FwVersion(0),
            serial: String::new(),
            mfg_date: String::new(),
            device_code: String::new(),
            link: LinkSpeed::Full,
            voltage: 0.0,
        });
        let mut rep = Collect::default();
        let err = read_region(&mut prog, &s, Region::code(&dev), &mut rep).unwrap_err();
        assert_eq!(err.code(), "unsupported");
    }

    #[test]
    fn short_read_block_is_protocol_error() {
        struct Short(FakeProg);
        impl Programmer for Short {
            fn info(&self) -> &ProgrammerInfo {
                self.0.info()
            }
            fn caps(&self) -> Caps {
                Caps::MEMORY
            }
            fn begin(&mut self, dev: &Device) -> Result<Session> {
                self.0.begin(dev)
            }
            fn end(&mut self, session: Session) -> Result<()> {
                self.0.end(session)
            }
            fn identify(&mut self, s: &Session) -> Result<ChipId> {
                self.0.identify(s)
            }
            fn reset(&mut self) -> Result<()> {
                Ok(())
            }
            fn memory(&mut self) -> Option<&mut dyn crate::caps::MemoryOps> {
                Some(self)
            }
        }
        impl crate::caps::MemoryOps for Short {
            fn read_block(&mut self, _s: &Session, _req: &BlockReq) -> Result<Vec<u8>> {
                Ok(vec![0; 1]) // always short
            }
            fn write_block(&mut self, s: &Session, req: &BlockReq, data: &[u8]) -> Result<()> {
                crate::caps::MemoryOps::write_block(&mut self.0, s, req, data)
            }
            fn erase(&mut self, s: &Session, kind: EraseKind) -> Result<()> {
                crate::caps::MemoryOps::erase(&mut self.0, s, kind)
            }
            fn blank_check(&mut self, s: &Session, region: Region) -> Result<bool> {
                crate::caps::MemoryOps::blank_check(&mut self.0, s, region)
            }
        }
        let dev = test_device(10, 4);
        let s = session_for(&dev);
        let mut prog = Short(FakeProg::new(vec![0; 10]));
        let mut rep = Collect::default();
        let err = read_region(&mut prog, &s, Region::code(&dev), &mut rep).unwrap_err();
        assert_eq!(err.code(), "protocol");
    }
}
