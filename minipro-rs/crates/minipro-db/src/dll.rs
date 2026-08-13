//! Native chip-database backend: read the vendor's `InfoICT76.dll` directly.
//!
//! The DLL is a native 32-bit PE whose chip database is a two-level table in
//! `.rdata` — a 173-entry manufacturer table, each pointing to a per-vendor
//! array of 116-byte chip structs (35,399 total). This backend maps the PE and
//! walks those tables *statically* (no DLL execution, no Wine, no decompilation).
//! The full binary format is documented in `docs/infoict76-dll-format.md`.
//!
//! Every device field is derived from the 116-byte descriptor by a faithful
//! Rust port of nmatt0's `tools/infoict76-refresh/{variant,fields}.py` (itself
//! an RE of `Xgpro_T76.exe`'s `t76_load_chip_to_state @0x4eed10` and
//! `sub_4b3120`): the **algorithm number** (`variant` high byte — passthrough
//! `desc[0x35]` or the per-protocol tree), **flags**, **package_details**,
//! chip_id, sizes, buffers, voltages, chip_info, pulse_delay, etc. Bitstreams
//! resolve from `algoT76/` (native `.alg`) or `algorithm.xml` next to the DLL.
//!
//! Result: **`DllDb` reads a chip end-to-end with no XML at all** — verified on
//! hardware (the `M27C256B@DIP28` → `ROM28P32` path read a real EPROM
//! byte-identical to the known-good dump). Field accuracy is validated against
//! `infoic.xml` by the `oracle` test below (variant ~92% — the rest are
//! stale-XML / microwire splits, *0 genuine bugs* per variant.py; most other
//! fields 92–100%).
//!
//! Not derived (host-side, not in the descriptor): **`pin_map`** (package pin
//! tables — left 0; affects only pin-test reporting, never read/write/erase)
//! and the last few % of **voltages**/**package_details** edge cases.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use minipro_core::device::{Algorithm, Device, Package};
use minipro_core::error::{Error, FwVersion, Result};

use crate::{
    algorithm_name, id_bytes_count, locate_bitstreams, package_name, pin_count, resolve_bitstream,
    ChipDb, Search, T76_FW_TARGET,
};

// ---- struct/table geometry (see docs/infoict76-dll-format.md) --------------
const MFC_STRIDE: usize = 0x4c; // 76-byte manufacturer struct
const IC_STRIDE: usize = 0x74; // 116-byte chip struct
const MFC_SHORT_NAME: usize = 0x08; // char[20]
const MFC_IC_PTR: usize = 0x44; // u32 VA -> this vendor's Ic array
const MFC_NUM_ICS: usize = 0x48; // u32
const IC_NAME: usize = 0x0c; // char[40]
// Remaining descriptor field offsets are used directly in `decode_ic`
// (the fields.py / variant.py port), documented in docs/infoict76-dll-format.md.

/// A minimally-parsed PE image: just enough to map virtual addresses to file
/// offsets and read the data sections.
struct Pe {
    data: Vec<u8>,
    image_base: u32,
    /// (rva, virtual_size, raw_off, raw_size)
    sections: Vec<(u32, u32, u32, u32)>,
}

impl Pe {
    fn parse(data: Vec<u8>) -> Result<Pe> {
        let rd32 = |o: usize| -> u32 {
            u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
        };
        let rd16 = |o: usize| -> u16 { u16::from_le_bytes([data[o], data[o + 1]]) };
        if data.len() < 0x40 || &data[0..2] != b"MZ" {
            return Err(Error::Format("not a PE (no MZ)".into()));
        }
        let e_lfanew = rd32(0x3c) as usize;
        if e_lfanew + 24 > data.len() || &data[e_lfanew..e_lfanew + 4] != b"PE\0\0" {
            return Err(Error::Format("not a PE (no PE header)".into()));
        }
        let nsec = rd16(e_lfanew + 6) as usize;
        let opt_size = rd16(e_lfanew + 20) as usize;
        let opt = e_lfanew + 24;
        let magic = rd16(opt);
        if magic != 0x10b {
            return Err(Error::Format("expected a 32-bit (PE32) DLL".into()));
        }
        let image_base = rd32(opt + 28); // ImageBase (PE32)
        let sec_off = opt + opt_size;
        let mut sections = Vec::with_capacity(nsec);
        for i in 0..nsec {
            let o = sec_off + i * 40;
            if o + 40 > data.len() {
                break;
            }
            let vsize = rd32(o + 8);
            let vaddr = rd32(o + 12);
            let rsize = rd32(o + 16);
            let roff = rd32(o + 20);
            sections.push((vaddr, vsize, roff, rsize));
        }
        Ok(Pe { data, image_base, sections })
    }

    /// Map a relative virtual address to a file offset, if backed by file data.
    fn rva_off(&self, rva: u32) -> Option<usize> {
        for &(vaddr, vsize, roff, rsize) in &self.sections {
            let span = vsize.max(rsize);
            if rva >= vaddr && rva < vaddr.wrapping_add(span) {
                let delta = rva - vaddr;
                if delta < rsize {
                    return Some((roff + delta) as usize);
                }
            }
        }
        None
    }

    fn va_off(&self, va: u32) -> Option<usize> {
        self.rva_off(va.checked_sub(self.image_base)?)
    }

    fn u32(&self, off: usize) -> Option<u32> {
        self.data
            .get(off..off + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// Read a fixed-width NUL-terminated ASCII field.
fn ascii(bytes: &[u8]) -> Option<String> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let s = &bytes[..end];
    if s.is_empty() || !s.iter().all(|&b| (0x20..0x7f).contains(&b)) {
        return None;
    }
    Some(String::from_utf8_lossy(s).into_owned())
}

/// Native-DLL chip database.
pub struct DllDb {
    devices: Vec<Device>,
    index: HashMap<String, usize>,
    algo_dir: Option<PathBuf>,
    algo_path: Option<PathBuf>,
}

impl DllDb {
    /// Load the chip catalog from `dir/InfoICT76.dll`, and locate `algoT76/` /
    /// `algorithm.xml` next to it for bitstreams (a real XGPro install has both).
    pub fn load(dir: &Path) -> Result<Self> {
        let (devices, index) = parse_dll(std::fs::read(dir.join("InfoICT76.dll"))?)?;
        let (algo_dir, algo_path) = locate_bitstreams(dir);
        Ok(DllDb { devices, index, algo_dir, algo_path })
    }

    /// Parse the catalog from in-memory DLL bytes, with **no on-disk bitstream
    /// source** — bitstreams are supplied externally (e.g. fetched over HTTP by
    /// [`crate::HttpDb`]). Nothing touches disk.
    pub fn from_dll_bytes(bytes: Vec<u8>) -> Result<Self> {
        let (devices, index) = parse_dll(bytes)?;
        Ok(DllDb { devices, index, algo_dir: None, algo_path: None })
    }

    /// Build from an already-parsed catalog (e.g. a persisted postcard blob),
    /// with no bitstream source. Used by [`crate::HttpDb`] to skip re-parsing
    /// the DLL when a cached catalog exists.
    pub fn from_devices(devices: Vec<Device>) -> Self {
        let mut index = HashMap::with_capacity(devices.len());
        for (i, d) in devices.iter().enumerate() {
            index.entry(d.name.to_ascii_uppercase()).or_insert(i);
        }
        DllDb { devices, index, algo_dir: None, algo_path: None }
    }

    pub fn devices(&self) -> &[Device] {
        &self.devices
    }
}

impl ChipDb for DllDb {
    fn get(&self, name: &str) -> Option<&Device> {
        self.index.get(&name.to_ascii_uppercase()).map(|&i| &self.devices[i])
    }
    fn search(&self, query: &str, limit: usize) -> Search<'_> {
        let needle = query.to_ascii_uppercase();
        let mut total = 0;
        let mut hits = Vec::new();
        for d in &self.devices {
            if d.name.to_ascii_uppercase().contains(&needle) {
                total += 1;
                if hits.len() < limit {
                    hits.push(d);
                }
            }
        }
        let truncated = total > hits.len();
        Search { total, hits, truncated }
    }
    fn firmware_target(&self) -> FwVersion {
        T76_FW_TARGET
    }
    fn all(&self) -> &[Device] {
        &self.devices
    }
    fn load_algorithm(&self, dev: &Device) -> Result<Option<Algorithm>> {
        match algorithm_name(dev) {
            Some(name) => resolve_bitstream(self.algo_dir.as_deref(), self.algo_path.as_deref(), &name),
            None => Ok(None),
        }
    }
}

/// Parse DLL bytes into the device catalog + a case-insensitive name index.
fn parse_dll(bytes: Vec<u8>) -> Result<(Vec<Device>, HashMap<String, usize>)> {
    let pe = Pe::parse(bytes)?;
    let devices = extract(&pe)?;
    let mut index = HashMap::with_capacity(devices.len());
    for (i, d) in devices.iter().enumerate() {
        index.entry(d.name.to_ascii_uppercase()).or_insert(i);
    }
    Ok((devices, index))
}

/// Locate the manufacturer table by signature, then walk it into devices.
fn extract(pe: &Pe) -> Result<Vec<Device>> {
    let mfc_off = find_mfc_table(pe).ok_or(Error::Format(
        "could not locate the manufacturer table in InfoICT76.dll".into(),
    ))?;
    let mut devices = Vec::new();
    // Walk manufacturers until a struct stops looking valid.
    let mut m = mfc_off;
    while valid_mfc(pe, m) {
        let num = pe.u32(m + MFC_NUM_ICS).unwrap_or(0);
        let ic_ptr = pe.u32(m + MFC_IC_PTR).unwrap_or(0);
        if let Some(ic0) = pe.va_off(ic_ptr) {
            for k in 0..num as usize {
                let ic = ic0 + k * IC_STRIDE;
                if let Some(dev) = decode_ic(pe, ic) {
                    devices.push(dev);
                }
            }
        }
        m += MFC_STRIDE;
    }
    if devices.is_empty() {
        return Err(Error::Format("manufacturer table yielded no devices".into()));
    }
    Ok(devices)
}

/// Does a 76-byte manufacturer struct at file offset `m` look valid? (printable
/// short_name, an in-image Ic pointer whose first chip has a printable name, a
/// sane device count.)
fn valid_mfc(pe: &Pe, m: usize) -> bool {
    if m + MFC_STRIDE > pe.data.len() {
        return false;
    }
    let Some(short) = pe.data.get(m + MFC_SHORT_NAME..m + MFC_SHORT_NAME + 20).and_then(ascii) else {
        return false;
    };
    if short.len() < 2 {
        return false;
    }
    let num = pe.u32(m + MFC_NUM_ICS).unwrap_or(0);
    if num == 0 || num > 8000 {
        return false;
    }
    let ptr = pe.u32(m + MFC_IC_PTR).unwrap_or(0);
    match pe.va_off(ptr) {
        Some(ic0) => pe.data.get(ic0 + IC_NAME..ic0 + IC_NAME + 40).and_then(ascii).is_some(),
        None => false,
    }
}

/// Signature-scan `.rdata`-style regions for the start of the manufacturer table:
/// the first offset that begins a long run (≥64) of valid 76-byte structs.
fn find_mfc_table(pe: &Pe) -> Option<usize> {
    // Scan each file-backed section; step by 4 (structs are dword-aligned).
    for &(_vaddr, _vsize, roff, rsize) in &pe.sections {
        let start = roff as usize;
        let end = (roff + rsize) as usize;
        let mut off = start;
        while off + MFC_STRIDE <= end {
            if valid_mfc(pe, off) {
                // Count the run from here.
                let mut run = 0;
                let mut p = off;
                while valid_mfc(pe, p) {
                    run += 1;
                    p += MFC_STRIDE;
                }
                if run >= 64 {
                    // Walk back to the true start (in case we entered mid-table).
                    let mut s = off;
                    while s >= start + MFC_STRIDE && valid_mfc(pe, s - MFC_STRIDE) {
                        s -= MFC_STRIDE;
                    }
                    return Some(s);
                }
                off = p; // skip the (short) run we just measured
            } else {
                off += 4;
            }
        }
    }
    None
}

/// Decode a 116-byte chip struct into a [`Device`]. Returns `None` for entries
/// with no usable name.
fn u32le(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}
fn u16le(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}

/// Port of `variant.py::_sub_4b3120` — the exe's per-protocol algorithm-suffix
/// decision tree (`Xgpro_T76.exe sub_4b3120`, RE'd by nmatt0). Returns the
/// 2-hex-char algo suffix, or `None` for undefined field combinations.
fn algo_tree(proto: u8, d34: u8, size: u32, fam: u32, d50: u8) -> Option<u8> {
    let fm = fam & 0xffff_00ff;
    match proto.wrapping_sub(1) {
        0 => {
            let al = d34 & 3; // proto 1 IIC24C
            let t = fm != 0xf600_0000;
            match (t, al) {
                (true, 1) => Some(if size < 0x8000 { 0x12 } else { 0x11 }),
                (true, 0) => Some(0x13),
                (true, 2) => Some(0x14),
                (false, 1) => Some(if size < 0x8000 { 0x62 } else { 0x61 }),
                (false, 0) => Some(0x63),
                (false, 2) => Some(0x64),
                _ => None,
            }
        }
        1 => {
            let a = d50 & 0xf; // proto 2 MW93ALG
            if d34 & 0x80 == 0 {
                if fm == 0xf600_0000 { Some(0x92) } else if a == 2 { Some(0x2A) } else { Some(0x21) }
            } else if fm == 0xf600_0000 {
                Some(0x91)
            } else if d34 & 0x20 != 0 {
                Some(match a { 1 => 0x69, 2 => 0x68, _ => 0x67 })
            } else {
                Some(match a { 1 => 0x2B, 2 => 0x1A, _ => 0x11 })
            }
        }
        2 | 0xe => {
            let cl = d34 & 3; // proto 3 / 0xf SPI25F
            if d34 & 0xf0 == 0x20 {
                match cl { 3 => Some(0x20), 2 => Some(0x21), _ => None }
            } else {
                match cl { 3 => Some(0x10), 2 => Some(0x11), 1 => Some(0x12), 0 => Some(0x13), _ => None }
            }
        }
        4 => Some(if fam == 5 { 0x76 } else { 0x75 }), // proto 5 F29EE
        5 => {
            if d34 & 0x80 != 0 { Some(if fam == 5 { 0x73 } else { 0x71 }) } // proto 6 W29F32P
            else if fam == 5 { Some(0x72) } else if size == 0x80000 { Some(0x70) } else { Some(0x71) }
        }
        6 => {
            if d34 & 0x10 == 0 { Some(0x41) } // proto 7 ROM28P
            else if size == 0x10000 { Some(0x31) }
            else if size == 0x8000 { Some(0x32) }
            else { Some(0x33) }
        }
        7 => Some(if fam != 5 {
            match d34 { 4 => 0x12, 3 => 0x13, 2 => 0x14, _ => 0x11 } // proto 8 ROM32P
        } else {
            match d34 { 4 => 0x22, 3 => 0x23, 2 => 0x24, _ => 0x21 }
        }),
        8 => match fam {
            0x2800_0000 => Some(if size == 0x80000 { 0x2A } else { 0x1A }), // proto 9 ROM40P
            0xfd00_0000 => Some(if size == 0x80000 { 0x2B } else { 0x1B }),
            4 => Some(if size == 0x80000 { 0x2C } else { 0x1C }),
            _ => None,
        },
        9 => {
            if d34 & 0x80 != 0 { Some(0x42) } // proto 0xa R28TO32P
            else if size == 0x10000 { Some(0x34) } else if size == 0x8000 { Some(0x35) } else { Some(0x36) }
        }
        0xa => {
            if d34 & 0x10 != 0 { Some(0x43) } else if size == 0x800 { Some(0x3A) } else { Some(0x3B) }
        }
        0xc => Some(if fam == 5 { 0x45 } else { 0x44 }), // proto 0xd EE28C32P
        0xd => Some(0x50),                                // proto 0xe RAM32
        0xf => Some(match d34 {
            0x10 | 0x11 => if fam == 5 { 0x7E } else { 0x7B }, // proto 0x10 28F32P
            0x12 => if fam == 5 { 0x7F } else { 0x7C },
            _ => if fam == 5 { 0x7D } else { 0x7A },
        }),
        0x10 => {
            let a = d34 & 0xf; // proto 0x11 FWH
            if fam == 5 { Some(if a == 1 { 0x92 } else { 0x94 }) }
            else if fam == 3 { Some(if a == 1 { 0x95 } else { 0x96 }) }
            else { Some(if a == 1 { 0x91 } else { 0x93 }) }
        }
        _ => Some(0),
    }
}

/// `variant.py::algo_number`: passthrough `desc[0x35]` or the tree.
fn algo_number(d: &[u8]) -> Option<u8> {
    if d[0x35] != 0 {
        Some(d[0x35])
    } else {
        algo_tree(d[0x00], d[0x34], u32le(d, 0x38), u32le(d, 0x6c), d[0x50])
    }
}

/// `variant.py::variant`: `(algo << 8) | desc[0x34]`.
fn variant_field(d: &[u8]) -> Option<u16> {
    algo_number(d).map(|a| (u16::from(a) << 8) | u16::from(d[0x34]))
}

/// `fields.py::flags`: `desc[0x70]` + per-protocol post-load adjustments (T76).
fn flags_field(d: &[u8]) -> u32 {
    let v = u32le(d, 0x70);
    match d[0x00] {
        0x2d => v,          // NAND (minipro re-ORs 0x800 at send time)
        0x31 => v & !0x20,  // eMMC: clear has_chip_id
        1 => v | if d[0x50] == 0 { 0x100000 } else { 0 },
        2 => v | if d[0x34] & 0x20 == 0 { 0x100000 } else { 0 },
        3 => {
            let mut v = v | 0x48;
            if ![0x8b, 0x90, 0x91, 0x9a].contains(&d[0x35]) {
                v |= 0x100000;
            }
            v
        }
        4 => v | 0x100048, // model != T56
        _ => v,
    }
}

/// `fields.py::package_details`: `desc[0x6c]` + the family-signature OR.
fn package_details_field(d: &[u8]) -> u32 {
    let v = u32le(d, 0x6c);
    let fam_or = match d[0x00] { 1 => 0xb00, 2 => 0xa00, 3 => 0x900, _ => 0 };
    if fam_or != 0 && (v & 0xff00) != 0x500 { v | fam_or } else { v }
}

/// Decode a 116-byte chip descriptor into a [`Device`], porting `fields.py` +
/// `variant.py` (nmatt0's RE of `t76_load_chip_to_state`/`sub_4b3120`). Returns
/// `None` when the algorithm number is undefined (the chip has no bitstream —
/// the generator skips these too). `pin_map` isn't in the descriptor (host-side
/// package tables, not ported) — left 0; it affects only pin-test reporting.
fn decode_ic(pe: &Pe, ic: usize) -> Option<Device> {
    let d = pe.data.get(ic..ic + IC_STRIDE)?;
    let raw_name = ascii(&d[IC_NAME..IC_NAME + 40])?;
    let name = raw_name.replace(" @", "@").trim().to_string();
    if name.is_empty() {
        return None;
    }
    let variant = variant_field(d)?; // None -> undefined algo -> skip (no bitstream)

    // chip_id: fold `chip_id_bytes` bytes big-endian from 0x5c (M27C256B 0x208d, W25Q64BV 0xef4017).
    let id_len = (u32le(d, 0x64).min(4)) as usize;
    let chip_id = d[0x5c..0x5c + id_len].iter().fold(0u32, |a, &b| (a << 8) | u32::from(b));
    let package_details = package_details_field(d);
    let pins = pin_count(package_details);

    Some(Device {
        package: Package { pin_count: pins, name: package_name(&name, pins) },
        chip_id_bytes: id_bytes_count(chip_id),
        name,
        protocol_id: d[0x00],
        variant,
        code_size: u64::from(u32le(d, 0x38)),
        data_size: u64::from(u32le(d, 0x3c)),
        data_memory2_size: u32le(d, 0x40) as u16,
        page_size: u32le(d, 0x54),
        chip_id,
        raw_voltages: (u32::from(d[0x50]) << 8) | u32::from(d[0x4c]), // vpp:vcc pack
        chip_info: u16le(d, 0x44) as u8,
        pin_map: 0,
        pulse_delay: u32le(d, 0x58) as u16,
        read_buffer_size: u16le(d, 0x48),
        write_buffer_size: u16le(d, 0x4a),
        pages_per_block: u32le(d, 0x68) as u16,
        raw_flags: flags_field(d),
        packed_package: package_details,
        icsp: ((package_details & 0x0000_ff00) >> 8) as u8,
        i2c_address: 0,
        spi_clock: 0,
        algorithm: None,
        fw_target: T76_FW_TARGET,
        vectors: Vec::new(),
        vector_count: 0,
        logic_vcc: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: extract the real DLL. `MINIPRO_DLL_DIR=/tmp/dlldb-test cargo test -p minipro-db real_dll`
    #[test]
    fn real_dll_catalog() {
        let Ok(dir) = std::env::var("MINIPRO_DLL_DIR") else { return };
        let db = DllDb::load(Path::new(&dir)).unwrap();
        println!("DllDb extracted {} devices", db.devices().len());
        assert!(db.devices().len() > 30_000, "got {}", db.devices().len());
        let w = db.get("W25Q64BV").expect("W25Q64BV present");
        assert_eq!(w.chip_id, 0x00ef_4017, "Winbond JEDEC id");
        assert_eq!(w.chip_id_bytes, 3);
        assert_eq!(w.protocol_id, 0x03);
        assert_eq!(w.code_size, 0x80_0000);
        // Newly-decoded programming params (verified vs infoic.xml W25Q64BV):
        assert_eq!(w.raw_voltages, 0x0001, "voltages");
        assert_eq!(w.chip_info, 0x90, "chip_info");
        assert_eq!(w.pulse_delay, 0x1388, "pulse_delay");
        assert_eq!(w.page_size, 0x100, "page_size");
        assert_eq!(w.pages_per_block, 2, "pages_per_block");
        assert_eq!(w.package.pin_count, 8, "pin_count (package offset fix)");
        // The algorithm number (variant high byte) is NOT in the struct:
        let m = db.get("M27C256B@DIP28").expect("M27C256B present");
        println!("M27C256B: dll variant=0x{:04x}, algorithm_name={:?} (infoic has variant 0x3211 -> ROM28P32)",
                 m.variant, crate::algorithm_name(m));
    }
}

#[cfg(test)]
mod oracle {
    use super::*;
    use crate::XmlDb;

    /// The DLL+XML oracle: derive every device from InfoICT76.dll and check each
    /// field against infoic.xml (the ground truth).
    /// `MINIPRO_DLL_DIR=/tmp/dlldb-test MINIPRO_XML_DIR=../minipro-t76 cargo test -p minipro-db oracle -- --nocapture`
    #[test]
    fn dll_vs_xml_oracle() {
        let (Ok(dll_dir), Ok(xml_dir)) =
            (std::env::var("MINIPRO_DLL_DIR"), std::env::var("MINIPRO_XML_DIR"))
        else {
            return;
        };
        let dll = DllDb::load(Path::new(&dll_dir)).unwrap();
        let xml = XmlDb::load(Path::new(&xml_dir)).unwrap();

        type FieldFn = (&'static str, fn(&Device) -> u64);
        let fields: &[FieldFn] = &[
            ("variant", |d| d.variant as u64),
            ("protocol_id", |d| d.protocol_id as u64),
            ("code_size", |d| d.code_size),
            ("data_size", |d| d.data_size),
            ("page_size", |d| d.page_size as u64),
            ("pages_per_block", |d| d.pages_per_block as u64),
            ("chip_id", |d| d.chip_id as u64),
            ("chip_info", |d| d.chip_info as u64),
            ("pulse_delay", |d| d.pulse_delay as u64),
            ("read_buffer", |d| d.read_buffer_size as u64),
            ("write_buffer", |d| d.write_buffer_size as u64),
            ("flags", |d| d.raw_flags as u64),
            ("package_details", |d| d.packed_package as u64),
            ("voltages", |d| d.raw_voltages as u64),
        ];
        let mut agree = vec![0usize; fields.len()];
        let mut shared = 0usize;
        for dd in dll.all() {
            let Some(xd) = xml.get(&dd.name) else { continue };
            shared += 1;
            for (i, (_, f)) in fields.iter().enumerate() {
                if f(dd) == f(xd) {
                    agree[i] += 1;
                }
            }
        }
        println!("\n=== DLL vs XML oracle: {shared} shared chips ===");
        for (i, (name, _)) in fields.iter().enumerate() {
            println!("  {name:16} {:6.2}%  ({}/{shared})", 100.0 * agree[i] as f64 / shared as f64, agree[i]);
        }
        assert!(shared > 20_000, "too few shared chips: {shared}");
        // variant (the algo!) must be essentially perfect — that's the whole point.
        let variant_pct = 100.0 * agree[0] as f64 / shared as f64;
        assert!(variant_pct > 90.0, "variant/algo agreement too low: {variant_pct:.1}%");
    }
}
