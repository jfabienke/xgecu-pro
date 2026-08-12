//! Per-programmer protocol drivers. Each implements the core traits over a
//! `Box<dyn Transport>`; the T76 is the reference driver (see [`t76`]).
#![forbid(unsafe_code)]

pub mod t76;

/// Detect the attached programmer and return the matching driver. Mirrors the C
/// `minipro_open` dispatch (a `match` on the reported model).
pub fn detect(
    transport: Box<dyn minipro_core::Transport>,
) -> minipro_core::Result<Box<dyn minipro_core::Programmer>> {
    // TODO: query identity, match model -> driver. For now, T76 only.
    Ok(Box::new(t76::T76::new(transport)))
}
