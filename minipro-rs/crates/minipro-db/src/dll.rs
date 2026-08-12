//! Native chip-database backend: read the vendor's `InfoICT76.dll` directly.
//!
//! The DLL is a native 32-bit PE whose chip database is a two-level table in
//! `.rdata` — a 173-entry manufacturer table, each pointing to a per-vendor
//! array of 116-byte chip structs (35,399 total). This backend maps the PE and
//! walks those tables *statically* (no DLL execution, no Wine, no decompilation).
//! The full binary format is documented in `docs/infoict76-dll-format.md`.
//!
//! Decoded fields (verified against `infoic.xml`): name, protocol_id, variant
//! (low byte), code/data sizes, read/write buffers, chip_id, chip_id_bytes,
//! package, and the ported `opts` programming params — **voltages** (`vpp:vcc`
//! at 0x50:0x4c), **chip_info** (0x44), **pulse_delay** (0x58), **page_size**
//! (0x54), **pages_per_block** (0x68). Bitstreams resolve from `algoT76/` or
//! `algorithm.xml` next to the DLL.
//!
//! **Reads are still blocked** — but by a *different* thing than the opts
//! transforms. Two parameters are not present in the `Ic` struct at all:
//! - The **algorithm number** (the high byte of `infoic.xml`'s `variant`, e.g.
//!   `0x32` making `M27C256B` → `ROM28P32`). The DLL stores only the low byte
//!   (`0x11`), so [`crate::algorithm_name`] derives `ROM28P00` and the wrong
//!   bitstream loads. XGPro assigns algorithms externally (not in this table).
//! - **`flags`** (0x70 holds a value that needs an unknown transform) and
//!   **`pin_map`** (derived from the package layout, not stored).
//!
//! So the catalog and most programming params are recoverable statically; a
//! working read additionally needs the device→algorithm assignment, which is a
//! separate reverse-engineering task, not an `opts` decode.

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
const IC_VARIANT: usize = 0x34; // low byte of the infoic variant (algo number is NOT here)
const IC_CODE_SIZE: usize = 0x38;
const IC_DATA_SIZE: usize = 0x3c;
const IC_DATA2_SIZE: usize = 0x40;
const IC_CHIP_INFO: usize = 0x44; // -> Device.chip_info (verified: M27C256B=6, W25Q64BV=0x90)
const IC_RBUF: usize = 0x48;
const IC_WBUF: usize = 0x4a;
const IC_VOLT_VCC: usize = 0x4c; // voltages low byte
const IC_VOLT_VPP: usize = 0x50; // voltages high byte
const IC_PAGE_SIZE: usize = 0x54; // -> Device.page_size (W25Q64BV=0x100)
const IC_PULSE_DELAY: usize = 0x58; // -> Device.pulse_delay (M27C256B=0x64, W25Q64BV=0x1388)
const IC_CHIP_ID: usize = 0x5c; // u8[8]
const IC_CHIP_ID_LEN: usize = 0x64; // u32
const IC_PAGES_PER_BLOCK: usize = 0x68; // -> Device.pages_per_block (W25Q64BV=2)
const IC_PACKAGE: usize = 0x6c; // u32 (0x70 is `flags`, which needs a transform)

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
    fn u16(&self, off: usize) -> Option<u16> {
        self.data.get(off..off + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
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
        let dll = dir.join("InfoICT76.dll");
        let pe = Pe::parse(std::fs::read(&dll)?)?;
        let devices = extract(&pe)?;
        let mut index = HashMap::with_capacity(devices.len());
        for (i, d) in devices.iter().enumerate() {
            index.entry(d.name.to_ascii_uppercase()).or_insert(i);
        }
        let (algo_dir, algo_path) = locate_bitstreams(dir);
        Ok(DllDb { devices, index, algo_dir, algo_path })
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
fn decode_ic(pe: &Pe, ic: usize) -> Option<Device> {
    let raw_name = pe.data.get(ic + IC_NAME..ic + IC_NAME + 40).and_then(ascii)?;
    // Vendor names occasionally embed the package as "NAME @PKG"; normalize to
    // the "NAME@PKG" convention infoic.xml uses.
    let name = raw_name.replace(" @", "@").trim().to_string();
    if name.is_empty() {
        return None;
    }
    let protocol_id = pe.u32(ic)? as u8;
    let variant = pe.u32(ic + IC_VARIANT)? as u16;
    let code_size = u64::from(pe.u32(ic + IC_CODE_SIZE)?);
    let data_size = u64::from(pe.u32(ic + IC_DATA_SIZE)?);
    let data_memory2_size = pe.u32(ic + IC_DATA2_SIZE)? as u16;
    let read_buffer_size = pe.u16(ic + IC_RBUF)?;
    let write_buffer_size = pe.u16(ic + IC_WBUF)?;
    let page_size = u32::from(pe.u16(ic + IC_PAGE_SIZE)?);
    let pulse_delay = pe.u16(ic + IC_PULSE_DELAY)?;
    let pages_per_block = pe.u16(ic + IC_PAGES_PER_BLOCK)?;
    let chip_info = pe.data.get(ic + IC_CHIP_INFO).copied()?;
    // Voltages pack as vpp:vcc (verified: M27C256B 0x50:0x70=0x5070, W25Q64BV 0x00:0x01=0x0001).
    let vcc = pe.data.get(ic + IC_VOLT_VCC).copied()?;
    let vpp = pe.data.get(ic + IC_VOLT_VPP).copied()?;
    let raw_voltages = (u32::from(vpp) << 8) | u32::from(vcc);
    let id_len = pe.u32(ic + IC_CHIP_ID_LEN)?.min(4) as usize;
    let id_bytes = pe.data.get(ic + IC_CHIP_ID..ic + IC_CHIP_ID + id_len)?;
    // Big-endian assembly: [ef,40,17] -> 0x00ef4017 (matches infoic.xml).
    let chip_id = id_bytes.iter().fold(0u32, |acc, &b| (acc << 8) | u32::from(b));
    let package_details = pe.u32(ic + IC_PACKAGE)?;
    let pins = pin_count(package_details);

    Some(Device {
        package: Package { pin_count: pins, name: package_name(&name, pins) },
        chip_id_bytes: id_bytes_count(chip_id),
        name,
        protocol_id,
        variant,
        code_size,
        data_size,
        data_memory2_size,
        page_size,
        chip_id,
        raw_voltages,
        chip_info,
        // pin_map isn't stored in the Ic struct (it's derived from the package
        // layout by XGPro tooling); raw_flags@0x70 needs an unknown transform.
        pin_map: 0,
        pulse_delay,
        read_buffer_size,
        write_buffer_size,
        pages_per_block,
        raw_flags: 0,
        packed_package: package_details,
        icsp: ((package_details & 0x0000_ff00) >> 8) as u8,
        i2c_address: 0,
        spi_clock: 0,
        algorithm: None,
        fw_target: T76_FW_TARGET,
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
