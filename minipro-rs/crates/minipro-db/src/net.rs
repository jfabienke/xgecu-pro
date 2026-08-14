// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! `HttpDb` — the native database served from a mirror of extracted files.
//!
//! Opt-in (`net` feature).
//!
//! The cache holds **the vendor files themselves**, in the same layout the
//! default (vendor-archive) source produces: `InfoICT76.dll` beside an
//! `algoT76/` directory of `.alg` bitstreams. There is deliberately no derived
//! format — an earlier version persisted a `catalog.postcard` blob, which was
//! measured at only 6 ms faster to load than parsing the DLL (97 ms vs 103 ms
//! for 33,612 devices) while costing a versioned binary format, a schema-bump
//! discipline, and a forced re-download whenever that schema changed. Keeping
//! the source of truth means a format change just re-parses locally, and
//! `--db <cache>/mirror` is directly usable by this tool or any other.
//!
//! **Freshness:** the first run of each (UTC) day does one cheap `HEAD` on the
//! DLL and compares its ETag/Last-Modified to the last-seen value. If the
//! mirror published a new version the DLL is refetched and the `.alg` cache is
//! cleared (bitstreams are version-specific), so only the latest is kept.
//! Same-version days, and offline runs, use the cache with no download.
//!
//! The mirror serves the extracted files — `<base>/InfoICT76.dll`,
//! `<base>/algoT76/<algo>.alg`.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use minipro_core::device::{Algorithm, Device};
use minipro_core::error::{Error, FwVersion, Result};

use crate::{algorithm_name, decode_alg, ChipDb, DllDb, Search};

const META_FILE: &str = "source.meta"; // "<version-tag>\n<utc-day>\n"

/// Subdirectory holding this mirror's copy of the vendor files. Kept separate
/// from the default source's `xgpro/` so the two can never interleave a DLL
/// from one version with bitstreams from another — a mismatch would upload the
/// wrong FPGA bitstream for a chip.
const MIRROR_DIR: &str = "mirror";

/// A [`DllDb`] provisioned from a mirror, cached as the vendor files
/// themselves; a once-a-day version check keeps only the latest on disk.
pub struct HttpDb {
    inner: DllDb,
    base_url: String,
    cache_dir: PathBuf,
}

impl HttpDb {
    pub fn open(base_url: &str, cache_dir: &Path, dll_sha256: Option<&str>) -> Result<Self> {
        let base_url = base_url.trim_end_matches('/').to_string();
        let dir = cache_dir.join(MIRROR_DIR);
        std::fs::create_dir_all(dir.join("algoT76"))?;
        let dll_path = dir.join("InfoICT76.dll");
        let dll_url = format!("{base_url}/InfoICT76.dll");
        let today = utc_day();
        let meta = read_meta(&dir); // Option<(version_tag, day)>

        // Decide whether to (re)fetch the DLL, and carry the version tag if the
        // daily check already fetched it.
        let (rebuild, checked_tag): (bool, Option<String>) = if !dll_path.is_file() {
            (true, None)
        } else if meta.as_ref().is_some_and(|(_, d)| *d == today) {
            (false, None) // already checked today — trust the cache, no network
        } else {
            // First run today: one HEAD to see if the mirror has a new version.
            match head_version(&dll_url) {
                Ok(cur) => {
                    let changed = meta.as_ref().map(|(t, _)| t != &cur).unwrap_or(true);
                    if !changed {
                        write_meta(&dir, &cur, today); // stamp today's check
                    }
                    (changed, Some(cur))
                }
                Err(_) => (false, None), // offline — keep using the cache
            }
        };

        if rebuild {
            let dll = http_get(&dll_url)?;
            if let Some(want) = dll_sha256 {
                verify_sha256(&dll, want)?;
            }
            // Parse before storing: a corrupt download should not replace a
            // working cache.
            DllDb::from_dll_bytes(dll.clone())?;
            let tmp = dll_path.with_extension("dll.part");
            std::fs::write(&tmp, &dll)?;
            std::fs::rename(&tmp, &dll_path)?;
            clear_alg_cache(&dir)?; // stale bitstreams: only the latest is kept
            let tag = checked_tag
                .or_else(|| head_version(&dll_url).ok())
                .unwrap_or_default();
            write_meta(&dir, &tag, today);
        }

        Ok(HttpDb {
            inner: DllDb::load(&dir)?,
            base_url,
            cache_dir: dir,
        })
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
        match algorithm_name(dev) {
            Some(name) => self.load_algorithm_named(&name),
            None => Ok(None),
        }
    }

    fn load_algorithm_named(&self, name: &str) -> Result<Option<Algorithm>> {
        let name = name.to_string();
        let local = self.cache_dir.join("algoT76").join(format!("{name}.alg"));
        if local.is_file() {
            let bytes = std::fs::read(&local)?;
            return Ok(Some(Algorithm {
                name,
                bitstream: decode_alg(&bytes)?,
            }));
        }
        // Utility bitstreams (TestLgcPull, TTL1, …) live in the same algoT76/
        // as chip bitstreams — fetched by name, cached the same way.
        for remote in [format!("{name}.alg"), format!("T7_{name}.alg")] {
            if let Ok(bytes) = http_get(&format!("{}/algoT76/{remote}", self.base_url)) {
                let tmp = local.with_extension("alg.part");
                std::fs::write(&tmp, &bytes)?;
                std::fs::rename(&tmp, &local)?;
                return Ok(Some(Algorithm {
                    name,
                    bitstream: decode_alg(&bytes)?,
                }));
            }
        }
        Ok(None)
    }
}

// ---- helpers ---------------------------------------------------------------

/// Days since the UNIX epoch (UTC). A cheap, dependency-free "which day is it".
fn utc_day() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0)
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

/// Delete the cached `.alg` bitstreams (called on a version change).
fn clear_alg_cache(dir: &Path) -> Result<()> {
    let algo = dir.join("algoT76");
    if let Ok(entries) = std::fs::read_dir(&algo) {
        for e in entries.flatten() {
            if e.path()
                .extension()
                .is_some_and(|x| x == "alg" || x == "part")
            {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

/// A version tag for the DLL, via `HEAD`: prefer ETag, then Last-Modified, then
/// Content-Length. Errors if the request fails (treated as "offline").
fn head_version(url: &str) -> Result<String> {
    let resp = agent()
        .head(url)
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

/// One shared agent using the **platform** TLS stack (Security.framework on
/// macOS, SChannel on Windows, OpenSSL on Linux) instead of a bundled one.
fn agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        let mut b = ureq::AgentBuilder::new();
        if let Ok(tls) = native_tls::TlsConnector::new() {
            b = b.tls_connector(std::sync::Arc::new(tls));
        }
        b.build()
    })
}

pub(crate) fn http_get(url: &str) -> Result<Vec<u8>> {
    let resp = agent()
        .get(url)
        .call()
        .map_err(|e| Error::Format(format!("HTTP GET {url}: {e}")))?;
    let mut buf = Vec::new();
    resp.into_reader().read_to_end(&mut buf)?;
    Ok(buf)
}

fn verify_sha256(bytes: &[u8], want: &str) -> Result<()> {
    use sha2::{Digest, Sha256};
    let got: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
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
        assert_eq!(
            verify_sha256(b"tampered", empty).unwrap_err().code(),
            "format"
        );
    }

    #[test]
    fn meta_roundtrip() {
        let dir = std::env::temp_dir().join(format!("minipro-meta-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write_meta(&dir, "\"abc123\"", 20_314);
        assert_eq!(read_meta(&dir), Some(("\"abc123\"".to_string(), 20_314)));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The mirror cache holds the vendor files themselves, in the same shape
    /// the default source produces, so `--db <cache>/mirror` is usable
    /// directly. Replaces an older test for a derived `catalog.postcard`
    /// format that was measured to be only ~6 ms faster to load than parsing
    /// the DLL, and was dropped rather than versioned forever.
    #[test]
    fn alg_cache_is_cleared_on_a_version_change() {
        let dir = std::env::temp_dir().join(format!("minipro-alg-{}", std::process::id()));
        let algs = dir.join("algoT76");
        std::fs::create_dir_all(&algs).unwrap();
        std::fs::write(algs.join("ROM28P32.alg"), b"stale").unwrap();
        std::fs::write(dir.join("InfoICT76.dll"), b"kept").unwrap();

        clear_alg_cache(&dir).unwrap();

        assert!(
            !algs.join("ROM28P32.alg").exists(),
            "bitstreams are version-specific and must not survive a DLL change"
        );
        assert!(
            dir.join("InfoICT76.dll").exists(),
            "clearing bitstreams must not remove the catalog itself"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
