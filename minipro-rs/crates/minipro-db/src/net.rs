//! `HttpDb` — the native database served from a mirror, **RAM-only**.
//!
//! Opt-in (`net` feature). `InfoICT76.dll` is fetched into memory, parsed, and
//! **discarded** — it is never written to disk. `.alg` bitstreams are likewise
//! fetched into RAM on demand (only the ones a read needs), decoded, and kept
//! only for the life of the operation. XGecu's proprietary files therefore
//! leave no trace on the user's disk.
//!
//! The mirror serves the *extracted* files — `<base>/InfoICT76.dll` and
//! `<base>/algoT76/<algo>.alg` — so no RAR decoding is needed. Provisioning is
//! meant to be explicit, version-pinned, and SHA-verifiable.
//!
//! Trade-off (accepted, by design): with no cache, each process re-fetches the
//! ~8 MB DLL once at startup and one small `.alg` per chip read. Nothing is
//! persisted.

use std::io::Read as _;

use minipro_core::device::{Algorithm, Device};
use minipro_core::error::{Error, FwVersion, Result};

use crate::{algorithm_name, decode_alg, ChipDb, DllDb, Search};

/// A [`DllDb`] catalog parsed from an in-RAM DLL, with bitstreams fetched over
/// HTTP on demand. Nothing is cached to disk.
pub struct HttpDb {
    inner: DllDb, // parsed from RAM; no local bitstream source
    base_url: String,
}

impl HttpDb {
    /// Fetch `InfoICT76.dll` from `<base_url>/InfoICT76.dll` into memory
    /// (optionally SHA-256 verified), parse it, and drop the bytes. No disk I/O.
    pub fn open(base_url: &str, dll_sha256: Option<&str>) -> Result<Self> {
        let base_url = base_url.trim_end_matches('/').to_string();
        let bytes = http_get(&format!("{base_url}/InfoICT76.dll"))?;
        if let Some(want) = dll_sha256 {
            verify_sha256(&bytes, want)?;
        }
        // `from_dll_bytes` consumes the Vec; after parsing, the DLL is gone.
        let inner = DllDb::from_dll_bytes(bytes)?;
        Ok(HttpDb { inner, base_url })
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
        // Fetch the bitstream straight into RAM and decode it — never cached.
        for remote in [format!("{name}.alg"), format!("T7_{name}.alg")] {
            if let Ok(bytes) = http_get(&format!("{}/algoT76/{remote}", self.base_url)) {
                return Ok(Some(Algorithm { name, bitstream: decode_alg(&bytes)? }));
            }
        }
        Ok(None) // no such bitstream on the mirror
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
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(verify_sha256(b"", empty).is_ok());
        assert!(verify_sha256(b"", empty.to_uppercase().as_str()).is_ok());
        assert_eq!(verify_sha256(b"tampered", empty).unwrap_err().code(), "format");
    }
}
