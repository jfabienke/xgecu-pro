//! Orchestration: the block loop, progress reporting, and built-in
//! verification — written once here, generic over any `dyn Programmer` and
//! `dyn Reporter`, so drivers stay small (they only implement `*_block`).

use crate::device::{Image, Region};
use crate::error::{Error, Result};
use crate::programmer::{Programmer, Session};
use crate::report::{Event, Reporter};

/// Read a whole region, looping block requests and reporting progress. This is
/// where a driver's block-granular [`crate::caps::MemoryOps`] becomes a
/// user-level "read the chip".
pub fn read_region(
    prog: &mut dyn Programmer,
    _s: &Session,
    region: Region,
    rep: &mut dyn Reporter,
) -> Result<Image> {
    let _mem = prog.memory().ok_or(Error::Unsupported("memory ops"))?;
    rep.event(&Event::Progress { done: 0, total: region.len });
    // TODO: loop BlockReqs over `region`, call mem.read_block, append, report.
    todo!("block loop + progress")
}

/// Read twice and confirm byte-identical — the "is this dump trustworthy?"
/// check, promoted to a first-class operation (our AHA-1542CP lesson).
pub fn read_verified(
    prog: &mut dyn Programmer,
    s: &Session,
    region: Region,
    rep: &mut dyn Reporter,
) -> Result<(Image, bool)> {
    let a = read_region(prog, s, region, rep)?;
    let b = read_region(prog, s, region, rep)?;
    let stable = a.bytes == b.bytes;
    Ok((a, stable))
}

/// Write a region and (unless suppressed) verify by read-back.
pub fn write_region(
    prog: &mut dyn Programmer,
    _s: &Session,
    _region: Region,
    _image: &Image,
    _rep: &mut dyn Reporter,
) -> Result<()> {
    let _mem = prog.memory().ok_or(Error::Unsupported("memory ops"))?;
    todo!("write loop + verify")
}
