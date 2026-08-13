//! One event stream, three renderers. The core never prints — it emits
//! [`Event`]s during an op and a terminal [`Outcome`]. `Reporter` impls live in
//! `minipro-cli` (human / JSON / TUI). With the default-on `serde` feature,
//! `Event`/`Warning` derive `Serialize` and `Outcome` carries a hand-written
//! `Serialize` that emits the exact documented JSON schema (see
//! `docs/rust-redesign.md`: `{"op":"read","ok":true,"crc32":"59318a17",…}`),
//! so the JSON reporter is just `serde_json::to_string(&outcome)`.

use crate::transport::LinkSpeed;
use std::borrow::Cow;

/// A non-terminal progress/diagnostic event.
///
/// Serializes externally tagged, snake_case: `{"progress":{"done":0,"total":8}}`,
/// `{"warn":…}`, `{"note":"…"}`.
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Event {
    Progress { done: u64, total: u64 },
    Warn(Warning),
    Note(Cow<'static, str>),
}

/// Structured warnings (so JSON can render a stable shape, not free text).
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum Warning {
    FirmwareMismatch,
    BadContact(Vec<u8>),
    ChipIdMismatch { expected: u32, got: u32 },
}

/// The terminal result of an operation. The JSON reporter emits this as the
/// final NDJSON line — `Serialize` (feature `serde`) produces the documented
/// agent-facing schema: hex as bare lowercase strings, `link` as `fs`/`hs`/`ss`.
pub enum Outcome {
    /// A completed read, carrying the built-in verification fields.
    Read {
        device: String,
        bytes: u64,
        crc32: u32,
        sha256: [u8; 32],
        reads: u8,
        stable: bool,
        link: LinkSpeed,
    },
    /// Programmer/chip status (`minipro info`).
    Info {
        model: String,
        firmware: String,
        firmware_expected: String,
        serial: String,
        mfg_date: String,
        device_code: String,
        link: LinkSpeed,
        vcc: f32,
    },
    /// A generic success with no payload (erase, write).
    Ok { op: &'static str },
}

#[cfg(feature = "serde")]
impl serde::Serialize for Outcome {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        fn hex(bytes: &[u8]) -> String {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        }
        match self {
            Outcome::Read {
                device,
                bytes,
                crc32,
                sha256,
                reads,
                stable,
                link,
            } => {
                let mut m = ser.serialize_map(Some(9))?;
                m.serialize_entry("op", "read")?;
                m.serialize_entry("ok", &true)?;
                m.serialize_entry("dev", device)?;
                m.serialize_entry("bytes", bytes)?;
                m.serialize_entry("crc32", &format!("{crc32:08x}"))?;
                m.serialize_entry("sha256", &hex(sha256))?;
                m.serialize_entry("reads", reads)?;
                m.serialize_entry("stable", stable)?;
                m.serialize_entry("link", link)?;
                m.end()
            }
            Outcome::Info {
                model,
                firmware,
                firmware_expected,
                serial,
                mfg_date,
                device_code,
                link,
                vcc,
            } => {
                let mut m = ser.serialize_map(Some(10))?;
                m.serialize_entry("op", "info")?;
                m.serialize_entry("ok", &true)?;
                m.serialize_entry("model", model)?;
                m.serialize_entry("fw", firmware)?;
                m.serialize_entry("fw_expected", firmware_expected)?;
                m.serialize_entry("serial", serial)?;
                m.serialize_entry("mfg_date", mfg_date)?;
                m.serialize_entry("device_code", device_code)?;
                m.serialize_entry("link", link)?;
                m.serialize_entry("vcc", vcc)?;
                m.end()
            }
            Outcome::Ok { op } => {
                let mut m = ser.serialize_map(Some(2))?;
                m.serialize_entry("op", op)?;
                m.serialize_entry("ok", &true)?;
                m.end()
            }
        }
    }
}

/// Renders the event stream. Object-safe and `Send` so the TUI can receive it
/// over a channel from a worker thread.
pub trait Reporter: Send {
    fn event(&mut self, ev: &Event);
    fn finish(&mut self, out: &Outcome);
}

/// A no-op reporter, useful in tests and non-interactive paths.
pub struct NullReporter;
impl Reporter for NullReporter {
    fn event(&mut self, _ev: &Event) {}
    fn finish(&mut self, _out: &Outcome) {}
}

#[cfg(all(test, feature = "serde"))]
mod tests {
    use super::*;

    #[test]
    fn outcome_read_serializes_to_documented_schema() {
        let out = Outcome::Read {
            device: "M27C256B@DIP28".into(),
            bytes: 32768,
            crc32: 0x5931_8a17,
            sha256: [0xab; 32],
            reads: 2,
            stable: true,
            link: LinkSpeed::High,
        };
        let v: serde_json::Value = serde_json::to_value(&out).unwrap();
        assert_eq!(v["op"], "read");
        assert_eq!(v["ok"], true);
        assert_eq!(v["dev"], "M27C256B@DIP28");
        assert_eq!(v["bytes"], 32768);
        assert_eq!(v["crc32"], "59318a17");
        assert_eq!(v["sha256"], "ab".repeat(32));
        assert_eq!(v["reads"], 2);
        assert_eq!(v["stable"], true);
        assert_eq!(v["link"], "hs");
    }

    #[test]
    fn outcome_ok_and_info_serialize() {
        let v: serde_json::Value = serde_json::to_value(Outcome::Ok { op: "erase" }).unwrap();
        assert_eq!(v, serde_json::json!({"op": "erase", "ok": true}));

        let out = Outcome::Info {
            model: "T76".into(),
            firmware: "00.1.17".into(),
            firmware_expected: "00.1.17".into(),
            serial: "SN123".into(),
            mfg_date: "20240101".into(),
            device_code: "T76".into(),
            link: LinkSpeed::Super,
            vcc: 5.0,
        };
        let v: serde_json::Value = serde_json::to_value(&out).unwrap();
        assert_eq!(v["op"], "info");
        assert_eq!(v["link"], "ss");
        assert_eq!(v["vcc"], 5.0);
        assert_eq!(v["serial"], "SN123");
        assert_eq!(v["mfg_date"], "20240101");
        assert_eq!(v["device_code"], "T76");
    }

    #[test]
    fn events_serialize_externally_tagged() {
        let v = serde_json::to_value(Event::Progress { done: 4, total: 10 }).unwrap();
        assert_eq!(v, serde_json::json!({"progress": {"done": 4, "total": 10}}));

        let v = serde_json::to_value(Event::Warn(Warning::BadContact(vec![3, 4]))).unwrap();
        assert_eq!(v, serde_json::json!({"warn": {"bad_contact": [3, 4]}}));

        let v = serde_json::to_value(Event::Note("hello".into())).unwrap();
        assert_eq!(v, serde_json::json!({"note": "hello"}));
    }
}
