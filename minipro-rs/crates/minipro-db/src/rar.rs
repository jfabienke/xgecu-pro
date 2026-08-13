// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! `RarDb` — the chip database read **straight out of the vendor archive**, with
//! nothing unpacked to disk.
//!
//! Opt-in (`rar` feature; see the licensing note below).
//!
//! XGecu ships the T76 database as `xgpro_T76_V*.rar` → a WinRAR **SFX
//! executable** → `InfoICT76.dll` + `algoT76/*.alg`. Normally you extract that
//! twice with `unar` and point `--db` at the result. This backend skips the
//! extraction entirely: point it at the `.rar` or the `.exe` and it pulls what
//! it needs, when it needs it.
//!
//! **Nothing is persisted.** `InfoICT76.dll` is decompressed into memory,
//! parsed, and dropped; each `.alg` bitstream is decompressed into memory on
//! demand. There is no cache directory, no catalog blob, and no staleness
//! logic — the archive on disk *is* the source of truth, exactly as
//! [`DllDb::load`] treats an extracted directory.
//!
//! That is affordable because these archives are **not solid**: extracting the
//! DLL costs ~0.3 s and a single bitstream ~10 ms, so a per-invocation read is
//! cheaper than the machinery a cache would need.
//!
//! # Two archive levels
//!
//! - Given the **SFX `.exe`**, entries are read directly.
//! - Given the outer **`.rar`**, the inner `.exe` is extracted to a temporary
//!   file first — the unrar API can only open archives by path, not from a
//!   buffer — and that temp file is deleted when the [`RarDb`] is dropped.
//!   Pointing at the `.exe` avoids the ~63 MB temp write per run.
//!
//! # Licensing
//!
//! This module links **unrar**, which is *not* MIT: its license permits
//! decompression but forbids using the source to build a RAR-compatible
//! compressor. It is feature-gated and off by default so the ordinary build
//! stays pure-Rust and MIT-clean. Enabling `rar` puts unrar's terms on your
//! binary.

use std::path::{Path, PathBuf};

use minipro_core::device::{Algorithm, Device};
use minipro_core::error::{Error, FwVersion, Result};

use crate::{algorithm_name, decode_alg, ChipDb, DllDb, Search};

/// The chip-parameter database inside the vendor archive.
const DLL_ENTRY: &str = "InfoICT76.dll";
/// Where the per-chip FPGA bitstreams live inside the vendor archive.
const ALGO_DIR: &str = "algoT76";

/// A [`DllDb`] sourced from a vendor `.rar` / SFX `.exe`, decompressed on demand
/// and never written to disk.
pub struct RarDb {
    inner: DllDb,
    /// The archive level that actually holds the payload (the SFX).
    archive: PathBuf,
    /// Holds the temporary SFX alive when the entry point was the outer `.rar`;
    /// dropping it removes the file.
    _sfx_temp: Option<tempfile::TempPath>,
}

impl RarDb {
    /// Open a vendor archive — either `xgpro_T76_V*.rar` or the
    /// `Xgpro_T76_V*.exe` it contains.
    pub fn open(archive: &Path) -> Result<Self> {
        let (payload, sfx_temp) = resolve_payload(archive)?;
        // Into RAM, parsed, dropped — the proprietary DLL never touches disk.
        let dll = read_entry(&payload, |name| name == DLL_ENTRY)?.ok_or_else(|| {
            Error::Format(format!(
                "{} holds no {DLL_ENTRY} — is this an XGPro T76 archive?",
                archive.display()
            ))
        })?;
        let inner = DllDb::from_dll_bytes(dll)?;
        Ok(RarDb {
            inner,
            archive: payload,
            _sfx_temp: sfx_temp,
        })
    }
}

impl ChipDb for RarDb {
    fn get(&self, name: &str) -> Option<&Device> {
        self.inner.get(name)
    }
    fn search(&self, query: &str, limit: usize) -> Search<'_> {
        self.inner.search(query, limit)
    }
    fn firmware_target(&self) -> FwVersion {
        self.inner.firmware_target()
    }
    fn all(&self) -> &[Device] {
        self.inner.all()
    }
    fn load_algorithm(&self, dev: &Device) -> Result<Option<Algorithm>> {
        match algorithm_name(dev) {
            Some(name) => self.load_algorithm_named(&name),
            None => Ok(None),
        }
    }

    /// Decompress one bitstream out of the archive into memory. Chip and utility
    /// algorithms share `algoT76/`; vendor files carry a `T7_` prefix, so both
    /// spellings are accepted.
    fn load_algorithm_named(&self, name: &str) -> Result<Option<Algorithm>> {
        let plain = format!("{ALGO_DIR}/{name}.alg");
        let prefixed = format!("{ALGO_DIR}/T7_{name}.alg");
        let found = read_entry(&self.archive, |entry| {
            // Vendor archives use '\' separators on some builds.
            let entry = entry.replace('\\', "/");
            entry.eq_ignore_ascii_case(&plain) || entry.eq_ignore_ascii_case(&prefixed)
        })?;
        match found {
            Some(bytes) => Ok(Some(Algorithm {
                name: name.to_string(),
                bitstream: decode_alg(&bytes)?,
            })),
            None => Ok(None),
        }
    }
}

// ---- unrar plumbing --------------------------------------------------------

fn rar_err(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Format(format!("vendor archive: {what}: {e}"))
}

/// Read the first entry matching `want` fully into memory. `Ok(None)` means the
/// archive simply has no such entry.
fn read_entry(archive: &Path, want: impl Fn(&str) -> bool) -> Result<Option<Vec<u8>>> {
    let mut cursor = unrar::Archive::new(archive)
        .open_for_processing()
        .map_err(|e| rar_err(&format!("open {}", archive.display()), e))?;
    while let Some(header) = cursor
        .read_header()
        .map_err(|e| rar_err("read header", e))?
    {
        let name = header.entry().filename.to_string_lossy().into_owned();
        if want(&name) {
            let (bytes, _rest) = header.read().map_err(|e| rar_err("extract", e))?;
            return Ok(Some(bytes));
        }
        cursor = header.skip().map_err(|e| rar_err("skip entry", e))?;
    }
    Ok(None)
}

/// List entry names (used to decide which archive level we were handed).
fn entry_names(archive: &Path) -> Result<Vec<String>> {
    let list = unrar::Archive::new(archive)
        .open_for_listing()
        .map_err(|e| rar_err(&format!("open {}", archive.display()), e))?;
    let mut names = Vec::new();
    for entry in list {
        let entry = entry.map_err(|e| rar_err("list", e))?;
        names.push(entry.filename.to_string_lossy().into_owned());
    }
    Ok(names)
}

/// Resolve `archive` to the level holding `InfoICT76.dll`.
///
/// The SFX `.exe` is used as-is. The outer `.rar` contains only that `.exe`, and
/// unrar cannot open an archive from memory, so the inner executable is spilled
/// to a temp file whose handle is returned and kept alive by the caller.
fn resolve_payload(archive: &Path) -> Result<(PathBuf, Option<tempfile::TempPath>)> {
    let names = entry_names(archive)?;
    if names.iter().any(|n| n.ends_with(DLL_ENTRY)) {
        return Ok((archive.to_path_buf(), None));
    }
    let inner = names
        .iter()
        .find(|n| n.to_ascii_lowercase().ends_with(".exe"))
        .ok_or_else(|| {
            Error::Format(format!(
                "{} holds neither {DLL_ENTRY} nor an installer .exe",
                archive.display()
            ))
        })?
        .clone();

    let bytes = read_entry(archive, |n| n == inner)?
        .ok_or_else(|| Error::Format(format!("vendor archive: {inner} vanished mid-read")))?;
    let mut temp = tempfile::Builder::new()
        .prefix("minipro-sfx-")
        .suffix(".exe")
        .tempfile()?;
    std::io::Write::write_all(&mut temp, &bytes)?;
    let path = temp.into_temp_path();
    Ok((path.to_path_buf(), Some(path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `RarDb` holds no `Debug`, so assert on the error arm directly.
    fn expect_err(result: Result<RarDb>) -> Error {
        match result {
            Err(e) => e,
            Ok(_) => panic!("expected an error"),
        }
    }

    /// Not an archive at all — must fail cleanly rather than panic.
    #[test]
    fn open_rejects_non_archive() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"not a rar").unwrap();
        assert_eq!(expect_err(RarDb::open(f.path())).code(), "format");
    }

    #[test]
    fn open_reports_missing_file() {
        let missing = expect_err(RarDb::open(Path::new("/nonexistent/xgpro.rar")));
        assert_eq!(missing.code(), "format");
    }

    /// End-to-end against a real vendor archive, when one is available:
    /// `MINIPRO_TEST_RAR=/path/to/xgpro_T76_V1321.rar cargo test -p minipro-db --features rar`
    #[test]
    fn reads_real_vendor_archive() {
        let Ok(path) = std::env::var("MINIPRO_TEST_RAR") else {
            eprintln!("skipped: set MINIPRO_TEST_RAR to a vendor .rar/.exe");
            return;
        };
        let db = RarDb::open(Path::new(&path)).expect("open vendor archive");
        assert!(db.all().len() > 1000, "catalog looks too small");

        let dev = db.get("M27C256B@DIP28").expect("known chip present");
        let algo = db
            .load_algorithm(dev)
            .expect("bitstream load")
            .expect("bitstream present");
        assert!(!algo.bitstream.is_empty());
    }
}
