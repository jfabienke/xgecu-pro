// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! Unpacking XGecu's installer with whatever RAR-capable tool the system
//! already has.
//!
//! The vendor ships the chip database as `xgpro_T76_V*.rar` → a WinRAR SFX
//! executable → `InfoICT76.dll` + `algoT76/*.alg`, so getting at it needs a
//! RAR5 decoder. Rather than linking one — the usable implementations are
//! either non-MIT (unrar) or C (libarchive), and bundling either would change
//! the licence of every binary — this drives an extractor that is already
//! installed. That keeps the shipped artifact pure-Rust and MIT.
//!
//! Tools are probed in order of how well they are known to work here:
//!
//! | Tool | Notes |
//! |---|---|
//! | `bsdtar` | libarchive's RAR5 reader; **ships with macOS**, `libarchive-tools` on Debian |
//! | `unar` | The Unarchiver / XADMaster |
//! | `unrar` | RARLAB's own extractor |
//!
//! **`7z` is deliberately excluded.** Both p7zip and the official `7zz` list
//! the archive happily and then write **zero-byte files** — a silent corruption
//! that looks like success. Verified on this vendor archive; using it would be
//! worse than reporting no extractor at all.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use minipro_core::error::{Error, Result};

/// Leading bytes that mark a file as a vendor archive we can unpack.
///
/// Content beats extension here: XGecu's own naming has varied (`.rar` today,
/// a bare SFX `.exe` inside it), users rename downloads, and a browser may
/// serve the installer as `.bin`. Sniffing also rejects the opposite mistake —
/// an unrelated `.exe` — before spawning an extractor on it.
const ARCHIVE_MAGIC: &[(&[u8], &str)] = &[
    (&[0x52, 0x61, 0x72, 0x21, 0x1a, 0x07, 0x01, 0x00], "RAR5"),
    (&[0x52, 0x61, 0x72, 0x21, 0x1a, 0x07, 0x00], "RAR4"),
    // WinRAR SFX: a PE executable with an archive appended.
    (&[0x4d, 0x5a], "PE/SFX"),
];

/// Does `path` look like a vendor archive, judged by its first bytes?
pub fn is_vendor_archive(path: &Path) -> bool {
    archive_kind(path).is_some()
}

/// The archive flavour of `path`, or `None` if it is not one (or unreadable).
pub fn archive_kind(path: &Path) -> Option<&'static str> {
    if !path.is_file() {
        return None;
    }
    let mut head = [0u8; 8];
    let n = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .ok()?;
    let head = head.get(..n)?;
    ARCHIVE_MAGIC
        .iter()
        .find(|(magic, _)| head.starts_with(magic))
        .map(|(_, name)| *name)
}

/// A RAR-capable extractor found on the system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extractor {
    /// `bsdtar -xf <archive>` (libarchive).
    BsdTar,
    /// `unar -q -f -o <dir> <archive>`.
    Unar,
    /// `unrar x -y <archive> <dir>/`.
    Unrar,
}

impl Extractor {
    pub fn program(self) -> &'static str {
        match self {
            Extractor::BsdTar => "bsdtar",
            Extractor::Unar => "unar",
            Extractor::Unrar => "unrar",
        }
    }

    /// Can this tool extract a named subset instead of the whole archive?
    /// `unar` has no member selection, so it always unpacks everything and
    /// relies on [`prune`] afterwards.
    fn supports_members(self) -> bool {
        matches!(self, Extractor::BsdTar | Extractor::Unrar)
    }

    /// Extract `archive` into `dest` (which must exist), optionally limited to
    /// `members`. Selecting members matters: the installer carries ~150 MB of
    /// drivers, language packs and GUI we never read.
    fn run(self, archive: &Path, dest: &Path, members: &[&str]) -> Result<()> {
        let mut cmd = Command::new(self.program());
        let members = if self.supports_members() {
            members
        } else {
            &[][..]
        };
        match self {
            // bsdtar has no output-directory flag; it extracts into the cwd.
            Extractor::BsdTar => {
                cmd.arg("-xf").arg(archive).args(members).current_dir(dest);
            }
            Extractor::Unar => {
                cmd.args(["-q", "-f", "-o"]).arg(dest).arg(archive);
            }
            Extractor::Unrar => {
                cmd.args(["x", "-y"]).arg(archive).args(members).arg(dest);
            }
        }
        let out = cmd
            .output()
            .map_err(|e| Error::Format(format!("could not run {}: {e}", self.program())))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(Error::Format(format!(
                "{} failed on {}: {}",
                self.program(),
                archive.display(),
                err.trim().lines().next().unwrap_or("(no output)")
            )));
        }
        Ok(())
    }
}

/// The first usable extractor on this system, if any.
pub fn find() -> Option<Extractor> {
    [Extractor::BsdTar, Extractor::Unar, Extractor::Unrar]
        .into_iter()
        .find(|e| probe(e.program()))
}

/// Is `program` present and runnable? Probed by execution rather than a PATH
/// search, so a shadowed or non-executable entry does not count as available.
fn probe(program: &str) -> bool {
    // `unar` has no --version; every candidate accepts being run bare and
    // exits, which is enough to prove it exists and starts.
    Command::new(program)
        .arg("--version")
        .output()
        .map(|o| o.status.success() || !o.stdout.is_empty() || !o.stderr.is_empty())
        .unwrap_or(false)
}

/// Advice shown when no extractor is installed. Names the exact packages,
/// because "install a RAR tool" is not actionable on an unfamiliar system.
pub fn install_hint() -> String {
    let install = if cfg!(target_os = "macos") {
        "  brew install unar          (bsdtar normally ships with macOS already)"
    } else if cfg!(target_os = "windows") {
        "  winget install RARLab.WinRAR   — or install 7-Zip and extract by hand"
    } else {
        "  apt install libarchive-tools   (bsdtar)\n  \
         dnf install bsdtar             — or `apt install unar`"
    };
    format!(
        "no RAR-capable extractor found. The chip database ships inside XGecu's\n\
         installer archive, which needs one of: bsdtar, unar, unrar.\n\n\
         Install one:\n{install}\n\n\
         Note: 7-Zip cannot be used — it lists the archive but writes zero-byte\n\
         files, which looks like success.\n\n\
         Or skip the download entirely: extract the archive on any machine and\n\
         point at the result:\n  \
         minipro --db /path/to/Xgpro_T76 …"
    )
}

/// Unpack the vendor archive into `dest`, returning the directory that actually
/// holds `InfoICT76.dll`.
///
/// The archive nests: the outer `.rar` contains a WinRAR SFX `.exe`, which
/// contains the payload — so this extracts, looks for the DLL, and if it only
/// found another executable, extracts that too.
pub fn unpack_vendor_archive(archive: &Path, dest: &Path) -> Result<PathBuf> {
    let tool = find().ok_or_else(|| Error::Format(install_hint()))?;
    std::fs::create_dir_all(dest)?;
    // The outer archive holds only the installer, so nothing to select there.
    tool.run(archive, dest, &[])?;

    if let Some(dir) = find_database_dir(dest) {
        prune(&dir);
        return Ok(dir);
    }
    // Outer layer only yielded the installer; unpack that as well.
    let inner = find_installer_exe(dest).ok_or_else(|| {
        Error::Format(format!(
            "{} unpacked {} but produced neither InfoICT76.dll nor an installer .exe",
            tool.program(),
            archive.display()
        ))
    })?;
    // Inner layer: take only the database, not the whole vendor install.
    tool.run(&inner, dest, &["InfoICT76.dll", "algoT76"])?;
    let _ = std::fs::remove_file(&inner); // the 65 MB installer has served its purpose
    let dir = find_database_dir(dest).ok_or_else(|| {
        Error::Format(format!(
            "unpacked {} but found no InfoICT76.dll — if you used 7-Zip earlier, \
             its zero-byte output may be cached; clear {} and retry",
            archive.display(),
            dest.display()
        ))
    })?;
    prune(&dir);
    Ok(dir)
}

/// Files the database actually needs. The vendor installer also carries
/// drivers, language packs and the GUI — ~150 MB we would otherwise keep
/// forever in a cache directory.
const KEEP: &[&str] = &[
    "InfoICT76.dll",
    "algoT76",
    "algorithm.xml",
    "infoic.xml",
    "logicic.xml",
];

/// Drop everything in `dir` that the database does not use. Best-effort: a
/// failure to prune is not a failure to load, so errors are ignored.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let keep = entry
            .file_name()
            .to_str()
            .is_some_and(|n| KEEP.iter().any(|k| k.eq_ignore_ascii_case(n)));
        if keep {
            continue;
        }
        let path = entry.path();
        let _ = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
    }
}

/// Directory containing a *non-empty* `InfoICT76.dll`, searched one level deep.
/// The size check is what catches 7-Zip's zero-byte output.
fn find_database_dir(root: &Path) -> Option<PathBuf> {
    let usable = |dir: &Path| {
        std::fs::metadata(dir.join("InfoICT76.dll"))
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false)
    };
    if usable(root) {
        return Some(root.to_path_buf());
    }
    std::fs::read_dir(root)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .find(|p| usable(p))
}

/// The nested installer executable, searched one level deep.
fn find_installer_exe(root: &Path) -> Option<PathBuf> {
    fn exe(p: &Path) -> bool {
        p.is_file()
            && p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ok()?.filter_map(|e| e.ok()) {
            let p = entry.path();
            if exe(&p) {
                return Some(p);
            }
            if p.is_dir() && dir == root {
                stack.push(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_detection_reads_content_not_extension() {
        let dir = tempfile::tempdir().unwrap();
        // Correct content, misleading name — must still be recognised.
        let odd = dir.path().join("download.bin");
        std::fs::write(&odd, b"Rar!\x1a\x07\x01\x00rest").unwrap();
        assert_eq!(archive_kind(&odd), Some("RAR5"));

        // Right extension, wrong content — must be rejected before we spawn
        // an extractor on it.
        let fake = dir.path().join("installer.exe");
        std::fs::write(&fake, b"#!/bin/sh\necho not an archive\n").unwrap();
        assert_eq!(archive_kind(&fake), None);

        // A real SFX is a PE image.
        let sfx = dir.path().join("setup.exe");
        std::fs::write(&sfx, b"MZ\x90\x00padding").unwrap();
        assert_eq!(archive_kind(&sfx), Some("PE/SFX"));
    }

    #[test]
    fn archive_detection_tolerates_tiny_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let tiny = dir.path().join("t");
        std::fs::write(&tiny, b"R").unwrap();
        assert_eq!(archive_kind(&tiny), None);
        assert_eq!(archive_kind(&dir.path().join("nope")), None);
        assert_eq!(
            archive_kind(dir.path()),
            None,
            "a directory is not an archive"
        );
    }

    #[test]
    fn install_hint_is_actionable() {
        let h = install_hint();
        assert!(h.contains("bsdtar"), "must name a tool: {h}");
        assert!(h.contains("--db"), "must offer the manual fallback: {h}");
        assert!(
            h.contains("7-Zip"),
            "must warn about the zero-byte trap: {h}"
        );
    }

    #[test]
    fn zero_byte_dll_is_not_accepted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("InfoICT76.dll"), b"").unwrap();
        assert!(
            find_database_dir(dir.path()).is_none(),
            "an empty DLL is 7-Zip's failure mode and must not count"
        );
    }

    #[test]
    fn nonempty_dll_is_found_one_level_deep() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("Xgpro_T76_V1321");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("InfoICT76.dll"), b"x").unwrap();
        assert_eq!(
            find_database_dir(dir.path()).as_deref(),
            Some(sub.as_path())
        );
    }

    /// At least one extractor should exist in a normal dev environment; this is
    /// informational rather than a hard requirement.
    #[test]
    fn probe_reports_something_on_this_machine() {
        eprintln!("extractor found: {:?}", find());
    }
}
