// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! Zero-setup chip database: fetch XGecu's own installer archive from the
//! community mirror and read the database straight out of it.
//!
//! Requires both `net` and `rar`.
//!
//! This is the default source, so that `minipro read …` works on a fresh
//! machine with no manual download-and-extract step. What it fetches is the
//! **vendor's own installer** (`xgpro_T76_V*.rar`) from the long-standing
//! community mirror — this project redistributes nothing itself, which is why
//! it points at the vendor's distribution channel rather than hosting a
//! derived copy of `InfoICT76.dll`.
//!
//! The archive is cached verbatim, then unpacked once with whatever
//! RAR-capable tool the system already has (see [`crate::extract`]) — no RAR
//! decoder is linked in, so the shipped binary stays pure-Rust and MIT.
//!
//! Every failure here is diagnosable: [`crate::vendor::FetchError`] distinguishes "you are
//! offline" from "the mirror moved" from "the download is corrupt", and each
//! renders with the concrete next step, because the fallback (`--db <dir>`)
//! only helps if the user knows to reach for it.

use std::path::{Path, PathBuf};

use minipro_core::error::{Error, Result};

use crate::{extract, DllDb};

/// Vendor installer used by default. Pinned deliberately: a floating "latest"
/// would silently change the chip database — and its firmware pairing —
/// underneath a user mid-session.
pub const DEFAULT_VENDOR_ARCHIVE: &str =
    "https://github.com/Kreeblah/XGecu_Software/raw/master/Xgpro/13/xgpro_T76_V1321.rar";

/// Local cache filename for the fetched archive.
const ARCHIVE_FILE: &str = "xgpro_vendor.rar";

/// RAR5 / RAR4 signatures — a cheap integrity gate that catches the common
/// failure of a mirror serving an HTML error page with a 200 status.
const RAR5_MAGIC: &[u8] = b"Rar!\x1a\x07\x01\x00";
const RAR4_MAGIC: &[u8] = b"Rar!\x1a\x07\x00";

/// Smallest plausible vendor archive; anything less is a truncated download.
const MIN_ARCHIVE_BYTES: u64 = 1 << 20;

/// What went wrong obtaining the vendor archive. Kept separate from
/// [`minipro_core::error::Error`] so the caller can decide whether to fall
/// back to a local database before turning this into a user-facing message.
#[derive(Debug)]
pub enum FetchError {
    /// The request never completed — no DNS, no route, TLS refused, timeout.
    Offline { url: String, detail: String },
    /// The server answered, but not with the archive (404 after a version
    /// bump, 5xx, or a captive-portal HTML page).
    BadResponse { url: String, detail: String },
    /// Downloaded bytes are not a RAR archive, or are impossibly short.
    Corrupt { url: String, detail: String },
    /// The cache directory could not be created or written.
    Cache { path: PathBuf, detail: String },
}

impl FetchError {
    /// A message that says what happened *and* what to do about it. The
    /// fallback is only useful if the user is told it exists.
    pub fn explain(&self) -> String {
        match self {
            FetchError::Offline { url, detail } => format!(
                "could not reach the chip-database mirror ({url}): {detail}\n\
                 The database is only needed to look up chips — `minipro info` works offline.\n\
                 If you are offline or behind a proxy, download the archive yourself and pass it:\n  \
                 minipro --db /path/to/xgpro_T76_V1321.rar …"
            ),
            FetchError::BadResponse { url, detail } => format!(
                "the chip-database mirror answered but did not serve the archive ({url}): {detail}\n\
                 The mirror has most likely reorganised or published a newer version.\n\
                 Point at another copy with --db-url, or download the archive and pass it:\n  \
                 minipro --db /path/to/xgpro_T76_V1321.rar …"
            ),
            FetchError::Corrupt { url, detail } => format!(
                "the download from {url} is not a usable vendor archive: {detail}\n\
                 The bad copy was not kept; re-running will fetch it again.\n\
                 If it keeps failing, download the archive yourself and pass it:\n  \
                 minipro --db /path/to/xgpro_T76_V1321.rar …"
            ),
            FetchError::Cache { path, detail } => format!(
                "cannot write the download cache at {}: {detail}\n\
                 Set XDG_CACHE_HOME to a writable directory, or skip the download entirely:\n  \
                 minipro --db /path/to/Xgpro_T76 …",
                path.display()
            ),
        }
    }
}

impl From<FetchError> for Error {
    fn from(e: FetchError) -> Error {
        Error::Format(e.explain())
    }
}

/// Path of the cached archive, if a plausible one is already present.
pub fn cached_archive(cache_dir: &Path) -> Option<PathBuf> {
    let p = cache_dir.join(ARCHIVE_FILE);
    let big_enough = std::fs::metadata(&p)
        .map(|m| m.is_file() && m.len() >= MIN_ARCHIVE_BYTES)
        .unwrap_or(false);
    big_enough.then_some(p)
}

/// Ensure the vendor archive is cached locally, downloading it if needed, and
/// return its path. A cached archive is reused without touching the network.
pub fn ensure_archive(cache_dir: &Path, url: &str) -> std::result::Result<PathBuf, FetchError> {
    if let Some(hit) = cached_archive(cache_dir) {
        return Ok(hit);
    }
    std::fs::create_dir_all(cache_dir).map_err(|e| FetchError::Cache {
        path: cache_dir.to_path_buf(),
        detail: e.to_string(),
    })?;

    let bytes = crate::net::http_get(url).map_err(|e| classify(url, &e))?;

    if (bytes.len() as u64) < MIN_ARCHIVE_BYTES {
        return Err(FetchError::Corrupt {
            url: url.to_string(),
            detail: format!(
                "only {} bytes — expected at least {} MiB",
                bytes.len(),
                MIN_ARCHIVE_BYTES / (1 << 20)
            ),
        });
    }
    if !bytes.starts_with(RAR5_MAGIC) && !bytes.starts_with(RAR4_MAGIC) {
        return Err(FetchError::Corrupt {
            url: url.to_string(),
            detail: "the response is not a RAR archive (an error page or redirect?)".into(),
        });
    }

    // Write via a temporary file so an interrupted run never leaves a partial
    // archive that a later run would trust.
    let final_path = cache_dir.join(ARCHIVE_FILE);
    let tmp = final_path.with_extension("rar.part");
    std::fs::write(&tmp, &bytes).map_err(|e| FetchError::Cache {
        path: tmp.clone(),
        detail: e.to_string(),
    })?;
    std::fs::rename(&tmp, &final_path).map_err(|e| FetchError::Cache {
        path: final_path.clone(),
        detail: e.to_string(),
    })?;
    Ok(final_path)
}

/// Where the unpacked database lives inside the cache.
const UNPACK_DIR: &str = "xgpro";

/// Open the default database: an already-unpacked copy if present, otherwise
/// fetch the vendor archive and unpack it once.
///
/// Returns `Err(FetchError)` only for download/cache problems; a missing
/// extractor or a failed unpack surfaces as `Ok(Err(_))`-style [`Error`] via
/// the second result, so the caller can tell "could not get the file" from
/// "got the file but cannot open it".
pub fn open(cache_dir: &Path, url: &str) -> std::result::Result<Result<DllDb>, FetchError> {
    let unpacked = cache_dir.join(UNPACK_DIR);
    // Already unpacked by an earlier run: no network, no extractor needed.
    if let Ok(db) = DllDb::load(&unpacked) {
        return Ok(Ok(db));
    }
    let archive = ensure_archive(cache_dir, url)?;
    let opened =
        extract::unpack_vendor_archive(&archive, &unpacked).and_then(|dir| DllDb::load(&dir));
    if opened.is_ok() {
        // The archive has served its purpose; the unpacked database is what
        // later runs use, and keeping both doubles the cache for nothing.
        let _ = std::fs::remove_file(&archive);
    }
    Ok(opened)
}

/// Map a transport error onto the taxonomy. ureq reports HTTP status and
/// connection failures through the same `Display`, so this keys off the shape
/// of the message: unreachable-host wording means offline, anything mentioning
/// a status code means the server answered.
fn classify(url: &str, e: &Error) -> FetchError {
    let detail = tidy(&e.to_string(), url);
    let lower = detail.to_ascii_lowercase();
    let answered = lower.contains("status code")
        || lower.contains("http status")
        || lower.contains("404")
        || lower.contains("403")
        || lower.contains("500")
        || lower.contains("503");
    if answered {
        FetchError::BadResponse {
            url: url.to_string(),
            detail,
        }
    } else {
        FetchError::Offline {
            url: url.to_string(),
            detail,
        }
    }
}

/// Strip the layers of context the transport stack has already added — the
/// `Error` category prefix and repeated copies of the URL — so the reported
/// detail is the actual cause rather than a chain of restatements.
fn tidy(detail: &str, url: &str) -> String {
    let mut d = detail.trim().to_string();
    for prefix in ["format: ", "io: ", "usb transport: "] {
        if let Some(rest) = d.strip_prefix(prefix) {
            d = rest.to_string();
        }
    }
    d = d.replace(&format!("HTTP GET {url}: "), "");
    while let Some(rest) = d.strip_prefix(&format!("{url}: ")) {
        d = rest.to_string();
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_archive_ignores_truncated_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(ARCHIVE_FILE), b"tiny").unwrap();
        assert!(
            cached_archive(dir.path()).is_none(),
            "a truncated archive must not count as a cache hit"
        );
    }

    #[test]
    fn every_failure_names_a_next_step() {
        let cases = [
            FetchError::Offline {
                url: "https://x/".into(),
                detail: "dns error".into(),
            },
            FetchError::BadResponse {
                url: "https://x/".into(),
                detail: "status code 404".into(),
            },
            FetchError::Corrupt {
                url: "https://x/".into(),
                detail: "not a RAR".into(),
            },
            FetchError::Cache {
                path: "/nope".into(),
                detail: "permission denied".into(),
            },
        ];
        for c in cases {
            let msg = c.explain();
            assert!(msg.contains("--db"), "no fallback offered in: {msg}");
            assert!(msg.len() > 80, "message too terse to act on: {msg}");
        }
    }

    #[test]
    fn tidy_removes_restated_context() {
        let raw = "format: HTTP GET https://x/a.rar: https://x/a.rar: Dns Failed: nodename";
        assert_eq!(tidy(raw, "https://x/a.rar"), "Dns Failed: nodename");
    }

    #[test]
    fn classify_distinguishes_offline_from_server_answered() {
        let offline = classify("u", &Error::Format("dns error: no record".into()));
        assert!(matches!(offline, FetchError::Offline { .. }));
        let answered = classify("u", &Error::Format("status code 404".into()));
        assert!(matches!(answered, FetchError::BadResponse { .. }));
    }
}
