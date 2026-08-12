//! `CachedDb` — provision the native database from a mirror over HTTP(S).
//!
//! Opt-in (`net` feature). Fetches `InfoICT76.dll` once into a local cache and
//! parses it with [`DllDb`]; `.alg` bitstreams are fetched **lazily, per chip**
//! (only when a read needs one), so first use pulls ~8 MB, not the 50 MB
//! `algoT76/` set. Everything caches to `cache_dir`, after which it works
//! offline. The mirror must serve the *extracted* files —
//! `<base>/InfoICT76.dll` and `<base>/algoT76/<algo>.alg` — so no RAR
//! decoding is needed (RAR-in-Rust being the one genuinely hard part).
//!
//! These are XGecu's proprietary files; provisioning is meant to be an explicit,
//! version-pinned, SHA-verified opt-in — never a silent per-run download.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use minipro_core::device::{Algorithm, Device};
use minipro_core::error::{Error, FwVersion, Result};

use crate::{algorithm_name, ChipDb, DllDb, Search};

/// A [`DllDb`] backed by a local cache that is provisioned from a mirror.
pub struct CachedDb {
    inner: DllDb,
    base_url: String,
    cache_dir: PathBuf,
}

impl CachedDb {
    /// Ensure `InfoICT76.dll` is cached in `cache_dir` (fetched from
    /// `<base_url>/InfoICT76.dll` if absent, optionally SHA-256 verified), then
    /// parse it. `.alg` bitstreams are fetched on demand by [`Self::load_algorithm`].
    pub fn provision(base_url: &str, cache_dir: &Path, dll_sha256: Option<&str>) -> Result<Self> {
        let base_url = base_url.trim_end_matches('/').to_string();
        std::fs::create_dir_all(cache_dir.join("algoT76"))?;

        let dll = cache_dir.join("InfoICT76.dll");
        if !dll.is_file() {
            let bytes = http_get(&format!("{base_url}/InfoICT76.dll"))?;
            if let Some(want) = dll_sha256 {
                verify_sha256(&bytes, want)?;
            }
            // Write atomically-ish (temp then rename) so a killed fetch can't
            // leave a truncated DLL in the cache.
            let tmp = dll.with_extension("dll.part");
            std::fs::write(&tmp, &bytes)?;
            std::fs::rename(&tmp, &dll)?;
        }

        // algoT76/ exists (created above), so DllDb picks it up as the bitstream
        // dir; fetched .alg files land there and resolve normally.
        let inner = DllDb::load(cache_dir)?;
        Ok(CachedDb { inner, base_url, cache_dir: cache_dir.to_path_buf() })
    }

    /// Fetch `<algo>.alg` into the cache if not already present (tries the
    /// `T7_` prefix too). Missing remotely is not an error — `inner` then
    /// returns `Ok(None)` like any absent bitstream.
    fn fetch_alg(&self, name: &str) -> Result<()> {
        let local = self.cache_dir.join("algoT76").join(format!("{name}.alg"));
        if local.is_file() {
            return Ok(());
        }
        for remote in [format!("{name}.alg"), format!("T7_{name}.alg")] {
            if let Ok(bytes) = http_get(&format!("{}/algoT76/{remote}", self.base_url)) {
                let tmp = local.with_extension("alg.part");
                std::fs::write(&tmp, &bytes)?;
                std::fs::rename(&tmp, &local)?;
                return Ok(());
            }
        }
        Ok(())
    }
}

impl ChipDb for CachedDb {
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
        if let Some(name) = algorithm_name(dev) {
            self.fetch_alg(&name)?; // populate the cache dir on demand
        }
        self.inner.load_algorithm(dev) // resolve from the now-populated cache
    }
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
        // sha256("") = e3b0c442...
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(verify_sha256(b"", empty).is_ok());
        assert!(verify_sha256(b"", empty.to_uppercase().as_str()).is_ok()); // case-insensitive
        assert_eq!(verify_sha256(b"tampered", empty).unwrap_err().code(), "format");
    }
}
