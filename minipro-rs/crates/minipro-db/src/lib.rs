// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! The chip database and FPGA-bitstream store.
//!
//! Two backends behind one trait: an editable XML source (`infoic.xml` +
//! `algorithm.xml`, parsed with quick-xml) and a compiled blob baked into the
//! binary (postcard + `include_bytes!`) for fast startup. All content derives
//! from XGecu's XGPro — see `docs/minipro-codebase-report.md` §5.
//!
//! The infoic.xml schema (the fields XGPro's database defines):
//!
//! `xml
//! <infoic>
//!   <database type="INFOICT76">
//!     <manufacturer name="WINBOND">
//!       <!-- `name` is a comma-separated alias list: one <ic> element can be
//!            many devices sharing one parameter set. -->
//!       <ic name="W25Q64BV,W25Q64BV@SOIC8" protocol_id="0x03" code_memory_size="0x800000" ... />
//!     </manufacturer>
//!     <custom name="ATMEL"> <ic .../> </custom>
//!   </database>
//! </infoic>
//! `
//!
//! `algorithm.xml` holds the per-op Anlogic FPGA bitstreams, base64-wrapped
//! gzip, ~50 MB on disk — so bitstreams are located and inflated *lazily* by
//! [`XmlDb::algorithm`], never at [`XmlDb::load`] time:
//!
//! `xml
//! <root><database type="ALGORITHMS">
//!   <algorithms_T76>
//!     <algorithm name="SPI2C" description="W25Q64BV" bitstream="H4sI..."/>
//!   </algorithms_T76>
//! </database></root>
//! `
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::io::{BufRead, Read as _};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use minipro_core::device::{chip_type, Algorithm, Device, Package};
use minipro_core::error::{Error, FwVersion, Result};

mod dll;
pub use dll::DllDb;

#[cfg(feature = "net")]
mod net;
#[cfg(feature = "net")]
pub use net::HttpDb;

/// The `<database type=…>` section of `infoic.xml` this crate targets.
const INFOIC_DB_TYPE: &str = "INFOICT76";
/// The bitstream container in `algorithm.xml` for the T76.
const ALGO_SECTION: &str = "algorithms_T76";
/// The T76 firmware these bitstreams pair with (`T76_FIRMWARE_VERSION` =
/// `0x111` / `"00.1.17"`).
pub(crate) const T76_FW_TARGET: FwVersion = FwVersion(0x0111);

/// Algorithm-name prefixes XGPro uses, indexed by `protocol_id - 1`. Empty
/// entries are unused ids. (These names are facts — what XGPro calls each
/// algorithm — and their order is dictated by the protocol-id enumeration.)
const ALGO_TABLE: &[&str] = &[
    "IIC24C", "MW93ALG", "SPI25F", "AT45D", "F29EE", "W29F32P", // 0x01..0x06
    "ROM28P", "ROM32P", "ROM40P", "R28TO32P", "ROM24P", "ROM44", // 0x07..0x0C
    "EE28C32P", "RAM32", "SPI25F", "28F32P", "FWH", "T48", // 0x0D..0x12
    "T40A", "T40B", "T88V", "PIC32X", "P18F87J", "P16F", // 0x13..0x18
    "P18F2", "P16F5X", "P16CX", "", "ATMGA_", "ATTINY_", // 0x19..0x1E
    "AT89P20_", "", "AT89C_", "P87C_", "SST89_", "W78E_", // 0x1F..0x24
    "", "", "ROM24P", "ROM28P", "RAM32", "GAL16", // 0x25..0x2A
    "GAL20", "GAL22", "NAND_", "PIC32X", "RAM36", "KB90", // 0x2B..0x30
    "EMMC_", "VGA_", "CPLD_", "GEN_", "ITE_", // 0x31..0x35
];

/// The FPGA algorithm name for a device: `algo_table[protocol_id-1]` +
/// `hex(variant >> 8)`, with the EMMC/ATMGA/AT89C special cases. Returns `None`
/// for logic/utility devices (`protocol_id == 0`), which use a separate table.
///
/// An `M27C256B@DIP28` (protocol 0x07 `ROM28P`, `variant>>8 == 0x32`) resolves
/// to `"ROM28P32"`:
/// ```
/// use minipro_core::device::Device;
/// let dev = Device { protocol_id: 0x07, variant: 0x3200, ..Default::default() };
/// assert_eq!(minipro_db::algorithm_name(&dev).as_deref(), Some("ROM28P32"));
/// ```
pub fn algorithm_name(dev: &Device) -> Option<String> {
    let pid = dev.protocol_id;
    let idx = (pid as usize).checked_sub(1)?;
    let prefix = ALGO_TABLE.get(idx).copied().filter(|s| !s.is_empty())?;
    let algo_number = (dev.variant >> 8) as u8;
    let mut name = String::from(prefix);
    match pid {
        0x31 => {
            // EMMC: lowercase number + voltage suffix (default 3.3V).
            name.push_str(&format!("{algo_number:02x}_33"));
        }
        // ATMGA (0x1D) / AT89C (0x21) with ISCP use "11S"/"2S"; the ZIF default
        // is the plain hex number, same as every other protocol.
        _ => name.push_str(&format!("{algo_number:02X}")),
    }
    Some(name)
}

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
    /// All devices, for reverse lookups (e.g. by electronic id). Default empty.
    fn all(&self) -> &[Device] {
        &[]
    }
    /// Resolve and inflate the FPGA bitstream a device needs before a session
    /// (derives the algorithm name via [`algorithm_name`], then loads it).
    /// Default `None` for backends without bitstreams.
    fn load_algorithm(&self, _dev: &Device) -> Result<Option<Algorithm>> {
        Ok(None)
    }

    /// Load an FPGA bitstream by algorithm *name* (e.g. `"TestLgcPull"`,
    /// `"SPI25F11"`) rather than by device. Utility algorithms — the logic-test
    /// and SPI-autodetect bitstreams — are chosen at op time and aren't tied to
    /// any chip's `protocol_id`, so the FPGA drivers fetch them by name from the
    /// same `algoT76/` source as chip bitstreams. Default `None`.
    fn load_algorithm_named(&self, _name: &str) -> Result<Option<Algorithm>> {
        Ok(None)
    }
}

/// XML-backed database (the editable source of truth).
pub struct XmlDb {
    devices: Vec<Device>,
    /// Uppercased name → index into `devices` (lookup is case-insensitive).
    index: HashMap<String, usize>,
    /// `algorithm.xml` next to `infoic.xml`, if present. Scanned lazily.
    algo_path: Option<PathBuf>,
    /// Native XGPro `algoT76/` directory of `.alg` bitstream files, if present.
    /// Preferred over `algorithm.xml` — it's the vendor source, no XML round-trip.
    algo_dir: Option<PathBuf>,
    fw: FwVersion,
}

impl XmlDb {
    /// Parse `infoic.xml` (+ `algorithm.xml` for bitstreams) from a directory.
    ///
    /// Only `infoic.xml` is read here (streaming, `INFOICT76` section);
    /// `algorithm.xml` is merely located — its ~50 MB of bitstreams are
    /// decoded on demand by [`XmlDb::algorithm`].
    pub fn load(dir: &Path) -> Result<Self> {
        let infoic = dir.join("infoic.xml");
        let file = std::fs::File::open(&infoic)?;
        let mut devices = parse_infoic(std::io::BufReader::new(file), INFOIC_DB_TYPE)?;

        // Logic ICs (with their test vectors) live in a separate logicic.xml,
        // appended when present.
        let logicic = dir.join("logicic.xml");
        if logicic.is_file() {
            let f = std::fs::File::open(&logicic)?;
            devices.extend(parse_logicic(std::io::BufReader::new(f))?);
        }

        let mut index = HashMap::with_capacity(devices.len());
        for (i, dev) in devices.iter().enumerate() {
            // First entry wins on (rare) duplicate names.
            index.entry(dev.name.to_ascii_uppercase()).or_insert(i);
        }

        let algo = dir.join("algorithm.xml");
        let algo_dir = dir.join("algoT76");
        Ok(XmlDb {
            devices,
            index,
            algo_path: algo.is_file().then_some(algo),
            algo_dir: algo_dir.is_dir().then_some(algo_dir),
            fw: T76_FW_TARGET,
        })
    }

    /// All devices, in file order.
    pub fn devices(&self) -> &[Device] {
        &self.devices
    }

    /// Locate and inflate one FPGA bitstream from `algorithm.xml` (lazy: the
    /// file is streamed until `name` matches, then base64-decoded and
    /// gunzipped). Returns `Ok(None)` when no `algorithm.xml` was found next to
    /// `infoic.xml` or no entry matches.
    ///
    /// `name` is the *algorithm* name (e.g. `"SPI2C"`), which the protocol
    /// layer derives from the device's protocol id/variant — not the chip name.
    ///
    /// Native `algoT76/*.alg` files are preferred when present (the vendor
    /// source), falling back to `algorithm.xml`.
    pub fn algorithm(&self, name: &str) -> Result<Option<Algorithm>> {
        resolve_bitstream(self.algo_dir.as_deref(), self.algo_path.as_deref(), name)
    }
}

/// Locate the two bitstream sources next to a DB file: `algoT76/` (native .alg)
/// and `algorithm.xml`. Shared by [`XmlDb`] and [`DllDb`].
pub(crate) fn locate_bitstreams(dir: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    let xml = dir.join("algorithm.xml");
    let algo = dir.join("algoT76");
    (algo.is_dir().then_some(algo), xml.is_file().then_some(xml))
}

/// Resolve one FPGA bitstream by algorithm name: native `algoT76/*.alg` first
/// (the vendor source), then `algorithm.xml`. Shared by [`XmlDb`]/[`DllDb`].
pub(crate) fn resolve_bitstream(
    algo_dir: Option<&Path>,
    algo_path: Option<&Path>,
    name: &str,
) -> Result<Option<Algorithm>> {
    if let Some(dir) = algo_dir {
        // XGPro names files "<algo>.alg" (some builds prefix "T7_").
        for cand in [format!("{name}.alg"), format!("T7_{name}.alg")] {
            let path = dir.join(&cand);
            if path.is_file() {
                let bytes = std::fs::read(&path)?;
                return Ok(Some(Algorithm {
                    name: name.to_string(),
                    bitstream: decode_alg(&bytes)?,
                }));
            }
        }
    }
    if let Some(path) = algo_path {
        let file = std::fs::File::open(path)?;
        return find_algorithm(std::io::BufReader::new(file), ALGO_SECTION, name);
    }
    Ok(None)
}

impl ChipDb for XmlDb {
    fn get(&self, name: &str) -> Option<&Device> {
        self.index
            .get(&name.to_ascii_uppercase())
            .map(|&i| &self.devices[i])
    }

    fn search(&self, query: &str, limit: usize) -> Search<'_> {
        let needle = query.to_ascii_uppercase();
        let mut total = 0;
        let mut hits = Vec::new();
        for dev in &self.devices {
            if dev.name.to_ascii_uppercase().contains(&needle) {
                total += 1;
                if hits.len() < limit {
                    hits.push(dev);
                }
            }
        }
        let truncated = total > hits.len();
        Search {
            total,
            hits,
            truncated,
        }
    }

    fn firmware_target(&self) -> FwVersion {
        self.fw
    }

    fn all(&self) -> &[Device] {
        &self.devices
    }

    fn load_algorithm(&self, dev: &Device) -> Result<Option<Algorithm>> {
        match algorithm_name(dev) {
            Some(name) => self.algorithm(&name),
            None => Ok(None),
        }
    }
    fn load_algorithm_named(&self, name: &str) -> Result<Option<Algorithm>> {
        self.algorithm(name)
    }
}

// A compiled/baked-in database was considered and deliberately dropped: the
// direct-to-source path is better. `DllDb` parses the vendor `InfoICT76.dll`
// directly and `HttpDb` provisions + caches it from a mirror with a daily
// version check, so the catalog is always the vendor's real data and updates
// live. A baked blob would be a frozen, rebuild-to-update snapshot that can't
// carry the ~48 MB of bitstreams anyway (only the metadata), so it would solve
// nothing the source-backed backends don't already cover.

// ---------------------------------------------------------------------------
// infoic.xml parsing
// ---------------------------------------------------------------------------

/// Stream-parse one `<database type=…>` section of `infoic.xml` into devices.
fn parse_infoic<R: BufRead>(reader: R, db_type: &str) -> Result<Vec<Device>> {
    let mut xml = Reader::from_reader(reader);
    let mut buf = Vec::new();
    let mut devices = Vec::new();
    let mut in_target_db = false;

    loop {
        match xml.read_event_into(&mut buf).map_err(xml_err)? {
            Event::Eof => break,
            Event::Start(e) => match e.name().as_ref() {
                b"database" => {
                    in_target_db = attr(&e, b"type")?.as_deref() == Some(db_type);
                }
                b"ic" if in_target_db => parse_ic(&e, &mut devices)?,
                _ => {}
            },
            Event::Empty(e) => {
                if in_target_db && e.name().as_ref() == b"ic" {
                    parse_ic(&e, &mut devices)?;
                }
            }
            Event::End(e) if e.name().as_ref() == b"database" => in_target_db = false,
            _ => {}
        }
        buf.clear();
    }
    Ok(devices)
}

/// Expand one `<ic …/>` element into devices — one per comma-separated alias
/// in its `name` attribute.
fn parse_ic(e: &BytesStart<'_>, devices: &mut Vec<Device>) -> Result<()> {
    let mut name = None;
    let mut protocol_id: u8 = 0;
    let mut chip_type: u8 = 0;
    let mut blank_value: u8 = 0xFF; // C default when the attribute is absent
    let mut variant: u16 = 0;
    let mut code_size: u64 = 0;
    let mut data_size: u64 = 0;
    let mut data_memory2_size: u16 = 0;
    let mut page_size: u32 = 0;
    let mut chip_id: u32 = 0;
    let mut raw_voltages: u32 = 0;
    let mut chip_info: u8 = 0;
    let mut pin_map: u8 = 0;
    let mut pulse_delay: u16 = 0;
    let mut read_buffer_size: u16 = 0;
    let mut write_buffer_size: u16 = 0;
    let mut pages_per_block: u32 = 0;
    let mut raw_flags: u32 = 0;
    let mut package_details: u32 = 0;

    for a in e.attributes() {
        let a = a.map_err(|e| Error::Format(format!("infoic.xml: bad attribute: {e}")))?;
        match a.key.as_ref() {
            b"name" => name = Some(attr_str(&a)?),
            b"protocol_id" => protocol_id = parse_num(&attr_str(&a)?)? as u8,
            b"type" => chip_type = parse_num(&attr_str(&a)?)? as u8,
            b"blank_value" => blank_value = parse_num(&attr_str(&a)?)? as u8,
            b"variant" => variant = parse_num(&attr_str(&a)?)? as u16,
            b"code_memory_size" => code_size = parse_num(&attr_str(&a)?)?,
            b"data_memory_size" => data_size = parse_num(&attr_str(&a)?)?,
            b"data_memory2_size" => data_memory2_size = parse_num(&attr_str(&a)?)? as u16,
            b"page_size" => page_size = parse_num(&attr_str(&a)?)? as u32,
            b"chip_id" => chip_id = parse_num(&attr_str(&a)?)? as u32,
            b"voltages" => raw_voltages = parse_num(&attr_str(&a)?)? as u32,
            b"chip_info" => chip_info = parse_num(&attr_str(&a)?)? as u8,
            b"pin_map" => pin_map = parse_num(&attr_str(&a)?)? as u8,
            b"pulse_delay" => pulse_delay = parse_num(&attr_str(&a)?)? as u16,
            b"read_buffer_size" => read_buffer_size = parse_num(&attr_str(&a)?)? as u16,
            b"write_buffer_size" => write_buffer_size = parse_num(&attr_str(&a)?)? as u16,
            b"pages_per_block" => pages_per_block = parse_num(&attr_str(&a)?)? as u32,
            b"flags" => raw_flags = parse_num(&attr_str(&a)?)? as u32,
            b"package_details" => package_details = parse_num(&attr_str(&a)?)? as u32,
            _ => {} // config, vectors (logicic.xml), … — not yet in core Device
        }
    }

    // XGecu SPI-NAND geometry fix-up: those descriptors
    // pack block_count into page_size, real pages-per-block into the low 24
    // bits of pages_per_block (flags in the top byte), and page+spare into
    // write_buffer_size, leaving code_memory_size = 0. Reconstruct the real
    // chip size; the signature is deliberately tight so parallel NAND and
    // everything else is untouched. page_size is left in place — the T76
    // BEGIN prelude needs it verbatim.
    const ALG_NAND: u8 = 0x2d; // NAND algorithm id
    if protocol_id == ALG_NAND && code_size == 0 && pages_per_block & 0xff00_0000 != 0 {
        let ppb = pages_per_block & 0x00ff_ffff;
        code_size = u64::from(page_size) * u64::from(ppb) * u64::from(write_buffer_size);
        pages_per_block = ppb;
        data_memory2_size = 0;
    }

    let names = name.ok_or_else(|| Error::Format("infoic.xml: <ic> without name".into()))?;
    let pin_count = pin_count(package_details);
    for alias in names.split(',').map(str::trim).filter(|a| !a.is_empty()) {
        devices.push(Device {
            package: Package {
                pin_count,
                name: package_name(alias, pin_count),
            },
            chip_id_bytes: id_bytes_count(chip_id),
            name: alias.to_string(),
            protocol_id,
            chip_type,
            blank_value,
            variant,
            code_size,
            data_size,
            data_memory2_size,
            page_size,
            chip_id,
            raw_voltages,
            chip_info,
            pin_map,
            pulse_delay,
            read_buffer_size,
            write_buffer_size,
            pages_per_block: pages_per_block.min(u32::from(u16::MAX)) as u16,
            raw_flags,
            packed_package: package_details,
            // the ICSP byte lives in package_details bits 8..16.
            icsp: ((package_details & 0x0000_ff00) >> 8) as u8,
            i2c_address: 0,  // runtime-adjustable, not in the DB
            spi_clock: 0,    // runtime-adjustable, not in the DB
            algorithm: None, // located lazily via XmlDb::algorithm
            fw_target: T76_FW_TARGET,
            vectors: Vec::new(),
            vector_count: 0,
            logic_vcc: 0,
        });
    }
    Ok(())
}

/// Significant bytes in a chip id.
pub(crate) fn id_bytes_count(chip_id: u32) -> u8 {
    match chip_id {
        0 => 0,
        id => (4 - id.leading_zeros() / 8) as u8,
    }
}

/// Pin count packed into bits 24..30 of `package_details`, with the
/// PLCC-adapter special values.
pub(crate) fn pin_count(package_details: u32) -> u8 {
    match (package_details >> 24) & 0x3f {
        0x38 => 20, // PLCC20 adapter
        0x3d => 44, // PLCC44 adapter
        0x3e => 28, // PLCC28 adapter
        0x3f => 32, // PLCC32 adapter
        n => n as u8,
    }
}

// ---------------------------------------------------------------------------
// logicic.xml — the separate logic-IC database (test vectors)
// ---------------------------------------------------------------------------

/// One logic-IC state code: `0 1 L H C Z X G V` -> 0..8.
/// `None` for any other (non-whitespace) character.
fn logic_state(c: u8) -> Option<u8> {
    Some(match c {
        b'0' => 0,
        b'1' => 1,
        b'L' => 2,
        b'H' => 3,
        b'C' => 4,
        b'Z' => 5,
        b'X' => 6,
        b'G' => 7,
        b'V' => 8,
        _ => return None,
    })
}

/// Map a logic `voltage` attribute to the VCC code; the
/// default is 5 V (`0x00`, `DEFAULT_LOGIC_VOLTAGE`).
fn logic_vcc(v: &str) -> u8 {
    match v.trim() {
        "1.8" => 0x03,
        "2.5" => 0x02,
        "3.3" => 0x01,
        _ => 0x00, // "5" and the default
    }
}

/// A logic IC accumulated across its `<vector>` children.
struct LogicIc {
    names: String,
    pin_count: u8,
    vcc: u8,
    vectors: Vec<u8>,
    vector_count: u16,
}

impl LogicIc {
    /// Append one `<vector>` row: exactly `pin_count` state chars, whitespace
    /// ignored. Stops at `pin_count` and errors if
    /// short.
    fn push_vector(&mut self, text: &[u8]) -> Result<()> {
        let mut n = 0u8;
        for &c in text {
            if c.is_ascii_whitespace() {
                continue;
            }
            let s = logic_state(c).ok_or_else(|| {
                Error::Format(format!("logicic.xml: bad vector char {:?}", c as char))
            })?;
            self.vectors.push(s);
            n += 1;
            if n >= self.pin_count {
                break;
            }
        }
        if n != self.pin_count {
            return Err(Error::Format(format!(
                "logicic.xml: vector has {n} states, expected {}",
                self.pin_count
            )));
        }
        self.vector_count += 1;
        Ok(())
    }

    /// Expand to one [`Device`] per comma-separated alias.
    fn into_devices(self) -> Vec<Device> {
        self.names
            .split(',')
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .map(|name| Device {
                name: name.to_string(),
                chip_type: chip_type::LOGIC,
                blank_value: 0xFF,
                package: Package {
                    pin_count: self.pin_count,
                    name: package_name(name, self.pin_count),
                },
                vectors: self.vectors.clone(),
                vector_count: self.vector_count,
                logic_vcc: self.vcc,
                ..Device::default()
            })
            .collect()
    }
}

fn parse_logic_ic(e: &BytesStart<'_>) -> Result<LogicIc> {
    Ok(LogicIc {
        names: attr(e, b"name")?.unwrap_or_default(),
        pin_count: attr(e, b"pins")?
            .map(|s| parse_num(&s))
            .transpose()?
            .unwrap_or(0) as u8,
        vcc: attr(e, b"voltage")?.map(|s| logic_vcc(&s)).unwrap_or(0),
        vectors: Vec::new(),
        vector_count: 0,
    })
}

/// Parse `logicic.xml` (the standalone logic-IC database) into logic devices
/// carrying their test vectors. Format: a
/// `<database type="LOGIC">` of `<ic type="5" pins=N voltage=V>` elements, each
/// holding `<vector>` children whose text is N pin-state chars.
///
/// NOTE: validated against the documented format, not a real
/// `logicic.xml` (which is absent from this tree). See the tests.
fn parse_logicic<R: BufRead>(reader: R) -> Result<Vec<Device>> {
    let mut xml = Reader::from_reader(reader);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut in_logic_db = false;
    let mut cur: Option<LogicIc> = None;
    let mut in_vector = false;

    loop {
        match xml.read_event_into(&mut buf).map_err(xml_err)? {
            Event::Eof => break,
            Event::Start(e) => match e.name().as_ref() {
                b"database" => {
                    in_logic_db =
                        attr(&e, b"type")?.is_some_and(|t| t.eq_ignore_ascii_case("LOGIC"))
                }
                b"ic" if in_logic_db => cur = Some(parse_logic_ic(&e)?),
                b"vector" if cur.is_some() => in_vector = true,
                _ => {}
            },
            // A logic <ic .../> with no vectors (self-closing) still records.
            Event::Empty(e) if in_logic_db && e.name().as_ref() == b"ic" => {
                out.extend(parse_logic_ic(&e)?.into_devices());
            }
            Event::Text(t) if in_vector => {
                let text = t.into_inner();
                if let Some(ic) = cur.as_mut() {
                    ic.push_vector(&text)?;
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"database" => in_logic_db = false,
                b"vector" => in_vector = false,
                b"ic" => {
                    if let Some(ic) = cur.take() {
                        out.extend(ic.into_devices());
                    }
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

/// Package label: the `@SOIC8`-style suffix of the chip name when present,
/// otherwise the default DIP/ZIF socket for the pin count.
pub(crate) fn package_name(chip_name: &str, pin_count: u8) -> String {
    match chip_name.split_once('@') {
        Some((_, pkg)) if !pkg.is_empty() => pkg.to_string(),
        _ if pin_count > 0 => format!("DIP{pin_count}"),
        _ => String::new(),
    }
}

/// Parse `"0x2c"` / `"1234"` numeric attribute values.
fn parse_num(s: &str) -> Result<u64> {
    let s = s.trim();
    let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => s.parse(),
    };
    parsed.map_err(|_| Error::Format(format!("infoic.xml: bad number {s:?}")))
}

// ---------------------------------------------------------------------------
// algorithm.xml parsing
// ---------------------------------------------------------------------------

/// Stream `algorithm.xml` until `<algorithm name=…>` matches inside `section`
/// (e.g. `algorithms_T76`), then base64-decode + gunzip its bitstream.
fn find_algorithm<R: BufRead>(reader: R, section: &str, name: &str) -> Result<Option<Algorithm>> {
    let mut xml = Reader::from_reader(reader);
    let mut buf = Vec::new();
    let mut in_section = false;

    loop {
        match xml.read_event_into(&mut buf).map_err(xml_err)? {
            Event::Eof => break,
            Event::Start(e) | Event::Empty(e) => {
                let tag = e.name();
                if tag.as_ref() == section.as_bytes() {
                    in_section = true;
                } else if in_section && tag.as_ref() == b"algorithm" {
                    let matches = attr(&e, b"name")?.is_some_and(|n| n.eq_ignore_ascii_case(name));
                    if matches {
                        let b64 = attr(&e, b"bitstream")?.ok_or_else(|| {
                            Error::Format(format!("algorithm {name:?} has no bitstream"))
                        })?;
                        return Ok(Some(Algorithm {
                            name: name.to_string(),
                            bitstream: inflate_bitstream(&b64)?,
                        }));
                    }
                }
            }
            Event::End(e) if e.name().as_ref() == section.as_bytes() => {
                // Section is over; the name is not in the other model's set.
                return Ok(None);
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(None)
}

/// base64 → gzip → raw Anlogic bitstream bytes.
fn inflate_bitstream(b64: &str) -> Result<Vec<u8>> {
    // Tolerate wrapped/padded attribute values.
    let clean: Vec<u8> = b64.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let gz = base64::engine::general_purpose::STANDARD
        .decode(&clean)
        .map_err(|e| Error::Format(format!("bitstream base64: {e}")))?;
    let mut blob = Vec::new();
    flate2::read::GzDecoder::new(gz.as_slice())
        .read_to_end(&mut blob)
        .map_err(|e| Error::Format(format!("bitstream gunzip: {e}")))?;
    level2_decompress(&blob)
}

/// Second-level (RLE-of-zeros) decompression of a T76 bitstream, matching
/// `get_algorithm` in . After gunzip the blob is
/// `[algo_size: u32 LE][crc: u32 LE][data…]` (offsets 0x00/0x04/0x08). The data
/// is a stream of little-endian u16 words: a non-zero word is emitted verbatim;
/// a zero word is a run marker whose following word gives the count of zero u16
/// words to emit. The expanded result is the actual Anlogic bitstream, exactly
/// `algo_size` bytes.
///
/// The embedded CRC is not re-verified here (a
/// wrong bitstream is rejected by the device anyway, and matching minipro's
/// exact CRC seed is unnecessary for correctness).
/// Offset of the RLE bitstream data inside a native `.alg` file
/// (`T76_ALG_OFFSET 4097` = 0x1000, `dump-alg-minipro.bash:49`).
const ALG_DATA_OFFSET: usize = 0x1000;

/// Decode a native XGPro `.alg` bitstream file into the raw Anlogic bitstream.
///
/// `.alg` layout: `[0x00..0x04]` magic, `[0x04..0x08]` decompressed size,
/// `[0x08..0x0C]` crc, `[0x10..0x1000]` ASCII description, `[0x1000..]`
/// RLE-of-zeros data. The 8-byte size/crc header (`[0x04..0x0C]`) followed by
/// the RLE data is exactly the two-level blob [`level2_decompress`] consumes/// the same splice `dump-alg-minipro.bash` gzips into `algorithm.xml` — so we
/// rebuild that blob and reuse the decompressor. No gzip/base64 round-trip.
fn decode_alg(alg: &[u8]) -> Result<Vec<u8>> {
    if alg.len() < ALG_DATA_OFFSET {
        return Err(Error::Format(format!(
            "alg file too short: {} < {ALG_DATA_OFFSET}",
            alg.len()
        )));
    }
    let mut blob = Vec::with_capacity(8 + (alg.len() - ALG_DATA_OFFSET));
    blob.extend_from_slice(&alg[4..12]); // size(4) + crc(4)
    blob.extend_from_slice(&alg[ALG_DATA_OFFSET..]); // RLE data
    level2_decompress(&blob)
}

/// Upper bound accepted for a decompressed bitstream. Real T76 bitstreams are
/// ~1 MiB; the declared size comes from an untrusted file, so anything larger
/// is treated as corrupt rather than allocated (a hostile header could
/// otherwise demand a 4 GiB buffer).
const MAX_BITSTREAM: usize = 16 * 1024 * 1024;

fn level2_decompress(blob: &[u8]) -> Result<Vec<u8>> {
    const DATA_OFF: usize = 0x08;
    if blob.len() < DATA_OFF {
        return Err(Error::Format("bitstream blob too short".into()));
    }
    let algo_size = u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]) as usize;
    if algo_size > MAX_BITSTREAM {
        return Err(Error::Format(format!(
            "bitstream declares {algo_size} bytes (max {MAX_BITSTREAM})"
        )));
    }
    let data = &blob[DATA_OFF..];

    let mut out = Vec::with_capacity(algo_size);
    let mut i = 0;
    // Everything past algo_size is discarded by the final resize, so stop
    // emitting once the buffer is full — this also bounds memory on a hostile
    // stream of maximal zero-run markers.
    while i + 1 < data.len() && out.len() < algo_size {
        let val = u16::from_le_bytes([data[i], data[i + 1]]);
        i += 2;
        if val != 0 {
            out.extend_from_slice(&val.to_le_bytes());
        } else if i + 1 < data.len() {
            let count = u16::from_le_bytes([data[i], data[i + 1]]) as usize;
            i += 2;
            let room = algo_size.saturating_sub(out.len());
            out.resize(out.len() + (count * 2).min(room), 0);
        }
    }
    // The vendor buffer is exactly algo_size (zero-filled): pad or trim to match.
    out.resize(algo_size, 0);
    Ok(out)
}

// ---------------------------------------------------------------------------
// small helpers
// ---------------------------------------------------------------------------

/// Fetch one attribute of an element as an unescaped string.
fn attr(e: &BytesStart<'_>, key: &[u8]) -> Result<Option<String>> {
    for a in e.attributes() {
        let a = a.map_err(|e| Error::Format(format!("bad attribute: {e}")))?;
        if a.key.as_ref() == key {
            return attr_str(&a).map(Some);
        }
    }
    Ok(None)
}

fn attr_str(a: &Attribute<'_>) -> Result<String> {
    a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
        .map(|v| v.into_owned())
        .map_err(|e| Error::Format(format!("xml: {e}")))
}

fn xml_err(e: quick_xml::Error) -> Error {
    Error::Format(format!("xml: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A miniature infoic.xml in the real file's shape (multiline attributes,
    /// multiple <database> sections, a <custom> block, a trailing <maps>).
    const FIXTURE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<infoic>
  <database type="INFOICT76">
    <manufacturer
        name="WINBOND"
      >
      <ic
          name="W25Q64BV,W25Q64BV@SOIC8"
          type="1"
          protocol_id="0x2c"
          variant="0x0700"
          read_buffer_size="0x400"
          write_buffer_size="0x400"
          code_memory_size="0x800000"
          data_memory_size="0x0"
          data_memory2_size="0x0"
          page_size="0x100"
          chip_id="0xef4017"
          voltages="0x0500"
          flags="0x00000082"
          chip_info="0x0000"
          package_details="0x08000000"
          blank_value="0x00"
          config="NULL"
      />
      <ic name="W25X20" type="1" protocol_id="0x2c" code_memory_size="0x800000"
          data_memory_size="0" page_size="256" chip_id="0xef4017"
          package_details="0x08000000" config="NULL"/>
    </manufacturer>
    <custom name="ATMEL">
      <ic name="AT89C51@PLCC44" type="2" protocol_id="0x88"
          code_memory_size="0x1000" data_memory_size="0x0" page_size="0x40"
          chip_id="0x1e51" package_details="0x3d000000" config="NULL"/>
    </custom>
  </database>
  <database type="INFOIC2PLUS">
    <manufacturer name="WINBOND">
      <ic name="OTHERDB_ONLY" type="1" protocol_id="0x01" code_memory_size="0x10"
          data_memory_size="0" page_size="0" chip_id="0" package_details="0"
          config="NULL"/>
    </manufacturer>
  </database>
  <maps>
    <map index="1"><gnd count="1">10</gnd><mask count="1">1</mask></map>
  </maps>
</infoic>
"#;

    fn fixture_db() -> XmlDb {
        let devices = parse_infoic(Cursor::new(FIXTURE), INFOIC_DB_TYPE).unwrap();
        let mut index = HashMap::new();
        for (i, d) in devices.iter().enumerate() {
            index.entry(d.name.to_ascii_uppercase()).or_insert(i);
        }
        XmlDb {
            devices,
            index,
            algo_path: None,
            algo_dir: None,
            fw: T76_FW_TARGET,
        }
    }

    #[test]
    fn parses_target_database_only() {
        let db = fixture_db();
        // W25Q64BV + W25Q64BV@SOIC8 (alias expansion) + W25X20 + AT89C51@PLCC44.
        assert_eq!(
            db.devices().len(),
            4,
            "custom block included, other DBs excluded"
        );
        assert!(db.get("OTHERDB_ONLY").is_none());
    }

    #[test]
    fn parses_ic_fields() {
        let db = fixture_db();
        let d = db.get("W25Q64BV@SOIC8").unwrap();
        assert_eq!(d.protocol_id, 0x2c);
        assert_eq!(d.code_size, 0x80_0000);
        assert_eq!(d.data_size, 0);
        assert_eq!(d.page_size, 0x100);
        assert_eq!(d.chip_id, 0xef4017);
        assert_eq!(d.chip_id_bytes, 3, "0xef4017 has 3 significant bytes");
        assert_eq!(d.package.pin_count, 8);
        assert_eq!(d.chip_type, 1, "type=\"1\" -> MEMORY");
        assert_eq!(
            d.blank_value, 0x00,
            "explicit blank_value attribute honored"
        );
        // An IC with no blank_value attribute defaults to the erased byte.
        assert_eq!(db.get("W25X20").unwrap().blank_value, 0xFF);
        assert_eq!(d.package.name, "SOIC8");
        assert!(d.algorithm.is_none());
        assert_eq!(d.fw_target, T76_FW_TARGET);
        // The full device_t field set the T76 BEGIN_TRANS packers consume.
        assert_eq!(d.variant, 0x0700);
        assert_eq!(d.raw_voltages, 0x0500);
        assert_eq!(d.raw_flags, 0x82);
        assert_eq!(d.read_buffer_size, 0x400);
        assert_eq!(d.write_buffer_size, 0x400);
        assert_eq!(d.data_memory2_size, 0);
        assert_eq!(d.chip_info, 0);
        assert_eq!(d.packed_package, 0x0800_0000);
        assert_eq!(d.icsp, 0, "no ICSP byte packed in package_details");
        assert_eq!(
            (d.i2c_address, d.spi_clock),
            (0, 0),
            "runtime knobs default 0"
        );

        // The other alias of the same <ic> shares the parameter set but keeps
        // its own package label (no @suffix → default socket for pin count).
        let d = db.get("W25Q64BV").unwrap();
        assert_eq!(d.chip_id, 0xef4017);
        assert_eq!(d.package.name, "DIP8", "no @suffix falls back to socket");

        // Decimal attribute values parse too.
        let d = db.get("W25X20").unwrap();
        assert_eq!(d.page_size, 256);
    }

    #[test]
    fn parse_logicic_loads_vectors() {
        let xml = r#"<infoic><database type="LOGIC"><manufacturer name="TI">
          <ic name="7400,SN7400" type="5" pins="14" voltage="5">
            <vector>V 1 1 H 1 1 H X X L X L X G</vector>
            <vector>V00L00LXXHXHXG</vector>
          </ic></manufacturer></database></infoic>"#;
        let devs = parse_logicic(Cursor::new(xml)).unwrap();
        assert_eq!(devs.len(), 2, "both name aliases expand");
        let d = &devs[0];
        assert_eq!(d.name, "7400");
        assert_eq!(d.chip_type, chip_type::LOGIC);
        assert_eq!(d.package.pin_count, 14);
        assert_eq!(d.logic_vcc, 0x00, "5V default code");
        assert_eq!(d.vector_count, 2);
        assert_eq!(d.vectors.len(), 14 * 2);
        // First row: pin0 'V'->8, pin3 'H'->3, pin9 'L'->2 (whitespace ignored).
        assert_eq!((d.vectors[0], d.vectors[3], d.vectors[9]), (8, 3, 2));
    }

    #[test]
    fn parse_logicic_rejects_short_vector() {
        let xml = r#"<infoic><database type="LOGIC">
          <ic name="X" type="5" pins="14" voltage="5"><vector>V1H</vector></ic>
          </database></infoic>"#;
        assert_eq!(
            parse_logicic(Cursor::new(xml)).unwrap_err().code(),
            "format"
        );
    }

    #[test]
    fn spi_nand_geometry_fixup_derives_size_from_packed_geometry() {
        // MX35LF1GE4AB-shaped descriptor: packed geometry, code_memory_size 0,
        // flag bits in the top byte of pages_per_block.
        let xml = r#"<infoic><database type="INFOICT76"><manufacturer name="MACRONIX">
            <ic name="MX35LF1GE4AB" protocol_id="0x2d" code_memory_size="0"
                page_size="0x400" pages_per_block="0x04000040"
                write_buffer_size="0x840" data_memory2_size="0x40"
                package_details="0x10000000" chip_id="0xc212" config="NULL"/>
            <ic name="PARALLEL_NAND" protocol_id="0x2d" code_memory_size="0x8400000"
                page_size="0x800" pages_per_block="64" write_buffer_size="0x840"
                package_details="0x30000000" chip_id="0xc2f1" config="NULL"/>
        </manufacturer></database></infoic>"#;
        let devices = parse_infoic(Cursor::new(xml), INFOIC_DB_TYPE).unwrap();

        let d = &devices[0];
        assert_eq!(d.code_size, 0x400 * 64 * 0x840, "reconstructed = 0x8400000");
        assert_eq!(d.pages_per_block, 64, "flag byte stripped");
        assert_eq!(
            d.page_size, 0x400,
            "block count left in place for the prelude"
        );
        assert_eq!(d.data_memory2_size, 0, "spurious user section cleared");

        // Parallel NAND with a real code size is untouched by the fix-up.
        let d = &devices[1];
        assert_eq!(d.code_size, 0x840_0000);
        assert_eq!(d.pages_per_block, 64);
    }

    #[test]
    fn plcc_adapter_pin_count() {
        let db = fixture_db();
        let d = db.get("AT89C51@PLCC44").unwrap();
        assert_eq!(d.package.pin_count, 44, "0x3d is the PLCC44 adapter code");
        assert_eq!(d.package.name, "PLCC44");
    }

    #[test]
    fn get_is_case_insensitive_exact() {
        let db = fixture_db();
        assert!(db.get("w25q64bv@soic8").is_some());
        assert!(db.get("W25Q64").is_none(), "get is exact, not substring");
    }

    #[test]
    fn search_caps_and_counts() {
        let db = fixture_db();
        let s = db.search("w25q", 1);
        assert_eq!(s.total, 2);
        assert_eq!(s.hits.len(), 1);
        assert!(s.truncated);

        let s = db.search("w25q", 10);
        assert_eq!((s.total, s.hits.len(), s.truncated), (2, 2, false));

        let s = db.search("nomatch", 10);
        assert_eq!((s.total, s.hits.len(), s.truncated), (0, 0, false));
    }

    #[test]
    fn firmware_target_is_t76() {
        assert_eq!(fixture_db().firmware_target().to_string(), "00.1.17");
    }

    #[test]
    fn id_bytes_count_is_highest_nonzero_byte() {
        // index of the highest non-zero byte.
        for (id, n) in [
            (0u32, 0u8),
            (0x1e, 1),
            (0x1e51, 2),
            (0xef4017, 3),
            (0x89014012, 4),
        ] {
            assert_eq!(id_bytes_count(id), n, "chip_id {id:#x}");
        }
    }

    #[test]
    fn parse_num_rejects_garbage() {
        assert!(parse_num("0xzz").is_err());
        assert!(parse_num("NULL").is_err());
        assert_eq!(parse_num(" 0x10 ").unwrap(), 16);
        assert_eq!(parse_num("42").unwrap(), 42);
    }

    /// Pack a payload the way XGPro does for `bitstream=`: wrap it in the
    /// two-level `[size:u32][crc:u32][RLE u16 stream]` container (zero words are
    /// run markers, next word is the zero-word count), then gzip + base64. The
    /// inverse of [`inflate_bitstream`], so fixtures round-trip.
    /// Build the two-level `[size:u32][crc:u32][RLE u16 stream]` blob (the
    /// inverse of [`level2_decompress`]) that both `algorithm.xml` and `.alg`
    /// files carry, so fixtures round-trip.
    fn two_level_blob(raw: &[u8]) -> Vec<u8> {
        let mut padded = raw.to_vec();
        if padded.len() % 2 == 1 {
            padded.push(0);
        }
        let mut data = Vec::new();
        let mut i = 0;
        while i < padded.len() {
            let w = u16::from_le_bytes([padded[i], padded[i + 1]]);
            i += 2;
            if w != 0 {
                data.extend_from_slice(&w.to_le_bytes());
            } else {
                let mut count = 1u16;
                while i < padded.len() && padded[i] == 0 && padded[i + 1] == 0 {
                    count += 1;
                    i += 2;
                }
                data.extend_from_slice(&0u16.to_le_bytes());
                data.extend_from_slice(&count.to_le_bytes());
            }
        }
        let mut blob = Vec::with_capacity(8 + data.len());
        blob.extend_from_slice(&(raw.len() as u32).to_le_bytes()); // decompressed size
        blob.extend_from_slice(&0u32.to_le_bytes()); // crc (not re-verified)
        blob.extend_from_slice(&data);
        blob
    }

    /// `algorithm.xml`-style value: gzip + base64 of the two-level blob.
    fn pack_bitstream(raw: &[u8]) -> String {
        use std::io::Write as _;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&two_level_blob(raw)).unwrap();
        base64::engine::general_purpose::STANDARD.encode(gz.finish().unwrap())
    }

    /// A native `.alg` file: 4-byte magic, then the blob's size/crc header at
    /// 0x04, a description at 0x10, and the RLE data at 0x1000.
    fn make_alg(raw: &[u8], desc: &str) -> Vec<u8> {
        let blob = two_level_blob(raw);
        let mut alg = vec![0u8; ALG_DATA_OFFSET];
        alg[0..4].copy_from_slice(&[0x35, 0x4c, 0x01, 0x00]); // arbitrary magic (device id)
        alg[4..12].copy_from_slice(&blob[..8]); // size + crc
        let d = desc.as_bytes();
        let n = d.len().min(ALG_DATA_OFFSET - 0x10);
        alg[0x10..0x10 + n].copy_from_slice(&d[..n]);
        alg.extend_from_slice(&blob[8..]); // RLE data at 0x1000
        alg
    }

    /// Opt-in: prove the native `.alg` path yields a byte-identical bitstream to
    /// the `algorithm.xml` path on the *real* files.
    /// `MINIPRO_XML_DIR=/path/to/xgpro-db MINIPRO_ALG_DIR=/tmp/alg-native cargo test -p minipro-db alg_matches_xml`
    #[test]
    fn alg_matches_xml_real() {
        let (Ok(xdir), Ok(adir)) = (
            std::env::var("MINIPRO_XML_DIR"),
            std::env::var("MINIPRO_ALG_DIR"),
        ) else {
            return;
        };
        let xdb = XmlDb::load(Path::new(&xdir)).unwrap(); // uses algorithm.xml
        let adb = XmlDb::load(Path::new(&adir)).unwrap(); // uses algoT76/*.alg
        let x = xdb.algorithm("ROM28P32").unwrap().expect("xml ROM28P32");
        let a = adb.algorithm("ROM28P32").unwrap().expect("alg ROM28P32");
        assert_eq!(a.bitstream.len(), x.bitstream.len(), "length");
        assert_eq!(
            a.bitstream, x.bitstream,
            "native .alg vs algorithm.xml bitstream"
        );
    }

    #[test]
    fn decode_alg_roundtrips() {
        for raw in [
            &b"ROM28P32 bitstream body"[..],
            &b"zeros\x00\x00\x00\x00\x00\x00then more"[..],
        ] {
            let alg = make_alg(raw, "24C02 desc");
            assert_eq!(
                decode_alg(&alg).unwrap(),
                raw,
                "native .alg round-trip {raw:?}"
            );
        }
        // Too short is a clean format error, not a panic.
        assert_eq!(decode_alg(&[0u8; 16]).unwrap_err().code(), "format");
    }

    #[test]
    fn alg_dir_is_preferred_over_algorithm_xml() {
        let dir = std::env::temp_dir().join(format!("minipro-alg-test-{}", std::process::id()));
        let algo = dir.join("algoT76");
        std::fs::create_dir_all(&algo).unwrap();
        std::fs::write(dir.join("infoic.xml"), FIXTURE).unwrap();
        // algorithm.xml says "from xml"; the .alg says "from alg" — .alg must win.
        std::fs::write(dir.join("algorithm.xml"), algo_fixture(b"from xml")).unwrap();
        std::fs::write(algo.join("SPI2C.alg"), make_alg(b"from alg", "W25Q64BV")).unwrap();

        let db = XmlDb::load(&dir).unwrap();
        assert_eq!(
            db.algorithm("SPI2C").unwrap().unwrap().bitstream,
            b"from alg"
        );
        // A name only present in algorithm.xml still falls back.
        assert_eq!(
            db.algorithm("EE24C").unwrap().unwrap().bitstream,
            b"wrong section or entry"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn level2_roundtrips_including_zero_runs() {
        for raw in [
            &b"anlogic bytes"[..],
            &b"\x00\x00\x00\x00zeros then data"[..],
            &b"mid\x00\x00run\x01\x02"[..],
        ] {
            let b64 = pack_bitstream(raw);
            assert_eq!(inflate_bitstream(&b64).unwrap(), raw, "round-trip {raw:?}");
        }
    }

    fn algo_fixture(payload: &[u8]) -> String {
        format!(
            "<root>\n<database type=\"ALGORITHMS\">\n<algorithms_T56>\n\
             <algorithm name=\"SPI2C\" description=\"t56 copy\" bitstream=\"{junk}\"/>\n\
             </algorithms_T56>\n<algorithms_T76>\n\
             <algorithm name=\"EE24C\"\ndescription=\"24C02\"\nbitstream=\"{junk}\"/>\n\
             <algorithm name=\"SPI2C\" description=\"W25Q64BV\" bitstream=\"{b64}\"/>\n\
             </algorithms_T76>\n</database>\n</root>\n",
            junk = pack_bitstream(b"wrong section or entry"),
            b64 = pack_bitstream(payload),
        )
    }

    #[test]
    fn finds_and_inflates_algorithm_lazily() {
        let payload = b"anlogic bitstream bytes \x00\x01\x02";
        let xml = algo_fixture(payload);
        let algo = find_algorithm(Cursor::new(&xml), ALGO_SECTION, "SPI2C")
            .unwrap()
            .expect("SPI2C present in algorithms_T76");
        assert_eq!(algo.name, "SPI2C");
        assert_eq!(
            algo.bitstream, payload,
            "picked the T76 entry, not the T56 one"
        );
    }

    #[test]
    fn missing_algorithm_is_none() {
        let xml = algo_fixture(b"x");
        assert!(find_algorithm(Cursor::new(&xml), ALGO_SECTION, "NOPE")
            .unwrap()
            .is_none());
    }

    #[test]
    fn corrupt_bitstream_is_format_error() {
        let xml = "<root><database type=\"ALGORITHMS\"><algorithms_T76>\
                   <algorithm name=\"BAD\" bitstream=\"!!!notbase64!!!\"/>\
                   </algorithms_T76></database></root>";
        // `Algorithm` has no `Debug` in core, so match instead of unwrap_err.
        match find_algorithm(Cursor::new(xml), ALGO_SECTION, "BAD") {
            Err(e) => assert_eq!(e.code(), "format"),
            Ok(_) => panic!("bad base64 must be a format error"),
        }

        // Valid base64 but not gzip.
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"plainly not gzip");
        let xml = format!(
            "<root><database type=\"ALGORITHMS\"><algorithms_T76>\
             <algorithm name=\"BAD\" bitstream=\"{b64}\"/>\
             </algorithms_T76></database></root>"
        );
        match find_algorithm(Cursor::new(&*xml), ALGO_SECTION, "BAD") {
            Err(e) => assert_eq!(e.code(), "format"),
            Ok(_) => panic!("non-gzip payload must be a format error"),
        }
    }

    #[test]
    fn load_from_directory_end_to_end() {
        let dir = std::env::temp_dir().join(format!("minipro-db-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("infoic.xml"), FIXTURE).unwrap();
        std::fs::write(dir.join("algorithm.xml"), algo_fixture(b"real payload")).unwrap();

        let db = XmlDb::load(&dir).unwrap();
        assert!(db.get("W25Q64BV@SOIC8").is_some());
        let algo = db.algorithm("SPI2C").unwrap().unwrap();
        assert_eq!(algo.bitstream, b"real payload");
        assert!(db.algorithm("NOPE").unwrap().is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Opt-in smoke test against the real vendored XML (19 MB + 50 MB):
    /// `MINIPRO_XML_DIR=/path/to/xgpro-db cargo test -p minipro-db real_vendor_db`.
    #[test]
    fn real_vendor_db_smoke() {
        let Ok(dir) = std::env::var("MINIPRO_XML_DIR") else {
            return; // not requested — the unit fixtures cover the schema
        };
        let db = XmlDb::load(Path::new(&dir)).unwrap();
        // infoic.xml's own header comment: "T76: XGPro_T76 V12.91, 32519 devices".
        assert!(db.devices().len() > 30_000, "got {}", db.devices().len());
        let d = db.get("W25Q64BV@SOIC8").expect("known chip present");
        assert_eq!(d.chip_id, 0x00ef4017, "W25Q64BV JEDEC id");
        assert_eq!(d.chip_id_bytes, 3);
        assert_eq!(d.package.pin_count, 8);
        assert_eq!(d.package.name, "SOIC8");
        let s = db.search("W25Q64", 5);
        assert!(s.truncated && s.total > 5);
        // A real bitstream (W25Q64BV's SPI25F + variant 0x11), inflated lazily.
        let a = db
            .algorithm("SPI25F11")
            .unwrap()
            .expect("SPI25F11 in algorithms_T76");
        assert!(
            a.bitstream.len() > 10_000,
            "got {} bytes",
            a.bitstream.len()
        );
    }

    #[test]
    fn load_without_algorithm_xml() {
        let dir =
            std::env::temp_dir().join(format!("minipro-db-test-noalgo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("infoic.xml"), FIXTURE).unwrap();

        let db = XmlDb::load(&dir).unwrap();
        assert!(
            db.algorithm("SPI2C").unwrap().is_none(),
            "no algorithm.xml → None"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

/// Property tests: the bitstream decompressors consume untrusted vendor-file
/// bytes — they must never panic, never allocate beyond [`MAX_BITSTREAM`], and
/// must invert a well-formed RLE stream exactly.
#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    /// Build a well-formed level-2 blob from RLE tokens, returning it with the
    /// expected expansion. `(true, n)` is a zero-run of `n` u16 words;
    /// `(false, w)` a verbatim non-zero word (0 is bumped to 1).
    fn build_rle(tokens: &[(bool, u16)]) -> (Vec<u8>, Vec<u8>) {
        let mut data = Vec::new();
        let mut expected = Vec::new();
        for &(is_run, v) in tokens {
            if is_run {
                data.extend_from_slice(&0u16.to_le_bytes());
                data.extend_from_slice(&v.to_le_bytes());
                expected.resize(expected.len() + usize::from(v) * 2, 0);
            } else {
                let w = v.max(1);
                data.extend_from_slice(&w.to_le_bytes());
                expected.extend_from_slice(&w.to_le_bytes());
            }
        }
        let mut blob = Vec::with_capacity(8 + data.len());
        blob.extend_from_slice(&(expected.len() as u32).to_le_bytes());
        blob.extend_from_slice(&0u32.to_le_bytes()); // crc: not re-verified
        blob.extend_from_slice(&data);
        (blob, expected)
    }

    proptest! {
        /// Arbitrary bytes: no panic, and any accepted output is exactly the
        /// declared size, which is itself bounded.
        #[test]
        fn level2_never_panics_and_is_bounded(blob in proptest::collection::vec(any::<u8>(), 0..2048)) {
            if let Ok(out) = level2_decompress(&blob) {
                prop_assert!(out.len() <= MAX_BITSTREAM);
                let declared = u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]) as usize;
                prop_assert_eq!(out.len(), declared);
            }
        }

        /// A well-formed RLE stream decompresses to exactly its expansion.
        #[test]
        fn level2_inverts_well_formed_rle(tokens in proptest::collection::vec((any::<bool>(), 0u16..64), 0..32)) {
            let (blob, expected) = build_rle(&tokens);
            let out = level2_decompress(&blob).expect("well-formed blob");
            prop_assert_eq!(out, expected);
        }

        /// A hostile declared size is rejected, not allocated.
        #[test]
        fn level2_rejects_oversize_declaration(size in (MAX_BITSTREAM as u32 + 1)..=u32::MAX) {
            let mut blob = vec![0u8; 16];
            blob[0..4].copy_from_slice(&size.to_le_bytes());
            prop_assert!(level2_decompress(&blob).is_err());
        }

        /// Arbitrary `.alg` file bytes never panic the decoder.
        #[test]
        fn decode_alg_never_panics(alg in proptest::collection::vec(any::<u8>(), 0..0x1200)) {
            let _ = decode_alg(&alg);
        }
    }
}
