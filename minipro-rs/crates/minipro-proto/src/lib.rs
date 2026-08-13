//! Per-programmer protocol drivers. Each implements the core traits over a
//! `Box<dyn Transport>`; the T76 is the reference driver (see [`t76`]).
#![forbid(unsafe_code)]

pub mod t76;
pub mod wire;

/// Detect the attached programmer and return the matching driver. Mirrors the C
/// `minipro_open` dispatch (a `match` on the reported model).
pub fn detect(
    transport: Box<dyn minipro_core::Transport>,
) -> minipro_core::Result<Box<dyn minipro_core::Programmer>> {
    // TODO: query identity, match model -> driver. For now, T76 only.
    let mut t76 = t76::T76::new(transport);
    t76.query_info()?; // read firmware/serial/voltage; also verifies it's a T76
    Ok(Box::new(t76))
}
