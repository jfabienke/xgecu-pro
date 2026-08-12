//! The XGecu T76 driver — reference implementation of the core traits.
//!
//! The T76 is FPGA-based: [`Programmer::begin`] uploads a per-operation Anlogic
//! bitstream, inits the socket adapter, and configures pin-drivers before any
//! chip op. This stub shows the *shape* — how one driver wires the capability
//! traits to the accessor upcasts — with `todo!()` bodies.

use minipro_core::caps::{EmmcOps, FirmwareUpdate, MemoryOps, PinTest};
use minipro_core::device::{BlockReq, ChipId, Device, EraseKind, Partition, Region};
use minipro_core::error::{FwVersion, Result};
use minipro_core::programmer::{Caps, Programmer, ProgrammerInfo, Session};
use minipro_core::transport::{LinkSpeed, Transport};

/// The T76 command channel + cached identity.
pub struct T76 {
    tx: Box<dyn Transport>,
    info: ProgrammerInfo,
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
        T76 { tx, info }
    }
}

impl Programmer for T76 {
    fn info(&self) -> &ProgrammerInfo { &self.info }

    fn caps(&self) -> Caps {
        Caps::MEMORY.with(Caps::EMMC).with(Caps::PINTEST).with(Caps::FWUPDATE)
    }

    fn begin(&mut self, _dev: &Device) -> Result<Session> {
        // TODO: t76_adapter_init -> pin detection -> write bitstream (BEGIN/BLOCK/END).
        let _ = &mut self.tx;
        todo!("bitstream upload + adapter init")
    }
    fn end(&mut self, _session: Session) -> Result<()> { todo!() }
    fn identify(&mut self, _s: &Session) -> Result<ChipId> { todo!() }
    fn reset(&mut self) -> Result<()> { self.tx.reset() }

    // Capability upcasts — the T76 supports these, so return `Some(self)`.
    // (This is the object-safe pattern the whole design turns on.)
    fn memory(&mut self) -> Option<&mut dyn MemoryOps> { Some(self) }
    fn emmc(&mut self) -> Option<&mut dyn EmmcOps> { Some(self) }
    fn pins(&mut self) -> Option<&mut dyn PinTest> { Some(self) }
    fn firmware(&mut self) -> Option<&mut dyn FirmwareUpdate> { Some(self) }
}

impl MemoryOps for T76 {
    fn read_block(&mut self, _s: &Session, _req: &BlockReq) -> Result<Vec<u8>> { todo!() }
    fn write_block(&mut self, _s: &Session, _req: &BlockReq, _data: &[u8]) -> Result<()> { todo!() }
    fn erase(&mut self, _s: &Session, _kind: EraseKind) -> Result<()> { todo!() }
    fn blank_check(&mut self, _s: &Session, _region: Region) -> Result<bool> { todo!() }
}

impl EmmcOps for T76 {
    fn select_partition(&mut self, _s: &Session, _part: Partition) -> Result<()> { todo!() }
    fn capacity(&self) -> u64 { 0 }
}

impl PinTest for T76 {
    fn contact_check(&mut self, _s: &Session) -> Result<Vec<u8>> { todo!() }
}

impl FirmwareUpdate for T76 {
    fn update(&mut self, _image: &[u8]) -> Result<()> { todo!() }
}
