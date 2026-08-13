//! `HttpDb` — the native database served from a mirror. The **derived catalog
//! and the bitstreams are persisted; the proprietary DLL never is**, and only
//! the *latest* version is kept.
//!
//! Opt-in (`net` feature).
//!
//! - `InfoICT76.dll` is fetched into memory, parsed, and **dropped** — never
//!   written to disk. What persists is our own `catalog.postcard` (the parsed
//!   device catalog, the `infoic.xml` equivalent) plus, under `algoT76/`, the
//!   `.alg` bitstreams fetched on demand (the `algorithm.xml` equivalent).
//! - **Freshness:** the first run of each (UTC) day does one cheap `HEAD` on the
//!   DLL and compares its ETag/Last-Modified to the last-seen value. If the
//!   mirror published a new version, the catalog is rebuilt and the `.alg` cache
//!   is cleared (bitstreams are version-specific), so only the latest is kept.
//!   Same-version days, and offline runs, use the cache with no download.
//!
//! The mirror serves the extracted files — `<base>/InfoICT76.dll`,
//! `<base>/algoT76/<algo>.alg`.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use minipro_core::device::{Algorithm, Device};
use minipro_core::error::{Error, FwVersion, Result};

use crate::{algorithm_name, decode_alg, ChipDb, DllDb, Search};

const CATALOG_FILE: &str = "catalog.postcard";
const META_FILE: &str = "source.meta"; // "<version-tag>\n<utc-day>\n"

/// A [`DllDb`] catalog persisted from a mirror; the DLL is never stored, and a
/// once-a-day version check keeps only the latest version on disk.
pub struct HttpDb {
    inner: DllDb,
    base_url: String,
    cache_dir: PathBuf,
}

impl HttpDb {
    pub fn open(base_url: &str, cache_dir: &Path, dll_sha256: Option<&str>) -> Result<Self> {
        let base_url = base_url.trim_end_matches('/').to_string();
        std::fs::create_dir_all(cache_dir.join("algoT76"))?;
        let catalog = cache_dir.join(CATALOG_FILE);
        let dll_url = format!("{base_url}/InfoICT76.dll");
        let today = utc_day();
        let meta = read_meta(cache_dir); // Option<(version_tag, day)>

        // Decide whether to (re)build from the DLL, and carry the version tag if
        // the daily check already fetched it.
        let (rebuild, checked_tag): (bool, Option<String>) = if !catalog.is_file() {
            (true, None)
        } else if meta.as_ref().is_some_and(|(_, d)| *d == today) {
            (false, None) // already checked today — trust the cache, no network
        } else {
            // First run today: one HEAD to see if the mirror has a new version.
            match head_version(&dll_url) {
                Ok(cur) => {
                    let changed = meta.as_ref().map(|(t, _)| t != &cur).unwrap_or(true);
                    if !changed {
                        write_meta(cache_dir, &cur, today); // stamp today's check
                    }
                    (changed, Some(cur))
                }
                Err(_) => (false, None), // offline — keep using the cache
            }
        };

        let inner = if rebuild {
            let dll = http_get(&dll_url)?; // into RAM only
            if let Some(want) = dll_sha256 {
                verify_sha256(&dll, want)?;
            }
            let inner = DllDb::from_dll_bytes(dll)?; // `dll` consumed & dropped
            persist_catalog(&catalog, inner.all())?;
            clear_alg_cache(cache_dir)?; // stale bitstreams: only the latest is kept
            let tag = checked_tag.or_else(|| head_version(&dll_url).ok()).unwrap_or_default();
            write_meta(cache_dir, &tag, today);
            inner
        } else {
            let devices: Vec<Device> = postcard::from_bytes(&std::fs::read(&catalog)?)
                .map_err(|e| Error::Format(format!("cached catalog decode: {e}")))?;
            DllDb::from_devices(devices)
        };

        Ok(HttpDb { inner, base_url, cache_dir: cache_dir.to_path_buf() })
    }
}

impl ChipDb for HttpDb {
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
        let Some(name) = algorithm_name(dev) else {
            return Ok(None);
        };
        let local = self.cache_dir.join("algoT76").join(format!("{name}.alg"));
        if local.is_file() {
            let bytes = std::fs::read(&local)?;
            return Ok(Some(Algorithm { name, bitstream: decode_alg(&bytes)? }));
        }
        for remote in [format!("{name}.alg"), format!("T7_{name}.alg")] {
            if let Ok(bytes) = http_get(&format!("{}/algoT76/{remote}", self.base_url)) {
                let tmp = local.with_extension("alg.part");
                std::fs::write(&tmp, &bytes)?;
                std::fs::rename(&tmp, &local)?;
                return Ok(Some(Algorithm { name, bitstream: decode_alg(&bytes)? }));
            }
        }
        Ok(None)
    }
}

// ---- helpers ---------------------------------------------------------------

/// Days since the UNIX epoch (UTC). A cheap, dependency-free "which day is it".
fn utc_day() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() / 86_400).unwrap_or(0)
}

fn read_meta(dir: &Path) -> Option<(String, u64)> {
    let s = std::fs::read_to_string(dir.join(META_FILE)).ok()?;
    let mut lines = s.lines();
    let tag = lines.next()?.to_string();
    let day = lines.next()?.trim().parse().ok()?;
    Some((tag, day))
}

fn write_meta(dir: &Path, tag: &str, day: u64) {
    // Best-effort; a failed stamp just means we re-check tomorrow.
    let _ = std::fs::write(dir.join(META_FILE), format!("{tag}\n{day}\n"));
}

fn persist_catalog(path: &Path, devices: &[Device]) -> Result<()> {
    let blob = postcard::to_allocvec(devices)
        .map_err(|e| Error::Format(format!("catalog encode: {e}")))?;
    let tmp = path.with_extension("postcard.part");
    std::fs::write(&tmp, &blob)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Delete the cached `.alg` bitstreams (called on a version change).
fn clear_alg_cache(dir: &Path) -> Result<()> {
    let algo = dir.join("algoT76");
    if let Ok(entries) = std::fs::read_dir(&algo) {
        for e in entries.flatten() {
            if e.path().extension().is_some_and(|x| x == "alg" || x == "part") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

/// A version tag for the DLL, via `HEAD`: prefer ETag, then Last-Modified, then
/// Content-Length. Errors if the request fails (treated as "offline").
fn head_version(url: &str) -> Result<String> {
    let resp = ureq::head(url)
        .call()
        .map_err(|e| Error::Format(format!("HTTP HEAD {url}: {e}")))?;
    let tag = resp
        .header("etag")
        .or_else(|| resp.header("last-modified"))
        .or_else(|| resp.header("content-length"))
        .unwrap_or("")
        .to_string();
    Ok(tag)
}

fn http_get(url: &str) -> Result<Vec<u8>> {
    let resp = ureq::get(url)
        .call()
        .map_err(|e| Error::Format(format!("HTTP GET {url}: {e}")))?;
    let mut buf = Vec::new();
    resp.into_reader().read_to_end(&mut buf)?;
    Ok(buf)
}

fn verify_sha256(bytes: &[u8], want: &str) -> Result<()> {
    use sha2::{Digest, Sha256};
    let got: String = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    if got.eq_ignore_ascii_case(want) {
        Ok(())
    } else {
        Err(Error::Format(format!(
            "InfoICT76.dll SHA-256 mismatch: got {got}, want {want}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_gate() {
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(verify_sha256(b"", empty).is_ok());
        assert!(verify_sha256(b"", empty.to_uppercase().as_str()).is_ok());
        assert_eq!(verify_sha256(b"tampered", empty).unwrap_err().code(), "format");
    }

    #[test]
    fn meta_roundtrip() {
        let dir = std::env::temp_dir().join(format!("minipro-meta-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write_meta(&dir, "\"abc123\"", 20_314);
        assert_eq!(read_meta(&dir), Some(("\"abc123\"".to_string(), 20_314)));
        std::fs::remove_dir_all(&dir).ok();
    }
}
