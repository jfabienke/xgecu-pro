//! The chip database and FPGA-bitstream store.
//!
//! Two backends behind one trait: an editable XML source (`infoic.xml` +
//! `algorithm.xml`, quick-xml + serde) and a compiled blob baked into the
//! binary (postcard + `include_bytes!`) for fast startup. All content derives
//! from XGecu's XGPro — see `docs/minipro-codebase-report.md` §5.
#![forbid(unsafe_code)]

use minipro_core::device::Device;
use minipro_core::error::{FwVersion, Result};

/// A bounded search result: capped hits plus the true total, so JSON mode never
/// dumps all 34k devices.
pub struct Search<'a> {
    pub total: usize,
    pub hits: Vec<&'a Device>,
    pub truncated: bool,
}

/// Chip lookup + the firmware version the bitstreams pair with.
pub trait ChipDb {
    /// Exact lookup, e.g. `"W25Q64BV@SOIC8"`.
    fn get(&self, name: &str) -> Option<&Device>;
    /// Capped, counted substring search.
    fn search(&self, query: &str, limit: usize) -> Search<'_>;
    /// The device firmware these bitstreams target (drives the mismatch check).
    fn firmware_target(&self) -> FwVersion;
}

/// XML-backed database (the editable source of truth).
pub struct XmlDb {
    // devices: Vec<Device>, index: HashMap<String, usize>, fw: FwVersion
}

impl XmlDb {
    /// Parse `infoic.xml` (+ `algorithm.xml` for bitstreams) from a directory.
    pub fn load(_dir: &std::path::Path) -> Result<Self> {
        todo!("quick-xml parse into Vec<Device> + name index")
    }
}

impl ChipDb for XmlDb {
    fn get(&self, _name: &str) -> Option<&Device> { todo!() }
    fn search(&self, _query: &str, _limit: usize) -> Search<'_> { todo!() }
    fn firmware_target(&self) -> FwVersion { todo!() }
}

/// Compiled database baked into the binary via `include_bytes!` (postcard).
pub struct CompiledDb;

impl CompiledDb {
    /// Deserialize the embedded blob (zero XML parsing at startup).
    pub fn embedded() -> Result<Self> {
        todo!("postcard::from_bytes(include_bytes!(...))")
    }
}
