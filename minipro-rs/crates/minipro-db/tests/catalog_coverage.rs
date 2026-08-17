// SPDX-FileCopyrightText: 2026 John Fabienke
// SPDX-License-Identifier: MIT

//! How many distinct FPGA bitstreams the T76 catalog actually references, and
//! whether every one of them exists in the vendor archive.
//!
//! A referenced algorithm with no `.alg` file is a chip that cannot be
//! programmed — the failure surfaces only when a user selects that part, so it
//! is worth being able to check the whole catalog at once.
//!
//! Needs an unpacked vendor database:
//! ```text
//! MINIPRO_DB_DIR="$HOME/Library/Caches/minipro/xgpro" \
//!   cargo test -p minipro-db --test catalog_coverage -- --ignored --nocapture
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use minipro_db::{algorithm_name, ChipDb, DllDb};

/// The unpacked database directory, or `None` if the caller did not point at one.
fn db_dir() -> Option<PathBuf> {
    std::env::var_os("MINIPRO_DB_DIR").map(PathBuf::from)
}

#[test]
#[ignore = "requires an unpacked vendor database (set MINIPRO_DB_DIR)"]
fn report_bitstream_coverage() {
    let Some(dir) = db_dir() else {
        println!("set MINIPRO_DB_DIR to an unpacked Xgpro_T76 directory");
        return;
    };
    let db = DllDb::load(&dir).expect("load InfoICT76.dll");
    let algo_dir = dir.join("algoT76");

    // Every shipped bitstream, keyed by lowercased stem so case differences can
    // be told apart from genuine absence. `resolve_bitstream` joins an exact
    // path, so a case difference resolves on a case-insensitive filesystem
    // (macOS APFS) and fails on a case-sensitive one (Linux ext4).
    let mut shipped_by_lower: BTreeMap<String, String> = BTreeMap::new();
    if let Ok(rd) = std::fs::read_dir(&algo_dir) {
        for e in rd.flatten() {
            let f = e.file_name().to_string_lossy().to_string();
            if let Some(stem) = f.strip_suffix(".alg").or_else(|| f.strip_suffix(".ALG")) {
                let stem = stem.strip_prefix("T7_").unwrap_or(stem);
                shipped_by_lower.insert(stem.to_ascii_lowercase(), stem.to_string());
            }
        }
    }

    let devices = db.all();
    let mut per_algo: BTreeMap<String, usize> = BTreeMap::new();
    let mut no_algo = 0usize;
    for d in devices {
        match algorithm_name(d) {
            Some(name) => *per_algo.entry(name).or_default() += 1,
            None => no_algo += 1,
        }
    }

    // Three outcomes per referenced algorithm: exact file, case-only
    // difference (a latent Linux failure), or nothing at all.
    let mut exact = Vec::new();
    let mut case_only: Vec<(&String, &String)> = Vec::new();
    let mut missing: Vec<&String> = Vec::new();
    for name in per_algo.keys() {
        if algo_dir.join(format!("{name}.alg")).is_file()
            || algo_dir.join(format!("T7_{name}.alg")).is_file()
        {
            // Compare against the on-disk spelling: on a case-insensitive
            // filesystem `is_file` succeeds even when the case differs.
            match shipped_by_lower.get(&name.to_ascii_lowercase()) {
                Some(disk) if disk != name => case_only.push((name, disk)),
                _ => exact.push(name),
            }
        } else if let Some(disk) = shipped_by_lower.get(&name.to_ascii_lowercase()) {
            case_only.push((name, disk));
        } else {
            missing.push(name);
        }
    }

    let referenced_lower: BTreeSet<String> =
        per_algo.keys().map(|n| n.to_ascii_lowercase()).collect();
    let unreferenced: Vec<&String> = shipped_by_lower
        .iter()
        .filter(|(lower, _)| !referenced_lower.contains(*lower))
        .map(|(_, disk)| disk)
        .collect();

    let devices_for = |names: &[&String]| -> usize { names.iter().map(|n| per_algo[*n]).sum() };

    println!("devices in catalog:        {}", devices.len());
    println!("  with an algorithm:       {}", devices.len() - no_algo);
    println!("  logic/utility (none):    {no_algo}");
    println!("distinct algorithms used:  {}", per_algo.len());
    println!("  exact filename match:    {}", exact.len());
    println!(
        "  CASE-ONLY match:         {}  ({} devices — breaks on case-sensitive filesystems)",
        case_only.len(),
        case_only.iter().map(|(n, _)| per_algo[*n]).sum::<usize>()
    );
    println!(
        "  MISSING entirely:        {}  ({} devices)",
        missing.len(),
        devices_for(&missing)
    );
    println!("bitstreams shipped:        {}", shipped_by_lower.len());
    println!("  never referenced:        {}", unreferenced.len());

    if !case_only.is_empty() {
        println!("\ncase-only matches (derived -> on disk):");
        for (derived, disk) in &case_only {
            println!(
                "  {derived:<14} -> {disk:<14} ({} devices)",
                per_algo[*derived]
            );
        }
    }

    if !missing.is_empty() {
        println!("\nreferenced but absent:");
        for n in &missing {
            println!("  {n}  ({} devices)", per_algo[*n]);
            for d in devices
                .iter()
                .filter(|d| algorithm_name(d).as_deref() == Some(n.as_str()))
                .take(3)
            {
                println!(
                    "      {:<28} protocol=0x{:02x} variant=0x{:04x}",
                    d.name, d.protocol_id, d.variant
                );
            }
        }
    }

    let mut busiest: Vec<(&String, &usize)> = per_algo.iter().collect();
    busiest.sort_by(|a, b| b.1.cmp(a.1));
    println!("\nbusiest algorithms:");
    for (name, count) in busiest.iter().take(12) {
        println!("  {name:<14} {count:>6} devices");
    }

    println!("\nnever referenced ({}):", unreferenced.len());
    for n in &unreferenced {
        println!("  {n}");
    }

    // The invariant worth locking in. Absent bitstreams are the vendor's
    // business, but a name that differs only by case is ours: it resolves on a
    // case-insensitive filesystem and fails on a case-sensitive one, so a
    // macOS-only developer cannot see the breakage.
    assert!(
        case_only.is_empty(),
        "{} algorithm name(s) match a shipped bitstream only by case, covering {} devices — \
         these resolve on macOS/Windows and fail on Linux; fix ALGO_TABLE to the on-disk spelling",
        case_only.len(),
        case_only.iter().map(|(n, _)| per_algo[*n]).sum::<usize>(),
    );
}
